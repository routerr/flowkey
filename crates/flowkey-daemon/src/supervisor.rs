use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use flowkey_config::Config;
use tokio::task::JoinHandle;
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;
use tracing::{error, warn};

pub struct DaemonHandle {
    shutdown: CancellationToken,
    join_handle: Arc<Mutex<Option<JoinHandle<()>>>>,
    running: Arc<AtomicBool>,
    restart_count: Arc<AtomicUsize>,
}

impl DaemonHandle {
    pub async fn shutdown(&self) {
        self.shutdown.cancel();

        let join_handle = self.join_handle.lock().expect("daemon handle mutex").take();
        if let Some(join_handle) = join_handle {
            let _ = join_handle.await;
        }

        self.running.store(false, Ordering::SeqCst);
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    pub fn restart_count(&self) -> usize {
        self.restart_count.load(Ordering::SeqCst)
    }
}

pub fn spawn_supervised(config: Config) -> DaemonHandle {
    spawn_supervised_with_runner(config, |config, shutdown| {
        Box::pin(crate::bootstrap::run_daemon_with_shutdown(config, shutdown))
    })
}

pub fn spawn_supervised_with_runner<F, Fut>(config: Config, runner: F) -> DaemonHandle
where
    F: Fn(Config, CancellationToken) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<()>> + Send + 'static,
{
    let shutdown = CancellationToken::new();
    let running = Arc::new(AtomicBool::new(true));
    let restart_count = Arc::new(AtomicUsize::new(0));
    let runner = Arc::new(runner);
    let supervisor_shutdown = shutdown.clone();
    let supervisor_running = Arc::clone(&running);
    let supervisor_restarts = Arc::clone(&restart_count);

    let join_handle = tokio::spawn(async move {
        let backoff = [1_u64, 2, 5, 10];
        let mut backoff_index = 0usize;

        loop {
            if supervisor_shutdown.is_cancelled() {
                break;
            }

            let child_shutdown = supervisor_shutdown.child_token();
            let runner = Arc::clone(&runner);
            let config = config.clone();
            let run_handle = tokio::spawn(async move { runner(config, child_shutdown).await });

            match run_handle.await {
                Ok(Ok(())) => {
                    if supervisor_shutdown.is_cancelled() {
                        break;
                    }
                    let restart_no = supervisor_restarts.fetch_add(1, Ordering::SeqCst) + 1;
                    warn!(
                        restart = restart_no,
                        "daemon stopped unexpectedly; restarting"
                    );
                }
                Ok(Err(error)) => {
                    if supervisor_shutdown.is_cancelled() {
                        break;
                    }
                    let restart_no = supervisor_restarts.fetch_add(1, Ordering::SeqCst) + 1;
                    warn!(restart = restart_no, %error, "daemon exited with error; restarting");
                }
                Err(join_error) => {
                    if supervisor_shutdown.is_cancelled() {
                        break;
                    }
                    let restart_no = supervisor_restarts.fetch_add(1, Ordering::SeqCst) + 1;
                    if join_error.is_panic() {
                        error!(restart = restart_no, "daemon panicked; restarting");
                    } else {
                        warn!(restart = restart_no, %join_error, "daemon task failed; restarting");
                    }
                }
            }

            if supervisor_shutdown.is_cancelled() {
                break;
            }

            let delay = Duration::from_secs(backoff[backoff_index]);
            if backoff_index + 1 < backoff.len() {
                backoff_index += 1;
            }
            tokio::select! {
                _ = sleep(delay) => {}
                _ = supervisor_shutdown.cancelled() => break,
            }
        }

        supervisor_running.store(false, Ordering::SeqCst);
    });

    DaemonHandle {
        shutdown,
        join_handle: Arc::new(Mutex::new(Some(join_handle))),
        running,
        restart_count,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flowkey_config::{CaptureMode, Config, NodeConfig, SwitchConfig};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::time::{timeout, Duration};

    fn test_config() -> Config {
        Config {
            node: NodeConfig {
                id: "local-node".to_string(),
                name: "Local Node".to_string(),
                listen_addr: "127.0.0.1:48571".to_string(),
                advertised_addr: None,
                private_key: "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".to_string(),
                public_key: "BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB".to_string(),
            },
            switch: SwitchConfig {
                hotkey: "Ctrl+Alt+Shift+K".to_string(),
                capture_mode: CaptureMode::Passive,
            },
            peers: Vec::new(),
        }
    }

    #[tokio::test]
    async fn supervisor_restarts_after_panic_once() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let runner_attempts = Arc::clone(&attempts);

        let handle = spawn_supervised_with_runner(test_config(), move |_config, shutdown| {
            let attempts = Arc::clone(&runner_attempts);
            async move {
                let attempt = attempts.fetch_add(1, Ordering::SeqCst);
                if attempt == 0 {
                    panic!("intentional test panic");
                }

                shutdown.cancelled().await;
                Ok(())
            }
        });

        timeout(Duration::from_secs(3), async {
            while attempts.load(Ordering::SeqCst) < 2 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("supervisor should restart after the panic");

        assert_eq!(handle.restart_count(), 1);
        handle.shutdown().await;
        assert!(!handle.is_running());
    }
}
