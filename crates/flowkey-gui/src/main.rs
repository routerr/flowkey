#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]

use flowkey_config::Config;
use flowkey_daemon::{spawn_supervised, DaemonHandle};
use flowkey_net::discovery::{DiscoveredPeer, DiscoveryAdvertisement};
use flowkey_net::pairing::{initiate_pairing_client, run_pairing_listener, PairingProposal};
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

#[tauri::command]
async fn get_discovered_peers() -> Result<Vec<DiscoveredPeer>, String> {
    let config = Config::load_or_default().map_err(|e| e.to_string())?;
    flowkey_net::discovery::discover(Duration::from_secs(1), Some(&config.node.id))
        .map_err(|e| e.to_string())
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
async fn enter_pairing_mode(state: State<'_, AppState>) -> Result<String, String> {
    let config = Config::load_or_default().map_err(|e| e.to_string())?;

    // Create a temporary listener for pairing on a random port
    let listener = TcpListener::bind("0.0.0.0:0")
        .await
        .map_err(|e| e.to_string())?;
    let pairing_port = listener.local_addr().map_err(|e| e.to_string())?.port();

    // Start temporary advertisement for pairing
    let advertisement = flowkey_net::discovery::advertise(&config, true, Some(pairing_port))
        .map_err(|e| e.to_string())?;

    {
        let mut active = state.active_discovery.lock().unwrap();
        *active = Some(advertisement);
    }

    let proposal = run_pairing_listener(config, listener)
        .await
        .map_err(|e| e.to_string())?;
    let sas_code = proposal.sas_code.clone();

    let mut active = state.active_pairing.lock().unwrap();
    *active = Some(proposal);

    Ok(sas_code)
}

#[tauri::command]
async fn connect_to_peer(peer_addr: String, state: State<'_, AppState>) -> Result<String, String> {
    let config = Config::load_or_default().map_err(|e| e.to_string())?;
    let addr = peer_addr
        .parse::<std::net::SocketAddr>()
        .map_err(|e| e.to_string())?;

    let proposal = initiate_pairing_client(config, addr)
        .await
        .map_err(|e| e.to_string())?;
    let sas_code = proposal.sas_code.clone();

    let mut active = state.active_pairing.lock().unwrap();
    *active = Some(proposal);

    Ok(sas_code)
}

#[tauri::command]
async fn confirm_pairing(state: State<'_, AppState>) -> Result<(), String> {
    let _ = {
        let mut active = state.active_discovery.lock().unwrap();
        active.take()
    };

    let proposal = {
        let mut active = state.active_pairing.lock().unwrap();
        active
            .take()
            .ok_or_else(|| "no active pairing session".to_string())?
    };

    let mut config = Config::load_or_default().map_err(|e| e.to_string())?;
    config.upsert_peer(flowkey_config::PeerConfig {
        id: proposal.peer.id,
        name: proposal.peer.name,
        addr: proposal.observed_addr.to_string(),
        public_key: proposal.peer.public_key,
        trusted: true,
    });

    config.save().map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
async fn cancel_pairing(state: State<'_, AppState>) -> Result<(), String> {
    {
        let mut active = state.active_discovery.lock().unwrap();
        *active = None;
    }
    {
        let mut active = state.active_pairing.lock().unwrap();
        *active = None;
    }
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
    let control_path = Config::control_path().map_err(|e| e.to_string())?;
    let cmd = flowkey_core::DaemonCommand::switch(peer_id);

    #[cfg(target_os = "macos")]
    {
        let socket_path = control_path.with_extension("sock");
        if !socket_path.exists() {
            return Err("daemon control socket not found; daemon may still be starting".to_string());
        }
        let mut stream = tokio::net::UnixStream::connect(&socket_path)
            .await
            .map_err(|e| format!("failed to connect to daemon: {e}"))?;
        cmd.send_to(&mut stream)
            .await
            .map_err(|e| format!("failed to send command to daemon: {e}"))?;
        return Ok(());
    }

    #[cfg(target_os = "windows")]
    {
        let config = Config::load_or_default().map_err(|e| e.to_string())?;
        let pipe_name = config.control_pipe_name();
        let mut pipe = tokio::net::windows::named_pipe::ClientOptions::new()
            .open(&pipe_name)
            .map_err(|e| format!("failed to connect to daemon pipe: {e}"))?;
        cmd.send_to(&mut pipe)
            .await
            .map_err(|e| format!("failed to send command to daemon: {e}"))?;
        return Ok(());
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    cmd.save_to_path(&control_path).map_err(|e| e.to_string())
}

#[tauri::command]
async fn release_control() -> Result<(), String> {
    let control_path = Config::control_path().map_err(|e| e.to_string())?;
    let cmd = flowkey_core::DaemonCommand::release();

    #[cfg(target_os = "macos")]
    {
        let socket_path = control_path.with_extension("sock");
        if !socket_path.exists() {
            return Err("daemon control socket not found; daemon may still be starting".to_string());
        }
        let mut stream = tokio::net::UnixStream::connect(&socket_path)
            .await
            .map_err(|e| format!("failed to connect to daemon: {e}"))?;
        cmd.send_to(&mut stream)
            .await
            .map_err(|e| format!("failed to send command to daemon: {e}"))?;
        return Ok(());
    }

    #[cfg(target_os = "windows")]
    {
        let config = Config::load_or_default().map_err(|e| e.to_string())?;
        let pipe_name = config.control_pipe_name();
        let mut pipe = tokio::net::windows::named_pipe::ClientOptions::new()
            .open(&pipe_name)
            .map_err(|e| format!("failed to connect to daemon pipe: {e}"))?;
        cmd.send_to(&mut pipe)
            .await
            .map_err(|e| format!("failed to send command to daemon: {e}"))?;
        return Ok(());
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    cmd.save_to_path(&control_path).map_err(|e| e.to_string())
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

fn main() {
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
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
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
                request_daemon_shutdown(event.window().app_handle().clone());
            }
        })
        .on_system_tray_event(|app, event| match event {
            SystemTrayEvent::MenuItemClick { id, .. } => match id.as_str() {
                "quit" => {
                    request_daemon_shutdown(app.clone());
                }
                "open" => {
                    let window = app.get_window("main").unwrap();
                    window.show().unwrap();
                    window.set_focus().unwrap();
                }
                _ => {}
            },
            _ => {}
        })
        .setup(|app| {
            let app_handle = app.handle();
            tauri::async_runtime::spawn(async move {
                if let Ok(config) = Config::load_or_create() {
                    let handle = Arc::new(spawn_supervised(config));
                    let state = app_handle.state::<AppState>();
                    let mut daemon = state.daemon.lock().unwrap();
                    *daemon = Some(handle);
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
