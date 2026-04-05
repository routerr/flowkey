# AI Agent Handoff

## Purpose

This file is for the next AI agent that continues `flowkey`.

## What Already Works

- persistent node identities with real Ed25519 keypairs
- signed pairing token generation and verification
- authenticated TCP handshake
- heartbeat-based session loop
- daemon listener and outbound session bootstrap
- runtime tracking for authenticated sessions
- native macOS and Windows input injection backends
- hotkey parsing, local capture scaffolding, and active-peer forwarding
- hotkey activation-chord suppression

## What Is Still Missing

- Windows UIPI elevation: daemon must run in interactive desktop session for enigo injection
- Windows firewall rule automation (port 48571 blocked by default)
- macOS Accessibility permission guided setup
- graceful degradation when rdev/enigo unavailable in non-interactive sessions
- native installers, code signing, and deeper platform UX polish

Recently completed:

- SwitchRequest/SwitchRelease send and receive are wired through the session channel and daemon callbacks
- pairing now supports a persistent `node.advertised_addr` config override plus `flky pair init --advertised-addr <ip:port>` for one-off tokens
- runtime notes are deduplicated so reconnects no longer spam identical diagnostics
- remote switch propagation now uses the controller node ID in `SwitchRequest`, so the controlled peer can transition into `controlled-by`
- mouse movement capture now initializes `last_mouse_position` correctly, so `MouseMove` events are emitted after the first observed pointer sample

## Cross-Platform Test Results (2026-04-05)

First real macOS-to-Windows test via Tailscale. See [cross-platform-test-report.md](./cross-platform-test-report.md) for details.

Key findings:
- Pairing, auth, session, heartbeat, reconnect all work
- Input events successfully captured, serialized, and delivered cross-platform
- Windows injection blocked by UIPI (daemon was started via SSH, not interactive desktop)
- Injection failure previously crashed the session (fixed: now logs warning and continues)
- `pair init` auto-detected IP may not be routable; Tailscale IPs were used as workaround
- Two real-machine bugs were fixed during the interactive validation loop:
  - `SwitchRequest` previously advertised the remote peer ID instead of the controller node ID, so remote state stayed `connected-idle`
  - local capture dropped all mouse-move events because the first `MouseMove` did not persist the initial cursor position before returning `None`

## Important Files

- [crates/flowkey-daemon/src/bootstrap.rs](/Users/raychang/repo/flowkey/crates/flowkey-daemon/src/bootstrap.rs)
- [crates/flowkey-core/src/daemon.rs](/Users/raychang/repo/flowkey/crates/flowkey-core/src/daemon.rs)
- [crates/flowkey-core/src/session.rs](/Users/raychang/repo/flowkey/crates/flowkey-core/src/session.rs)
- [crates/flowkey-crypto/src/handshake.rs](/Users/raychang/repo/flowkey/crates/flowkey-crypto/src/handshake.rs)
- [crates/flowkey-net/src/connection.rs](/Users/raychang/repo/flowkey/crates/flowkey-net/src/connection.rs)
- [crates/flowkey-net/src/frame.rs](/Users/raychang/repo/flowkey/crates/flowkey-net/src/frame.rs)
- [crates/flowkey-input/src/lib.rs](/Users/raychang/repo/flowkey/crates/flowkey-input/src/lib.rs)
- [crates/flowkey-platform-macos/src/inject.rs](/Users/raychang/repo/flowkey/crates/flowkey-platform-macos/src/inject.rs)
- [crates/flowkey-platform-windows/src/inject.rs](/Users/raychang/repo/flowkey/crates/flowkey-platform-windows/src/inject.rs)

## Working Rules

- keep the protocol crate stable unless there is a strong reason to change it
- preserve the signed pairing model
- release pressed input state on disconnect or session termination
- prefer small vertical slices over broad refactors
- do not rework the daemon bootstrap unless the change affects session handling

## Best Next Implementation Slice

1. re-run the cross-platform validation from an interactive Windows desktop session
2. resolve Windows UIPI injection limits with stronger operator guidance, manifest work, or elevation strategy
3. add native installers for macOS and Windows
4. improve platform-specific user experience details

## Suggested Ownership Split

- one worker for macOS input capture/injection
- one worker for Windows input capture/injection
- one worker for daemon/session policy and reconnect handling

## Notes

- The repository passes `cargo build`; keep `cargo test` current and update this note when the suite changes.
- The network/auth stack is a solid base; do not discard it.
- The platform sink abstraction is the best place to hook the next real OS-specific work.
- self-injected loopback suppression now shares one filter across capture and injection paths.
- Windows daemon must run from an interactive desktop session (not SSH, not `Start-Process`) for input injection to work. UIPI blocks injection from lower-privilege processes.
- `pair init` auto-detects the listen interface IP by default, but this may be wrong when machines are on different subnets. Tailscale IPs (100.x.x.x) work reliably, and you can now override the published address explicitly.
- If clicks work but the pointer does not move, check whether `MouseMove` lines are present in the controller daemon log before blaming platform injection.
