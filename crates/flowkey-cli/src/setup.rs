use std::time::Duration;

use anyhow::{Context, Result};
use flowkey_config::{CaptureMode, Config, PeerConfig};
use flowkey_crypto::{HandshakeOffer, NodeIdentity};
use flowkey_net::discovery::{discover, DiscoveredPeer};
use inquire::{Select, Text};

pub async fn run_interactive_setup() -> Result<()> {
    println!("Welcome to flowkey interactive setup!");
    println!("------------------------------------");

    // 1. Load or create config
    let mut config = Config::load_or_create().context("failed to load or create config")?;

    // 2. Configure Node Name
    let new_name = Text::new("What should this device be called?")
        .with_default(&config.node.name)
        .prompt()
        .context("failed to prompt for node name")?;

    if new_name != config.node.name {
        config.node.name = new_name;
        config.save().context("failed to save config")?;
        println!("Node name updated to '{}'.\n", config.node.name);
    } else {
        println!("Node name kept as '{}'.\n", config.node.name);
    }

    // 3. Configure Hotkey
    let common_hotkeys = vec![
        "Ctrl+Alt+Shift+K",
        "Ctrl+Shift+Space",
        "Meta+Shift+K", // Meta = Win/Cmd
        "Alt+Shift+Q",
        "Custom...",
    ];

    let current_hotkey = config.switch.hotkey.clone();
    println!("Current switch hotkey is: {}", current_hotkey);

    let hotkey_choice = Select::new("Choose a hotkey to switch control:", common_hotkeys)
        .with_help_message("Press Enter to select, or start typing to filter")
        .prompt()
        .context("failed to prompt for hotkey")?;

    let new_hotkey = if hotkey_choice == "Custom..." {
        Text::new("Enter custom hotkey (e.g. Ctrl+Alt+M):")
            .with_default(&current_hotkey)
            .prompt()
            .context("failed to prompt for custom hotkey")?
    } else {
        hotkey_choice.to_string()
    };

    if new_hotkey != current_hotkey {
        config.switch.hotkey = new_hotkey;
        config.save().context("failed to save config")?;
        println!("Hotkey updated to '{}'.\n", config.switch.hotkey);
    } else {
        println!("Hotkey kept as '{}'.\n", config.switch.hotkey);
    }

    // 4. Configure Capture Mode
    let capture_modes = vec![
        "Passive (Standard - input events are captured and passed through)",
        "Exclusive (Advanced - input events are intercepted and suppressed on the local machine when switching)",
    ];

    let current_capture_mode = config.switch.capture_mode;
    println!("Current capture mode is: {}", current_capture_mode.as_str());

    let capture_mode_choice = Select::new("Choose a capture mode:", capture_modes)
        .with_help_message("Passive is recommended for most users. Exclusive mode provides better isolation but requires more permissions.")
        .prompt()
        .context("failed to prompt for capture mode")?;

    let new_capture_mode = if capture_mode_choice.starts_with("Passive") {
        CaptureMode::Passive
    } else {
        CaptureMode::Exclusive
    };

    if new_capture_mode != current_capture_mode {
        config.switch.capture_mode = new_capture_mode;
        config.save().context("failed to save config")?;
        println!(
            "Capture mode updated to '{}'.\n",
            config.switch.capture_mode.as_str()
        );
    } else {
        println!(
            "Capture mode kept as '{}'.\n",
            config.switch.capture_mode.as_str()
        );
    }

    // 5. Discovery
    println!("Searching for flowkey devices on the local network (2 seconds)...");
    let mut discovered = match discover(Duration::from_secs(2), Some(&config.node.id)) {
        Ok(peers) => peers,
        Err(e) => {
            println!("Discovery failed: {e}");
            Vec::new()
        }
    };

    // Filter out ourselves
    discovered.retain(|p| p.id != config.node.id);

    let mut selected_peer: Option<DiscoveredPeer> = None;

    if !discovered.is_empty() {
        println!("Found {} device(s).", discovered.len());
        let mut options = discovered
            .iter()
            .map(|p| format!("{} ({})", p.name, p.addrs.first().unwrap_or(&p.hostname)))
            .collect::<Vec<_>>();
        options.push("Skip / Enter token manually".to_string());

        let peer_choice = Select::new("Do you want to pair with a discovered device?", options)
            .prompt()
            .context("failed to prompt for peer selection")?;

        if peer_choice != "Skip / Enter token manually" {
            // Find the selected peer
            if let Some(index) = discovered.iter().position(|p| {
                format!("{} ({})", p.name, p.addrs.first().unwrap_or(&p.hostname)) == peer_choice
            }) {
                selected_peer = Some(discovered[index].clone());
            }
        }
    } else {
        println!("No other devices found on the local network.");
    }

    // 6. Pairing Token Exchange
    println!("\n--- Pairing ---");
    println!("To connect two devices, they must trust each other's pairing tokens.");

    // Generate our token
    let identity = NodeIdentity {
        node_id: config.node.id.clone(),
        node_name: config.node.name.clone(),
        listen_addr: config
            .advertised_listen_addr()
            .unwrap_or(config.node.listen_addr.clone()),
        public_key: config.node.public_key.clone(),
    };

    let offer = HandshakeOffer::new(identity, &config.node.private_key)
        .context("failed to generate local pairing offer")?;
    let token = offer
        .to_token()
        .context("failed to serialize pairing offer")?;

    println!("\nYOUR pairing token (paste this on the OTHER device):");
    println!("\n{}\n", token);

    if let Some(peer) = selected_peer {
        println!("You selected to pair with '{}'.", peer.name);
    }

    let remote_token =
        Text::new("Paste the pairing token from the OTHER device (or press Enter to finish):")
            .prompt()
            .context("failed to prompt for remote token")?;

    let trimmed_token = remote_token.trim();
    if !trimmed_token.is_empty() {
        match HandshakeOffer::from_token(trimmed_token) {
            Ok(offer) => {
                config.upsert_peer(PeerConfig {
                    id: offer.node.node_id.clone(),
                    name: offer.node.node_name.clone(),
                    addr: offer.node.listen_addr.clone(),
                    public_key: offer.node.public_key.clone(),
                    trusted: true,
                });
                config
                    .save()
                    .context("failed to save config with new peer")?;
                println!("\nSuccessfully paired with '{}'!", offer.node.node_name);
            }
            Err(e) => {
                println!("\nFailed to parse pairing token: {e}");
                println!("No new devices were added.");
            }
        }
    } else {
        println!("\nSkipped entering remote token.");
    }

    println!("\nSetup complete!");
    println!("Run `flky daemon` on both devices to start sharing.");

    Ok(())
}
