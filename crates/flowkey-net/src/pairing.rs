use std::net::SocketAddr;
use anyhow::{anyhow, Context, Result};
use tokio::net::{TcpListener, TcpStream};
use tracing::info;

use flowkey_config::Config;
use flowkey_protocol::message::{PairingMessage, generate_sas_code};

pub struct PairingIdentity {
    pub id: String,
    pub name: String,
    pub public_key: String,
}

/// Result of a successful pairing handshake before user confirmation.
pub struct PairingProposal {
    pub peer: PairingIdentity,
    pub sas_code: String,
}

pub async fn run_pairing_listener(
    config: Config,
    listener: TcpListener,
) -> Result<PairingProposal> {
    let (mut stream, addr) = listener.accept().await.context("failed to accept pairing connection")?;
    info!(%addr, "accepted pairing connection");

    // 1. Receive Proposal
    let proposal = match read_pairing_message(&mut stream).await? {
        PairingMessage::Propose { node_id, node_name, public_key } => {
            PairingIdentity { id: node_id, name: node_name, public_key }
        }
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
    })
}

pub async fn initiate_pairing_client(
    config: Config,
    peer_addr: SocketAddr,
) -> Result<PairingProposal> {
    let mut stream = TcpStream::connect(peer_addr).await.context("failed to connect to peer pairing port")?;
    
    // 1. Send Propose
    let proposal = PairingMessage::Propose {
        node_id: config.node.id.clone(),
        node_name: config.node.name.clone(),
        public_key: config.node.public_key.clone(),
    };
    write_pairing_message(&mut stream, &proposal).await?;

    // 2. Receive Acknowledge
    let peer_identity = match read_pairing_message(&mut stream).await? {
        PairingMessage::Acknowledge { node_id, node_name, public_key } => {
            PairingIdentity { id: node_id, name: node_name, public_key }
        }
        other => return Err(anyhow!("expected PairingAcknowledge, got {:?}", other)),
    };

    let sas_code = generate_sas_code(&config.node.public_key, &peer_identity.public_key);

    Ok(PairingProposal {
        peer: peer_identity,
        sas_code,
    })
}

async fn read_pairing_message(stream: &mut TcpStream) -> Result<PairingMessage> {
    use tokio::io::AsyncReadExt;
    let len = stream.read_u32().await.context("failed to read pairing message length")?;
    if len > 65536 {
        return Err(anyhow!("pairing message too large"));
    }
    let mut buf = vec![0u8; len as usize];
    stream.read_exact(&mut buf).await.context("failed to read pairing message payload")?;
    let msg = bincode::deserialize(&buf).context("failed to deserialize pairing message")?;
    Ok(msg)
}

async fn write_pairing_message(stream: &mut TcpStream, msg: &PairingMessage) -> Result<()> {
    use tokio::io::AsyncWriteExt;
    let buf = bincode::serialize(msg).context("failed to serialize pairing message")?;
    stream.write_u32(buf.len() as u32).await.context("failed to write pairing message length")?;
    stream.write_all(&buf).await.context("failed to write pairing message payload")?;
    stream.flush().await?;
    Ok(())
}
