use anyhow::Result;
use clap::{Parser, Subcommand};
use flowkey_config::{Config, PeerConfig};
use flowkey_core::{DaemonCommand, DaemonStatus};
use flowkey_crypto::{HandshakeOffer, NodeIdentity};
use flowkey_daemon::run_daemon;
use std::path::Path;
use std::time::Duration;
use tracing::info;
use tracing::warn;
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(name = "flky", version, about = "LAN keyboard/mouse sharing daemon")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run the daemon in the foreground
    Daemon,
    /// Pairing commands
    Pair {
        #[command(subcommand)]
        command: PairCommand,
    },
    /// Discover peers advertising themselves on the local network
    Discover,
    /// List configured peers
    Peers,
    /// Switch control to a trusted peer
    Switch { peer_id: String },
    /// Release control back to the local machine
    Release,
    /// Show basic runtime or config status
    Status,
}

#[derive(Debug, Subcommand)]
enum PairCommand {
    /// Create a local pairing offer
    Init,
    /// Accept a pairing token
    Accept { token: String },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_target(false)
        .compact()
        .init();

    let cli = Cli::parse();

    match cli.command {
        Command::Daemon => {
            let config = Config::load_or_create()?;
            run_daemon(config).await?;
        }
        Command::Pair { command } => match command {
            PairCommand::Init => {
                let config = Config::load_or_create()?;
                let advertised_listen_addr = config.advertised_listen_addr()?;
                let offer = HandshakeOffer::new(
                    NodeIdentity {
                        node_id: config.node.id.clone(),
                        node_name: config.node.name.clone(),
                        listen_addr: advertised_listen_addr.clone(),
                        public_key: config.node.public_key.clone(),
                    },
                    &config.node.private_key,
                )?;
                let token = offer.to_token()?;

                info!(node_id = %config.node.id, short_code = %offer.short_code, "created pairing offer");
                println!("Pairing token:");
                println!("{token}");
                println!();
                println!("Short code: {}", offer.short_code);
                println!("Expires at epoch seconds: {}", offer.expires_at_epoch_secs);
                println!("Advertised listen addr: {advertised_listen_addr}");
            }
            PairCommand::Accept { token } => {
                let offer = HandshakeOffer::from_token(&token)?;
                let mut config = Config::load_or_create()?;

                config.upsert_peer(PeerConfig {
                    id: offer.node.node_id.clone(),
                    name: offer.node.node_name.clone(),
                    addr: offer.node.listen_addr.clone(),
                    public_key: offer.node.public_key.clone(),
                    trusted: true,
                });
                config.save()?;

                info!(peer_id = %offer.node.node_id, short_code = %offer.short_code, "accepted pairing offer");
                println!("trusted peer added");
                println!("id: {}", offer.node.node_id);
                println!("name: {}", offer.node.node_name);
                println!("addr: {}", offer.node.listen_addr);
                println!("short code: {}", offer.short_code);
            }
        },
        Command::Discover => {
            let config = Config::load_or_default()?;
            let peers = flowkey_net::discovery::discover(Duration::from_secs(2))?;
            let peers = peers
                .into_iter()
                .filter(|peer| peer.id != config.node.id)
                .collect::<Vec<_>>();

            if peers.is_empty() {
                println!("no LAN peers discovered");
            } else {
                for peer in peers {
                    let trusted = config
                        .peers
                        .iter()
                        .any(|configured| configured.id == peer.id && configured.trusted);
                    println!(
                        "{}\t{}\t{}\t{}",
                        peer.id,
                        peer.name,
                        peer.addr,
                        if trusted { "trusted" } else { "untrusted" }
                    );
                }
            }
        }
        Command::Peers => {
            let config = Config::load_or_default()?;
            if config.peers.is_empty() {
                println!("no peers configured");
            } else {
                for peer in config.peers {
                    println!(
                        "{}\t{}\t{}\t{}",
                        peer.id,
                        peer.name,
                        peer.addr,
                        if peer.trusted { "trusted" } else { "untrusted" }
                    );
                }
            }
        }
        Command::Switch { peer_id } => {
            let control_path = Config::control_path()?;
            DaemonCommand::switch(&peer_id).save_to_path(&control_path)?;
            info!(%peer_id, path = %control_path.display(), "queued switch request");
            println!("switch request queued");
            println!("peer: {peer_id}");
            println!("control file: {}", control_path.display());
        }
        Command::Release => {
            let control_path = Config::control_path()?;
            DaemonCommand::release().save_to_path(&control_path)?;
            info!(path = %control_path.display(), "queued release request");
            println!("release request queued");
            println!("control file: {}", control_path.display());
        }
        Command::Status => {
            let config = Config::load_or_default()?;
            let status_path = Config::status_path()?;
            let status = load_status_snapshot(&status_path)?;

            render_status(&config, status.as_ref());
        }
    }

    Ok(())
}

fn load_status_snapshot(path: &Path) -> Result<Option<DaemonStatus>> {
    if !path.exists() {
        return Ok(None);
    }

    match DaemonStatus::load_from_path(path) {
        Ok(status) => Ok(Some(status)),
        Err(error) => {
            warn!(%error, path = %path.display(), "failed to load daemon status");
            Ok(None)
        }
    }
}

fn render_status(config: &Config, status: Option<&DaemonStatus>) {
    for line in status_lines(config, status) {
        println!("{line}");
    }
}

fn status_lines(config: &Config, status: Option<&DaemonStatus>) -> Vec<String> {
    let mut lines = vec![
        format!("node: {}", config.node.name),
        format!("listen: {}", config.node.listen_addr),
        format!(
            "state: {}",
            status
                .map(|snapshot| snapshot.state.as_str())
                .unwrap_or("daemon-stopped")
        ),
        format!("peer: {}", active_peer_label(status).unwrap_or("-")),
        format!(
            "trusted: {}",
            if active_peer_is_trusted(config, status) {
                "yes"
            } else {
                "no"
            }
        ),
        format!(
            "session: {}",
            match status {
                Some(snapshot) if snapshot.session_healthy => "healthy",
                Some(_) => "unhealthy",
                None => "unavailable",
            }
        ),
    ];

    if let Some(snapshot) = status {
        lines.push(format!(
            "capture: {}",
            if snapshot.local_capture_enabled {
                "enabled"
            } else {
                "disabled"
            }
        ));
        lines.push(format!("inject: {}", snapshot.input_injection_backend));

        for note in &snapshot.notes {
            lines.push(format!("note: {}", note));
        }
    }

    lines
}

fn active_peer_label<'a>(status: Option<&'a DaemonStatus>) -> Option<&'a str> {
    status.and_then(|snapshot| snapshot.active_peer_id.as_deref())
}

fn active_peer_is_trusted(config: &Config, status: Option<&DaemonStatus>) -> bool {
    let Some(active_peer_id) = active_peer_label(status) else {
        return false;
    };

    config
        .peers
        .iter()
        .any(|peer| peer.id == active_peer_id && peer.trusted)
}

#[cfg(test)]
mod tests {
    use super::{active_peer_is_trusted, status_lines};
    use flowkey_config::{Config, PeerConfig};
    use flowkey_core::DaemonStatus;

    #[test]
    fn render_status_reflects_runtime_snapshot() {
        let mut config = Config::default();
        config.node.name = "macbook-air".to_string();
        config.node.listen_addr = "0.0.0.0:48571".to_string();
        config.upsert_peer(PeerConfig {
            id: "office-pc".to_string(),
            name: "Office PC".to_string(),
            addr: "192.168.1.25:48571".to_string(),
            public_key: "cHVibGljX3Rlc3Q".to_string(),
            trusted: true,
        });
        let status = DaemonStatus {
            state: "connected-idle".to_string(),
            active_peer_id: Some("office-pc".to_string()),
            session_healthy: true,
            local_capture_enabled: true,
            input_injection_backend: "native".to_string(),
            notes: vec!["accessibility permission granted".to_string()],
        };

        assert!(active_peer_is_trusted(&config, Some(&status)));
        assert_eq!(
            status_lines(&config, Some(&status)),
            vec![
                "node: macbook-air".to_string(),
                "listen: 0.0.0.0:48571".to_string(),
                "state: connected-idle".to_string(),
                "peer: office-pc".to_string(),
                "trusted: yes".to_string(),
                "session: healthy".to_string(),
                "capture: enabled".to_string(),
                "inject: native".to_string(),
                "note: accessibility permission granted".to_string(),
            ]
        );
    }

    #[test]
    fn render_status_falls_back_when_daemon_is_not_running() {
        let config = Config::default();

        assert!(!active_peer_is_trusted(&config, None));
        assert_eq!(
            status_lines(&config, None),
            vec![
                "node: Local Node".to_string(),
                "listen: 0.0.0.0:48571".to_string(),
                "state: daemon-stopped".to_string(),
                "peer: -".to_string(),
                "trusted: no".to_string(),
                "session: unavailable".to_string(),
            ]
        );
    }
}
