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

/* ─── Status helpers ─── */
function statusColor(state: string, healthy: boolean) {
  if (state === 'controlling') return 'controlling'
  if (state === 'controlled-by') return 'controlled'
  return healthy ? 'healthy' : 'unhealthy'
}

function statusDotClass(state: string, healthy: boolean) {
  const c = statusColor(state, healthy)
  return `status-dot--${c}`
}

function statusBadgeClass(state: string, healthy: boolean) {
  const c = statusColor(state, healthy)
  return `status-badge--${c}`
}

function peerInitial(name: string) {
  return name.charAt(0).toUpperCase()
}

/* ─── App ─── */
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
    <div className="app-shell">
      {/* ─── Top Navigation Bar ─── */}
      <header className="top-bar">
        <div className="top-bar-left">
          <div className="app-logo">
            <div className="app-logo-icon">⌨</div>
            <span className="app-logo-text">flow<span>key</span></span>
          </div>
          {config && (
            <div className="node-badge">
              <span className="node-badge-dot" />
              {config.node.name}
            </div>
          )}
        </div>
        <div className="top-bar-right">
          {status && (
            <span className={`status-badge ${statusBadgeClass(status.state, status.session_healthy)}`}>
              <span className={`status-dot ${statusDotClass(status.state, status.session_healthy)}`} />
              {status.state === 'idle' ? 'Standby' : status.state}
            </span>
          )}
          <button
            className="btn btn-secondary btn-sm"
            onClick={startPairingMode}
            disabled={isPairing}
          >
            ＋ Pair Device
          </button>
        </div>
      </header>

      {/* ─── Notifications ─── */}
      {error && (
        <div className="notification-bar notification-bar--error">
          <span>{error}</span>
          <button className="btn-close" onClick={() => setError(null)}>✕</button>
        </div>
      )}

      {disconnectNotif && (
        <div className="notification-bar notification-bar--disconnect">
          <span>{disconnectNotif}</span>
          <button className="btn-close" onClick={() => setDisconnectNotif(null)}>✕</button>
        </div>
      )}

      {missingPermissions && (
        <div className="permission-banner">
          <div className="permission-banner-content">
            <strong>🔒 Permissions Required</strong>
            <p>
              macOS permissions are still missing for input control or capture.
              Open System Settings to finish setup.
            </p>
          </div>
          <button onClick={openPermissions} className="btn btn-primary btn-sm">
            Open Settings
          </button>
        </div>
      )}

      {/* ─── Active Control Banner ─── */}
      {(status?.state === 'controlling' || status?.state === 'controlled-by') && (
        <div className={`control-banner control-banner--${status.state === 'controlling' ? 'controlling' : 'controlled'}`}>
          <div className="control-banner-info">
            <span className="control-banner-icon">
              {status.state === 'controlling' ? '⌨' : '🖥'}
            </span>
            <span>
              {status.state === 'controlling'
                ? <>Controlling <strong>{status.active_peer_id}</strong> — all input forwarded remotely</>
                : <>Controlled by <strong>{status.active_peer_id}</strong></>
              }
            </span>
          </div>
          <button onClick={releaseControl} className="btn btn-danger btn-sm">
            Release Control
          </button>
        </div>
      )}

      {/* ─── Main Content ─── */}
      {isPairing ? (
        <div className="pairing-overlay">
          <div className="pairing-card">
            <div className="pairing-icon">🔗</div>
            <h2>Pairing in Progress</h2>
            <p>Securely connecting your devices over LAN</p>

            {pairingSas ? (
              <>
                <div className="sas-display">
                  <div className="sas-label">Verify this code on both machines</div>
                  <div className="sas-code">{pairingSas}</div>
                </div>
                <div className="sas-actions">
                  <button onClick={confirmPairing} className="btn btn-success">
                    ✓ Confirm &amp; Pair
                  </button>
                  <button onClick={cancelPairing} className="btn btn-secondary">
                    Cancel
                  </button>
                </div>
              </>
            ) : (
              <div className="pairing-waiting">
                <div className="pairing-spinner" />
                <p style={{ color: 'var(--text-muted)', fontSize: '0.85rem', marginBottom: 0 }}>
                  Waiting for incoming connection...
                </p>
                <button onClick={cancelPairing} className="btn btn-secondary" style={{ marginTop: 8 }}>
                  Cancel
                </button>
              </div>
            )}
          </div>
        </div>
      ) : (
        <div className="content-grid">
          {/* ─── Main Column ─── */}
          <div className="content-main">
            {/* Discovered Peers */}
            <section className="glass-card">
              <div className="card-header">
                <h2>Discovered Devices</h2>
                <button className="btn btn-ghost btn-sm" onClick={startPairingMode}>
                  Make Discoverable
                </button>
              </div>
              <div className="card-body">
                {discoveredPeers.length === 0 ? (
                  <div className="empty-state">
                    <div className="empty-state-icon">🔍</div>
                    <strong>No devices found</strong>
                    <span>Click "Make Discoverable" on the other computer or check your network</span>
                  </div>
                ) : (
                  <ul className="peer-list">
                    {discoveredPeers.map((peer, i) => (
                      <li key={peer.id} className="peer-item" style={{ animationDelay: `${i * 0.05}s` }}>
                        <div className="peer-item-left">
                          <div className={`peer-avatar ${peer.is_pairing ? 'peer-avatar--pairing' : ''}`}>
                            {peerInitial(peer.name)}
                          </div>
                          <div className="peer-details">
                            <span className="peer-name">{peer.name}</span>
                            <span className="peer-id">{peer.id.slice(0, 16)}…</span>
                          </div>
                        </div>
                        <div className="peer-actions">
                          {peer.is_pairing ? (
                            <button
                              onClick={() => connectToPeer(peer)}
                              className="btn btn-primary btn-sm"
                            >
                              Connect
                            </button>
                          ) : (
                            <span className="peer-status-tag peer-status-tag--connected">
                              <span style={{ width: 5, height: 5, borderRadius: '50%', background: 'var(--green)', display: 'inline-block' }} />
                              Connected
                            </span>
                          )}
                        </div>
                      </li>
                    ))}
                  </ul>
                )}
              </div>
            </section>

            {/* Diagnostics & Input Debug */}
            <section className="glass-card">
              <div className="card-header">
                <h2>Diagnostics</h2>
              </div>
              <div className="card-body">
                <div className="diag-grid">
                  <div className="diag-cell">
                    <span className="diag-label">Input Capture</span>
                    <span className={`diag-value ${status?.local_capture_enabled ? 'diag-value--active' : 'diag-value--inactive'}`}>
                      {status?.local_capture_enabled ? 'Active' : 'Inactive'}
                    </span>
                  </div>
                  <div className="diag-cell">
                    <span className="diag-label">Injection Backend</span>
                    <span className="diag-value">{status?.input_injection_backend || '—'}</span>
                  </div>
                </div>
                {status && status.notes.length > 0 && (
                  <div className="diag-notes">
                    {status.notes.map((note, i) => (
                      <div key={i} className="diag-note">{note}</div>
                    ))}
                  </div>
                )}
              </div>
            </section>

            {/* Input Debug */}
            <section className="glass-card">
              <div className="card-header">
                <h2>Input Debug</h2>
                <button
                  className="btn btn-ghost btn-sm"
                  onClick={() => setInputDebugEvents([])}
                >
                  Clear
                </button>
              </div>
              <div className="card-body" style={{ padding: '8px 10px 10px' }}>
                {inputDebugEvents.length === 0 ? (
                  <div className="debug-empty">No keyboard debug events in this session.</div>
                ) : (
                  <div className="debug-feed">
                    {inputDebugEvents.map((item, i) => (
                      <div key={`${item.timestamp_ms}-${i}`} className="debug-line">
                        <span className="debug-kind">{item.kind}</span>
                        <span className="debug-detail">{item.detail}</span>
                      </div>
                    ))}
                  </div>
                )}
              </div>
            </section>
          </div>

          {/* ─── Side Column ─── */}
          <div className="content-side">
            {/* Trusted Peers */}
            <section className="glass-card">
              <div className="card-header">
                <h2>Trusted Peers</h2>
              </div>
              <div className="card-body" style={{ padding: '10px 12px 12px' }}>
                {config?.peers && config.peers.length > 0 ? (
                  <ul className="trusted-list">
                    {config.peers.map((peer, i) => {
                      const isConnected = status?.connected_peer_ids.includes(peer.id) ?? false
                      const isControlling = status?.state === 'controlling' && status.active_peer_id === peer.id
                      return (
                        <li key={peer.id} className="trusted-item" style={{ animationDelay: `${i * 0.04}s` }}>
                          <div className="trusted-item-left">
                            <div
                              className="trusted-avatar"
                              style={{
                                background: isConnected
                                  ? 'linear-gradient(135deg, var(--accent-dim), var(--blue))'
                                  : 'var(--bg-hover)',
                                opacity: isConnected ? 1 : 0.5,
                              }}
                            >
                              {peerInitial(peer.name)}
                            </div>
                            <div className="trusted-item-info">
                              <span className="trusted-item-name">{peer.name}</span>
                              <span className="trusted-item-id">{peer.id.slice(0, 12)}…</span>
                            </div>
                            <span className={`trusted-item-conn ${isConnected ? 'trusted-item-conn--connected' : 'trusted-item-conn--offline'}`}>
                              {isConnected ? 'Online' : 'Offline'}
                            </span>
                          </div>
                          <div className="trusted-item-actions">
                            {isControlling ? (
                              <button onClick={releaseControl} className="btn btn-danger btn-sm">
                                Release
                              </button>
                            ) : (
                              <button
                                onClick={() => switchToPeer(peer.id)}
                                className="btn btn-primary btn-sm"
                                disabled={!isConnected}
                                title={isConnected ? `Control ${peer.name}` : 'Peer is offline'}
                              >
                                Control
                              </button>
                            )}
                            <button
                              onClick={() => removePeer(peer.id)}
                              className="btn btn-ghost btn-sm"
                              style={{ color: 'var(--text-muted)' }}
                            >
                              ✕
                            </button>
                          </div>
                        </li>
                      )
                    })}
                  </ul>
                ) : (
                  <div className="empty-state" style={{ padding: '20px 12px' }}>
                    <div className="empty-state-icon" style={{ fontSize: '1.5rem' }}>🤝</div>
                    <strong>No trusted peers yet</strong>
                    <span>Pair with another device to get started</span>
                  </div>
                )}
              </div>
            </section>

            {/* Settings */}
            <section className="glass-card">
              <div className="card-header">
                <h2>Settings</h2>
              </div>
              <div className="card-body" style={{ padding: '10px 0' }}>
                <div className="settings-group">
                  <div className="setting-row" style={{ padding: '10px 16px' }}>
                    <div className="setting-label-group">
                      <span className="setting-label">Launch at login</span>
                      <span className="setting-desc">Auto-start when you sign in</span>
                    </div>
                    <button
                      onClick={toggleAutostart}
                      className={`toggle ${autostartEnabled ? 'active' : ''}`}
                      aria-label="Toggle launch at login"
                    >
                      <span className="toggle-knob" />
                    </button>
                  </div>
                  <div className="setting-row" style={{ padding: '10px 16px' }}>
                    <div className="setting-label-group">
                      <span className="setting-label">Remote control mode</span>
                      <span className="setting-desc">Allow trusted peers to control without prompt</span>
                    </div>
                    <button
                      onClick={toggleRemoteControl}
                      className={`toggle ${config?.node.accept_remote_control ? 'active' : ''}`}
                      aria-label="Toggle remote control mode"
                    >
                      <span className="toggle-knob" />
                    </button>
                  </div>
                </div>
              </div>
            </section>

            {/* About / Status Footer */}
            <div style={{
              padding: '8px 4px',
              textAlign: 'center',
              fontSize: '0.72rem',
              color: 'var(--text-muted)',
              letterSpacing: '0.02em',
            }}>
              flowkey &middot; LAN keyboard &amp; mouse sharing
              {config && <span> &middot; v0.1.0</span>}
            </div>
          </div>
        </div>
      )}
    </div>
  )
}

export default App
