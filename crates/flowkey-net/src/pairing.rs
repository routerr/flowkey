use anyhow::{anyhow, Context, Result};
use std::collections::HashSet;
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream};
use tokio::time::timeout;
use tracing::info;

use flowkey_config::Config;
use flowkey_protocol::message::{generate_sas_code, PairingMessage};

#[derive(Debug, Clone)]
pub struct PairingIdentity {
    pub id: String,
    pub name: String,
    pub public_key: String,
    pub listen_addr: String,
}

/// Result of a successful pairing handshake before user confirmation.
#[derive(Debug, Clone)]
pub struct PairingProposal {
    pub peer: PairingIdentity,
    pub sas_code: String,
    pub observed_addr: SocketAddr,
}

impl PairingProposal {
    /// Prefer the network path that completed pairing while retaining the peer's daemon port.
    pub fn preferred_peer_addr(&self) -> String {
        let daemon_port = self
            .peer
            .listen_addr
            .parse::<SocketAddr>()
            .ok()
            .map(|addr| addr.port())
            .or_else(|| {
                self.peer
                    .listen_addr
                    .rsplit_once(':')
                    .and_then(|(_, port)| port.parse::<u16>().ok())
            })
            .unwrap_or(48571);

        SocketAddr::new(self.observed_addr.ip(), daemon_port).to_string()
    }
}

fn pairing_target_with_default_port(target: &str, default_port: u16) -> Result<String> {
    let target = target.trim();
    if target.is_empty() {
        return Err(anyhow!("pairing address is required"));
    }
    if target.contains("://") {
        return Err(anyhow!("enter a host or IP address, not a URL"));
    }
    if target.parse::<SocketAddr>().is_ok() {
        return Ok(target.to_string());
    }
    if let Ok(ip) = target.trim_matches(&['[', ']'][..]).parse::<IpAddr>() {
        return Ok(SocketAddr::new(ip, default_port).to_string());
    }
    if let Some((host, port)) = target.rsplit_once(':') {
        if host.is_empty() {
            return Err(anyhow!("pairing host is required"));
        }
        port.parse::<u16>()
            .with_context(|| format!("invalid pairing port in {target}"))?;
        return Ok(target.to_string());
    }

    Ok(format!("{target}:{default_port}"))
}

pub async fn initiate_pairing_client_to_target(
    config: Config,
    target: &str,
    default_port: u16,
    attempt_timeout: Duration,
) -> Result<PairingProposal> {
    let target = pairing_target_with_default_port(target, default_port)?;
    let resolved = tokio::net::lookup_host(&target)
        .await
        .with_context(|| format!("could not resolve pairing address {target}"))?;
    let mut seen = HashSet::new();
    let addrs = resolved
        .filter(|addr| seen.insert(*addr))
        .take(4)
        .collect::<Vec<_>>();
    if addrs.is_empty() {
        return Err(anyhow!("pairing address {target} resolved to no endpoints"));
    }

    let mut failures = Vec::new();
    for addr in addrs {
        match timeout(
            attempt_timeout,
            initiate_pairing_client(config.clone(), addr),
        )
        .await
        {
            Ok(Ok(proposal)) => return Ok(proposal),
            Ok(Err(error)) => failures.push(format!("{addr}: {error:#}")),
            Err(_) => failures.push(format!("{addr}: timed out")),
        }
    }

    Err(anyhow!(
        "could not pair with {target}; {}",
        failures.join("; ")
    ))
}

pub async fn run_pairing_listener(
    config: Config,
    listener: TcpListener,
) -> Result<PairingProposal> {
    accept_pairing_listener(&config, &listener).await
}

pub async fn accept_pairing_listener(
    config: &Config,
    listener: &TcpListener,
) -> Result<PairingProposal> {
    let (mut stream, addr) = listener
        .accept()
        .await
        .context("failed to accept pairing connection")?;
    info!(%addr, "accepted pairing connection");

    // 1. Receive Proposal
    let proposal = match read_pairing_message(&mut stream).await? {
        PairingMessage::Propose {
            node_id,
            node_name,
            public_key,
            listen_addr,
        } => PairingIdentity {
            id: node_id,
            name: node_name,
            public_key,
            listen_addr,
        },
        other => return Err(anyhow!("expected PairingPropose, got {:?}", other)),
    };

    // 2. Send Acknowledge
    let response = PairingMessage::Acknowledge {
        node_id: config.node.id.clone(),
        node_name: config.node.name.clone(),
        public_key: config.node.public_key.clone(),
        listen_addr: config
            .advertised_listen_addr_for_pairing(None)
            .unwrap_or_else(|_| config.node.listen_addr.clone()),
    };
    write_pairing_message(&mut stream, &response).await?;

    let sas_code = generate_sas_code(&config.node.public_key, &proposal.public_key);

    Ok(PairingProposal {
        peer: proposal,
        sas_code,
        observed_addr: addr,
    })
}

pub async fn initiate_pairing_client(
    config: Config,
    peer_addr: SocketAddr,
) -> Result<PairingProposal> {
    let mut stream = TcpStream::connect(peer_addr)
        .await
        .context("failed to connect to peer pairing port")?;

    // 1. Send Propose
    let proposal = PairingMessage::Propose {
        node_id: config.node.id.clone(),
        node_name: config.node.name.clone(),
        public_key: config.node.public_key.clone(),
        listen_addr: config
            .advertised_listen_addr_for_pairing(None)
            .unwrap_or_else(|_| config.node.listen_addr.clone()),
    };
    write_pairing_message(&mut stream, &proposal).await?;

    // 2. Receive Acknowledge
    let peer_identity = match read_pairing_message(&mut stream).await? {
        PairingMessage::Acknowledge {
            node_id,
            node_name,
            public_key,
            listen_addr,
        } => PairingIdentity {
            id: node_id,
            name: node_name,
            public_key,
            listen_addr,
        },
        other => return Err(anyhow!("expected PairingAcknowledge, got {:?}", other)),
    };

    let sas_code = generate_sas_code(&config.node.public_key, &peer_identity.public_key);

    Ok(PairingProposal {
        peer: peer_identity,
        sas_code,
        observed_addr: peer_addr,
    })
}

async fn read_pairing_message(stream: &mut TcpStream) -> Result<PairingMessage> {
    use tokio::io::AsyncReadExt;
    let len = stream
        .read_u32()
        .await
        .context("failed to read pairing message length")?;
    if len > 65536 {
        return Err(anyhow!("pairing message too large"));
    }
    let mut buf = vec![0u8; len as usize];
    stream
        .read_exact(&mut buf)
        .await
        .context("failed to read pairing message payload")?;
    let msg = bincode::deserialize(&buf).context("failed to deserialize pairing message")?;
    Ok(msg)
}

async fn write_pairing_message(stream: &mut TcpStream, msg: &PairingMessage) -> Result<()> {
    use tokio::io::AsyncWriteExt;
    let buf = bincode::serialize(msg).context("failed to serialize pairing message")?;
    stream
        .write_u32(buf.len() as u32)
        .await
        .context("failed to write pairing message length")?;
    stream
        .write_all(&buf)
        .await
        .context("failed to write pairing message payload")?;
    stream.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;

    use flowkey_config::{CaptureMode, Config, NodeConfig, SwitchConfig};
    use tokio::net::TcpListener;

    use super::{
        initiate_pairing_client, initiate_pairing_client_to_target,
        pairing_target_with_default_port, run_pairing_listener, PairingIdentity, PairingProposal,
    };

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
    async fn pairing_proposals_record_observed_addresses() {
        let config = test_config();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server_task = tokio::spawn(run_pairing_listener(config.clone(), listener));
        let client_proposal = initiate_pairing_client(config, addr).await.unwrap();
        let server_proposal = server_task.await.unwrap().unwrap();

        assert_eq!(client_proposal.observed_addr, addr);
        assert!(server_proposal.observed_addr.ip().is_loopback());
        assert_ne!(server_proposal.observed_addr.port(), 0);
        assert_ne!(
            server_proposal.observed_addr,
            SocketAddr::from(([0, 0, 0, 0], 0))
        );
    }

    #[tokio::test]
    async fn pairing_client_connects_to_hostname_with_explicit_port() {
        let config = test_config();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server_task = tokio::spawn(run_pairing_listener(config.clone(), listener));

        let client_proposal = initiate_pairing_client_to_target(
            config,
            &format!("localhost:{port}"),
            48572,
            std::time::Duration::from_secs(2),
        )
        .await
        .unwrap();
        let server_proposal = server_task.await.unwrap().unwrap();

        assert!(client_proposal.observed_addr.ip().is_loopback());
        assert!(server_proposal.observed_addr.ip().is_loopback());
    }

    #[test]
    fn pairing_targets_default_and_preserve_ports() {
        assert_eq!(
            pairing_target_with_default_port("192.168.1.102", 48572).unwrap(),
            "192.168.1.102:48572"
        );
        assert_eq!(
            pairing_target_with_default_port("win.example.test", 48572).unwrap(),
            "win.example.test:48572"
        );
        assert_eq!(
            pairing_target_with_default_port("win.example.test:50000", 48572).unwrap(),
            "win.example.test:50000"
        );
        assert_eq!(
            pairing_target_with_default_port("2001:db8::1", 48572).unwrap(),
            "[2001:db8::1]:48572"
        );
        assert!(pairing_target_with_default_port("ssh://win", 48572).is_err());
    }

    #[test]
    fn preferred_peer_address_uses_observed_route_and_daemon_port() {
        let proposal = PairingProposal {
            peer: PairingIdentity {
                id: "win".to_string(),
                name: "Windows".to_string(),
                public_key: "key".to_string(),
                listen_addr: "win.example.test:48571".to_string(),
            },
            sas_code: "123456".to_string(),
            observed_addr: "192.168.1.102:48572".parse().unwrap(),
        };

        assert_eq!(proposal.preferred_peer_addr(), "192.168.1.102:48571");
    }
}
