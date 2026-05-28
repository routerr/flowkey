use std::env;
use std::fs;
use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use base64::engine::general_purpose::STANDARD_NO_PAD;
use base64::Engine;
use ed25519_dalek::SigningKey;
use rand::distributions::Alphanumeric;
use rand::{thread_rng, Rng};
use serde::{Deserialize, Serialize};

pub const DEFAULT_INPUT_COALESCE_WINDOW_MS: u64 = 4;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub node: NodeConfig,
    pub switch: SwitchConfig,
    #[serde(default)]
    pub peers: Vec<PeerConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeConfig {
    pub id: String,
    pub name: String,
    pub listen_addr: String,
    #[serde(default)]
    pub advertised_addr: Option<String>,
    #[serde(default = "default_accept_remote_control")]
    pub accept_remote_control: bool,
    pub private_key: String,
    pub public_key: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CaptureMode {
    Passive,
    Exclusive,
}

impl Default for CaptureMode {
    fn default() -> Self {
        Self::Exclusive
    }
}

impl CaptureMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Passive => "passive",
            Self::Exclusive => "exclusive",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwitchConfig {
    pub hotkey: String,
    #[serde(default)]
    pub capture_mode: CaptureMode,
    #[serde(default = "default_input_coalesce_window_ms")]
    pub input_coalesce_window_ms: u64,
}

impl SwitchConfig {
    pub fn input_coalesce_window(&self) -> std::time::Duration {
        std::time::Duration::from_millis(self.input_coalesce_window_ms)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerConfig {
    pub id: String,
    pub name: String,
    pub addr: String,
    pub public_key: String,
    pub trusted: bool,
}

impl Config {
    pub fn load() -> Result<Self> {
        let path = Self::default_path()?;
        Self::load_from_path(&path)
    }

    pub fn load_or_default() -> Result<Self> {
        let path = Self::default_path()?;

        if path.exists() {
            Self::load_from_path(&path)
        } else {
            Ok(Self::default())
        }
    }

    pub fn load_or_create() -> Result<Self> {
        let path = Self::default_path()?;

        if path.exists() {
            let mut config = Self::load_from_path(&path)?;
            let mut needs_save = false;

            if config.node.private_key.is_empty() || config.node.public_key.is_empty() {
                config.regenerate_node_keys()?;
                needs_save = true;
            }

            // Migrate existing configs from passive to exclusive capture mode.
            if config.switch.capture_mode == CaptureMode::Passive {
                config.switch.capture_mode = CaptureMode::Exclusive;
                needs_save = true;
            }

            // If this device is still using a generic generated identity, upgrade it to a
            // more natural machine/Tailscale-based display name. Only rewrite the internal
            // node id if there are no paired peers yet, to avoid breaking trust relationships.
            if let Some(preferred_name) = detect_hostname() {
                if is_generic_node_name(&config.node.name) && config.node.name != preferred_name {
                    config.node.name = preferred_name.clone();
                    needs_save = true;
                }
                if config.peers.is_empty() && is_generic_node_id(&config.node.id) {
                    let suffix = config
                        .node
                        .id
                        .strip_prefix("local-node-")
                        .map(str::to_string)
                        .unwrap_or_else(|| generate_token_fragment(8));
                    let normalized = normalize_id(&preferred_name);
                    let new_id = format!("{normalized}-{suffix}");
                    if config.node.id != new_id {
                        config.node.id = new_id;
                        needs_save = true;
                    }
                }
            }

            if needs_save {
                config.save_to_path(&path)?;
            }

            Ok(config)
        } else {
            let config = Self::generated_default();
            config.save_to_path(&path)?;
            Ok(config)
        }
    }

    pub fn load_from_path(path: &Path) -> Result<Self> {
        let raw = fs::read_to_string(path)
            .with_context(|| format!("failed to read config from {}", path.display()))?;
        let config = toml::from_str::<Self>(&raw)
            .with_context(|| format!("failed to parse config from {}", path.display()))?;

        Ok(config)
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::default_path()?;
        self.save_to_path(&path)
    }

    pub fn save_to_path(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!("failed to create config directory {}", parent.display())
            })?;
        }

        let raw = toml::to_string_pretty(self).context("failed to serialize config")?;
        fs::write(path, raw)
            .with_context(|| format!("failed to write config to {}", path.display()))?;

        Ok(())
    }

    pub fn upsert_peer(&mut self, peer: PeerConfig) {
        if let Some(existing) = self
            .peers
            .iter_mut()
            .find(|candidate| candidate.id == peer.id)
        {
            *existing = peer;
        } else {
            self.peers.push(peer);
        }
    }

    pub fn regenerate_node_keys(&mut self) -> Result<()> {
        let signing_key = SigningKey::generate(&mut rand::rngs::OsRng);
        self.node.private_key = STANDARD_NO_PAD.encode(signing_key.to_bytes());
        self.node.public_key = STANDARD_NO_PAD.encode(signing_key.verifying_key().to_bytes());
        Ok(())
    }

    pub fn local_routable_ips() -> Result<Vec<IpAddr>> {
        let mut ips = Vec::new();

        #[cfg(target_os = "macos")]
        {
            if let Ok(interfaces) = if_addrs::get_if_addrs() {
                for interface in interfaces {
                    let ip = interface.ip();
                    if !ip.is_loopback() && ip.is_ipv4() {
                        ips.push(ip);
                    }
                }
            }
        }

        #[cfg(target_os = "windows")]
        {
            if let Ok(interfaces) = if_addrs::get_if_addrs() {
                for interface in interfaces {
                    let ip = interface.ip();
                    if !ip.is_loopback() && ip.is_ipv4() {
                        ips.push(ip);
                    }
                }
            }
        }

        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        {
            if let Ok(interfaces) = if_addrs::get_if_addrs() {
                for interface in interfaces {
                    let ip = interface.ip();
                    if !ip.is_loopback() && ip.is_ipv4() {
                        ips.push(ip);
                    }
                }
            }
        }

        if ips.is_empty() {
            if let Ok(ip) = detect_local_ip_address() {
                ips.push(ip);
            }
        }

        Ok(ips)
    }
}

fn detect_local_ip_address() -> Result<IpAddr> {
    let socket =
        UdpSocket::bind("0.0.0.0:0").context("failed to create local address probe socket")?;

    for target in ["1.1.1.1:80", "8.8.8.8:80"] {
        if socket.connect(target).is_ok() {
            if let Ok(local_addr) = socket.local_addr() {
                let ip = local_addr.ip();
                if !ip.is_loopback() {
                    return Ok(ip);
                }
            }
        }
    }

    Err(anyhow::anyhow!("could not determine local ip address"))
}

impl Config {
    pub fn default_path() -> Result<PathBuf> {
        if let Ok(path) = env::var("FLKY_CONFIG") {
            return Ok(PathBuf::from(path));
        }

        #[cfg(target_os = "macos")]
        {
            let home = env::var("HOME").context("HOME is not set")?;
            return Ok(PathBuf::from(home)
                .join("Library")
                .join("Application Support")
                .join("flowkey")
                .join("config.toml"));
        }

        #[cfg(target_os = "windows")]
        {
            let app_data = env::var("APPDATA").context("APPDATA is not set")?;
            return Ok(PathBuf::from(app_data).join("flowkey").join("config.toml"));
        }

        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        {
            let home = env::var("HOME").context("HOME is not set")?;
            Ok(PathBuf::from(home)
                .join(".config")
                .join("flowkey")
                .join("config.toml"))
        }
    }

    pub fn status_path() -> Result<PathBuf> {
        let config_path = Self::default_path()?;
        Ok(status_path_for_config_path(&config_path))
    }

    pub fn control_path() -> Result<PathBuf> {
        let config_path = Self::default_path()?;
        Ok(control_path_for_config_path(&config_path))
    }

    pub fn control_pipe_name(&self) -> String {
        format!(r"\\.\pipe\flowkey-{}", normalize_id(&self.node.id))
    }

    pub fn log_dir() -> Result<PathBuf> {
        let config_path = Self::default_path()?;
        Ok(log_dir_for_config_path(&config_path))
    }

    pub fn advertised_listen_addr(&self) -> Result<String> {
        if let Some(override_addr) = self.node.advertised_addr.as_deref() {
            advertised_listen_addr_with_override(&self.node.listen_addr, Some(override_addr))
        } else {
            advertised_listen_addr(&self.node.listen_addr)
        }
    }

    pub fn advertised_listen_addr_for_pairing(
        &self,
        override_addr: Option<&str>,
    ) -> Result<String> {
        if let Some(override_addr) = override_addr.or(self.node.advertised_addr.as_deref()) {
            return advertised_listen_addr_with_override(&self.node.listen_addr, Some(override_addr));
        }

        let socket_addr = self
            .node
            .listen_addr
            .parse::<SocketAddr>()
            .with_context(|| format!("invalid listen address {}", self.node.listen_addr))?;
        let port = socket_addr.port();

        if let Some(dns_name) = detect_tailscale_dns_name() {
            return Ok(format!("{}:{}", dns_name.trim_end_matches('.'), port));
        }

        if let Some(ts_ip) = detect_tailscale_ipv4() {
            return Ok(SocketAddr::new(ts_ip, port).to_string());
        }

        advertised_listen_addr_with_override(&self.node.listen_addr, None)
    }

    fn generated_default() -> Self {
        let suffix = generate_token_fragment(8);
        let hostname = detect_hostname().unwrap_or_else(|| "local-node".to_string());
        let normalized = normalize_id(&hostname);
        let signing_key = SigningKey::generate(&mut rand::rngs::OsRng);

        Self {
            node: NodeConfig {
                id: format!("{normalized}-{suffix}"),
                name: hostname,
                listen_addr: "0.0.0.0:48571".to_string(),
                advertised_addr: None,
                accept_remote_control: true,
                private_key: STANDARD_NO_PAD.encode(signing_key.to_bytes()),
                public_key: STANDARD_NO_PAD.encode(signing_key.verifying_key().to_bytes()),
            },
            switch: SwitchConfig {
                hotkey: "Ctrl+Alt+Shift+K".to_string(),
                capture_mode: CaptureMode::Exclusive,
                input_coalesce_window_ms: DEFAULT_INPUT_COALESCE_WINDOW_MS,
            },
            peers: Vec::new(),
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            node: NodeConfig {
                id: "local-node".to_string(),
                name: "Local Node".to_string(),
                listen_addr: "0.0.0.0:48571".to_string(),
                advertised_addr: None,
                accept_remote_control: true,
                private_key: "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".to_string(),
                public_key: "BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB".to_string(),
            },
            switch: SwitchConfig {
                hotkey: "Ctrl+Alt+Shift+K".to_string(),
                capture_mode: CaptureMode::Exclusive,
                input_coalesce_window_ms: DEFAULT_INPUT_COALESCE_WINDOW_MS,
            },
            peers: Vec::new(),
        }
    }
}

fn detect_hostname() -> Option<String> {
    detect_tailscale_dns_name()
        .map(|dns| dns.trim_end_matches('.').split('.').next().unwrap_or("local-node").to_string())
        .filter(|value| !value.trim().is_empty())
        .or_else(detect_platform_hostname)
        .or_else(|| {
            env::var("COMPUTERNAME")
                .ok()
                .filter(|value| !value.trim().is_empty())
        })
        .or_else(|| {
            env::var("HOSTNAME")
                .ok()
                .filter(|value| !value.trim().is_empty())
        })
        .map(|value| value.trim().to_string())
}

fn detect_platform_hostname() -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        for command in [
            ("scutil", &["--get", "LocalHostName"] as &[&str]),
            ("scutil", &["--get", "ComputerName"] as &[&str]),
            ("hostname", &["-s"] as &[&str]),
        ] {
            if let Ok(output) = Command::new(command.0).args(command.1).output() {
                if output.status.success() {
                    let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
                    if !value.is_empty() {
                        return Some(value);
                    }
                }
            }
        }
    }

    #[cfg(target_os = "windows")]
    {
        for command in [
            ("hostname", &[] as &[&str]),
            ("cmd", &["/C", "echo", "%COMPUTERNAME%"] as &[&str]),
        ] {
            if let Ok(output) = Command::new(command.0).args(command.1).output() {
                if output.status.success() {
                    let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
                    if !value.is_empty() && value != "%COMPUTERNAME%" {
                        return Some(value);
                    }
                }
            }
        }
    }

    None
}

fn detect_tailscale_status_json() -> Option<serde_json::Value> {
    let output = Command::new("tailscale").args(["status", "--json"]).output().ok()?;
    if !output.status.success() {
        return None;
    }
    serde_json::from_slice(&output.stdout).ok()
}

fn detect_tailscale_dns_name() -> Option<String> {
    detect_tailscale_status_json()?
        .get("Self")?
        .get("DNSName")?
        .as_str()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn detect_tailscale_ipv4() -> Option<IpAddr> {
    let json = detect_tailscale_status_json()?;
    let ips = json
        .get("Self")?
        .get("TailscaleIPs")?
        .as_array()?;
    for ip in ips {
        let ip = ip.as_str()?;
        if ip.contains(':') {
            continue;
        }
        if let Ok(ip) = ip.parse::<IpAddr>() {
            return Some(ip);
        }
    }
    None
}

fn is_generic_node_name(value: &str) -> bool {
    let value = value.trim().to_ascii_lowercase();
    value.is_empty() || value == "local-node" || value == "local node"
}

fn is_generic_node_id(value: &str) -> bool {
    value == "local-node" || value.starts_with("local-node-")
}

fn normalize_id(value: &str) -> String {
    let filtered: String = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();

    let compact = filtered
        .split('-')
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>()
        .join("-");

    if compact.is_empty() {
        "local-node".to_string()
    } else {
        compact
    }
}

fn default_accept_remote_control() -> bool {
    true
}

fn default_input_coalesce_window_ms() -> u64 {
    DEFAULT_INPUT_COALESCE_WINDOW_MS
}

fn generate_token_fragment(len: usize) -> String {
    thread_rng()
        .sample_iter(&Alphanumeric)
        .take(len)
        .map(char::from)
        .collect::<String>()
        .to_lowercase()
}

pub fn status_path_for_config_path(config_path: &Path) -> PathBuf {
    config_path
        .parent()
        .map(|parent| parent.join("status.toml"))
        .unwrap_or_else(|| PathBuf::from("status.toml"))
}

pub fn control_path_for_config_path(config_path: &Path) -> PathBuf {
    config_path
        .parent()
        .map(|parent| parent.join("control.toml"))
        .unwrap_or_else(|| PathBuf::from("control.toml"))
}

pub fn log_dir_for_config_path(config_path: &Path) -> PathBuf {
    config_path
        .parent()
        .map(|parent| parent.join("logs"))
        .unwrap_or_else(|| PathBuf::from("logs"))
}

pub fn advertised_listen_addr(listen_addr: &str) -> Result<String> {
    advertised_listen_addr_with_override(listen_addr, None)
}

pub fn advertised_listen_addr_with_override(
    listen_addr: &str,
    override_addr: Option<&str>,
) -> Result<String> {
    if let Some(override_addr) = override_addr {
        let socket_addr = override_addr
            .parse::<SocketAddr>()
            .with_context(|| format!("invalid advertised address {override_addr}"))?;

        if socket_addr.ip().is_loopback() || socket_addr.ip().is_unspecified() {
            return Err(anyhow::anyhow!(
                "advertised address must be a routable ip:port, got {}",
                override_addr
            ));
        }

        return Ok(socket_addr.to_string());
    }

    let socket_addr = listen_addr
        .parse::<SocketAddr>()
        .with_context(|| format!("invalid listen address {listen_addr}"))?;

    if !socket_addr.ip().is_unspecified() {
        return Ok(socket_addr.to_string());
    }

    let advertised_ip = detect_local_ip_address()?;
    if advertised_ip.is_loopback() || advertised_ip.is_unspecified() {
        return Err(anyhow::anyhow!(
            "could not determine a routable local address from {}",
            listen_addr
        ));
    }

    Ok(SocketAddr::new(advertised_ip, socket_addr.port()).to_string())
}

pub fn unix_timestamp_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock should be after unix epoch")
        .as_secs()
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::net::IpAddr;
    use std::net::SocketAddr;
    use std::path::PathBuf;

    use anyhow::{Context, Result};

    use super::{
        advertised_listen_addr, advertised_listen_addr_with_override, control_path_for_config_path,
        log_dir_for_config_path, normalize_id, status_path_for_config_path, CaptureMode, Config,
        PeerConfig,
    };

    #[test]
    fn default_config_round_trips_through_toml() {
        let config = Config::default();
        let encoded = toml::to_string(&config).expect("config should serialize");
        let decoded: Config = toml::from_str(&encoded).expect("config should deserialize");

        assert_eq!(decoded.node.id, "local-node");
        assert_eq!(decoded.switch.hotkey, "Ctrl+Alt+Shift+K");
        assert_eq!(decoded.switch.capture_mode, CaptureMode::Exclusive);
        assert_eq!(
            decoded.switch.input_coalesce_window_ms,
            super::DEFAULT_INPUT_COALESCE_WINDOW_MS
        );
        assert!(decoded.node.accept_remote_control);
        assert_eq!(
            decoded.node.private_key,
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"
        );
        assert_eq!(
            decoded.node.public_key,
            "BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB"
        );
        assert!(decoded.peers.is_empty());
    }

    #[test]
    fn upsert_peer_replaces_matching_peer() {
        let mut config = Config::default();
        config.upsert_peer(PeerConfig {
            id: "office-pc".to_string(),
            name: "Office PC".to_string(),
            addr: "192.168.1.25:48571".to_string(),
            public_key: "cHVibGljX2E".to_string(),
            trusted: true,
        });
        config.upsert_peer(PeerConfig {
            id: "office-pc".to_string(),
            name: "Office PC Updated".to_string(),
            addr: "192.168.1.26:48571".to_string(),
            public_key: "cHVibGljX2I".to_string(),
            trusted: true,
        });

        assert_eq!(config.peers.len(), 1);
        assert_eq!(config.peers[0].addr, "192.168.1.26:48571");
        assert_eq!(config.peers[0].public_key, "cHVibGljX2I");
    }

    #[test]
    fn normalize_id_compacts_non_alphanumeric_content() {
        assert_eq!(normalize_id("MacBook Air"), "macbook-air");
        assert_eq!(normalize_id("***"), "local-node");
    }

    #[test]
    fn save_and_reload_preserves_peers() {
        let mut config = Config::default();
        config.upsert_peer(PeerConfig {
            id: "office-pc".to_string(),
            name: "Office PC".to_string(),
            addr: "192.168.1.25:48571".to_string(),
            public_key: "cHVibGljX3Rlc3Q".to_string(),
            trusted: true,
        });

        let path = std::env::temp_dir().join(format!(
            "flowkey-config-test-{}.toml",
            super::generate_token_fragment(8)
        ));

        config
            .save_to_path(&path)
            .expect("config should save to temp path");
        let reloaded = Config::load_from_path(&path).expect("config should reload from temp path");
        fs::remove_file(&path).expect("temp config should be removable");

        assert_eq!(reloaded.peers.len(), 1);
        assert_eq!(reloaded.peers[0].id, "office-pc");
    }

    #[test]
    fn generated_default_contains_keypair_material() {
        let config = Config::generated_default();

        assert!(!config.node.private_key.is_empty());
        assert!(!config.node.public_key.is_empty());
        assert_ne!(config.node.private_key, config.node.public_key);
    }

    #[test]
    fn legacy_switch_config_defaults_capture_mode_to_exclusive() {
        let decoded: Config = toml::from_str(
            r#"
    [node]
    id = "legacy"
    name = "Legacy Node"
    listen_addr = "0.0.0.0:48571"
    accept_remote_control = true
    private_key = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"
    public_key = "BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB"

    [switch]
    hotkey = "Ctrl+Alt+Shift+K"
    "#,
        )
        .expect("legacy config should deserialize");

        assert_eq!(decoded.switch.capture_mode, CaptureMode::Exclusive);

        assert_eq!(
            decoded.switch.input_coalesce_window_ms,
            super::DEFAULT_INPUT_COALESCE_WINDOW_MS
        );
    }

    #[test]
    fn status_path_follows_config_location() {
        let config_path = PathBuf::from("/tmp/flowkey/config.toml");

        let status_path = status_path_for_config_path(&config_path);

        assert_eq!(status_path, PathBuf::from("/tmp/flowkey/status.toml"));
    }

    #[test]
    fn control_path_follows_config_location() {
        let config_path = PathBuf::from("/tmp/flowkey/config.toml");

        let control_path = control_path_for_config_path(&config_path);

        assert_eq!(control_path, PathBuf::from("/tmp/flowkey/control.toml"));
    }

    #[test]
    fn control_pipe_name_uses_normalized_node_id() {
        let mut config = Config::default();
        config.node.id = "My Work PC".to_string();

        assert_eq!(config.control_pipe_name(), r"\\.\pipe\flowkey-my-work-pc");
    }

    #[test]
    fn log_dir_follows_config_location() {
        let config_path = PathBuf::from("/tmp/flowkey/config.toml");

        let log_dir = log_dir_for_config_path(&config_path);

        assert_eq!(log_dir, PathBuf::from("/tmp/flowkey/logs"));
    }

    #[test]
    fn advertised_listen_addr_rewrites_wildcard_bind_to_local_ip() {
        let listen_addr = "0.0.0.0:48571";
        let advertised = advertised_listen_addr_with_resolver(listen_addr, || {
            Ok("192.168.1.10".parse().expect("test ip should parse"))
        })
        .expect("wildcard bind should be converted to local ip");

        assert_eq!(advertised, "192.168.1.10:48571");
    }

    #[test]
    fn advertised_listen_addr_keeps_specific_bind_address() {
        let advertised = advertised_listen_addr("192.168.1.25:48571")
            .expect("specific listen address should be preserved");

        assert_eq!(advertised, "192.168.1.25:48571");
    }

    #[test]
    fn advertised_listen_addr_prefers_explicit_override() {
        let advertised =
            advertised_listen_addr_with_override("0.0.0.0:48571", Some("100.79.183.18:48571"))
                .expect("explicit advertised address should be preserved");

        assert_eq!(advertised, "100.79.183.18:48571");
    }

    #[test]
    fn advertised_listen_addr_rejects_non_routable_override() {
        let error = advertised_listen_addr_with_override("0.0.0.0:48571", Some("0.0.0.0:48571"))
            .expect_err("wildcard override should be rejected");

        assert!(error
            .to_string()
            .contains("advertised address must be a routable ip:port"));
    }

    fn advertised_listen_addr_with_resolver<F>(
        listen_addr: &str,
        resolve_local_ip: F,
    ) -> Result<String>
    where
        F: FnOnce() -> Result<IpAddr>,
    {
        let socket_addr = listen_addr
            .parse::<SocketAddr>()
            .with_context(|| format!("invalid listen address {listen_addr}"))?;

        if !socket_addr.ip().is_unspecified() {
            return Ok(socket_addr.to_string());
        }

        let advertised_ip = resolve_local_ip()?;
        if advertised_ip.is_loopback() || advertised_ip.is_unspecified() {
            return Err(anyhow::anyhow!(
                "could not determine a routable local address from {}",
                listen_addr
            ));
        }

        Ok(SocketAddr::new(advertised_ip, socket_addr.port()).to_string())
    }
}
