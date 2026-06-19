use std::time::Duration;

use flowkey_config::{CaptureMode, Config, NodeConfig, SwitchConfig};
use flowkey_net::pairing::{initiate_pairing_client_to_target, run_pairing_listener};
use tokio::net::TcpListener;

fn test_config() -> Config {
    Config {
        node: NodeConfig {
            id: "test-node".to_string(),
            name: "Test Node".to_string(),
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
async fn manual_hostname_pairing_uses_the_observed_route_for_daemon_connections() {
    let config = test_config();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server_task = tokio::spawn(run_pairing_listener(config.clone(), listener));

    let client_proposal = initiate_pairing_client_to_target(
        config,
        &format!("localhost:{port}"),
        48572,
        Duration::from_secs(2),
    )
    .await
    .unwrap();
    let server_proposal = server_task.await.unwrap().unwrap();

    assert!(client_proposal.observed_addr.ip().is_loopback());
    assert!(server_proposal.observed_addr.ip().is_loopback());
    assert_eq!(client_proposal.preferred_peer_addr(), "127.0.0.1:48571");
}
