use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use anyhow::Result;
use flowkey_config::Config;
use tokio::runtime::Builder;
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;
use tracing::{error, warn};

pub struct DaemonHandle {
    shutdown: CancellationToken,
    thread_handle: Arc<Mutex<Option<thread::JoinHandle<()>>>>,
    running: Arc<AtomicBool>,
    restart_count: Arc<AtomicUsize>,
}

impl DaemonHandle {
    pub async fn shutdown(&self) {
        self.shutdown.cancel();

        // The supervisor owns its own Tokio runtime on a dedicated thread.
        // Cancelling the token above signals it to wind down; we detach the
        // thread here rather than joining it so we do not depend on whichever
        // runtime the caller happens to be running under.
        let _ = self
            .thread_handle
            .lock()
            .expect("daemon handle mutex")
            .take();

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

    // Run the supervisor loop on a dedicated OS thread with its own
    // multi-threaded Tokio runtime. The GUI caller may be running under
    // Tauri's async executor which is not guaranteed to be Tokio, so we
    // cannot rely on `tokio::spawn` working at call time.
    let thread_handle = thread::Builder::new()
        .name("flowkey-daemon-supervisor".into())
        .spawn(move || {
            let runtime = match Builder::new_multi_thread().enable_all().build() {
                Ok(runtime) => runtime,
                Err(error) => {
                    error!(%error, "failed to build daemon tokio runtime");
                    supervisor_running.store(false, Ordering::SeqCst);
                    return;
                }
            };

            runtime.block_on(supervisor_loop(
                runner,
                config,
                supervisor_shutdown,
                supervisor_running.clone(),
                supervisor_restarts,
            ));

            supervisor_running.store(false, Ordering::SeqCst);
        })
        .expect("failed to spawn daemon supervisor thread");

    DaemonHandle {
        shutdown,
        thread_handle: Arc::new(Mutex::new(Some(thread_handle))),
        running,
        restart_count,
    }
}

async fn supervisor_loop<F, Fut>(
    runner: Arc<F>,
    config: Config,
    supervisor_shutdown: CancellationToken,
    _supervisor_running: Arc<AtomicBool>,
    supervisor_restarts: Arc<AtomicUsize>,
) where
    F: Fn(Config, CancellationToken) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<()>> + Send + 'static,
{
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
                accept_remote_control: true,
                private_key: "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".to_string(),
                public_key: "BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB".to_string(),
            },
            switch: SwitchConfig {
                hotkey: "Ctrl+Alt+Shift+K".to_string(),
                capture_mode: CaptureMode::Passive,
                input_coalesce_window_ms: flowkey_config::DEFAULT_INPUT_COALESCE_WINDOW_MS,
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
