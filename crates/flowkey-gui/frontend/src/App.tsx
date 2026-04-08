import { useState, useEffect } from 'react'
import { invoke } from '@tauri-apps/api/tauri'
import { listen } from '@tauri-apps/api/event'
import { type DiscoveredPeer, type Config, type DaemonStatus } from './types'
import './App.css'

function App() {
  const [config, setConfig] = useState<Config | null>(null)
  const [status, setStatus] = useState<DaemonStatus | null>(null)
  const [discoveredPeers, setDiscoveredPeers] = useState<DiscoveredPeer[]>([])
  const [pairingSas, setPairingSas] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [isPairing, setIsPairing] = useState(false)

  // Load config on mount
  useEffect(() => {
    loadConfig()
  }, [])

  // Listen for daemon status events
  useEffect(() => {
    const unlisten = listen<DaemonStatus>('daemon-status', (event) => {
      setStatus(event.payload)
    })
    return () => {
      unlisten.then(fn => fn())
    }
  }, [])

  // Poll for discovery
  useEffect(() => {
    const interval = setInterval(async () => {
      try {
        const peers = await invoke<DiscoveredPeer[]>('get_discovered_peers')
        setDiscoveredPeers(peers)
      } catch (e) {
        console.error("Discovery error:", e)
      }
    }, 2000)
    return () => clearInterval(interval)
  }, [])

  async function loadConfig() {
    try {
      const cfg = await invoke<Config>('get_config')
      setConfig(cfg)
    } catch (e) {
      setError(String(e))
    }
  }

  async function startPairingMode() {
    setIsPairing(true)
    setError(null)
    try {
      const sas = await invoke<string>('enter_pairing_mode')
      setPairingSas(sas)
    } catch (e) {
      setError(String(e))
      setIsPairing(false)
    }
  }

  async function connectToPeer(peer: DiscoveredPeer) {
    if (!peer.pairing_port || peer.addrs.length === 0) {
      setError("Peer is not in pairing mode or has no address")
      return
    }
    
    const ip = peer.addrs[0].split(':')[0]
    const addr = `${ip}:${peer.pairing_port}`
    
    setIsPairing(true)
    setError(null)
    try {
      const sas = await invoke<string>('connect_to_peer', { peerAddr: addr })
      setPairingSas(sas)
    } catch (e) {
      setError(String(e))
      setIsPairing(false)
    }
  }

  async function confirmPairing() {
    try {
      await invoke('confirm_pairing')
      setPairingSas(null)
      setIsPairing(false)
      loadConfig()
    } catch (e) {
      setError(String(e))
    }
  }

  async function cancelPairing() {
    try {
      await invoke('cancel_pairing')
      setPairingSas(null)
      setIsPairing(false)
    } catch (e) {
      setError(String(e))
    }
  }

  async function removePeer(peerId: string) {
    if (!confirm(`Are you sure you want to remove peer ${peerId}?`)) return
    try {
      await invoke('remove_peer', { peerId })
      loadConfig()
    } catch (e) {
      setError(String(e))
    }
  }

  async function switchToPeer(peerId: string) {
    try {
      await invoke('switch_to_peer', { peerId })
    } catch (e) {
      setError(String(e))
    }
  }

  async function releaseControl() {
    try {
      await invoke('release_control')
    } catch (e) {
      setError(String(e))
    }
  }

  return (
    <div className="container">
      <header>
        <div className="title-area">
          <h1>flowkey Manager</h1>
          {status && (
            <span className={`status-badge ${status.session_healthy ? 'healthy' : 'unhealthy'}`}>
              {status.state}
            </span>
          )}
        </div>
        {config && <div className="node-info">Node: {config.node.name} ({config.node.id})</div>}
      </header>

      {error && <div className="error-bar">{error}</div>}

      <main>
        {isPairing ? (
          <section className="pairing-screen">
            <h2>Pairing in Progress</h2>
            {pairingSas ? (
              <div className="sas-display">
                <p>Verify this code on BOTH machines:</p>
                <div className="sas-code">{pairingSas}</div>
                <div className="pairing-actions">
                  <button onClick={confirmPairing} className="btn-primary">Confirm & Pair</button>
                  <button onClick={cancelPairing} className="btn-secondary">Cancel</button>
                </div>
              </div>
            ) : (
              <div className="waiting">
                <p>Waiting for connection...</p>
                <button onClick={cancelPairing} className="btn-secondary">Cancel</button>
              </div>
            )}
          </section>
        ) : (
          <div className="dashboard">
            <section className="peers-section">
              {status?.state.startsWith('controlling') && (
                <div className="active-control-banner">
                  <span>Currently controlling <strong>{status.active_peer_id}</strong></span>
                  <button onClick={releaseControl} className="btn-small btn-error">Release Control</button>
                </div>
              )}
              <div className="section-header">
                <h2>Discovered Devices</h2>
                <button onClick={startPairingMode} className="btn-small">Make Discoverable</button>
              </div>
              <ul className="peer-list">
                {discoveredPeers.length === 0 && <li className="empty">No devices found on LAN...</li>}
                {discoveredPeers.map(peer => (
                  <li key={peer.id} className="peer-item">
                    <div className="peer-info">
                      <span className="peer-name">{peer.name}</span>
                      <span className="peer-id">{peer.id}</span>
                    </div>
                    {peer.is_pairing ? (
                      <button onClick={() => connectToPeer(peer)} className="btn-connect">Connect</button>
                    ) : (
                      <span className="status-tag">Connected</span>
                    )}
                  </li>
                ))}
              </ul>
            </section>

            <div className="side-column">
              <section className="config-section">
                <h2>Trusted Peers</h2>
                <ul className="trusted-list">
                  {config?.peers.map(peer => (
                    <li key={peer.id} className="trusted-item">
                      <div className="peer-info">
                        <span className="peer-name">{peer.name}</span>
                        <span className="peer-id">{peer.id}</span>
                      </div>
                      <div className="peer-actions">
                        <button onClick={() => switchToPeer(peer.id)} className="btn-small btn-primary">Control</button>
                        <button onClick={() => removePeer(peer.id)} className="btn-text">Remove</button>
                      </div>
                    </li>
                  ))}

                  {config?.peers.length === 0 && <li className="empty">No trusted peers yet.</li>}
                </ul>
              </section>

              <section className="diagnostics-section">
                <h2>Diagnostics</h2>
                <div className="diag-info">
                  <div className="diag-item">
                    <span className="label">Capture:</span>
                    <span className={status?.local_capture_enabled ? "text-success" : "text-error"}>
                      {status?.local_capture_enabled ? "Enabled" : "Disabled"}
                    </span>
                  </div>
                  <div className="diag-item">
                    <span className="label">Backend:</span>
                    <span>{status?.input_injection_backend || "-"}</span>
                  </div>
                  <div className="notes-list">
                    {status?.notes.map((note, i) => (
                      <div key={i} className="note-item">• {note}</div>
                    ))}
                  </div>
                </div>
              </section>
            </div>
          </div>
        )}
      </main>
    </div>
  )
}

export default App
