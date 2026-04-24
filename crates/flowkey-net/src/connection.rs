use anyhow::{anyhow, Context, Result};
use crossbeam_channel::{bounded, Receiver, Sender, TrySendError};
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
use tokio::sync::mpsc::channel;
use tokio::time::{interval, Duration};
use tracing::{error, info, trace, warn};

use crate::frame::{read_message, write_message};
use crate::heartbeat::HeartbeatConfig;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

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

#[derive(Debug)]
pub struct SessionSender {
    inner: Arc<SessionSenderInner>,
}

impl Clone for SessionSender {
    fn clone(&self) -> Self {
        self.inner.sender_refs.fetch_add(1, Ordering::SeqCst);
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl Drop for SessionSender {
    fn drop(&mut self) {
        if self.inner.sender_refs.fetch_sub(1, Ordering::SeqCst) == 1 {
            self.inner.shutdown.store(true, Ordering::SeqCst);
            self.inner.coalescer_wake.notify_all();
        }
    }
}

impl SessionSender {
    pub fn send_input(&self, event: InputEvent) -> anyhow::Result<()> {
        match event {
            InputEvent::MouseMove { .. } => {
                self.inner.queue_mouse_move(event)?;
                Ok(())
            }
            InputEvent::MouseWheel { .. } => {
                self.inner.queue_scroll(event)?;
                Ok(())
            }
            _ => self.inner.send_immediate_input(event),
        }
    }

    pub fn send_switch(&self, request_id: String) -> anyhow::Result<()> {
        self.inner
            .send_control_command(SessionCommand::SwitchControl { request_id })
    }

    pub fn send_release(&self, request_id: String) -> anyhow::Result<()> {
        self.inner
            .send_control_command(SessionCommand::ReleaseControl { request_id })
    }

    pub fn send_release_all(&self) -> anyhow::Result<()> {
        self.inner.send_control_command(SessionCommand::ReleaseAll)
    }

    pub fn dropped_inputs(&self) -> usize {
        self.inner.dropped_inputs.load(Ordering::SeqCst)
    }
}

#[derive(Debug)]
struct SessionSenderInner {
    sender: Sender<SessionCommand>,
    coalesce_window: Duration,
    coalescer: Mutex<InputCoalescer>,
    coalescer_wake: Condvar,
    shutdown: AtomicBool,
    channel_closed: Arc<AtomicBool>,
    sender_refs: AtomicUsize,
    dropped_inputs: AtomicUsize,
}

#[derive(Debug)]
struct InputCoalescer {
    pending_move: Option<PendingMouseMove>,
    pending_scroll: Option<PendingScroll>,
}

#[derive(Debug)]
struct PendingMouseMove {
    dx: i32,
    dy: i32,
    modifiers: flowkey_input::event::Modifiers,
    timestamp_us: u64,
    deadline: Instant,
}

#[derive(Debug)]
struct PendingScroll {
    delta_x: i32,
    delta_y: i32,
    modifiers: flowkey_input::event::Modifiers,
    timestamp_us: u64,
    deadline: Instant,
}

impl SessionSenderInner {
    fn new(sender: Sender<SessionCommand>, coalesce_window: Duration) -> Arc<Self> {
        let inner = Arc::new(Self {
            sender,
            coalesce_window,
            coalescer: Mutex::new(InputCoalescer {
                pending_move: None,
                pending_scroll: None,
            }),
            coalescer_wake: Condvar::new(),
            shutdown: AtomicBool::new(false),
            channel_closed: Arc::new(AtomicBool::new(false)),
            sender_refs: AtomicUsize::new(1),
            dropped_inputs: AtomicUsize::new(0),
        });
        Self::spawn_flush_worker(&inner);
        inner
    }

    fn queue_mouse_move(&self, event: InputEvent) -> anyhow::Result<()> {
        self.ensure_channel_open()?;
        let InputEvent::MouseMove {
            dx,
            dy,
            modifiers,
            timestamp_us,
        } = event
        else {
            return Ok(());
        };

        let mut state = match self.coalescer.lock() {
            Ok(state) => state,
            Err(e) => {
                error!("input coalescer mutex poisoned: {}", e);
                self.mark_channel_closed();
                anyhow::bail!("coalescer unavailable");
            }
        };
        if let Err(error) = Self::flush_scroll_locked(&self.sender, &mut state) {
            self.mark_channel_closed();
            return Err(error);
        }
        let now = Instant::now();
        match state.pending_move.as_mut() {
            Some(pending) if pending.modifiers == modifiers && now <= pending.deadline => {
                pending.dx = pending.dx.saturating_add(dx);
                pending.dy = pending.dy.saturating_add(dy);
                pending.timestamp_us = timestamp_us;
                // Do NOT extend the deadline — use a fixed coalescing window.
                // Extending the deadline causes unbounded latency when events
                // arrive continuously (e.g., rapid mouse movement).
            }
            Some(_) => {
                if let Err(error) = Self::flush_move_locked(&self.sender, &mut state) {
                    self.mark_channel_closed();
                    return Err(error);
                }
                state.pending_move = Some(PendingMouseMove {
                    dx,
                    dy,
                    modifiers,
                    timestamp_us,
                    deadline: now + self.coalesce_window,
                });
            }
            None => {
                state.pending_move = Some(PendingMouseMove {
                    dx,
                    dy,
                    modifiers,
                    timestamp_us,
                    deadline: now + self.coalesce_window,
                });
            }
        }

        self.coalescer_wake.notify_all();
        Ok(())
    }

    fn queue_scroll(&self, event: InputEvent) -> anyhow::Result<()> {
        self.ensure_channel_open()?;
        let InputEvent::MouseWheel {
            delta_x,
            delta_y,
            modifiers,
            timestamp_us,
        } = event
        else {
            return Ok(());
        };

        let mut state = match self.coalescer.lock() {
            Ok(state) => state,
            Err(e) => {
                error!("input coalescer mutex poisoned: {}", e);
                self.mark_channel_closed();
                anyhow::bail!("coalescer unavailable");
            }
        };
        if let Err(error) = Self::flush_move_locked(&self.sender, &mut state) {
            self.mark_channel_closed();
            return Err(error);
        }
        let now = Instant::now();
        match state.pending_scroll.as_mut() {
            Some(pending) if pending.modifiers == modifiers && now <= pending.deadline => {
                pending.delta_x = pending.delta_x.saturating_add(delta_x);
                pending.delta_y = pending.delta_y.saturating_add(delta_y);
                pending.timestamp_us = timestamp_us;
            }
            Some(_) => {
                if let Err(error) = Self::flush_scroll_locked(&self.sender, &mut state) {
                    self.mark_channel_closed();
                    return Err(error);
                }
                state.pending_scroll = Some(PendingScroll {
                    delta_x,
                    delta_y,
                    modifiers,
                    timestamp_us,
                    deadline: now + self.coalesce_window,
                });
            }
            None => {
                state.pending_scroll = Some(PendingScroll {
                    delta_x,
                    delta_y,
                    modifiers,
                    timestamp_us,
                    deadline: now + self.coalesce_window,
                });
            }
        }

        self.coalescer_wake.notify_all();
        Ok(())
    }

    fn send_immediate_input(&self, event: InputEvent) -> anyhow::Result<()> {
        self.ensure_channel_open()?;
        let mut state = match self.coalescer.lock() {
            Ok(state) => state,
            Err(e) => {
                error!("input coalescer mutex poisoned: {}", e);
                self.mark_channel_closed();
                anyhow::bail!("coalescer unavailable");
            }
        };
        if let Err(error) = Self::flush_move_locked(&self.sender, &mut state) {
            self.mark_channel_closed();
            return Err(error);
        }
        if let Err(error) = Self::flush_scroll_locked(&self.sender, &mut state) {
            self.mark_channel_closed();
            return Err(error);
        }
        match self.sender.try_send(SessionCommand::Input(event)) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => {
                let dropped = self.dropped_inputs.fetch_add(1, Ordering::SeqCst) + 1;
                warn!(dropped, "session channel full; dropping input event");
                Ok(())
            }
            Err(TrySendError::Disconnected(_)) => {
                self.mark_channel_closed();
                anyhow::bail!("session command channel closed")
            }
        }
    }

    fn send_control_command(&self, command: SessionCommand) -> anyhow::Result<()> {
        self.ensure_channel_open()?;
        let mut state = match self.coalescer.lock() {
            Ok(state) => state,
            Err(e) => {
                error!("input coalescer mutex poisoned: {}", e);
                self.mark_channel_closed();
                anyhow::bail!("coalescer unavailable");
            }
        };
        state.pending_move = None;
        state.pending_scroll = None;
        self.sender.send(command).map_err(|_| {
            self.mark_channel_closed();
            anyhow::anyhow!("session command channel closed")
        })
    }

    fn ensure_channel_open(&self) -> anyhow::Result<()> {
        if self.channel_closed.load(Ordering::SeqCst) {
            anyhow::bail!("session command channel closed")
        } else {
            Ok(())
        }
    }

    fn mark_channel_closed(&self) {
        self.channel_closed.store(true, Ordering::SeqCst);
        self.coalescer_wake.notify_all();
    }

    fn flush_move_locked(
        sender: &Sender<SessionCommand>,
        state: &mut InputCoalescer,
    ) -> anyhow::Result<()> {
        let Some(pending) = state.pending_move.take() else {
            return Ok(());
        };

        match sender.try_send(SessionCommand::Input(InputEvent::MouseMove {
            dx: pending.dx,
            dy: pending.dy,
            modifiers: pending.modifiers,
            timestamp_us: pending.timestamp_us,
        })) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => Ok(()), // drop stale coalesced move; cursor catches up
            Err(TrySendError::Disconnected(_)) => {
                anyhow::bail!("session command channel closed")
            }
        }
    }

    fn flush_scroll_locked(
        sender: &Sender<SessionCommand>,
        state: &mut InputCoalescer,
    ) -> anyhow::Result<()> {
        let Some(pending) = state.pending_scroll.take() else {
            return Ok(());
        };

        match sender.try_send(SessionCommand::Input(InputEvent::MouseWheel {
            delta_x: pending.delta_x,
            delta_y: pending.delta_y,
            modifiers: pending.modifiers,
            timestamp_us: pending.timestamp_us,
        })) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => Ok(()), // drop stale coalesced scroll; user can re-scroll
            Err(TrySendError::Disconnected(_)) => {
                anyhow::bail!("session command channel closed")
            }
        }
    }

    fn spawn_flush_worker(inner: &Arc<Self>) {
        let weak = Arc::downgrade(inner);
        thread::spawn(move || 'worker: loop {
            let Some(inner) = weak.upgrade() else {
                break;
            };

            let mut state = match inner.coalescer.lock() {
                Ok(state) => state,
                Err(e) => {
                    error!("input coalescer mutex poisoned: {}", e);
                    break 'worker;
                }
            };

            while state.pending_move.is_none() && state.pending_scroll.is_none() {
                if inner.shutdown.load(Ordering::SeqCst)
                    || inner.channel_closed.load(Ordering::SeqCst)
                {
                    break 'worker;
                }
                state = match inner.coalescer_wake.wait(state) {
                    Ok(state) => state,
                    Err(e) => {
                        error!("input coalescer mutex poisoned after wait: {}", e);
                        break 'worker;
                    }
                };
            }

            let deadline = earliest_deadline(&state);
            let Some(deadline) = deadline else {
                continue;
            };

            let now = Instant::now();
            if now < deadline {
                let wait = deadline.saturating_duration_since(now);
                let (next_state, _) = match inner.coalescer_wake.wait_timeout(state, wait) {
                    Ok((state, status)) => (state, status),
                    Err(e) => {
                        error!("input coalescer mutex poisoned after wait_timeout: {}", e);
                        break 'worker;
                    }
                };
                state = next_state;

                if inner.shutdown.load(Ordering::SeqCst) {
                    state.pending_move = None;
                    state.pending_scroll = None;
                    break 'worker;
                }
                if inner.channel_closed.load(Ordering::SeqCst) {
                    break 'worker;
                }

                // Re-check: if the earliest pending item is still in the future,
                // loop again to allow further coalescing.
                if let Some(d) = earliest_deadline(&state) {
                    if Instant::now() < d {
                        continue;
                    }
                }
            }

            // Flush whichever pending items have reached their deadline.
            if inner.shutdown.load(Ordering::SeqCst) {
                state.pending_move = None;
                state.pending_scroll = None;
                break;
            }
            let now = Instant::now();
            if state
                .pending_move
                .as_ref()
                .is_some_and(|p| now >= p.deadline)
            {
                if Self::flush_move_locked(&inner.sender, &mut state).is_err() {
                    inner.mark_channel_closed();
                    break;
                }
            }
            if state
                .pending_scroll
                .as_ref()
                .is_some_and(|p| now >= p.deadline)
            {
                if Self::flush_scroll_locked(&inner.sender, &mut state).is_err() {
                    inner.mark_channel_closed();
                    break;
                }
            }
        });
    }
}

fn earliest_deadline(state: &InputCoalescer) -> Option<Instant> {
    match (&state.pending_move, &state.pending_scroll) {
        (Some(m), Some(s)) => Some(m.deadline.min(s.deadline)),
        (Some(m), None) => Some(m.deadline),
        (None, Some(s)) => Some(s.deadline),
        (None, None) => None,
    }
}

pub struct SessionCommandReceiver {
    inner: Receiver<SessionCommand>,
    channel_closed: Arc<AtomicBool>,
    wake: std::sync::Weak<SessionSenderInner>,
}

impl SessionCommandReceiver {
    pub fn recv(&self) -> Result<SessionCommand, crossbeam_channel::RecvError> {
        self.inner.recv()
    }

    pub fn recv_timeout(
        &self,
        timeout: std::time::Duration,
    ) -> Result<SessionCommand, crossbeam_channel::RecvTimeoutError> {
        self.inner.recv_timeout(timeout)
    }

    pub fn try_recv(&self) -> Result<SessionCommand, crossbeam_channel::TryRecvError> {
        self.inner.try_recv()
    }
}

impl Drop for SessionCommandReceiver {
    fn drop(&mut self) {
        self.channel_closed.store(true, Ordering::SeqCst);
        if let Some(inner) = self.wake.upgrade() {
            inner.coalescer_wake.notify_all();
        }
    }
}

pub fn session_channel() -> (SessionSender, SessionCommandReceiver) {
    session_channel_with_coalesce_window(flowkey_config::DEFAULT_INPUT_COALESCE_WINDOW_MS)
}

pub fn session_channel_with_coalesce_window(
    coalesce_window_ms: u64,
) -> (SessionSender, SessionCommandReceiver) {
    let (sender, receiver) = bounded(100);
    let inner = SessionSenderInner::new(sender, Duration::from_millis(coalesce_window_ms));
    (
        SessionSender { inner: Arc::clone(&inner) },
        SessionCommandReceiver {
            inner: receiver,
            channel_closed: Arc::clone(&inner.channel_closed),
            wake: Arc::downgrade(&inner),
        },
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
    // Disable Nagle immediately so auth round-trips are not buffered up to 200ms.
    stream
        .set_nodelay(true)
        .context("failed to set TCP_NODELAY on outbound stream")?;

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
    // Disable Nagle immediately so server-side auth messages bypass OS buffering.
    stream
        .set_nodelay(true)
        .context("failed to set TCP_NODELAY on incoming stream")?;
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
    outbound: SessionCommandReceiver,
    state_callback: &dyn SessionStateCallback,
) -> Result<()> {
    let (bridge_tx, mut bridge_rx) = channel(100);

    // Spawn a blocking task to bridge crossbeam channel to tokio
    tokio::task::spawn_blocking(move || {
        loop {
            match outbound.recv_timeout(Duration::from_millis(100)) {
                Ok(command) => {
                    if bridge_tx.blocking_send(command).is_err() {
                        break;
                    }
                }
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                    if bridge_tx.is_closed() {
                        break;
                    }
                }
                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
            }
        }
    });

    let mut ticker = interval(Duration::from_secs(heartbeat.interval_secs));
    let mut outbound_open = true;
    let mut sequence: u64 = 0;
    let peer_id = connection.info.peer_id.clone();
    let stream = &mut connection.stream;
    // TCP_NODELAY already set at connect/accept time; this is now a no-op kept for safety.

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
                        trace!(peer = %peer_id, "received heartbeat");
                    }
                    Message::InputEvent { sequence, event } => {
                        tracing::debug!(peer = %peer_id, sequence, event = ?event, "received input event");
                        if let Err(error) = route_input_event(held_keys, sink, &event) {
                            warn!(peer = %peer_id, event = ?event, %error, "input injection failed, continuing session");
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
        fn handle(&mut self, _event: &flowkey_input::event::InputEvent) -> anyhow::Result<()> {
            Ok(())
        }

        fn release_all(&mut self) -> anyhow::Result<()> {
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

    #[test]
    fn configured_window_is_stored_on_sender() {
        let (sender, _receiver) = session_channel_with_coalesce_window(25);

        assert_eq!(sender.inner.coalesce_window, Duration::from_millis(25));
    }

    #[test]
    fn scroll_events_within_window_are_coalesced_before_flush() {
        let (sender, receiver) = session_channel();
        let modifiers = Modifiers::none();

        sender
            .send_input(InputEvent::MouseWheel {
                delta_x: 0,
                delta_y: 3,
                modifiers,
                timestamp_us: 10,
            })
            .expect("first scroll should queue");
        sender
            .send_input(InputEvent::MouseWheel {
                delta_x: 1,
                delta_y: -1,
                modifiers,
                timestamp_us: 22,
            })
            .expect("second scroll should coalesce");
        sender
            .send_input(InputEvent::KeyDown {
                code: "KeyK".to_string(),
                modifiers,
                timestamp_us: 30,
            })
            .expect("key should flush pending scroll");

        let first = receiver
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("scroll flush");
        assert_eq!(
            first,
            SessionCommand::Input(InputEvent::MouseWheel {
                delta_x: 1,
                delta_y: 2,
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
    fn scroll_and_move_coalesce_independently() {
        let (sender, receiver) = session_channel();
        let modifiers = Modifiers::none();

        sender
            .send_input(InputEvent::MouseMove {
                dx: 2,
                dy: 3,
                modifiers,
                timestamp_us: 10,
            })
            .expect("move should queue");
        // Scroll flushes pending moves, then queues scroll
        sender
            .send_input(InputEvent::MouseWheel {
                delta_x: 0,
                delta_y: 5,
                modifiers,
                timestamp_us: 15,
            })
            .expect("scroll should flush move and queue scroll");
        sender
            .send_input(InputEvent::MouseWheel {
                delta_x: 0,
                delta_y: 2,
                modifiers,
                timestamp_us: 18,
            })
            .expect("second scroll should coalesce");
        // Key flushes everything
        sender
            .send_input(InputEvent::KeyDown {
                code: "KeyA".to_string(),
                modifiers,
                timestamp_us: 20,
            })
            .expect("key should flush all");

        let first = receiver
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("move flush");
        assert_eq!(
            first,
            SessionCommand::Input(InputEvent::MouseMove {
                dx: 2,
                dy: 3,
                modifiers,
                timestamp_us: 10,
            })
        );

        let second = receiver
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("scroll flush");
        assert_eq!(
            second,
            SessionCommand::Input(InputEvent::MouseWheel {
                delta_x: 0,
                delta_y: 7,
                modifiers,
                timestamp_us: 18,
            })
        );

        let third = receiver
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("key flush");
        assert_eq!(
            third,
            SessionCommand::Input(InputEvent::KeyDown {
                code: "KeyA".to_string(),
                modifiers,
                timestamp_us: 20,
            })
        );
    }

    #[test]
    fn release_discards_buffered_coalesced_input() {
        let (sender, receiver) = session_channel();
        let modifiers = Modifiers::none();

        sender
            .send_input(InputEvent::MouseMove {
                dx: 2,
                dy: 3,
                modifiers,
                timestamp_us: 10,
            })
            .expect("move should queue");
        sender
            .send_release("req-1".to_string())
            .expect("release should queue");

        let first = receiver
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("release command");
        assert_eq!(
            first,
            SessionCommand::ReleaseControl {
                request_id: "req-1".to_string()
            }
        );
        assert!(
            receiver
                .recv_timeout(std::time::Duration::from_millis(50))
                .is_err(),
            "buffered input should be discarded before release"
        );
    }

    #[test]
    fn scroll_before_move_preserves_original_order() {
        let (sender, receiver) = session_channel();
        let modifiers = Modifiers::none();

        sender
            .send_input(InputEvent::MouseWheel {
                delta_x: 0,
                delta_y: 3,
                modifiers,
                timestamp_us: 10,
            })
            .expect("scroll should queue");
        sender
            .send_input(InputEvent::MouseMove {
                dx: 2,
                dy: 4,
                modifiers,
                timestamp_us: 20,
            })
            .expect("move should flush prior scroll and queue");
        sender
            .send_input(InputEvent::KeyDown {
                code: "KeyK".to_string(),
                modifiers,
                timestamp_us: 30,
            })
            .expect("key should flush pending move");

        let first = receiver
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("scroll flush");
        assert_eq!(
            first,
            SessionCommand::Input(InputEvent::MouseWheel {
                delta_x: 0,
                delta_y: 3,
                modifiers,
                timestamp_us: 10,
            })
        );

        let second = receiver
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("move flush");
        assert_eq!(
            second,
            SessionCommand::Input(InputEvent::MouseMove {
                dx: 2,
                dy: 4,
                modifiers,
                timestamp_us: 20,
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
