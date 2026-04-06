use anyhow::{Context, Result};
use rand::{thread_rng, Rng};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::time::Duration;
use tokio::net::UdpSocket;
use tokio::time::timeout;
use tracing::{debug, warn};

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ProbeMessage {
    ProbeRequest { sender_id: String, nonce: String },
    ProbeResponse { responder_id: String, nonce: String },
}

pub async fn run_reachability_race(
    candidates: &[String],
    expected_peer_id: &str,
    timeout_duration: Duration,
) -> Result<String> {
    if candidates.is_empty() {
        return Err(anyhow::anyhow!("no candidate addresses provided"));
    }

    if candidates.len() == 1 {
        // Optimization: if there's only one candidate, we can just return it without racing,
        // as the TCP connection will act as the reachability test anyway.
        return Ok(candidates[0].clone());
    }

    let socket = UdpSocket::bind("0.0.0.0:0")
        .await
        .context("failed to bind UDP socket for reachability probe")?;

    // Broadcast enabled just in case, though we are sending to specific IPs
    socket.set_broadcast(true).ok();

    let nonce: String = {
        let mut rng = thread_rng();
        (0..8).map(|_| format!("{:02x}", rng.gen::<u8>())).collect()
    };

    let request = ProbeMessage::ProbeRequest {
        sender_id: "probe-client".to_string(), // Client ID isn't strictly necessary for the response match
        nonce: nonce.clone(),
    };

    let payload = serde_json::to_vec(&request)?;

    for candidate in candidates {
        if let Ok(addr) = candidate.parse::<SocketAddr>() {
            debug!(candidate = %addr, "sending reachability probe");
            if let Err(error) = socket.send_to(&payload, addr).await {
                warn!(candidate = %addr, %error, "failed to send probe");
            }
        } else {
            warn!(candidate = %candidate, "invalid candidate address format");
        }
    }

    let mut buf = [0u8; 1024];

    let result = timeout(timeout_duration, async {
        loop {
            match socket.recv_from(&mut buf).await {
                Ok((len, addr)) => {
                    if let Ok(ProbeMessage::ProbeResponse { responder_id, nonce: resp_nonce }) = serde_json::from_slice(&buf[..len]) {
                        if resp_nonce == nonce && responder_id == expected_peer_id {
                            debug!(winner = %addr, "received valid probe response");
                            return Ok(addr.to_string());
                        } else {
                            debug!(winner = %addr, expected = %expected_peer_id, got = %responder_id, "probe response mismatch");
                        }
                    }
                }
                Err(error) => {
                    warn!(%error, "error receiving probe response");
                }
            }
        }
    }).await;

    match result {
        Ok(Ok(winner)) => Ok(winner),
        Ok(Err(e)) => Err(e),
        Err(_) => Err(anyhow::anyhow!(
            "reachability probe timed out before receiving a valid response"
        )),
    }
}

pub async fn listen_for_probes(listen_addr: String, local_node_id: String) {
    let socket = match UdpSocket::bind(&listen_addr).await {
        Ok(socket) => socket,
        Err(error) => {
            warn!(%error, addr = %listen_addr, "failed to bind UDP socket for probe responder");
            return;
        }
    };

    debug!(addr = %listen_addr, "listening for reachability probes");

    let mut buf = [0u8; 1024];
    loop {
        match socket.recv_from(&mut buf).await {
            Ok((len, addr)) => {
                if let Ok(ProbeMessage::ProbeRequest { nonce, .. }) =
                    serde_json::from_slice(&buf[..len])
                {
                    let response = ProbeMessage::ProbeResponse {
                        responder_id: local_node_id.clone(),
                        nonce,
                    };

                    if let Ok(payload) = serde_json::to_vec(&response) {
                        if let Err(error) = socket.send_to(&payload, addr).await {
                            warn!(peer = %addr, %error, "failed to send probe response");
                        }
                    }
                }
            }
            Err(error) => {
                warn!(%error, "error receiving UDP packet in probe responder");
                // Brief pause to prevent tight spin on persistent errors
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
}
