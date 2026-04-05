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
| Remote peer state update | STALE RESULT | This report predates the session-channel/control-message implementation now present in the codebase. Re-run the interactive test to confirm behavior on real machines. |

### 7. Input Forwarding

| Step | Result | Notes |
|------|--------|-------|
| Local input capture (macOS) | PASS | rdev captures keyboard/mouse events |
| Event serialization and TCP send | PASS | `InputEvent` messages sent over wire |
| Remote event receive (Windows) | PASS | `received input event` logged with correct key code |
| Remote input injection (Windows) | FAIL | `enigo` blocked by UIPI: "not all input events were sent. they may have been blocked by UIPI" |
| Session survives injection failure | FAIL (fixed) | Injection error propagated via `?` and killed session; fixed to log warning and continue |

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

### P1 - High

3. **`pair init` advertised address may be unreachable** - Auto-detected IP address from `pair init` may not be routable from the peer's network. In our test, macOS advertised `192.168.50.104` but Windows could not reach it. The repo now supports `node.advertised_addr` and `flky pair init --advertised-addr <ip:port>`; re-test with one of those paths.

### P2 - Medium

4. **Windows daemon crashes when started via `Start-Process`** - Daemon prints startup banner then exits silently. Stderr is empty. Likely related to rdev requiring an interactive session for input hooks. Needs graceful degradation or clearer operator guidance.

5. **Windows firewall blocks daemon port by default** - Port 48571 is not open by default. Requires manual `New-NetFirewallRule` or the daemon/installer should configure this.

6. **macOS Accessibility permission not granted** - Native input injection unavailable; falls back to logging sink. User needs to manually grant Accessibility permission to the terminal or daemon binary.

### P3 - Low

7. **Duplicate diagnostic notes accumulate** (FIXED) - Status output previously repeated identical notes after reconnect. Runtime note insertion is now deduplicated.

## Fix Plan

### Phase 1: Session Resilience (immediate)

1. **[DONE]** Fix injection failure crash - Change `flowkey_net_route_input_event(sink, &event)?` to log-and-continue in `connection.rs:366`.

2. **Re-run interactive switch-state validation** - The current codebase now contains session-channel control messages and daemon callbacks for remote switch/release. Verify the real-machine behavior and replace this stale section with fresh results.

### Phase 2: Platform Hardening

4. **Document UIPI requirement** - Add README/docs note that Windows daemon must run in an interactive desktop session. Consider adding a manifest requesting `uiAccess=true` or admin elevation.

5. **Graceful degradation for non-interactive sessions** - Detect when rdev/enigo will fail and log a clear error message instead of silent crash.

5. **Fix duplicate diagnostic notes** - Completed by deduplicating runtime note insertion.

### Phase 3: Network UX

6. **Allow manual advertised address override** - Completed with `node.advertised_addr` and `flky pair init --advertised-addr <ip:port>`.

8. **Auto-detect Tailscale/VPN interfaces** - When multiple interfaces exist, prefer routable addresses or let user choose.

## Verified Environment Notes

- Windows requires firewall rule: `New-NetFirewallRule -DisplayName "flowkey" -Direction Inbound -Protocol TCP -LocalPort 48571 -Action Allow`
- Tailscale provides reliable cross-subnet connectivity for testing
- macOS terminal needs Accessibility permission for input injection
- Windows daemon must run from interactive desktop session (not SSH/Start-Process) for input injection
