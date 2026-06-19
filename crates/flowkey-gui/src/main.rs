#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]

use flowkey_config::Config;
use flowkey_daemon::{spawn_supervised, DaemonHandle};
use flowkey_net::discovery::{
    discover_tailscale_peers, resolve_hostname_to_addrs, DiscoveredPeer, DiscoveryAdvertisement,
    DEFAULT_PAIRING_PORT,
};
use flowkey_net::pairing::{
    accept_pairing_listener, initiate_pairing_client_to_target, PairingProposal,
};
#[cfg(target_os = "macos")]
use flowkey_platform_macos::control_ipc::connect_to_control_socket;
#[cfg(target_os = "windows")]
use flowkey_platform_windows::control_ipc::connect_to_control_pipe;
use serde::Serialize;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{
    CustomMenuItem, Manager, State, SystemTray, SystemTrayEvent, SystemTrayMenu,
    SystemTrayMenuItem, WindowEvent,
};
use tauri_plugin_autostart::MacosLauncher;
use tokio::net::TcpListener;

struct AppState {
    active_pairing: Arc<Mutex<Option<PairingProposal>>>,
    active_discovery: Arc<Mutex<Option<DiscoveryAdvertisement>>>,
    daemon: Arc<Mutex<Option<Arc<DaemonHandle>>>>,
}

#[derive(Debug, Clone, Serialize)]
struct PermissionStatusView {
    accessibility: bool,
    input_monitoring: bool,
}

#[derive(Debug, Clone, Serialize)]
struct PendingPairingView {
    sas_code: String,
    peer_name: String,
}

fn is_generic_display_name(value: &str) -> bool {
    let value = value.trim().to_ascii_lowercase();
    value.is_empty() || value == "local-node" || value == "local node"
}

fn peer_host_keys(addrs: &[String], hostname: &str) -> std::collections::HashSet<String> {
    let mut keys = std::collections::HashSet::new();
    for addr in addrs {
        if let Ok(sa) = addr.parse::<std::net::SocketAddr>() {
            keys.insert(sa.ip().to_string());
        } else if let Some((host, port)) = addr.rsplit_once(':') {
            if port.parse::<u16>().is_ok() {
                keys.insert(host.trim_matches(&['[', ']'][..]).trim_end_matches('.').to_ascii_lowercase());
            } else {
                keys.insert(addr.trim_matches(&['[', ']'][..]).trim_end_matches('.').to_ascii_lowercase());
            }
        } else {
            keys.insert(addr.trim_matches(&['[', ']'][..]).trim_end_matches('.').to_ascii_lowercase());
        }
    }
    if !hostname.is_empty() {
        keys.insert(hostname.trim_end_matches('.').to_ascii_lowercase());
    }
    keys
}

#[tauri::command]
async fn get_discovered_peers() -> Result<Vec<DiscoveredPeer>, String> {
    let config = Config::load_or_default().map_err(|e| e.to_string())?;
    let local_id = config.node.id.clone();

    // 1. mDNS discovery (works for same-subnet)
    let mut peers = match flowkey_net::discovery::discover(Duration::from_secs(2), Some(&local_id)) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(error = %e, "mDNS discovery failed");
            Vec::new()
        }
    };
    let mut known_ids: std::collections::HashSet<String> =
        peers.iter().map(|p| p.id.clone()).collect();

    // 2. Add Tailscale peers (works across subnets and with Magic DNS)
    for peer in discover_tailscale_peers() {
        let addr_overlap = peers.iter().any(|existing| {
            (!existing.hostname.is_empty() && existing.hostname == peer.hostname)
                || existing
                    .addrs
                    .iter()
                    .any(|addr| peer.addrs.iter().any(|candidate| candidate == addr))
        });
        if known_ids.contains(&peer.id) || addr_overlap {
            continue;
        }
        known_ids.insert(peer.id.clone());
        peers.push(peer);
    }

    // 3. Add configured peers with DNS-resolved addresses
    for peer in &config.peers {
        if peer.id == local_id || known_ids.contains(&peer.id) {
            continue;
        }

        // Resolve the configured address: if it's a hostname, resolve it via DNS
        let mut addrs = Vec::new();
        let default_port = 48571;
        let (_configured_host, _configured_port) = match peer.addr.parse::<std::net::SocketAddr>() {
            Ok(sa) => {
                if sa.port() != default_port {
                    addrs.push(std::net::SocketAddr::new(sa.ip(), default_port).to_string());
                }
                addrs.push(sa.to_string());
                (sa.ip().to_string(), sa.port())
            }
            Err(_) => {
                // It's a hostname — extract host and port
                let host = peer.addr.rsplit_once(':')
                    .map(|(h, _)| h)
                    .unwrap_or(&peer.addr);
                let port = peer.addr.rsplit_once(':')
                    .and_then(|(_, p)| p.parse().ok())
                    .unwrap_or(default_port);
                let resolved = resolve_hostname_to_addrs(host, port);
                for addr in &resolved {
                    if !addrs.contains(addr) {
                        addrs.push(addr.clone());
                    }
                }
                (host.to_string(), port)
            }
        };

        // Also try resolving the peer's name/ID as a hostname via DNS
        // (handles Tailscale Magic DNS, Bonjour .local., etc.)
        let name_addrs = resolve_hostname_to_addrs(&peer.id, default_port);
        for addr in name_addrs {
            if !addrs.contains(&addr) {
                addrs.push(addr);
            }
        }

        // If we still have no addresses, try the peer's display name too
        if addrs.is_empty() && peer.name != peer.id {
            let name_addrs = resolve_hostname_to_addrs(&peer.name, default_port);
            for addr in name_addrs {
                if !addrs.contains(&addr) {
                    addrs.push(addr);
                }
            }
        }

        if !addrs.is_empty() {
            let candidate_keys = peer_host_keys(&addrs, "");
            if let Some(existing) = peers.iter_mut().find(|existing| {
                let existing_keys = peer_host_keys(&existing.addrs, &existing.hostname);
                !existing_keys.is_disjoint(&candidate_keys)
            }) {
                for addr in &addrs {
                    if !existing.addrs.contains(addr) {
                        existing.addrs.push(addr.clone());
                    }
                }
                existing.addrs.sort();
                existing.addrs.dedup();
                existing.id = peer.id.clone();
                if !is_generic_display_name(&peer.name) || is_generic_display_name(&existing.name) {
                    existing.name = peer.name.clone();
                }
                existing.is_pairing = false;
                existing.pairing_port = None;
                known_ids.insert(peer.id.clone());
                continue;
            }

            known_ids.insert(peer.id.clone());
            peers.push(DiscoveredPeer {
                id: peer.id.clone(),
                name: peer.name.clone(),
                addrs,
                hostname: String::new(),
                service_name: String::new(),
                is_pairing: false,
                pairing_port: None,
            });
        }
    }

    peers.sort_by(|a, b| a.name.cmp(&b.name).then(a.id.cmp(&b.id)));
    Ok(peers)
}

#[tauri::command]
async fn get_pending_pairing(
    state: State<'_, AppState>,
) -> Result<Option<PendingPairingView>, String> {
    let active = state.active_pairing.lock().unwrap();
    Ok(active.as_ref().map(|proposal| PendingPairingView {
        sas_code: proposal.sas_code.clone(),
        peer_name: proposal.peer.name.clone(),
    }))
}

#[tauri::command]
async fn get_config() -> Result<Config, String> {
    Config::load_or_default().map_err(|e| e.to_string())
}

#[tauri::command]
async fn get_permission_status() -> Result<PermissionStatusView, String> {
    #[cfg(target_os = "macos")]
    {
        let status = flowkey_platform_macos::permissions::PermissionStatus::probe();
        Ok(PermissionStatusView {
            accessibility: status.accessibility,
            input_monitoring: status.input_monitoring,
        })
    }

    #[cfg(not(target_os = "macos"))]
    {
        Ok(PermissionStatusView {
            accessibility: true,
            input_monitoring: true,
        })
    }
}

#[tauri::command]
async fn open_permissions() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        flowkey_platform_macos::permissions::PermissionStatus::open_accessibility_pane()?;
        flowkey_platform_macos::permissions::PermissionStatus::open_input_monitoring_pane()?;
    }

    Ok(())
}

#[tauri::command]
async fn set_accept_remote_control(enabled: bool) -> Result<(), String> {
    let mut config = Config::load_or_create().map_err(|e| e.to_string())?;
    config.node.accept_remote_control = enabled;
    config.save().map_err(|e| e.to_string())
}

#[tauri::command]
async fn enter_pairing_mode(_state: State<'_, AppState>) -> Result<String, String> {
    // Pairing is always available while the GUI is running.
    // Return an empty string so the frontend shows the waiting state.
    Ok(String::new())
}

#[tauri::command]
async fn connect_to_peer(peer_addr: String, state: State<'_, AppState>) -> Result<String, String> {
    let config = Config::load_or_default().map_err(|e| e.to_string())?;
    let proposal = initiate_pairing_client_to_target(
        config,
        &peer_addr,
        DEFAULT_PAIRING_PORT,
        Duration::from_secs(5),
    )
    .await
    .map_err(|e| e.to_string())?;
    let sas_code = proposal.sas_code.clone();

    let mut active = state.active_pairing.lock().unwrap();
    *active = Some(proposal);

    Ok(sas_code)
}

#[tauri::command]
async fn confirm_pairing(state: State<'_, AppState>) -> Result<(), String> {
    let proposal = {
        let mut active = state.active_pairing.lock().unwrap();
        active
            .take()
            .ok_or_else(|| "no active pairing session".to_string())?
    };

    let mut config = Config::load_or_default().map_err(|e| e.to_string())?;
    let preferred_addr = proposal.preferred_peer_addr();
    config.upsert_peer(flowkey_config::PeerConfig {
        id: proposal.peer.id,
        name: proposal.peer.name,
        addr: preferred_addr,
        public_key: proposal.peer.public_key,
        trusted: true,
    });

    config.save().map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
async fn cancel_pairing(state: State<'_, AppState>) -> Result<(), String> {
    let mut active = state.active_pairing.lock().unwrap();
    *active = None;
    Ok(())
}

#[tauri::command]
async fn remove_peer(peer_id: String) -> Result<(), String> {
    let mut config = Config::load_or_default().map_err(|e| e.to_string())?;
    config.peers.retain(|p| p.id != peer_id);
    config.save().map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
async fn switch_to_peer(peer_id: String) -> Result<(), String> {
    tracing::info!(peer_id = %peer_id, "gui: switch_to_peer invoked");
    let cmd = flowkey_core::DaemonCommand::switch(peer_id.clone());

    #[cfg(target_os = "macos")]
    {
        let control_path = Config::control_path().map_err(|e| e.to_string())?;
        let socket_path = control_path.with_extension("sock");
        if let Some(mut stream) = connect_to_control_socket(&socket_path).await {
            cmd.send_to(&mut stream).await.map_err(|e| {
                tracing::error!(%e, "gui: failed to write switch command");
                format!("failed to send command to daemon: {e}")
            })?;
            tracing::info!(peer_id = %peer_id, "gui: switch command written to socket");
            return Ok(());
        } else {
            tracing::warn!(path = %socket_path.display(), "gui: daemon control socket missing");
            return Err(
                "daemon control socket not found; daemon may still be starting".to_string(),
            );
        }
    }

    #[cfg(target_os = "windows")]
    {
        let config = Config::load_or_default().map_err(|e| e.to_string())?;
        let pipe_name = config.control_pipe_name();
        if let Some(mut pipe) = connect_to_control_pipe(&pipe_name).await {
            cmd.send_to(&mut pipe)
                .await
                .map_err(|e| format!("failed to send command to daemon: {e}"))?;
            return Ok(());
        } else {
            return Err(format!("failed to connect to daemon pipe: {pipe_name}"));
        }
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let control_path = Config::control_path().map_err(|e| e.to_string())?;
        cmd.save_to_path(&control_path).map_err(|e| e.to_string())
    }
}

#[tauri::command]
async fn release_control() -> Result<(), String> {
    tracing::info!("gui: release_control invoked");
    let cmd = flowkey_core::DaemonCommand::release();

    #[cfg(target_os = "macos")]
    {
        let control_path = Config::control_path().map_err(|e| e.to_string())?;
        let socket_path = control_path.with_extension("sock");
        if let Some(mut stream) = connect_to_control_socket(&socket_path).await {
            cmd.send_to(&mut stream).await.map_err(|e| {
                tracing::error!(%e, "gui: failed to write release command");
                format!("failed to send command to daemon: {e}")
            })?;
            tracing::info!("gui: release command written to socket");
            return Ok(());
        } else {
            tracing::warn!(path = %socket_path.display(), "gui: daemon control socket missing");
            return Err(
                "daemon control socket not found; daemon may still be starting".to_string(),
            );
        }
    }

    #[cfg(target_os = "windows")]
    {
        let config = Config::load_or_default().map_err(|e| e.to_string())?;
        let pipe_name = config.control_pipe_name();
        if let Some(mut pipe) = connect_to_control_pipe(&pipe_name).await {
            cmd.send_to(&mut pipe)
                .await
                .map_err(|e| format!("failed to send command to daemon: {e}"))?;
            return Ok(());
        } else {
            return Err(format!("failed to connect to daemon pipe: {pipe_name}"));
        }
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let control_path = Config::control_path().map_err(|e| e.to_string())?;
        cmd.save_to_path(&control_path).map_err(|e| e.to_string())
    }
}

fn request_daemon_shutdown(app: tauri::AppHandle) {
    let handle = app
        .state::<AppState>()
        .daemon
        .lock()
        .unwrap()
        .as_ref()
        .cloned();

    tauri::async_runtime::spawn(async move {
        if let Some(handle) = handle {
            handle.shutdown().await;
        }
        app.exit(0);
    });
}

fn init_tracing() {
    use tracing_subscriber::EnvFilter;

    let Ok(log_dir) = Config::log_dir() else {
        return;
    };
    if std::fs::create_dir_all(&log_dir).is_err() {
        return;
    }

    let file_appender = tracing_appender::rolling::never(&log_dir, "flowkey.log");
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        EnvFilter::new(
            "debug,flowkey_daemon=debug,flowkey_net=debug,flowkey_platform_windows=trace,flowkey_platform_macos=debug,keyboard_trace=trace",
        )
    });

    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(file_appender)
        .with_ansi(false)
        .try_init();
}

#[cfg(target_os = "macos")]
fn set_activation_policy(policy: tauri::ActivationPolicy) {
    #[link(name = "objc")]
    extern "C" {
        fn objc_getClass(name: *const std::os::raw::c_char) -> *mut std::ffi::c_void;
        fn sel_registerName(name: *const std::os::raw::c_char) -> *mut std::ffi::c_void;
        fn objc_msgSend(
            obj: *mut std::ffi::c_void,
            sel: *mut std::ffi::c_void,
            arg: isize,
        ) -> *mut std::ffi::c_void;
    }

    let cls_name = std::ffi::CString::new("NSApplication").unwrap();
    let shared_app_sel = std::ffi::CString::new("sharedApplication").unwrap();
    let set_policy_sel = std::ffi::CString::new("setActivationPolicy:").unwrap();

    unsafe {
        let ns_app_class = objc_getClass(cls_name.as_ptr());
        if ns_app_class.is_null() {
            return;
        }
        let shared_app_sel_ptr = sel_registerName(shared_app_sel.as_ptr());
        let msg_send_shared_app: unsafe extern "C" fn(*mut std::ffi::c_void, *mut std::ffi::c_void) -> *mut std::ffi::c_void = std::mem::transmute(objc_msgSend as *const ());
        let app = msg_send_shared_app(ns_app_class, shared_app_sel_ptr);
        if app.is_null() {
            return;
        }

        let (policy_val, is_regular): (isize, bool) = match policy {
            tauri::ActivationPolicy::Regular => (0, true),
            tauri::ActivationPolicy::Accessory => (1, false),
            tauri::ActivationPolicy::Prohibited => (2, false),
            _ => (1, false),
        };

        let set_policy_sel_ptr = sel_registerName(set_policy_sel.as_ptr());
        let msg_send_set_policy: unsafe extern "C" fn(*mut std::ffi::c_void, *mut std::ffi::c_void, isize) -> *mut std::ffi::c_void = std::mem::transmute(objc_msgSend as *const ());
        msg_send_set_policy(app, set_policy_sel_ptr, policy_val);

        if is_regular {
            let activate_sel = std::ffi::CString::new("activateIgnoringOtherApps:").unwrap();
            let activate_sel_ptr = sel_registerName(activate_sel.as_ptr());
            let msg_send_activate: unsafe extern "C" fn(*mut std::ffi::c_void, *mut std::ffi::c_void, i8) -> *mut std::ffi::c_void = std::mem::transmute(objc_msgSend as *const ());
            msg_send_activate(app, activate_sel_ptr, 1);
        }
    }
}

fn main() {
    init_tracing();

    // Set up panic hook for debugging
    std::panic::set_hook(Box::new(|info| {
        let msg = match info.payload().downcast_ref::<&str>() {
            Some(s) => *s,
            None => match info.payload().downcast_ref::<String>() {
                Some(s) => &**s,
                None => "Box<Any>",
            },
        };
        let location = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_default();
        let panic_msg = format!("Panic occurred at {}: {}\n", location, msg);

        if let Ok(log_dir) = Config::log_dir() {
            let _ = std::fs::create_dir_all(&log_dir);
            let log_path = log_dir.join("flowkey-panic.log");
            let _ = std::fs::write(log_path, &panic_msg);
        }

        eprintln!("{}", panic_msg);
    }));

    let open = CustomMenuItem::new("open".to_string(), "Open Manager");
    let quit = CustomMenuItem::new("quit".to_string(), "Quit");
    let tray_menu = SystemTrayMenu::new()
        .add_item(open)
        .add_native_item(SystemTrayMenuItem::Separator)
        .add_item(quit);

    let system_tray = SystemTray::new().with_menu(tray_menu);

    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            if let Some(window) = app.get_window("main") {
                #[cfg(target_os = "macos")]
                set_activation_policy(tauri::ActivationPolicy::Regular);
                let _ = window.show();
                let _ = window.unminimize();
                let _ = window.set_focus();
            }
        }))
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::AppleScript,
            None,
        ))
        .manage(AppState {
            active_pairing: Arc::new(Mutex::new(None)),
            active_discovery: Arc::new(Mutex::new(None)),
            daemon: Arc::new(Mutex::new(None)),
        })
        .system_tray(system_tray)
        .on_window_event(|event| {
            if let WindowEvent::CloseRequested { api, .. } = event.event() {
                api.prevent_close();
                let window = event.window();
                window.hide().unwrap();
                #[cfg(target_os = "macos")]
                set_activation_policy(tauri::ActivationPolicy::Accessory);
            }
        })
        .on_system_tray_event(|app, event| match event {
            SystemTrayEvent::DoubleClick { .. } => {
                let window = app.get_window("main").unwrap();
                #[cfg(target_os = "macos")]
                set_activation_policy(tauri::ActivationPolicy::Regular);
                window.show().unwrap();
                window.set_focus().unwrap();
            }
            SystemTrayEvent::MenuItemClick { id, .. } => match id.as_str() {
                "quit" => {
                    request_daemon_shutdown(app.clone());
                }
                "open" => {
                    let window = app.get_window("main").unwrap();
                    #[cfg(target_os = "macos")]
                    set_activation_policy(tauri::ActivationPolicy::Regular);
                    window.show().unwrap();
                    window.set_focus().unwrap();
                }
                _ => {}
            },
            _ => {}
        })
        .setup(|app| {
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Regular);

            let app_handle = app.handle();
            #[cfg(target_os = "windows")]
            {
                let debug_handle = app_handle.clone();
                flowkey_platform_windows::debug::set_debug_sink(move |event| {
                    let _ = debug_handle.emit_all("input-debug-event", event);
                });
            }

            tauri::async_runtime::spawn(async move {
                if let Ok(config) = Config::load_or_create() {
                    let handle = Arc::new(spawn_supervised(config));
                    let state = app_handle.state::<AppState>();
                    let mut daemon = state.daemon.lock().unwrap();
                    *daemon = Some(handle);
                }
            });

            let pairing_handle = app.handle();
            tauri::async_runtime::spawn(async move {
                let config = match Config::load_or_create() {
                    Ok(config) => config,
                    Err(error) => {
                        tracing::warn!(%error, "failed to load config for always-on pairing listener");
                        return;
                    }
                };

                let listener = match TcpListener::bind(format!("0.0.0.0:{}", DEFAULT_PAIRING_PORT)).await {
                    Ok(listener) => listener,
                    Err(error) => {
                        tracing::warn!(%error, port = DEFAULT_PAIRING_PORT, "failed to bind always-on pairing listener");
                        return;
                    }
                };

                match flowkey_net::discovery::advertise(&config, true, Some(DEFAULT_PAIRING_PORT)) {
                    Ok(advertisement) => {
                        let state = pairing_handle.state::<AppState>();
                        let mut active = state.active_discovery.lock().unwrap();
                        *active = Some(advertisement);
                    }
                    Err(error) => {
                        tracing::warn!(%error, "failed to advertise always-on pairing listener");
                    }
                }

                loop {
                    match accept_pairing_listener(&config, &listener).await {
                        Ok(proposal) => {
                            let state = pairing_handle.state::<AppState>();
                            let mut active = state.active_pairing.lock().unwrap();
                            *active = Some(proposal);
                        }
                        Err(error) => {
                            tracing::warn!(%error, "always-on pairing listener failed; retrying");
                            tokio::time::sleep(Duration::from_millis(500)).await;
                        }
                    }
                }
            });

            if let Some(window) = app.get_window("main") {
                let _ = window.show();
                let _ = window.set_focus();
            }

            if let Ok(log_dir) = Config::log_dir() {
                let _ = std::fs::create_dir_all(log_dir);
            }

            let status_handle = app.handle();
            tauri::async_runtime::spawn(async move {
                let status_path = Config::status_path().unwrap();
                loop {
                    if status_path.exists() {
                        if let Ok(status) = flowkey_core::DaemonStatus::load_from_path(&status_path)
                        {
                            let _ = status_handle.emit_all("daemon-status", status);
                        }
                    }
                    tokio::time::sleep(Duration::from_millis(1000)).await;
                }
            });

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_discovered_peers,
            get_pending_pairing,
            get_config,
            get_permission_status,
            open_permissions,
            set_accept_remote_control,
            enter_pairing_mode,
            connect_to_peer,
            confirm_pairing,
            cancel_pairing,
            remove_peer,
            switch_to_peer,
            release_control
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
