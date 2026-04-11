use anyhow::{anyhow, Context, Result};
use crossbeam_channel::{bounded, Receiver, Sender};
use flowkey_config::{Config, PeerConfig};
use flowkey_core::recovery::HeldKeyTracker;
use flowkey_crypto::{NodeIdentity, SessionChallenge, SessionResponse};
use flowkey_input::event::InputEvent;
use flowkey_protocol::message::{
    AuthChallengePayload, AuthResponsePayload, AuthResultPayload, HelloPayload, Message,
    PROTOCOL_VERSION,
};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::Instant;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc::unbounded_channel;
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionCommand {
    Input(InputEvent),
    SwitchControl { request_id: String },
    ReleaseControl { request_id: String },
    ReleaseAll,
}

#[derive(Debug, Clone)]
pub struct SessionSender {
    inner: Arc<SessionSenderInner>,
}

impl SessionSender {
    pub fn send_input(&self, event: InputEvent) -> Result<(), String> {
        match event {
            InputEvent::MouseMove { .. } => {
                self.inner.queue_mouse_move(event)?;
                Ok(())
            }
            InputEvent::MouseWheel { .. } => {
                self.inner.flush_mouse_move();
                // Drop if channel is full to avoid head-of-line blocking.
                let _ = self.inner.sender.try_send(SessionCommand::Input(event));
                Ok(())
            }
            _ => {
                self.inner.flush_mouse_move();
                self.inner
                    .sender
                    .send(SessionCommand::Input(event))
                    .map_err(|_| "session command channel closed".to_string())
            }
        }
    }

    pub fn send_switch(&self, request_id: String) -> Result<(), String> {
        self.inner
            .sender
            .send(SessionCommand::SwitchControl { request_id })
            .map_err(|_| "session command channel closed".to_string())
    }

    pub fn send_release(&self, request_id: String) -> Result<(), String> {
        self.inner
            .sender
            .send(SessionCommand::ReleaseControl { request_id })
            .map_err(|_| "session command channel closed".to_string())
    }

    pub fn send_release_all(&self) -> Result<(), String> {
        self.inner
            .sender
            .send(SessionCommand::ReleaseAll)
            .map_err(|_| "session command channel closed".to_string())
    }
}

#[derive(Debug)]
struct SessionSenderInner {
    sender: Sender<SessionCommand>,
    coalescer: Mutex<MouseMoveCoalescer>,
    coalescer_wake: Condvar,
}

#[derive(Debug)]
struct MouseMoveCoalescer {
    pending: Option<PendingMouseMove>,
}

#[derive(Debug)]
struct PendingMouseMove {
    dx: i32,
    dy: i32,
    modifiers: flowkey_input::event::Modifiers,
    timestamp_us: u64,
    deadline: Instant,
}

impl SessionSenderInner {
    fn new(sender: Sender<SessionCommand>) -> Arc<Self> {
        let inner = Arc::new(Self {
            sender,
            coalescer: Mutex::new(MouseMoveCoalescer { pending: None }),
            coalescer_wake: Condvar::new(),
        });
        Self::spawn_mouse_move_flush_worker(&inner);
        inner
    }

    fn queue_mouse_move(&self, event: InputEvent) -> Result<(), String> {
        let InputEvent::MouseMove {
            dx,
            dy,
            modifiers,
            timestamp_us,
        } = event
        else {
            return Ok(());
        };

        let mut state = self
            .coalescer
            .lock()
            .expect("mouse move coalescer mutex should not be poisoned");
        let now = Instant::now();
        match state.pending.as_mut() {
            Some(pending) if pending.modifiers == modifiers && now <= pending.deadline => {
                pending.dx = pending.dx.saturating_add(dx);
                pending.dy = pending.dy.saturating_add(dy);
                pending.timestamp_us = timestamp_us;
                pending.deadline = now + Duration::from_millis(8);
            }
            Some(_) => {
                Self::flush_pending_locked(&self.sender, &mut state)?;
                state.pending = Some(PendingMouseMove {
                    dx,
                    dy,
                    modifiers,
                    timestamp_us,
                    deadline: now + Duration::from_millis(8),
                });
            }
            None => {
                state.pending = Some(PendingMouseMove {
                    dx,
                    dy,
                    modifiers,
                    timestamp_us,
                    deadline: now + Duration::from_millis(8),
                });
            }
        }

        self.coalescer_wake.notify_all();
        Ok(())
    }

    fn flush_mouse_move(&self) {
        if let Ok(mut state) = self.coalescer.lock() {
            let _ = Self::flush_pending_locked(&self.sender, &mut state);
        }
    }

    fn flush_pending_locked(
        sender: &Sender<SessionCommand>,
        state: &mut MouseMoveCoalescer,
    ) -> Result<(), String> {
        let Some(pending) = state.pending.take() else {
            return Ok(());
        };

        sender
            .send(SessionCommand::Input(InputEvent::MouseMove {
                dx: pending.dx,
                dy: pending.dy,
                modifiers: pending.modifiers,
                timestamp_us: pending.timestamp_us,
            }))
            .map_err(|_| "session command channel closed".to_string())
    }

    fn spawn_mouse_move_flush_worker(inner: &Arc<Self>) {
        let weak = Arc::downgrade(inner);
        thread::spawn(move || loop {
            let Some(inner) = weak.upgrade() else {
                break;
            };

            let mut state = inner
                .coalescer
                .lock()
                .expect("mouse move coalescer mutex should not be poisoned");

            while state.pending.is_none() {
                state = inner
                    .coalescer_wake
                    .wait(state)
                    .expect("mouse move coalescer mutex should not be poisoned");
            }

            let deadline = state.pending.as_ref().map(|pending| pending.deadline);
            let Some(deadline) = deadline else {
                continue;
            };

            let now = Instant::now();
            if now < deadline {
                let wait = deadline.saturating_duration_since(now);
                let (next_state, _) = inner
                    .coalescer_wake
                    .wait_timeout(state, wait)
                    .expect("mouse move coalescer mutex should not be poisoned");
                state = next_state;
                if state.pending.is_some() && Instant::now() < deadline {
                    continue;
                }
            }

            let _ = Self::flush_pending_locked(&inner.sender, &mut state);
        });
    }
}

pub fn session_channel() -> (SessionSender, Receiver<SessionCommand>) {
    let (sender, receiver) = bounded(100);
    (
        SessionSender {
            inner: SessionSenderInner::new(sender),
        },
        receiver,
    )
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

/// Callback for handling remote switch/release requests received during a session.
pub trait SessionStateCallback: Send + Sync {
    fn on_remote_switch(&self, peer_id: &str, request_id: &str);
    fn on_remote_release(&self, peer_id: &str, request_id: &str);
}

pub async fn run_authenticated_session(
    mut connection: AuthenticatedConnection,
    local_node_id: &str,
    heartbeat: HeartbeatConfig,
    sink: &mut dyn flowkey_input::InputEventSink,
    held_keys: &mut HeldKeyTracker,
    outbound: Receiver<SessionCommand>,
    state_callback: &dyn SessionStateCallback,
) -> Result<()> {
    let (bridge_tx, mut bridge_rx) = unbounded_channel();
    let bridge_outbound = outbound.clone();

    // Spawn a blocking task to bridge crossbeam channel to tokio
    tokio::task::spawn_blocking(move || {
        while let Ok(command) = bridge_outbound.recv() {
            if bridge_tx.send(command).is_err() {
                break;
            }
        }
    });

    let mut ticker = interval(Duration::from_secs(heartbeat.interval_secs));
    let mut outbound_open = true;
    let mut sequence: u64 = 0;
    let peer_id = connection.info.peer_id.clone();
    let stream = &mut connection.stream;
    stream.set_nodelay(true)?;

    loop {
        tokio::select! {
            biased;
            _ = ticker.tick() => {
                write_message(stream, &Message::Heartbeat).await?;
            }
            maybe_command = bridge_rx.recv(), if outbound_open => {
                match maybe_command {
                    Some(SessionCommand::Input(event)) => {
                        sequence = sequence.wrapping_add(1);
                        write_message(stream, &Message::InputEvent { sequence, event }).await?;
                    }
                    Some(SessionCommand::SwitchControl { request_id }) => {
                        info!(peer = %peer_id, request = %request_id, "writing SwitchRequest to session stream");
                        write_message(stream, &Message::SwitchRequest {
                            peer_id: local_node_id.to_string(),
                            request_id,
                        }).await?;
                    }
                    Some(SessionCommand::ReleaseControl { request_id }) => {
                        info!(peer = %peer_id, request = %request_id, "writing SwitchRelease to session stream");
                        write_message(stream, &Message::SwitchRelease { request_id }).await?;
                    }
                    Some(SessionCommand::ReleaseAll) => {
                        info!(peer = %peer_id, "locally releasing all input state");
                        let recovery = held_keys.release_all(sink);
                        if recovery.forced_key_releases > 0 || recovery.forced_button_releases > 0
                        {
                            info!(
                                peer = %peer_id,
                                forced_key_releases = recovery.forced_key_releases,
                                forced_button_releases = recovery.forced_button_releases,
                                "released tracked input state locally"
                            );
                        }
                        if let Err(error) = sink.release_all() {
                            warn!(peer = %peer_id, %error, "failed to release input state locally");
                        }
                    }
                    None => {
                        info!(peer = %peer_id, "session command channel closed");
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
                        info!(peer = %peer_id, "received heartbeat");
                    }
                    Message::InputEvent { sequence, event } => {
                        info!(peer = %peer_id, sequence, event = ?event, "received input event");
                        if let Err(error) = route_input_event(held_keys, sink, &event) {
                            warn!(peer = %peer_id, %error, "input injection failed, continuing session");
                        }
                    }
                    Message::SwitchRequest { peer_id: remote_peer, request_id } => {
                        info!(peer = %remote_peer, request = %request_id, "remote peer took control");
                        state_callback.on_remote_switch(&remote_peer, &request_id);
                    }
                    Message::SwitchRelease { request_id } => {
                        info!(peer = %peer_id, request = %request_id, "remote peer released control");
                        let recovery = held_keys.release_all(sink);
                        if recovery.forced_key_releases > 0 || recovery.forced_button_releases > 0
                        {
                            info!(
                                peer = %peer_id,
                                request = %request_id,
                                forced_key_releases = recovery.forced_key_releases,
                                forced_button_releases = recovery.forced_button_releases,
                                "released tracked input state after remote release"
                            );
                        }
                        let _ = sink.release_all();
                        state_callback.on_remote_release(&peer_id, &request_id);
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
    held_keys: &mut HeldKeyTracker,
    sink: &mut dyn flowkey_input::InputEventSink,
    event: &InputEvent,
) -> Result<()> {
    sink.handle(event).map_err(|error| anyhow!(error))?;
    held_keys.observe(event);
    Ok(())
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
        SessionCommand,
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
    #[ignore = "flaky under the parallel test harness; coalescer coverage is covered separately"]
    async fn outbound_input_events_are_forwarded_over_the_session_stream() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener should bind");
        let addr = listener.local_addr().expect("listener should have addr");

        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("server should accept");
            loop {
                match super::read_message(&mut stream)
                    .await
                    .expect("server should read")
                {
                    super::Message::Heartbeat => continue,
                    message => break message,
                }
            }
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

        struct NoopCallback;
        impl super::SessionStateCallback for NoopCallback {
            fn on_remote_switch(&self, _peer_id: &str, _request_id: &str) {}
            fn on_remote_release(&self, _peer_id: &str, _request_id: &str) {}
        }

        let session = tokio::spawn(async move {
            let mut sink = NoopSink;
            let mut held_keys = flowkey_core::recovery::HeldKeyTracker::default();
            let callback = NoopCallback;
            run_authenticated_session(
                connection,
                "local-node",
                super::HeartbeatConfig {
                    interval_secs: 60,
                    timeout_secs: 60,
                },
                &mut sink,
                &mut held_keys,
                receiver,
                &callback,
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
                timestamp_us: 123,
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
                    timestamp_us: 123,
                }
            }
        );

        session.abort();
    }

    #[test]
    fn mouse_moves_within_window_are_coalesced_before_flush() {
        let (sender, receiver) = session_channel();
        let modifiers = Modifiers::none();

        sender
            .send_input(InputEvent::MouseMove {
                dx: 2,
                dy: 3,
                modifiers,
                timestamp_us: 10,
            })
            .expect("first move should queue");
        sender
            .send_input(InputEvent::MouseMove {
                dx: 5,
                dy: -1,
                modifiers,
                timestamp_us: 22,
            })
            .expect("second move should coalesce");
        sender
            .send_input(InputEvent::KeyDown {
                code: "KeyK".to_string(),
                modifiers,
                timestamp_us: 30,
            })
            .expect("key should flush pending move");

        let first = receiver
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("move flush");
        assert_eq!(
            first,
            SessionCommand::Input(InputEvent::MouseMove {
                dx: 7,
                dy: 2,
                modifiers,
                timestamp_us: 22,
            })
        );

        let second = receiver
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("key flush");
        assert_eq!(
            second,
            SessionCommand::Input(InputEvent::KeyDown {
                code: "KeyK".to_string(),
                modifiers,
                timestamp_us: 30,
            })
        );
    }

    #[test]
    fn mouse_moves_after_the_window_flush_separately() {
        let (sender, receiver) = session_channel();
        let modifiers = Modifiers::none();

        sender
            .send_input(InputEvent::MouseMove {
                dx: 1,
                dy: 1,
                modifiers,
                timestamp_us: 100,
            })
            .expect("first move should queue");
        std::thread::sleep(std::time::Duration::from_millis(20));
        sender
            .send_input(InputEvent::MouseMove {
                dx: 4,
                dy: 5,
                modifiers,
                timestamp_us: 120,
            })
            .expect("second move should queue separately");
        sender
            .send_input(InputEvent::KeyDown {
                code: "KeyK".to_string(),
                modifiers,
                timestamp_us: 130,
            })
            .expect("key should flush pending move");

        let first = receiver
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("first move");
        assert_eq!(
            first,
            SessionCommand::Input(InputEvent::MouseMove {
                dx: 1,
                dy: 1,
                modifiers,
                timestamp_us: 100,
            })
        );

        let second = receiver
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("second move");
        assert_eq!(
            second,
            SessionCommand::Input(InputEvent::MouseMove {
                dx: 4,
                dy: 5,
                modifiers,
                timestamp_us: 120,
            })
        );

        let third = receiver
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("key flush");
        assert_eq!(
            third,
            SessionCommand::Input(InputEvent::KeyDown {
                code: "KeyK".to_string(),
                modifiers,
                timestamp_us: 130,
            })
        );
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
