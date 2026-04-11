export interface DiscoveredPeer {
  id: string;
  name: string;
  addrs: string[];
  hostname: string;
  service_name: string;
  is_pairing: boolean;
  pairing_port?: number;
}

export interface NodeConfig {
  id: string;
  name: string;
  listen_addr: string;
  advertised_addr?: string;
  accept_remote_control: boolean;
  public_key: string;
}

export interface PeerConfig {
  id: string;
  name: string;
  addr: string;
  public_key: string;
  trusted: boolean;
}

export interface Config {
  node: NodeConfig;
  switch: {
    hotkey: string;
    capture_mode: 'passive' | 'exclusive';
  };
  peers: PeerConfig[];
}

export interface PermissionStatus {
  accessibility: boolean;
  input_monitoring: boolean;
}

export interface DaemonStatus {
  state: string;
  active_peer_id?: string;
  session_healthy: boolean;
  local_capture_enabled: boolean;
  capture_restarts: number;
  input_injection_backend: string;
  notes: string[];
  connected_peer_ids: string[];
}
