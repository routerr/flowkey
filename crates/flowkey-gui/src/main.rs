#![cfg_attr(
  all(not(debug_assertions), target_os = "windows"),
  windows_subsystem = "windows"
)]

use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{CustomMenuItem, SystemTray, SystemTrayMenu, SystemTrayMenuItem, SystemTrayEvent, Manager, State};
use flowkey_config::Config;
use flowkey_net::discovery::{DiscoveredPeer, DiscoveryAdvertisement};
use flowkey_net::pairing::{PairingProposal, run_pairing_listener, initiate_pairing_client};
use tokio::net::TcpListener;

struct AppState {
  active_pairing: Arc<Mutex<Option<PairingProposal>>>,
  active_discovery: Arc<Mutex<Option<DiscoveryAdvertisement>>>,
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
async fn enter_pairing_mode(state: State<'_, AppState>) -> Result<String, String> {
  let config = Config::load_or_default().map_err(|e| e.to_string())?;
  
  // Create a temporary listener for pairing on a random port
  let listener = TcpListener::bind("0.0.0.0:0").await.map_err(|e| e.to_string())?;
  let pairing_port = listener.local_addr().map_err(|e| e.to_string())?.port();
  
  // Start temporary advertisement for pairing
  let advertisement = flowkey_net::discovery::advertise(&config, true, Some(pairing_port))
    .map_err(|e| e.to_string())?;
  
  {
    let mut active = state.active_discovery.lock().unwrap();
    *active = Some(advertisement);
  }
  
  let proposal = run_pairing_listener(config, listener).await.map_err(|e| e.to_string())?;
  let sas_code = proposal.sas_code.clone();
  
  let mut active = state.active_pairing.lock().unwrap();
  *active = Some(proposal);
  
  Ok(sas_code)
}

#[tauri::command]
async fn connect_to_peer(peer_addr: String, state: State<'_, AppState>) -> Result<String, String> {
  let config = Config::load_or_default().map_err(|e| e.to_string())?;
  let addr = peer_addr.parse::<std::net::SocketAddr>().map_err(|e| e.to_string())?;
  
  let proposal = initiate_pairing_client(config, addr).await.map_err(|e| e.to_string())?;
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
    active.take().ok_or_else(|| "no active pairing session".to_string())?
  };
  
  let mut config = Config::load_or_default().map_err(|e| e.to_string())?;
  config.upsert_peer(flowkey_config::PeerConfig {
    id: proposal.peer.id,
    name: proposal.peer.name,
    addr: "".to_string(), // Will be resolved via mDNS
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
    if socket_path.exists() {
      if let Ok(mut stream) = tokio::net::UnixStream::connect(&socket_path).await {
        if cmd.send_to(&mut stream).await.is_ok() {
          return Ok(());
        }
      }
    }
  }

  cmd.save_to_path(&control_path).map_err(|e| e.to_string())
}

#[tauri::command]
async fn release_control() -> Result<(), String> {
  let control_path = Config::control_path().map_err(|e| e.to_string())?;
  let cmd = flowkey_core::DaemonCommand::release();
  
  #[cfg(target_os = "macos")]
  {
    let socket_path = control_path.with_extension("sock");
    if socket_path.exists() {
      if let Ok(mut stream) = tokio::net::UnixStream::connect(&socket_path).await {
        if cmd.send_to(&mut stream).await.is_ok() {
          return Ok(());
        }
      }
    }
  }

  cmd.save_to_path(&control_path).map_err(|e| e.to_string())
}

fn main() {
  let open = CustomMenuItem::new("open".to_string(), "Open Manager");
  let quit = CustomMenuItem::new("quit".to_string(), "Quit");
  let tray_menu = SystemTrayMenu::new()
    .add_item(open)
    .add_native_item(SystemTrayMenuItem::Separator)
    .add_item(quit);

  let system_tray = SystemTray::new().with_menu(tray_menu);

  tauri::Builder::default()
    .manage(AppState {
      active_pairing: Arc::new(Mutex::new(None)),
      active_discovery: Arc::new(Mutex::new(None)),
    })
    .system_tray(system_tray)
    .on_system_tray_event(|app, event| match event {
      SystemTrayEvent::MenuItemClick { id, .. } => {
        match id.as_str() {
          "quit" => {
            std::process::exit(0);
          }
          "open" => {
            let window = app.get_window("main").unwrap();
            window.show().unwrap();
            window.set_focus().unwrap();
          }
          _ => {}
        }
      }
      _ => {}
    })
    .setup(|app| {
      let _app_handle = app.handle();
      
      // Start daemon in background
      tauri::async_runtime::spawn(async move {
        if let Ok(config) = Config::load_or_create() {
          let _ = flowkey_daemon::run_daemon(config).await;
        }
      });

      // Periodically emit daemon status to frontend
      let status_handle = app.handle();
      tauri::async_runtime::spawn(async move {
        let status_path = Config::status_path().unwrap();
        loop {
          if status_path.exists() {
            if let Ok(status) = flowkey_core::DaemonStatus::load_from_path(&status_path) {
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
