use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::engine::general_purpose::STANDARD_NO_PAD;
use base64::Engine;
use ed25519_dalek::SigningKey;
use flowkey_config::{Config, PeerConfig};
use flowkey_core::recovery::{HeldKeyTracker, RecoveryState};
use flowkey_input::event::{InputEvent, Modifiers};
use flowkey_input::InputEventSink;
use flowkey_net::connection::{
    authenticate_incoming_stream, connect_and_authenticate, run_authenticated_session,
    session_channel, SessionStateCallback,
};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc::unbounded_channel;

#[derive(Default)]
struct NoopCallback;

impl SessionStateCallback for NoopCallback {
    fn on_remote_switch(&self, _peer_id: &str, _request_id: &str) {}

    fn on_remote_release(&self, _peer_id: &str, _request_id: &str) {}
}

#[derive(Clone)]
struct RecordingSink {
    events: Arc<Mutex<Vec<InputEvent>>>,
    event_tx: tokio::sync::mpsc::UnboundedSender<InputEvent>,
}

impl RecordingSink {
    fn new(
        events: Arc<Mutex<Vec<InputEvent>>>,
        event_tx: tokio::sync::mpsc::UnboundedSender<InputEvent>,
    ) -> Self {
        Self { events, event_tx }
    }
}

#[derive(Default)]
struct NoopSink;

impl InputEventSink for NoopSink {
    fn handle(&mut self, _event: &InputEvent) -> Result<(), String> {
        Ok(())
    }

    fn release_all(&mut self) -> Result<(), String> {
        Ok(())
    }
}

impl InputEventSink for RecordingSink {
    fn handle(&mut self, event: &InputEvent) -> Result<(), String> {
        self.events
            .lock()
            .expect("recording sink mutex should not be poisoned")
            .push(event.clone());
        let _ = self.event_tx.send(event.clone());
        Ok(())
    }

    fn release_all(&mut self) -> Result<(), String> {
        Ok(())
    }
}

async fn spawn_proxy_once(
    proxy_listener: Arc<TcpListener>,
    upstream_addr: std::net::SocketAddr,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let (mut downstream, _) = proxy_listener
            .accept()
            .await
            .expect("proxy should accept client");
        let mut upstream = TcpStream::connect(upstream_addr)
            .await
            .expect("proxy should connect upstream");
        let _ = tokio::io::copy_bidirectional(&mut downstream, &mut upstream).await;
    })
}

fn test_config(node_id: &str, node_name: &str, listen_addr: &str) -> Config {
    let mut config = Config::default();
    let signing_key = SigningKey::generate(&mut rand::rngs::OsRng);

    config.node.id = node_id.to_string();
    config.node.name = node_name.to_string();
    config.node.listen_addr = listen_addr.to_string();
    config.node.private_key = STANDARD_NO_PAD.encode(signing_key.to_bytes());
    config.node.public_key = STANDARD_NO_PAD.encode(signing_key.verifying_key().to_bytes());
    config
}

#[test]
fn reconnect_chaos_keeps_releasing_held_keys() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .expect("test runtime should build");

    runtime.block_on(async {
        reconnect_chaos_keeps_releasing_held_keys_async().await;
    });
    runtime.shutdown_timeout(Duration::from_secs(1));
}

async fn reconnect_chaos_keeps_releasing_held_keys_async() {
    let server_listener = Arc::new(
        TcpListener::bind("127.0.0.1:0")
            .await
            .expect("server listener should bind"),
    );
    let server_addr = server_listener
        .local_addr()
        .expect("server listener should have addr");

    let proxy_listener = Arc::new(
        TcpListener::bind("127.0.0.1:0")
            .await
            .expect("proxy listener should bind"),
    );
    let proxy_addr = proxy_listener
        .local_addr()
        .expect("proxy listener should have addr");

    let server_config = test_config("server-node", "Server Node", &server_addr.to_string());
    let client_config = test_config("client-node", "Client Node", "127.0.0.1:0");

    let client_signing_key = SigningKey::generate(&mut rand::rngs::OsRng);
    let server_signing_key = SigningKey::generate(&mut rand::rngs::OsRng);

    let mut server_config = server_config;
    let mut client_config = client_config;

    server_config.node.private_key = STANDARD_NO_PAD.encode(server_signing_key.to_bytes());
    server_config.node.public_key =
        STANDARD_NO_PAD.encode(server_signing_key.verifying_key().to_bytes());
    client_config.node.private_key = STANDARD_NO_PAD.encode(client_signing_key.to_bytes());
    client_config.node.public_key =
        STANDARD_NO_PAD.encode(client_signing_key.verifying_key().to_bytes());

    server_config.upsert_peer(PeerConfig {
        id: client_config.node.id.clone(),
        name: client_config.node.name.clone(),
        addr: proxy_addr.to_string(),
        public_key: client_config.node.public_key.clone(),
        trusted: true,
    });
    client_config.upsert_peer(PeerConfig {
        id: server_config.node.id.clone(),
        name: server_config.node.name.clone(),
        addr: proxy_addr.to_string(),
        public_key: server_config.node.public_key.clone(),
        trusted: true,
    });

    let mut recoveries = Vec::new();

    for cycle in 0..3 {
        let (event_tx, mut event_rx) = unbounded_channel();
        let handled_events = Arc::new(Mutex::new(Vec::new()));
        let proxy_handle = spawn_proxy_once(Arc::clone(&proxy_listener), server_addr).await;
        let server_listener_for_task = Arc::clone(&server_listener);

        let server_task = {
            let server_config = server_config.clone();
            let handled_events = Arc::clone(&handled_events);
            tokio::spawn(async move {
                let (stream, _) = server_listener_for_task
                    .accept()
                    .await
                    .expect("server should accept proxied client");
                let connection = authenticate_incoming_stream(&server_config, stream)
                    .await
                    .expect("server should authenticate client");
                let mut sink = RecordingSink::new(handled_events, event_tx);
                let mut held_keys = HeldKeyTracker::default();
                let callback = NoopCallback;
                let server_node_id = server_config.node.id.clone();

                let (_sender, receiver) = session_channel();
                let session_result = run_authenticated_session(
                    connection,
                    &server_node_id,
                    flowkey_net::heartbeat::HeartbeatConfig {
                        interval_secs: 60,
                        timeout_secs: 60,
                    },
                    &mut sink,
                    &mut held_keys,
                    receiver,
                    &callback,
                )
                .await;
                assert!(session_result.is_err(), "session should end after sever");
                tokio::time::sleep(Duration::from_millis(20)).await;

                let recovery = held_keys.release_all(&mut sink);
                let followup = held_keys.release_all(&mut sink);
                assert_eq!(followup, RecoveryState::default());
                recovery
            })
        };

        let client_config_for_task = client_config.clone();
        let server_config_for_task = server_config.clone();
        let client_task = tokio::spawn(async move {
            let client_node_id = client_config_for_task.node.id.clone();
            let connection = connect_and_authenticate(
                &client_config_for_task,
                &PeerConfig {
                    id: server_config_for_task.node.id.clone(),
                    name: server_config_for_task.node.name.clone(),
                    addr: proxy_addr.to_string(),
                    public_key: server_config_for_task.node.public_key.clone(),
                    trusted: true,
                },
            )
            .await
            .expect("client should authenticate server through proxy");

            let (sender, receiver) = session_channel();
            let mut sink = NoopSink;
            let mut held_keys = HeldKeyTracker::default();
            let callback = NoopCallback;

            let session = tokio::spawn(async move {
                run_authenticated_session(
                    connection,
                    &client_node_id,
                    flowkey_net::heartbeat::HeartbeatConfig {
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

            tokio::task::yield_now().await;
            sender
                .send_input(InputEvent::KeyDown {
                    code: "KeyK".to_string(),
                    modifiers: Modifiers {
                        shift: true,
                        control: true,
                        alt: true,
                        meta: false,
                    },
                    timestamp_us: cycle as u64 + 1,
                })
                .expect("client should queue keydown");
            drop(sender);

            let _ = session.await;
            tokio::time::sleep(Duration::from_millis(20)).await;
        });

        // Wait until the server sees the keydown, then cut the connection.
        tokio::time::timeout(Duration::from_secs(5), event_rx.recv())
            .await
            .expect("server should observe the keydown before severing")
            .expect("server sink should send keydown event");
        proxy_handle.abort();
        let _ = tokio::time::timeout(Duration::from_secs(5), proxy_handle)
            .await
            .expect("proxy task should finish after abort");

        let recovery = tokio::time::timeout(Duration::from_secs(5), server_task)
            .await
            .expect("server task should finish after severing")
            .expect("server task should not panic");
        recoveries.push(recovery);

        tokio::time::timeout(Duration::from_secs(5), client_task)
            .await
            .expect("client task should finish after severing")
            .expect("client task should not panic");

        let events = handled_events
            .lock()
            .expect("recording sink mutex should not be poisoned")
            .clone();
        assert!(
            events
                .iter()
                .any(|event| matches!(event, InputEvent::KeyDown { .. })),
            "cycle {cycle}: server should observe the keydown"
        );
        assert!(
            events
                .iter()
                .any(|event| matches!(event, InputEvent::KeyUp { .. })),
            "cycle {cycle}: server should force-release the held key"
        );
    }

    assert_eq!(recoveries.len(), 3);
    assert!(recoveries.iter().all(|recovery| recovery
        == &RecoveryState {
            forced_key_releases: 1,
            forced_button_releases: 0,
        }));
}
