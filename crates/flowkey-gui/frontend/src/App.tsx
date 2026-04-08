import { useState, useEffect } from 'react'
import { invoke } from '@tauri-apps/api/tauri'
import { DiscoveredPeer, Config } from './types'
import './App.css'

function App() {
  const [config, setConfig] = useState<Config | null>(null)
  const [discoveredPeers, setDiscoveredPeers] = useState<DiscoveredPeer[]>([])
  const [pairingSas, setPairingSas] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [isPairing, setIsPairing] = useState(false)

  // Load config on mount
  useEffect(() => {
    loadConfig()
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
    
    // Pick the first address and use the pairing port
    // Usually addrs are like "192.168.1.5:48571"
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

  return (
    <div className="container">
      <header>
        <h1>flowkey Manager</h1>
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

            <section className="config-section">
              <h2>Trusted Peers</h2>
              <ul className="trusted-list">
                {config?.peers.map(peer => (
                  <li key={peer.id} className="trusted-item">
                    <span>{peer.name}</span>
                    <button className="btn-text">Remove</button>
                  </li>
                ))}
                {config?.peers.length === 0 && <li className="empty">No trusted peers yet. Pair a device to get started.</li>}
              </ul>
            </section>
          </div>
        )}
      </main>
    </div>
  )
}

export default App
