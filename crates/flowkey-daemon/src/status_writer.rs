use std::fs;
use std::sync::{Arc, Mutex};

use arc_swap::ArcSwap;
use flowkey_config::Config;
use flowkey_core::daemon::DaemonRuntime;
use flowkey_core::status::{DaemonStatus, RuntimeSnapshot};
use flowkey_net::discovery::DiscoveryAdvertisement;
use tracing::warn;

use crate::platform::push_runtime_note;

pub(crate) fn advertise_discovery_service(
    config: &Config,
    runtime: &Arc<Mutex<DaemonRuntime>>,
    status_snapshot: &Arc<ArcSwap<RuntimeSnapshot>>,
    status_path: &std::path::Path,
) -> Option<DiscoveryAdvertisement> {
    match flowkey_net::discovery::advertise(config, false, None) {
        Ok(discovery) => {
            {
                let mut runtime = runtime
                    .lock()
                    .expect("daemon runtime mutex should not be poisoned");
                push_runtime_note(
                    &mut runtime,
                    "LAN discovery advertisement enabled".to_string(),
                );
            }
            refresh_and_persist_status_snapshot(runtime, status_snapshot, status_path);
            Some(discovery)
        }
        Err(error) => {
            {
                let mut runtime = runtime
                    .lock()
                    .expect("daemon runtime mutex should not be poisoned");
                push_runtime_note(&mut runtime, format!("LAN discovery unavailable: {error}"));
            }
            refresh_and_persist_status_snapshot(runtime, status_snapshot, status_path);
            warn!(%error, "failed to advertise discovery service");
            None
        }
    }
}

pub(crate) fn publish_status_snapshot(
    runtime: &Arc<Mutex<DaemonRuntime>>,
    status_snapshot: &Arc<ArcSwap<RuntimeSnapshot>>,
) {
    let runtime = runtime
        .lock()
        .expect("daemon runtime mutex should not be poisoned");
    status_snapshot.store(Arc::new(RuntimeSnapshot::from_runtime(&runtime)));
}

pub(crate) fn persist_status_snapshot(
    status_snapshot: &Arc<ArcSwap<RuntimeSnapshot>>,
    status_path: &std::path::Path,
) {
    let status = DaemonStatus::from_snapshot(&status_snapshot.load());

    if let Err(error) = status.save_to_path(status_path) {
        warn!(%error, path = %status_path.display(), "failed to persist daemon status");
    }
}

pub(crate) fn refresh_and_persist_status_snapshot(
    runtime: &Arc<Mutex<DaemonRuntime>>,
    status_snapshot: &Arc<ArcSwap<RuntimeSnapshot>>,
    status_path: &std::path::Path,
) {
    publish_status_snapshot(runtime, status_snapshot);
    persist_status_snapshot(status_snapshot, status_path);
}

pub(crate) fn clear_status_snapshot(status_path: &std::path::Path) {
    match fs::remove_file(status_path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => warn!(%error, path = %status_path.display(), "failed to clear daemon status"),
    }
}
