# Cross-Platform Test Report

## Date: 2026-04-05

## Environment

| Machine | OS | IP (LAN) | IP (Tailscale) | Node ID |
|---------|-----|----------|-----------------|---------|
| MacBook | macOS (Darwin 25.4.0) | 192.168.50.104 | 100.79.183.18 | local-node-rty6vaqv |
| Desktop | Windows (DESKTOP-X96F117) | 192.168.1.102 | 100.109.163.62 | desktop-x96f117-byfwtfh7 |

Connection: Tailscale VPN (LAN IPs were on different subnets and not mutually routable).

## Test Results

### 1. Build

| Platform | Result |
|----------|--------|
| macOS (`cargo build`) | PASS |
| Windows (`cargo build`) | PASS |

### 2. Pairing (Ed25519 Signed Tokens)

| Step | Result |
|------|--------|
| `flky pair init` on macOS | PASS - token generated with correct advertised addr |
| `flky pair accept <token>` on Windows | PASS - peer trusted and persisted |
| `flky pair init` on Windows | PASS |
| `flky pair accept <token>` on macOS | PASS - mutual trust established |
| Signature verification | PASS - invalid tokens rejected |

### 3. Daemon Startup

| Platform | Result | Notes |
|----------|--------|-------|
| macOS `flky daemon` | PASS | Binds 0.0.0.0:48571, mDNS advertises, hotkey capture enabled |
| Windows `flky daemon` (interactive) | PASS | Same as above |
| Windows `flky daemon` (via `Start-Process`) | FAIL | Process exits silently after startup; likely rdev capture init fails in non-interactive session |

### 4. Outbound Peer Connection

| Behavior | Result | Notes |
|----------|--------|-------|
| ID-based outbound selection | PASS | Only the node with `peer.id > node.id` initiates outbound |
| TCP connect + Hello/HelloAck | PASS | |
| AuthChallenge/AuthResponse handshake | PASS | Ed25519 mutual auth verified |
| Session established | PASS | `ESTABLISHED` TCP connection confirmed via lsof |
| Heartbeat keepalive | PASS | Session stays healthy indefinitely |
| Reconnect on disconnect | PASS | Auto-reconnects with backoff (1, 2, 4, 8s) |

### 5. Network Connectivity

| Path | Result | Notes |
|------|--------|-------|
| macOS -> Windows (192.168.1.102) | PASS | ping OK, TCP OK after firewall rule added |
| Windows -> macOS (192.168.50.104) | FAIL | Different subnet, no route |
| macOS -> Windows (100.109.163.62 Tailscale) | PASS | |
| Windows -> macOS (100.79.183.18 Tailscale) | PASS | |

### 6. Control Switching

| Step | Result | Notes |
|------|--------|-------|
| `flky switch <peer-id>` | PASS | Local state transitions to `controlling` |
| `flky status` shows `controlling` | PASS | |
| `flky release` | PASS | Local state transitions back to `connected-idle` |
| Remote peer state update | PASS (after fix) | Controlled peer now transitions to `controlled-by` once `SwitchRequest` carries the controller node ID instead of the remote peer ID |

### 7. Input Forwarding

| Step | Result | Notes |
|------|--------|-------|
| Local input capture (controller) | PASS (after fix) | rdev captures keyboard, mouse buttons, wheel, and now mouse movement after initializing the first cursor sample correctly |
| Event serialization and TCP send | PASS | `InputEvent` messages sent over wire |
| Remote event receive | PASS | `received input event` logged with correct key/button codes and mouse movement |
| Remote input injection (Windows) | PASS in interactive desktop session | Fails under SSH / non-interactive session because UIPI blocks injection |
| Remote input injection (macOS) | PASS when terminal has Accessibility/Input Monitoring permission | Falls back to logging sink if the launching terminal lacks permission |
| Session survives injection failure | PASS | Injection errors now log warnings and the session remains up |

### 8. Platform Diagnostics

| Check | macOS | Windows |
|-------|-------|---------|
| Input capture | enabled (rdev) | enabled (rdev) |
| Input injection | logging sink (needs Accessibility permission) | native (enigo), but UIPI blocks non-interactive sessions |
| Permission probes | Reports Accessibility needed, Input Monitoring granted | Reports interactive session required |
| mDNS discovery | PASS | PASS |

## Issues Found

### P0 - Critical

1. **Input injection blocked by UIPI on Windows** - `enigo` fails with "not all input events were sent. they may have been blocked by UIPI" when daemon runs via SSH or `Start-Process`. Windows UIPI prevents lower-privilege processes from injecting input into higher-privilege windows. Daemon must run in an interactive desktop session, possibly elevated.

2. **Injection failure crashes session** (FIXED) - `route_input_event` error propagated via `?` in `run_authenticated_session`, causing the entire TCP session to disconnect on first injection failure. Fixed: now logs warning and continues.

3. **Remote switch used the wrong controller peer ID** (FIXED) - `SwitchRequest` was carrying the remote peer ID instead of the controller node ID, so the target daemon rejected the transition to `controlled-by` with `controlled peer must already be authenticated`.

### P1 - High

4. **`pair init` advertised address may be unreachable** - Auto-detected IP address from `pair init` may not be routable from the peer's network. In our test, macOS advertised `192.168.50.104` but Windows could not reach it. The repo now supports `node.advertised_addr` and `flky pair init --advertised-addr <ip:port>`; re-test with one of those paths.

5. **Mouse movement capture emitted no `MouseMove` events** (FIXED) - the first observed `MouseMove` returned before storing the initial cursor position, so all later movement samples were also dropped. Fixed by persisting the position before delta normalization.

### P2 - Medium

6. **Windows daemon crashes when started via `Start-Process`** - Daemon prints startup banner then exits silently. Stderr is empty. Likely related to rdev requiring an interactive session for input hooks. Needs graceful degradation or clearer operator guidance.

7. **Windows firewall blocks daemon port by default** - Port 48571 is not open by default. Requires manual `New-NetFirewallRule` or the daemon/installer should configure this.

8. **macOS Accessibility permission not granted** - Native input injection unavailable; falls back to logging sink. User needs to manually grant Accessibility permission to the terminal or daemon binary.

### P3 - Low

9. **Duplicate diagnostic notes accumulate** (FIXED) - Status output previously repeated identical notes after reconnect. Runtime note insertion is now deduplicated.

## Fix Plan

### Phase 1: Session Resilience (immediate)

1. **[DONE]** Fix injection failure crash - Change `flowkey_net_route_input_event(sink, &event)?` to log-and-continue in `connection.rs:366`.

2. **Re-run a short cross-platform regression pass** - mouse move, click, drag, wheel, typing, hotkey switch, and release from real desktop sessions on both platforms.

### Phase 2: Platform Hardening

4. **Document UIPI requirement** - Add README/docs note that Windows daemon must run in an interactive desktop session. Consider adding a manifest requesting `uiAccess=true` or admin elevation.

5. **Graceful degradation for non-interactive sessions** - Detect when rdev/enigo will fail and log a clear error message instead of silent crash.

6. **Fix duplicate diagnostic notes** - Completed by deduplicating runtime note insertion.

### Phase 3: Network UX

6. **Allow manual advertised address override** - Completed with `node.advertised_addr` and `flky pair init --advertised-addr <ip:port>`.

8. **Auto-detect Tailscale/VPN interfaces** - When multiple interfaces exist, prefer routable addresses or let user choose.

## Verified Environment Notes

- Windows requires firewall rule: `New-NetFirewallRule -DisplayName "flowkey" -Direction Inbound -Protocol TCP -LocalPort 48571 -Action Allow`
- Tailscale provides reliable cross-subnet connectivity for testing
- macOS terminal needs Accessibility permission for input injection
- Windows daemon must run from interactive desktop session (not SSH/Start-Process) for input injection
