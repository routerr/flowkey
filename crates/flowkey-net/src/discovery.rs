use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, SocketAddr};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use flowkey_config::Config;
use mdns_sd::{ScopedIp, ServiceDaemon, ServiceEvent, ServiceInfo};

pub const SERVICE_TYPE: &str = "_flky._tcp.local.";
const PROPERTY_NODE_ID: &str = "node_id";
const PROPERTY_NODE_NAME: &str = "node_name";

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredPeer {
    pub id: String,
    pub name: String,
    pub addrs: Vec<String>,
    pub hostname: String,
    pub service_name: String,
}

pub fn advertise(config: &Config) -> Result<DiscoveryAdvertisement> {
    let advertised_addr = config
        .advertised_listen_addr()
        .context("failed to derive advertised listen address for discovery")?;
    let socket_addr: SocketAddr = advertised_addr
        .parse()
        .with_context(|| format!("invalid advertised address {advertised_addr}"))?;
    let hostname = format!("{}.local.", sanitize_label(&config.node.id));
    let properties = [
        (PROPERTY_NODE_ID, config.node.id.as_str()),
        (PROPERTY_NODE_NAME, config.node.name.as_str()),
    ];
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

pub fn discover(timeout: Duration) -> Result<Vec<DiscoveredPeer>> {
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
    
    addrs.sort(); // Predictable order
    addrs.dedup();

    if addrs.is_empty() {
        return None;
    }

    Some(DiscoveredPeer {
        id,
        name,
        addrs,
        hostname: service.host.clone(),
        service_name: service.fullname.clone(),
    })
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
        let properties = [("node_id", "office-pc"), ("node_name", "Office PC")];
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
