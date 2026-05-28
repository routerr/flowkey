use std::collections::HashMap;
use std::net::{SocketAddr, ToSocketAddrs};
use std::process::Command;
use std::time::{Duration, Instant};

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

use anyhow::{Context, Result};
use flowkey_config::Config;
use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use serde::Serialize;

pub const SERVICE_TYPE: &str = "_flky._tcp.local.";
pub const PROPERTY_NODE_ID: &str = "node_id";
pub const PROPERTY_NODE_NAME: &str = "node_name";
pub const PROPERTY_IS_PAIRING: &str = "is_pairing";
pub const PROPERTY_PAIRING_PORT: &str = "pairing_port";
pub const PROPERTY_HOSTNAME: &str = "hostname";
pub const DEFAULT_PAIRING_PORT: u16 = 48572;

pub struct DiscoveryAdvertisement {
    daemon: ServiceDaemon,
}

impl DiscoveryAdvertisement {
    pub fn shutdown(&self) -> Result<()> {
        let receiver = self
            .daemon
            .shutdown()
            .context("failed to request discovery daemon shutdown")?;
        let _ = receiver
            .recv_timeout(Duration::from_secs(1))
            .context("timed out waiting for discovery daemon shutdown")?;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DiscoveredPeer {
    pub id: String,
    pub name: String,
    pub addrs: Vec<String>,
    pub hostname: String,
    pub service_name: String,
    pub is_pairing: bool,
    pub pairing_port: Option<u16>,
}

pub fn advertise(
    config: &Config,
    is_pairing: bool,
    pairing_port: Option<u16>,
) -> Result<DiscoveryAdvertisement> {
    let advertised_addr = config
        .advertised_listen_addr()
        .context("failed to derive advertised listen address for discovery")?;
    let socket_addr: SocketAddr = advertised_addr
        .parse()
        .with_context(|| format!("invalid advertised address {advertised_addr}"))?;
    let hostname = format!("{}.local.", sanitize_label(&config.node.id));

    let routable_ips = Config::local_routable_ips()
        .unwrap_or_default()
        .into_iter()
        .map(|ip| ip.to_string())
        .collect::<Vec<_>>()
        .join(",");

    let mut properties = vec![
        (PROPERTY_NODE_ID, config.node.id.as_str()),
        (PROPERTY_NODE_NAME, config.node.name.as_str()),
        (
            PROPERTY_IS_PAIRING,
            if is_pairing { "true" } else { "false" },
        ),
        ("ips", routable_ips.as_str()),
    ];

    // Include local hostname for DNS-based discovery (e.g., Tailscale Magic DNS)
    let hostname_prop: String;
    if let Ok(h) = std::env::var("HOSTNAME").or_else(|_| std::env::var("COMPUTERNAME")) {
        let trimmed = h.trim().to_string();
        if cfg!(target_os = "macos") {
            // On macOS, append .local for Bonjour-style lookup
            hostname_prop = format!("{}.local.", trimmed);
        } else {
            hostname_prop = trimmed;
        }
        properties.push((PROPERTY_HOSTNAME, hostname_prop.as_str()));
    }

    let port_str;
    if let Some(port) = pairing_port {
        port_str = port.to_string();
        properties.push((PROPERTY_PAIRING_PORT, port_str.as_str()));
    }

    let service = ServiceInfo::new(
        SERVICE_TYPE,
        &config.node.id,
        &hostname,
        socket_addr.ip().to_string(),
        socket_addr.port(),
        &properties[..],
    )
    .context("failed to construct discovery service info")?;

    let daemon = ServiceDaemon::new().context("failed to start mDNS discovery daemon")?;
    daemon
        .register(service)
        .context("failed to advertise discovery service")?;

    Ok(DiscoveryAdvertisement { daemon })
}

pub fn discover(timeout: Duration, exclude_id: Option<&str>) -> Result<Vec<DiscoveredPeer>> {
    let daemon = ServiceDaemon::new().context("failed to start mDNS discovery daemon")?;
    let receiver = daemon
        .browse(SERVICE_TYPE)
        .context("failed to browse discovery services")?;
    let deadline = Instant::now() + timeout;
    let mut peers = HashMap::<String, DiscoveredPeer>::new();

    while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
        if remaining.is_zero() {
            break;
        }

        match receiver.recv_timeout(remaining) {
            Ok(ServiceEvent::ServiceResolved(service)) => {
                if let Some(peer) = peer_from_resolved_service(&service) {
                    if let Some(exclude) = exclude_id {
                        if peer.id == exclude {
                            continue;
                        }
                    }
                    peers.insert(peer.id.clone(), peer);
                }
            }
            Ok(ServiceEvent::ServiceRemoved(_, fullname)) => {
                peers.retain(|_, peer| peer.service_name != fullname);
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }

    let _ = daemon.shutdown();

    let mut peers = peers.into_values().collect::<Vec<_>>();
    peers.sort_by(|left, right| left.name.cmp(&right.name).then(left.id.cmp(&right.id)));
    Ok(peers)
}

/// Discover peers from Tailscale without requiring mDNS or prior flowkey pairing.
/// This uses `tailscale status --json` and returns online peers with their
/// Tailscale DNS name and Tailscale IPs mapped to the always-on GUI pairing port.
pub fn discover_tailscale_peers() -> Vec<DiscoveredPeer> {
    #[cfg(target_os = "windows")]
    const CREATE_NO_WINDOW: u32 = 0x08000000;

    let mut command = Command::new("tailscale");
    command.args(["status", "--json"]);
    #[cfg(target_os = "windows")]
    command.creation_flags(CREATE_NO_WINDOW);

    let output = match command.output() {
        Ok(output) if output.status.success() => output,
        _ => return Vec::new(),
    };

    let json: serde_json::Value = match serde_json::from_slice(&output.stdout) {
        Ok(json) => json,
        Err(_) => return Vec::new(),
    };

    let self_dns = json
        .get("Self")
        .and_then(|v| v.get("DNSName"))
        .and_then(|v| v.as_str())
        .map(|s| s.trim_end_matches('.').to_string());

    let mut peers = Vec::new();
    let Some(map) = json.get("Peer").and_then(|v| v.as_object()) else {
        return peers;
    };

    for value in map.values() {
        if !value
            .get("Online")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            continue;
        }

        let dns_name = value
            .get("DNSName")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim_end_matches('.')
            .to_string();
        if self_dns.as_deref() == Some(dns_name.as_str()) {
            continue;
        }

        let host_name = value
            .get("HostName")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let short_name = dns_name
            .split('.')
            .next()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| host_name.as_str())
            .to_string();
        if short_name.is_empty() {
            continue;
        }

        let mut addrs = Vec::new();
        if let Some(ts_ips) = value.get("TailscaleIPs").and_then(|v| v.as_array()) {
            for ip in ts_ips {
                if let Some(ip) = ip.as_str() {
                    if ip.contains(':') {
                        continue; // IPv6 later if needed
                    }
                    let addr = format!("{}:{}", ip, DEFAULT_PAIRING_PORT);
                    if !addrs.contains(&addr) {
                        addrs.push(addr);
                    }
                }
            }
        }
        if !dns_name.is_empty() {
            let dns_addr = format!("{}:{}", dns_name, DEFAULT_PAIRING_PORT);
            if !addrs.contains(&dns_addr) {
                addrs.push(dns_addr);
            }
        }

        if addrs.is_empty() {
            continue;
        }

        peers.push(DiscoveredPeer {
            id: short_name.clone(),
            name: short_name,
            addrs,
            hostname: dns_name.clone(),
            service_name: format!("tailscale:{}", dns_name),
            is_pairing: true,
            pairing_port: Some(DEFAULT_PAIRING_PORT),
        });
    }

    peers.sort_by(|left, right| left.name.cmp(&right.name).then(left.id.cmp(&right.id)));
    peers
}

fn peer_from_resolved_service(service: &mdns_sd::ResolvedService) -> Option<DiscoveredPeer> {
    let id = service
        .txt_properties
        .get_property_val_str(PROPERTY_NODE_ID)
        .map(str::trim)
        .filter(|value| !value.is_empty())?
        .to_string();
    let name = service
        .txt_properties
        .get_property_val_str(PROPERTY_NODE_NAME)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(&id)
        .to_string();
    let mut addrs: Vec<String> = service
        .addresses
        .iter()
        .map(mdns_sd::ScopedIp::to_ip_addr)
        .filter(|ip| !ip.is_loopback())
        .map(|ip| SocketAddr::new(ip, service.port).to_string())
        .collect();

    if let Some(ips_str) = service.txt_properties.get_property_val_str("ips") {
        for ip_str in ips_str.split(',') {
            if let Ok(ip) = ip_str.trim().parse::<std::net::IpAddr>() {
                if !ip.is_loopback() {
                    let addr = SocketAddr::new(ip, service.port).to_string();
                    if !addrs.contains(&addr) {
                        addrs.push(addr);
                    }
                }
            }
        }
    }

    // Try DNS resolution from the advertised hostname (for Tailscale Magic DNS)
    if let Some(hostname) = service.txt_properties.get_property_val_str(PROPERTY_HOSTNAME) {
        let dns_addrs = resolve_hostname_to_addrs(hostname, service.port);
        for addr in dns_addrs {
            if !addrs.contains(&addr) {
                addrs.push(addr);
            }
        }
    }

    addrs.sort(); // Predictable order
    addrs.dedup();

    if addrs.is_empty() {
        return None;
    }

    let is_pairing = service
        .txt_properties
        .get_property_val_str(PROPERTY_IS_PAIRING)
        .map(|val| val == "true")
        .unwrap_or(false);

    let pairing_port = service
        .txt_properties
        .get_property_val_str(PROPERTY_PAIRING_PORT)
        .and_then(|val| val.parse::<u16>().ok());

    Some(DiscoveredPeer {
        id,
        name,
        addrs,
        hostname: service.host.clone(),
        service_name: service.fullname.clone(),
        is_pairing,
        pairing_port,
    })
}

/// Resolve a hostname to SocketAddr strings via DNS.
/// Supports standard DNS, Bonjour `.local.` suffix, and Tailscale Magic DNS.
pub fn resolve_hostname_to_addrs(hostname: &str, port: u16) -> Vec<String> {
    let mut results = Vec::new();

    // Already an IP? Skip resolution.
    if hostname.parse::<std::net::IpAddr>().is_ok() {
        return results;
    }

    let hostname = hostname.trim_end_matches('.');

    // Try the hostname as-is
    if let Ok(addrs) = format!("{}:{}", hostname, port).to_socket_addrs() {
        for addr in addrs {
            let s = addr.to_string();
            if !results.contains(&s) {
                results.push(s);
            }
        }
    }

    // Try resolving as a Tailscale Magic DNS hostname
    let tailscale_domains = [".ts.net", ".tailscale.ts.net"];
    for domain in &tailscale_domains {
        if !hostname.ends_with(domain) {
            let short = hostname
                .trim_end_matches(".local")
                .trim_end_matches(".ts.net")
                .trim_end_matches(".tailscale.ts.net");
            let fqdn = format!("{}{}", short, domain);
            if let Ok(addrs) = format!("{}:{}", fqdn, port).to_socket_addrs() {
                for addr in addrs {
                    let s = addr.to_string();
                    if !results.contains(&s) {
                        results.push(s);
                    }
                }
            }
        }
    }

    // Try Bonjour `.local.` resolution
    if !hostname.ends_with(".local") {
        if let Ok(addrs) = format!("{}:{}", hostname, port).to_socket_addrs() {
            for addr in addrs {
                let s = addr.to_string();
                if !results.contains(&s) {
                    results.push(s);
                }
            }
        }
    }

    results
}

/// Collect candidate addresses for a peer using all available resolution methods.
/// Merges mDNS-discovered addresses with DNS-resolved hostnames.
pub fn collect_peer_candidates(
    configured_addr: &str,
    mdn_addrs: &[String],
    _peer_id: &str,
) -> Vec<String> {
    let mut candidates: Vec<String> = Vec::new();

    // Add mDNS-discovered addresses first (most up-to-date for LAN)
    for addr in mdn_addrs {
        if !candidates.contains(addr) {
            candidates.push(addr.clone());
        }
    }

    // Parse the configured address
    let default_port = 48571;
    let (host_str, port) = if let Ok(sa) = configured_addr.parse::<SocketAddr>() {
        // Already an IP:port pair — add it directly
        let s = sa.to_string();
        if !candidates.contains(&s) {
            candidates.push(s);
        }
        // Pairing bugs or old configs may have captured an ephemeral source port.
        // Always also try the same host on the default daemon port to self-heal.
        if sa.port() != default_port {
            let fallback = SocketAddr::new(sa.ip(), default_port).to_string();
            if !candidates.contains(&fallback) {
                candidates.push(fallback);
            }
        }
        (sa.ip().to_string(), sa.port())
    } else if let Some((host_part, port_part)) = configured_addr.rsplit_once(':') {
        let p = port_part.parse::<u16>().unwrap_or(default_port);
        (host_part.to_string(), p)
    } else {
        (configured_addr.to_string(), default_port)
    };

    // Try DNS resolution
    let dns_addrs = resolve_hostname_to_addrs(&host_str, port);
    for addr in dns_addrs {
        if !candidates.contains(&addr) {
            candidates.push(addr);
        }
    }

    // Try short hostname with Tailscale suffixes (even if host_str is not clearly a Tailscale name)
    let short = host_str.trim_end_matches(".local").trim_end_matches(".ts.net").trim_end_matches(".tailscale.ts.net");
    for suffix in [".ts.net", ".tailscale.ts.net"] {
        if !host_str.ends_with(suffix) {
            let fqdn = format!("{}{}", short, suffix);
            if let Ok(dns_addrs) = format!("{}:{}", fqdn, port).to_socket_addrs() {
                for addr in dns_addrs {
                    let s = addr.to_string();
                    if !candidates.contains(&s) {
                        candidates.push(s);
                    }
                }
            }
        }
    }

    candidates
}

fn sanitize_label(value: &str) -> String {
    let label = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string();

    if label.is_empty() {
        "flky-node".to_string()
    } else {
        label
    }
}

#[cfg(test)]
mod tests {
    use mdns_sd::ServiceInfo;

    use super::{peer_from_resolved_service, sanitize_label};

    #[test]
    fn sanitize_label_rewrites_invalid_hostname_chars() {
        assert_eq!(sanitize_label("Office PC.local"), "office-pc-local");
        assert_eq!(sanitize_label("***"), "flky-node");
    }

    #[test]
    fn resolved_service_maps_to_discovered_peer() {
        let properties = [
            ("node_id", "office-pc"),
            ("node_name", "Office PC"),
            ("is_pairing", "true"),
        ];
        let service = ServiceInfo::new(
            "_flky._tcp.local.",
            "office-pc",
            "office-pc.local.",
            "192.168.1.25",
            48571,
            &properties[..],
        )
        .expect("service info should build")
        .as_resolved_service();

        let peer = peer_from_resolved_service(&service).expect("service should parse");
        assert_eq!(peer.id, "office-pc");
        assert_eq!(peer.name, "Office PC");
        assert_eq!(peer.addrs, vec!["192.168.1.25:48571".to_string()]);
        assert_eq!(peer.hostname, "office-pc.local.");
        assert!(peer.is_pairing);
    }

    #[test]
    fn resolved_service_without_node_id_is_ignored() {
        let properties = [("node_name", "Office PC")];
        let service = ServiceInfo::new(
            "_flky._tcp.local.",
            "office-pc",
            "office-pc.local.",
            "192.168.1.25",
            48571,
            &properties[..],
        )
        .expect("service info should build")
        .as_resolved_service();

        assert!(peer_from_resolved_service(&service).is_none());
    }
}
