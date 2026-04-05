use anyhow::{anyhow, Context, Result};
use flowkey_config::{Config, PeerConfig};
use flowkey_crypto::{NodeIdentity, SessionChallenge, SessionResponse};
use flowkey_input::event::InputEvent;
use flowkey_protocol::message::{
    AuthChallengePayload, AuthResponsePayload, AuthResultPayload, HelloPayload, Message,
    PROTOCOL_VERSION,
};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use tokio::time::{interval, Duration};
use tracing::{info, warn};

use crate::frame::{read_message, write_message};
use crate::heartbeat::HeartbeatConfig;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionInfo {
    pub peer_id: String,
    pub connected: bool,
    pub authenticated: bool,
}

pub struct AuthenticatedConnection {
    pub info: ConnectionInfo,
    stream: TcpStream,
}

impl AuthenticatedConnection {
    pub fn into_parts(self) -> (ConnectionInfo, TcpStream) {
        (self.info, self.stream)
    }
}

#[derive(Debug, Clone)]
pub struct SessionSender {
    sender: UnboundedSender<InputEvent>,
}

impl SessionSender {
    pub fn send_input(&self, event: InputEvent) -> Result<(), String> {
        self.sender
            .send(event)
            .map_err(|_| "session command channel closed".to_string())
    }
}

pub fn session_channel() -> (SessionSender, UnboundedReceiver<InputEvent>) {
    let (sender, receiver) = unbounded_channel();
    (SessionSender { sender }, receiver)
}

pub fn find_trusted_peer<'a>(config: &'a Config, peer_id: &str) -> Result<&'a PeerConfig> {
    config
        .peers
        .iter()
        .find(|peer| peer.id == peer_id && peer.trusted)
        .ok_or_else(|| anyhow!("trusted peer not found"))
}

pub fn authenticate_trusted_peer(
    config: &Config,
    challenge: &SessionChallenge,
    response: &SessionResponse,
) -> Result<ConnectionInfo> {
    let peer = find_trusted_peer(config, &response.responder_node_id)?;
    let peer_identity = NodeIdentity {
        node_id: peer.id.clone(),
        node_name: peer.name.clone(),
        listen_addr: peer.addr.clone(),
        public_key: peer.public_key.clone(),
    };

    challenge.verify_response(response, &peer_identity)?;

    Ok(ConnectionInfo {
        peer_id: peer.id.clone(),
        connected: true,
        authenticated: true,
    })
}

pub async fn connect_and_authenticate(
    config: &Config,
    peer: &PeerConfig,
) -> Result<AuthenticatedConnection> {
    let mut stream = TcpStream::connect(&peer.addr)
        .await
        .with_context(|| format!("failed to connect to {}", peer.addr))?;

    write_message(
        &mut stream,
        &Message::Hello(HelloPayload {
            version: PROTOCOL_VERSION,
            node_id: config.node.id.clone(),
            node_name: config.node.name.clone(),
        }),
    )
    .await?;

    let server_hello = match read_message(&mut stream).await? {
        Message::HelloAck(payload) => payload,
        other => return Err(anyhow!("expected HelloAck, got {:?}", other)),
    };

    if server_hello.version != PROTOCOL_VERSION {
        return Err(anyhow!("protocol version mismatch"));
    }

    if server_hello.node_id != peer.id {
        return Err(anyhow!("connected peer id does not match trusted config"));
    }

    let challenge = match read_message(&mut stream).await? {
        Message::AuthChallenge(payload) => SessionChallenge {
            session_id: payload.session_id,
            challenger_node_id: payload.challenger_node_id,
            nonce: payload.nonce,
        },
        other => return Err(anyhow!("expected AuthChallenge, got {:?}", other)),
    };

    let response = challenge.sign_response(&config.node.id, &config.node.private_key)?;
    write_message(
        &mut stream,
        &Message::AuthResponse(AuthResponsePayload {
            session_id: response.session_id.clone(),
            responder_node_id: response.responder_node_id.clone(),
            signature: response.signature.clone(),
        }),
    )
    .await?;

    match read_message(&mut stream).await? {
        Message::AuthResult(result) if result.ok => {}
        Message::AuthResult(result) => {
            return Err(anyhow!(
                "server rejected auth: {}",
                result.error.unwrap_or_else(|| "unknown error".to_string())
            ))
        }
        other => {
            return Err(anyhow!(
                "expected AuthResult after client auth, got {:?}",
                other
            ))
        }
    }

    let server_challenge = SessionChallenge::new(config.node.id.clone());
    write_message(
        &mut stream,
        &Message::AuthChallenge(AuthChallengePayload {
            session_id: server_challenge.session_id.clone(),
            challenger_node_id: server_challenge.challenger_node_id.clone(),
            nonce: server_challenge.nonce.clone(),
        }),
    )
    .await?;

    let server_response = match read_message(&mut stream).await? {
        Message::AuthResponse(payload) => SessionResponse {
            session_id: payload.session_id,
            responder_node_id: payload.responder_node_id,
            signature: payload.signature,
        },
        other => return Err(anyhow!("expected server AuthResponse, got {:?}", other)),
    };

    let server_identity = NodeIdentity {
        node_id: peer.id.clone(),
        node_name: peer.name.clone(),
        listen_addr: peer.addr.clone(),
        public_key: peer.public_key.clone(),
    };
    server_challenge.verify_response(&server_response, &server_identity)?;

    write_message(
        &mut stream,
        &Message::AuthResult(AuthResultPayload {
            ok: true,
            peer_id: Some(peer.id.clone()),
            error: None,
        }),
    )
    .await?;

    Ok(AuthenticatedConnection {
        info: ConnectionInfo {
            peer_id: peer.id.clone(),
            connected: true,
            authenticated: true,
        },
        stream,
    })
}

pub async fn authenticate_incoming_stream(
    config: &Config,
    mut stream: TcpStream,
) -> Result<AuthenticatedConnection> {
    let client_hello = match read_message(&mut stream).await? {
        Message::Hello(payload) => payload,
        other => return Err(anyhow!("expected Hello, got {:?}", other)),
    };

    if client_hello.version != PROTOCOL_VERSION {
        return Err(anyhow!("protocol version mismatch"));
    }

    let peer = find_trusted_peer(config, &client_hello.node_id)?;

    write_message(
        &mut stream,
        &Message::HelloAck(HelloPayload {
            version: PROTOCOL_VERSION,
            node_id: config.node.id.clone(),
            node_name: config.node.name.clone(),
        }),
    )
    .await?;

    let challenge = SessionChallenge::new(config.node.id.clone());
    write_message(
        &mut stream,
        &Message::AuthChallenge(AuthChallengePayload {
            session_id: challenge.session_id.clone(),
            challenger_node_id: challenge.challenger_node_id.clone(),
            nonce: challenge.nonce.clone(),
        }),
    )
    .await?;

    let client_response = match read_message(&mut stream).await? {
        Message::AuthResponse(payload) => SessionResponse {
            session_id: payload.session_id,
            responder_node_id: payload.responder_node_id,
            signature: payload.signature,
        },
        other => return Err(anyhow!("expected client AuthResponse, got {:?}", other)),
    };

    let auth_result = authenticate_trusted_peer(config, &challenge, &client_response);
    match auth_result {
        Ok(_) => {
            write_message(
                &mut stream,
                &Message::AuthResult(AuthResultPayload {
                    ok: true,
                    peer_id: Some(peer.id.clone()),
                    error: None,
                }),
            )
            .await?;
        }
        Err(error) => {
            write_message(
                &mut stream,
                &Message::AuthResult(AuthResultPayload {
                    ok: false,
                    peer_id: None,
                    error: Some(error.to_string()),
                }),
            )
            .await?;
            return Err(error);
        }
    }

    let server_challenge = match read_message(&mut stream).await? {
        Message::AuthChallenge(payload) => SessionChallenge {
            session_id: payload.session_id,
            challenger_node_id: payload.challenger_node_id,
            nonce: payload.nonce,
        },
        other => return Err(anyhow!("expected client AuthChallenge, got {:?}", other)),
    };

    let server_response =
        server_challenge.sign_response(&config.node.id, &config.node.private_key)?;
    write_message(
        &mut stream,
        &Message::AuthResponse(AuthResponsePayload {
            session_id: server_response.session_id,
            responder_node_id: server_response.responder_node_id,
            signature: server_response.signature,
        }),
    )
    .await?;

    match read_message(&mut stream).await? {
        Message::AuthResult(result) if result.ok => {}
        Message::AuthResult(result) => {
            return Err(anyhow!(
                "client rejected server auth: {}",
                result.error.unwrap_or_else(|| "unknown error".to_string())
            ))
        }
        other => return Err(anyhow!("expected final AuthResult, got {:?}", other)),
    }

    Ok(AuthenticatedConnection {
        info: ConnectionInfo {
            peer_id: peer.id.clone(),
            connected: true,
            authenticated: true,
        },
        stream,
    })
}

pub async fn accept_and_authenticate(
    config: &Config,
    listener: TcpListener,
) -> Result<AuthenticatedConnection> {
    let (stream, _) = listener
        .accept()
        .await
        .context("failed to accept connection")?;
    authenticate_incoming_stream(config, stream).await
}

pub async fn run_authenticated_session(
    mut connection: AuthenticatedConnection,
    heartbeat: HeartbeatConfig,
    sink: &mut dyn flowkey_input::InputEventSink,
    mut outbound: UnboundedReceiver<InputEvent>,
) -> Result<()> {
    let mut ticker = interval(Duration::from_secs(heartbeat.interval_secs));
    let mut outbound_open = true;
    let mut sequence: u64 = 0;
    let peer_id = connection.info.peer_id.clone();
    let stream = &mut connection.stream;

    loop {
        tokio::select! {
            biased;
            _ = ticker.tick() => {
                write_message(stream, &Message::Heartbeat).await?;
            }
            maybe_event = outbound.recv(), if outbound_open => {
                match maybe_event {
                    Some(event) => {
                        sequence = sequence.wrapping_add(1);
                        write_message(stream, &Message::InputEvent { sequence, event }).await?;
                    }
                    None => {
                        outbound_open = false;
                    }
                }
            }
            result = tokio::time::timeout(Duration::from_secs(heartbeat.timeout_secs), read_message(stream)) => {
                let message = match result {
                    Ok(result) => result?,
                    Err(_) => {
                        return Err(anyhow!("heartbeat timeout for peer {}", peer_id));
                    }
                };

                match message {
                    Message::Heartbeat => {
                        // Keepalive acknowledged.
                    }
                    Message::InputEvent { sequence, event } => {
                        info!(peer = %peer_id, sequence, event = ?event, "received input event");
                        if let Err(error) = flowkey_net_route_input_event(sink, &event) {
                            warn!(peer = %peer_id, %error, "input injection failed, continuing session");
                        }
                    }
                    Message::SwitchRequest { peer_id, request_id } => {
                        warn!(peer = %peer_id, request = %request_id, "switch request received but not yet handled");
                    }
                    Message::SwitchRelease { request_id } => {
                        warn!(request = %request_id, "switch release received but not yet handled");
                    }
                    Message::Error { code, message } => {
                        return Err(anyhow!("peer error {}: {}", code, message));
                    }
                    other => {
                        warn!(peer = %peer_id, message = ?other, "unexpected session message");
                    }
                }
            }
        }
    }
}

pub fn route_input_event(
    sink: &mut dyn flowkey_input::InputEventSink,
    event: &InputEvent,
) -> Result<()> {
    sink.handle(event).map_err(|error| anyhow!(error))
}

fn flowkey_net_route_input_event(
    sink: &mut dyn flowkey_input::InputEventSink,
    event: &InputEvent,
) -> Result<()> {
    route_input_event(sink, event)
}

#[cfg(test)]
mod tests {
    use base64::engine::general_purpose::STANDARD_NO_PAD;
    use base64::Engine;
    use ed25519_dalek::SigningKey;
    use flowkey_config::{Config, PeerConfig};
    use flowkey_crypto::SessionChallenge;
    use flowkey_input::event::{InputEvent, Modifiers};
    use tokio::net::{TcpListener, TcpStream};

    use super::{
        accept_and_authenticate, authenticate_trusted_peer, connect_and_authenticate,
        run_authenticated_session, session_channel, AuthenticatedConnection, ConnectionInfo,
    };

    struct NoopSink;

    impl flowkey_input::InputEventSink for NoopSink {
        fn handle(&mut self, _event: &flowkey_input::event::InputEvent) -> Result<(), String> {
            Ok(())
        }

        fn release_all(&mut self) -> Result<(), String> {
            Ok(())
        }
    }

    #[test]
    fn authenticates_known_trusted_peer() {
        let signing_key = SigningKey::generate(&mut rand::rngs::OsRng);
        let public_key = STANDARD_NO_PAD.encode(signing_key.verifying_key().to_bytes());
        let private_key = STANDARD_NO_PAD.encode(signing_key.to_bytes());

        let mut config = Config::default();
        config.upsert_peer(PeerConfig {
            id: "office-pc".to_string(),
            name: "Office PC".to_string(),
            addr: "192.168.1.25:48571".to_string(),
            public_key,
            trusted: true,
        });

        let challenge = SessionChallenge::new("macbook-air");
        let response = challenge
            .sign_response("office-pc", &private_key)
            .expect("response should sign");

        let result =
            authenticate_trusted_peer(&config, &challenge, &response).expect("peer should auth");

        assert_eq!(result.peer_id, "office-pc");
        assert!(result.connected);
        assert!(result.authenticated);
    }

    #[test]
    fn rejects_unknown_peer() {
        let signing_key = SigningKey::generate(&mut rand::rngs::OsRng);
        let private_key = STANDARD_NO_PAD.encode(signing_key.to_bytes());
        let config = Config::default();
        let challenge = SessionChallenge::new("macbook-air");
        let response = challenge
            .sign_response("unknown-peer", &private_key)
            .expect("response should sign");

        let result = authenticate_trusted_peer(&config, &challenge, &response);
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn tcp_handshake_authenticates_bidirectionally() {
        let mut server_config = test_config("server-node", "Server Node");
        let mut client_config = test_config("client-node", "Client Node");

        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener should bind");
        let addr = listener.local_addr().expect("listener should have addr");
        server_config.node.listen_addr = addr.to_string();

        server_config.upsert_peer(PeerConfig {
            id: client_config.node.id.clone(),
            name: client_config.node.name.clone(),
            addr: "client-placeholder".to_string(),
            public_key: client_config.node.public_key.clone(),
            trusted: true,
        });
        client_config.upsert_peer(PeerConfig {
            id: server_config.node.id.clone(),
            name: server_config.node.name.clone(),
            addr: addr.to_string(),
            public_key: server_config.node.public_key.clone(),
            trusted: true,
        });

        let server_task = tokio::spawn(async move {
            accept_and_authenticate(&server_config, listener)
                .await
                .expect("server should authenticate client")
        });

        let client_peer = client_config.peers[0].clone();
        let client_result = connect_and_authenticate(&client_config, &client_peer)
            .await
            .expect("client should authenticate server");
        let server_result = server_task.await.expect("server task should complete");

        assert_eq!(client_result.info.peer_id, "server-node");
        assert!(client_result.info.authenticated);
        assert_eq!(server_result.info.peer_id, "client-node");
        assert!(server_result.info.authenticated);
    }

    #[tokio::test]
    async fn incoming_stream_authenticates_client_side() {
        let mut server_config = test_config("server-node", "Server Node");
        let client_config = test_config("client-node", "Client Node");
        let server_public_key = server_config.node.public_key.clone();

        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener should bind");
        let addr = listener.local_addr().expect("listener should have addr");
        server_config.node.listen_addr = addr.to_string();

        server_config.upsert_peer(PeerConfig {
            id: client_config.node.id.clone(),
            name: client_config.node.name.clone(),
            addr: addr.to_string(),
            public_key: client_config.node.public_key.clone(),
            trusted: true,
        });

        let server_task = tokio::spawn(async move {
            accept_and_authenticate(&server_config, listener)
                .await
                .expect("server should authenticate client")
        });

        let client_peer = PeerConfig {
            id: "server-node".to_string(),
            name: "Server Node".to_string(),
            addr: addr.to_string(),
            public_key: server_public_key,
            trusted: true,
        };
        let client_result = connect_and_authenticate(&client_config, &client_peer)
            .await
            .expect("client should authenticate server");
        let server_result = server_task.await.expect("server task should complete");

        assert_eq!(client_result.info.peer_id, "server-node");
        assert!(client_result.info.authenticated);
        assert_eq!(server_result.info.peer_id, "client-node");
        assert!(server_result.info.authenticated);
    }

    #[tokio::test]
    async fn outbound_input_events_are_forwarded_over_the_session_stream() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener should bind");
        let addr = listener.local_addr().expect("listener should have addr");

        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("server should accept");
            super::read_message(&mut stream)
                .await
                .expect("server should read")
        });

        let client = TcpStream::connect(addr)
            .await
            .expect("client should connect");
        let connection = AuthenticatedConnection {
            info: ConnectionInfo {
                peer_id: "office-pc".to_string(),
                connected: true,
                authenticated: true,
            },
            stream: client,
        };
        let (sender, receiver) = session_channel();

        let session = tokio::spawn(async move {
            let mut sink = NoopSink;
            run_authenticated_session(
                connection,
                super::HeartbeatConfig {
                    interval_secs: 60,
                    timeout_secs: 60,
                },
                &mut sink,
                receiver,
            )
            .await
        });

        sender
            .send_input(InputEvent::KeyDown {
                code: "KeyK".to_string(),
                modifiers: Modifiers {
                    shift: false,
                    control: true,
                    alt: false,
                    meta: false,
                },
            })
            .expect("sender should accept event");

        let message = server.await.expect("server task should complete");
        assert_eq!(
            message,
            super::Message::InputEvent {
                sequence: 1,
                event: InputEvent::KeyDown {
                    code: "KeyK".to_string(),
                    modifiers: Modifiers {
                        shift: false,
                        control: true,
                        alt: false,
                        meta: false,
                    },
                }
            }
        );

        session.abort();
    }

    fn test_config(node_id: &str, node_name: &str) -> Config {
        let mut config = Config::default();
        config.node.id = node_id.to_string();
        config.node.name = node_name.to_string();
        config
            .regenerate_node_keys()
            .expect("test config should generate keys");
        config
    }
}
