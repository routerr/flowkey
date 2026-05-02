import { useState, useEffect, useRef } from 'react'
import { invoke, event } from '@tauri-apps/api'
import { enable, disable, isEnabled } from 'tauri-plugin-autostart-api'
import {
  type DiscoveredPeer,
  type Config,
  type DaemonStatus,
  type InputDebugEvent,
  type PermissionStatus,
} from './types'
import './App.css'

const CONTROL_RELEASE_GUARD_MS = 2000

function App() {
  const [config, setConfig] = useState<Config | null>(null)
  const [status, setStatus] = useState<DaemonStatus | null>(null)
  const [permissions, setPermissions] = useState<PermissionStatus | null>(null)
  const [autostartEnabled, setAutostartEnabled] = useState<boolean | null>(null)
  const [discoveredPeers, setDiscoveredPeers] = useState<DiscoveredPeer[]>([])
  const [pairingSas, setPairingSas] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [disconnectNotif, setDisconnectNotif] = useState<string | null>(null)
  const [isPairing, setIsPairing] = useState(false)
  const [inputDebugEvents, setInputDebugEvents] = useState<InputDebugEvent[]>([])
  const prevStateRef = useRef<string | null>(null)
  const userReleasedRef = useRef(false)
  const controlStartedAtRef = useRef<number | null>(null)

  // Load config on mount
  useEffect(() => {
    loadConfig()
    loadPermissionStatus()
    loadAutostartStatus()
  }, [])

  // Listen for daemon status events
  useEffect(() => {
    const unlisten = event.listen<DaemonStatus>('daemon-status', (e) => {
      const next = e.payload.state
      const prev = prevStateRef.current
      if (prev === 'controlling' && next !== 'controlling' && !userReleasedRef.current) {
        setDisconnectNotif('Control session ended — the remote peer disconnected.')
      }
      userReleasedRef.current = false
      prevStateRef.current = next
      setStatus(e.payload)
    })
    return () => {
      unlisten.then(fn => fn())
    }
  }, [])

  useEffect(() => {
    const unlisten = event.listen<InputDebugEvent>('input-debug-event', (e) => {
      setInputDebugEvents((events) => [...events, e.payload].slice(-80))
    })
    return () => {
      unlisten.then(fn => fn())
    }
  }, [])

  // While controlling a remote peer, swallow keyboard events at the window
  // level so that focused UI elements (notably the "Release Control" button)
  // cannot be re-activated by Enter/Space when the platform-level capture
  // hook fails to suppress local key delivery (observed on some Windows
  // setups where WH_KEYBOARD_LL is dropped and only the rdev/polling
  // fallbacks deliver events to the daemon).
  useEffect(() => {
    if (status?.state !== 'controlling') return

    if (document.activeElement instanceof HTMLElement) {
      document.activeElement.blur()
    }

    const swallow = (e: KeyboardEvent) => {
      e.preventDefault()
      e.stopPropagation()
      e.stopImmediatePropagation()
    }

    window.addEventListener('keydown', swallow, { capture: true })
    window.addEventListener('keyup', swallow, { capture: true })
    window.addEventListener('keypress', swallow, { capture: true })

    return () => {
      window.removeEventListener('keydown', swallow, { capture: true })
      window.removeEventListener('keyup', swallow, { capture: true })
      window.removeEventListener('keypress', swallow, { capture: true })
    }
  }, [status?.state])

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

  async function loadPermissionStatus() {
    try {
      const next = await invoke<PermissionStatus>('get_permission_status')
      setPermissions(next)
    } catch (e) {
      console.error('Permission status error:', e)
    }
  }

  async function loadAutostartStatus() {
    try {
      setAutostartEnabled(await isEnabled())
    } catch (e) {
      console.error('Autostart status error:', e)
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
    if (status && !status.connected_peer_ids.includes(peerId)) {
      setError(`Cannot control ${peerId}: peer is not currently connected. Make sure flowkey is running on the remote device.`)
      return
    }
    setError(null)
    try {
      await invoke('switch_to_peer', { peerId })
      controlStartedAtRef.current = Date.now()
    } catch (e) {
      setError(String(e))
    }
  }

  async function releaseControl() {
    if (
      status?.state === 'controlling' &&
      controlStartedAtRef.current !== null &&
      Date.now() - controlStartedAtRef.current < CONTROL_RELEASE_GUARD_MS
    ) {
      setError('Control just started. Try releasing again after a moment.')
      return
    }

    userReleasedRef.current = true
    try {
      await invoke('release_control')
      controlStartedAtRef.current = null
    } catch (e) {
      userReleasedRef.current = false
      setError(String(e))
    }
  }

  async function openPermissions() {
    try {
      await invoke('open_permissions')
      await loadPermissionStatus()
    } catch (e) {
      setError(String(e))
    }
  }

  async function toggleAutostart() {
    try {
      if (autostartEnabled) {
        await disable()
      } else {
        await enable()
      }
      await loadAutostartStatus()
    } catch (e) {
      setError(String(e))
    }
  }

  async function toggleRemoteControl() {
    if (!config) return
    try {
      await invoke('set_accept_remote_control', {
        enabled: !config.node.accept_remote_control,
      })
      await loadConfig()
    } catch (e) {
      setError(String(e))
    }
  }

  const missingPermissions =
    permissions && (!permissions.accessibility || !permissions.input_monitoring)

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
      {disconnectNotif && (
        <div className="error-bar" style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center' }}>
          <span>{disconnectNotif}</span>
          <button onClick={() => setDisconnectNotif(null)} style={{ background: 'none', border: 'none', cursor: 'pointer', fontWeight: 'bold', fontSize: '1rem' }}>×</button>
        </div>
      )}
      {missingPermissions && (
        <section className="permission-banner">
          <div>
            <strong>Permissions needed</strong>
            <p>
              macOS permissions are still missing for input control or capture.
              Open System Settings to finish setup.
            </p>
          </div>
          <button onClick={openPermissions} className="btn-primary">
            Open Settings
          </button>
        </section>
      )}

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
              {(status?.state === 'controlling' || status?.state === 'controlled-by') && (
                <div className="active-control-banner">
                  {status.state === 'controlling' ? (
                    <span>Controlling <strong>{status.active_peer_id}</strong> — local input goes to remote only</span>
                  ) : (
                    <span>Controlled by <strong>{status.active_peer_id}</strong></span>
                  )}
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
                  {config?.peers.map(peer => {
                    const isConnected = status?.connected_peer_ids.includes(peer.id) ?? false
                    const isControlling = status?.state === 'controlling' && status.active_peer_id === peer.id
                    return (
                      <li key={peer.id} className="trusted-item">
                        <div className="peer-info">
                          <span className="peer-name">{peer.name}</span>
                          <span className="peer-id">{peer.id}</span>
                          <span className={`peer-conn-badge ${isConnected ? 'connected' : 'offline'}`}>
                            {isConnected ? 'Connected' : 'Offline'}
                          </span>
                        </div>
                        <div className="peer-actions">
                          {isControlling ? (
                            <button onClick={releaseControl} className="btn-small btn-error">Release</button>
                          ) : (
                            <button
                              onClick={() => switchToPeer(peer.id)}
                              className="btn-small btn-primary"
                              disabled={!isConnected}
                              title={isConnected ? `Control ${peer.name}` : 'Peer is offline'}
                            >Control</button>
                          )}
                          <button onClick={() => removePeer(peer.id)} className="btn-text">Remove</button>
                        </div>
                      </li>
                    )
                  })}

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

              <section className="diagnostics-section">
                <div className="section-header">
                  <h2>Input Debug</h2>
                  <button onClick={() => setInputDebugEvents([])} className="btn-small">Clear</button>
                </div>
                <div className="debug-feed">
                  {inputDebugEvents.length === 0 ? (
                    <div className="empty">No keyboard debug events seen in this GUI session.</div>
                  ) : (
                    inputDebugEvents.map((item, i) => (
                      <div key={`${item.timestamp_ms}-${i}`} className="debug-line">
                        <span className="debug-kind">{item.kind}</span>
                        <span className="debug-detail">{item.detail}</span>
                      </div>
                    ))
                  )}
                </div>
              </section>

              <section className="settings-section">
                <h2>Settings</h2>
                <div className="settings-card">
                  <div className="setting-row">
                    <div>
                      <span className="setting-label">Launch at login</span>
                      <p>Start the manager automatically when you sign in.</p>
                    </div>
                    <button
                      onClick={toggleAutostart}
                      className={`toggle-button ${autostartEnabled ? 'enabled' : 'disabled'}`}
                    >
                      {autostartEnabled ? 'On' : 'Off'}
                    </button>
                  </div>
                  <div className="setting-row">
                    <div>
                      <span className="setting-label">Remote control mode</span>
                      <p>Allow trusted peers to take control without a local prompt.</p>
                    </div>
                    <button
                      onClick={toggleRemoteControl}
                      className={`toggle-button ${config?.node.accept_remote_control ? 'enabled' : 'disabled'}`}
                    >
                      {config?.node.accept_remote_control ? 'On' : 'Off'}
                    </button>
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
