import { useState, useEffect, useRef } from 'react'
import { invoke, event } from '@tauri-apps/api'
import { appWindow, LogicalSize } from '@tauri-apps/api/window'
import { enable, disable, isEnabled } from 'tauri-plugin-autostart-api'
import {
  type DiscoveredPeer,
  type Config,
  type DaemonStatus,
  type InputDebugEvent,
  type PendingPairingView,
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

function statusBadgeClass(state: string, healthy: boolean) {
  const c = statusColor(state, healthy)
  return `status-badge--${c}`
}

function peerInitial(name: string) {
  return name.charAt(0).toUpperCase()
}

/* ─── Resize helper ─── */
async function resizeWindow(width: number, height: number) {
  try {
    await appWindow.setSize(new LogicalSize(width, height))
  } catch (e) {
    console.error('Failed to resize window:', e)
  }
}

type Screen = 'home' | 'pairing' | 'diagnostics'

/* ─── App ─── */
function App() {
  const [config, setConfig] = useState<Config | null>(null)
  const [status, setStatus] = useState<DaemonStatus | null>(null)
  const [permissions, setPermissions] = useState<PermissionStatus | null>(null)
  const [autostartEnabled, setAutostartEnabled] = useState<boolean | null>(null)
  const [discoveredPeers, setDiscoveredPeers] = useState<DiscoveredPeer[]>([])
  const [pairingSas, setPairingSas] = useState<string | null>(null)
  const [pairingPeerName, setPairingPeerName] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [disconnectNotif, setDisconnectNotif] = useState<string | null>(null)
  const [isPairing, setIsPairing] = useState(false)
  const [inputDebugEvents, setInputDebugEvents] = useState<InputDebugEvent[]>([])
  const prevStateRef = useRef<string | null>(null)
  const userReleasedRef = useRef(false)
  const controlStartedAtRef = useRef<number | null>(null)

  // Sizing and Screen state
  const [screen, setScreen] = useState<Screen>('home')

  const transitionToScreen = async (nextScreen: Screen) => {
    setScreen(nextScreen)
    if (nextScreen === 'home') {
      await resizeWindow(360, 300)
    } else if (nextScreen === 'pairing') {
      await resizeWindow(380, 420)
    } else if (nextScreen === 'diagnostics') {
      await resizeWindow(680, 580)
    }
  }

  // Load config on mount
  useEffect(() => {
    loadConfig()
    loadPermissionStatus()
    loadAutostartStatus()
    // Default to widget size on load
    resizeWindow(360, 300)
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

  // Sync isPairing state to navigate to pairing screen and resize
  useEffect(() => {
    if (isPairing) {
      transitionToScreen('pairing')
    } else if (screen === 'pairing') {
      transitionToScreen('home')
    }
  }, [isPairing])

  // While controlling a remote peer, swallow keyboard events at the window level
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

  // Poll for discovery + incoming pairing proposals
  useEffect(() => {
    const interval = setInterval(async () => {
      try {
        const peers = await invoke<DiscoveredPeer[]>('get_discovered_peers')
        setDiscoveredPeers(peers)

        const pending = await invoke<PendingPairingView | null>('get_pending_pairing')
        if (pending) {
          setPairingSas(pending.sas_code)
          setPairingPeerName(pending.peer_name)
          setIsPairing(true)
        }
      } catch (e) {
        console.error('Discovery/pairing poll error:', e)
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
      setPairingSas(sas || null)
      setPairingPeerName(null)
    } catch (e) {
      setError(String(e))
      setIsPairing(false)
    }
  }

  async function connectToPeer(peer: DiscoveredPeer) {
    if (!peer.pairing_port || peer.addrs.length === 0) {
      setError('Peer has no reachable pairing address')
      return
    }

    const ip = peer.addrs[0].split(':')[0]
    const addr = `${ip}:${peer.pairing_port}`

    setIsPairing(true)
    setError(null)
    try {
      const sas = await invoke<string>('connect_to_peer', { peerAddr: addr })
      setPairingSas(sas)
      setPairingPeerName(peer.name)
    } catch (e) {
      setError(String(e))
      setIsPairing(false)
    }
  }

  async function confirmPairing() {
    try {
      await invoke('confirm_pairing')
      setPairingSas(null)
      setPairingPeerName(null)
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
      setPairingPeerName(null)
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

  const availablePeers = config?.peers.filter(peer => status?.connected_peer_ids.includes(peer.id)) || []

  return (
    <div className={`app-shell app-shell--${screen}`}>
      {/* ─── Header ─── */}
      <header className="app-header">
        <div className="app-header-left">
          <div className="app-logo">
            <svg width="18" height="18" fill="none" viewBox="0 0 48 48" className="app-logo-icon">
              <rect width="48" height="48" rx="10" fill="url(#g)" />
              <path d="M 15,9 L 35.5,30 L 25,30 L 29.5,41.5 L 25,43 L 20.5,32 L 15,37 Z" fill="white" />
              <defs>
                <linearGradient id="g" x1="0" y1="0" x2="48" y2="48" gradientUnits="userSpaceOnUse">
                  <stop stop-color="#a78bfa"/>
                  <stop offset="1" stop-color="#6366f1"/>
                </linearGradient>
              </defs>
            </svg>
            <span className="app-logo-text">flow<span>key</span></span>
          </div>
          {screen === 'home' && config && (
            <div className="node-badge-mini" title={`Your node name: ${config.node.name}`}>
              {config.node.name}
            </div>
          )}
        </div>
        
        <div className="app-header-right">
          {screen === 'home' && (
            <>
              {status && (status.state === 'controlling' || status.state === 'controlled-by') && (
                <span className={`status-badge-mini ${statusBadgeClass(status.state, status.session_healthy)}`} />
              )}
              <button
                className="header-icon-btn"
                onClick={startPairingMode}
                disabled={isPairing}
                title="Pair new device"
              >
                ＋
              </button>
              <button
                className="header-icon-btn"
                onClick={() => transitionToScreen('diagnostics')}
                title="Diagnostics & Settings"
              >
                ⚙️
              </button>
            </>
          )}
          
          {screen !== 'home' && (
            <button
              className="btn btn-secondary btn-sm"
              onClick={() => {
                if (screen === 'pairing') {
                  cancelPairing()
                } else {
                  transitionToScreen('home')
                }
              }}
            >
              ← Back
            </button>
          )}
        </div>
      </header>

      {/* ─── Main Content ─── */}
      
      {/* ─── Home Screen View ─── */}
      {screen === 'home' && (
        <div className="screen-home-content">
          {error && (
            <div className="error-banner-mini">
              <span>{error}</span>
              <button onClick={() => setError(null)}>✕</button>
            </div>
          )}
          
          {disconnectNotif && (
            <div className="info-banner-mini">
              <span>{disconnectNotif}</span>
              <button onClick={() => setDisconnectNotif(null)}>✕</button>
            </div>
          )}

          {missingPermissions && (
            <div className="permission-warning-mini" onClick={openPermissions}>
              ⚠️ System permissions missing. Click to open settings.
            </div>
          )}

          {/* Connected state */}
          {status && (status.state === 'controlling' || status.state === 'controlled-by') ? (
            <div className="widget-connected-card">
              <div className="widget-conn-info">
                <span className="widget-conn-icon">
                  {status.state === 'controlling' ? '⌨' : '🖥'}
                </span>
                <div className="widget-conn-text">
                  <div className="widget-conn-title">
                    {status.state === 'controlling' ? 'Controlling remote peer' : 'Controlled by remote peer'}
                  </div>
                  <div className="widget-conn-peer">
                    {status.active_peer_id}
                  </div>
                </div>
              </div>
              <button onClick={releaseControl} className="btn btn-danger btn-block">
                Disconnect
              </button>
            </div>
          ) : (
            /* Not Connected State */
            <div className="widget-peers-list-container">
              <div className="widget-section-header">
                Available Paired Peers
              </div>
              
              {availablePeers.length === 0 ? (
                <div className="widget-empty-state">
                  <div className="widget-empty-icon">🔌</div>
                  {config?.peers && config.peers.length > 0 ? (
                    <>
                      <strong>All paired devices offline</strong>
                      <span>Turn on flowkey on other devices to connect.</span>
                    </>
                  ) : (
                    <>
                      <strong>No paired devices yet</strong>
                      <span>Click the '＋' button above to pair your first device.</span>
                    </>
                  )}
                </div>
              ) : (
                <ul className="widget-peer-list">
                  {availablePeers.map((peer) => (
                    <li key={peer.id} className="widget-peer-item">
                      <div className="widget-peer-details">
                        <div className="widget-peer-avatar">
                          {peerInitial(peer.name)}
                        </div>
                        <span className="widget-peer-name" title={peer.name}>{peer.name}</span>
                      </div>
                      <button
                        onClick={() => switchToPeer(peer.id)}
                        className="btn btn-primary btn-sm"
                      >
                        Connect
                      </button>
                    </li>
                  ))}
                </ul>
              )}
            </div>
          )}
        </div>
      )}

      {/* ─── Pairing Screen View ─── */}
      {screen === 'pairing' && (
        <div className="screen-pairing-content">
          <div className="pairing-widget-card">
            <div className="pairing-icon-small">🔗</div>
            <h3>Device Pairing</h3>
            <p className="pairing-desc">Setup secure pairing with a remote computer.</p>
            
            {pairingSas ? (
              <div className="sas-widget-display">
                {pairingPeerName && <div className="sas-peer-name">Pairing with {pairingPeerName}</div>}
                <div className="sas-code-small">{pairingSas}</div>
                <p className="sas-instruction">Verify this 6-digit code on both machines.</p>
                <div className="sas-widget-actions">
                  <button onClick={confirmPairing} className="btn btn-success btn-sm">
                    ✓ Confirm
                  </button>
                  <button onClick={cancelPairing} className="btn btn-secondary btn-sm">
                    Cancel
                  </button>
                </div>
              </div>
            ) : (
              <div className="pairing-waiting-widget">
                <div className="pairing-spinner-small" />
                <span className="pairing-status-text">Discoverable as "{config?.node.name}"</span>
                
                {/* Fallback connection for discovered but not paired peers */}
                {discoveredPeers.filter(p => !config?.peers.some(kp => kp.id === p.id)).length > 0 && (
                  <div className="discovered-fallback-container">
                    <div className="fallback-header">Discovered on LAN:</div>
                    <ul className="fallback-list">
                      {discoveredPeers
                        .filter(p => !config?.peers.some(kp => kp.id === p.id))
                        .map(peer => (
                          <li key={peer.id} className="fallback-item">
                            <span className="fallback-name">{peer.name}</span>
                            <button
                              onClick={() => connectToPeer(peer)}
                              className="btn btn-primary btn-sm btn-xs"
                            >
                              Pair
                            </button>
                          </li>
                        ))}
                    </ul>
                  </div>
                )}
                
                <button onClick={cancelPairing} className="btn btn-secondary btn-sm">
                  Cancel
                </button>
              </div>
            )}
          </div>
        </div>
      )}

      {/* ─── Diagnostics Screen View ─── */}
      {screen === 'diagnostics' && (
        <div className="screen-diagnostics-content">
          <div className="diag-header-row">
            <h3>Diagnostics & Settings</h3>
          </div>
          
          <div className="diag-panels-grid">
            {/* Left Panel: Logs & Diagnostic Status */}
            <div className="diag-panel-left">
              <div className="diag-subcard">
                <h4>System Status</h4>
                <div className="diag-mini-grid">
                  <div className="diag-mini-cell">
                    <span className="lbl">Capture</span>
                    <span className={`val ${status?.local_capture_enabled ? 'active' : 'inactive'}`}>
                      {status?.local_capture_enabled ? 'Active' : 'Inactive'}
                    </span>
                  </div>
                  <div className="diag-mini-cell">
                    <span className="lbl">Backend</span>
                    <span className="val">{status?.input_injection_backend || '—'}</span>
                  </div>
                </div>
                {status && status.notes.length > 0 && (
                  <div className="diag-notes-box">
                    {status.notes.map((note, i) => (
                      <div key={i} className="diag-note-item">{note}</div>
                    ))}
                  </div>
                )}
              </div>

              <div className="diag-subcard debug-subcard">
                <div className="debug-header">
                  <h4>Keyboard Events Feed</h4>
                  <button
                    className="btn btn-ghost btn-sm btn-xs"
                    onClick={() => setInputDebugEvents([])}
                  >
                    Clear
                  </button>
                </div>
                <div className="debug-feed-box">
                  {inputDebugEvents.length === 0 ? (
                    <div className="debug-empty-small">No logs recorded yet. Use keys to test capture.</div>
                  ) : (
                    <div className="debug-feed-list">
                      {inputDebugEvents.map((item, i) => (
                        <div key={`${item.timestamp_ms}-${i}`} className="debug-row">
                          <span className="kind">{item.kind}</span>
                          <span className="detail">{item.detail}</span>
                        </div>
                      ))}
                    </div>
                  )}
                </div>
              </div>
            </div>

            {/* Right Panel: Settings & Trusted Peers list */}
            <div className="diag-panel-right">
              <div className="diag-subcard">
                <h4>Preferences</h4>
                <div className="preferences-list">
                  <div className="pref-row">
                    <div className="pref-info">
                      <span className="pref-lbl">Launch at login</span>
                      <span className="pref-desc">Auto-start flowkey at sign in</span>
                    </div>
                    <button
                      onClick={toggleAutostart}
                      className={`toggle toggle-sm ${autostartEnabled ? 'active' : ''}`}
                    >
                      <span className="toggle-knob" />
                    </button>
                  </div>
                  <div className="pref-row">
                    <div className="pref-info">
                      <span className="pref-lbl">Remote control</span>
                      <span className="pref-desc">Allow peers to switch without prompt</span>
                    </div>
                    <button
                      onClick={toggleRemoteControl}
                      className={`toggle toggle-sm ${config?.node.accept_remote_control ? 'active' : ''}`}
                    >
                      <span className="toggle-knob" />
                    </button>
                  </div>
                </div>
              </div>

              <div className="diag-subcard peers-subcard">
                <h4>Paired Devices ({config?.peers.length || 0})</h4>
                {config?.peers && config.peers.length > 0 ? (
                  <ul className="paired-devices-list">
                    {config.peers.map((peer) => {
                      const isOnline = status?.connected_peer_ids.includes(peer.id) ?? false
                      return (
                        <li key={peer.id} className="paired-device-item">
                          <div className="paired-device-details">
                            <span className="paired-name">{peer.name}</span>
                            <span className="paired-id">{peer.id.slice(0, 8)}…</span>
                          </div>
                          <div className="paired-actions">
                            <span className={`online-indicator ${isOnline ? 'online' : 'offline'}`} />
                            <button
                              onClick={() => removePeer(peer.id)}
                              className="btn-remove"
                              title="Unpair device"
                            >
                              ✕
                            </button>
                          </div>
                        </li>
                      )
                    })}
                  </ul>
                ) : (
                  <div className="debug-empty-small">No paired devices.</div>
                )}
              </div>
            </div>
          </div>
        </div>
      )}
    </div>
  )
}

export default App
