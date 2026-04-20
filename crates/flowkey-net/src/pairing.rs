use anyhow::{anyhow, Context, Result};
use std::net::SocketAddr;
use tokio::net::{TcpListener, TcpStream};
use tracing::info;

use flowkey_config::Config;
use flowkey_protocol::message::{generate_sas_code, PairingMessage};

pub struct PairingIdentity {
    pub id: String,
    pub name: String,
    pub public_key: String,
}

/// Result of a successful pairing handshake before user confirmation.
pub struct PairingProposal {
    pub peer: PairingIdentity,
    pub sas_code: String,
    pub observed_addr: SocketAddr,
}

pub async fn run_pairing_listener(
    config: Config,
    listener: TcpListener,
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
        } => PairingIdentity {
            id: node_id,
            name: node_name,
            public_key,
        },
        other => return Err(anyhow!("expected PairingPropose, got {:?}", other)),
    };

    // 2. Send Acknowledge
    let response = PairingMessage::Acknowledge {
        node_id: config.node.id.clone(),
        node_name: config.node.name.clone(),
        public_key: config.node.public_key.clone(),
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
    };
    write_pairing_message(&mut stream, &proposal).await?;

    // 2. Receive Acknowledge
    let peer_identity = match read_pairing_message(&mut stream).await? {
        PairingMessage::Acknowledge {
            node_id,
            node_name,
            public_key,
        } => PairingIdentity {
            id: node_id,
            name: node_name,
            public_key,
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

    use super::{initiate_pairing_client, run_pairing_listener};

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
}
