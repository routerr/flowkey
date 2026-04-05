# Implementation Status

## Completed

- Rust workspace scaffold
- CLI skeleton
- persistent config loading and saving
- Ed25519 node identity generation
- signed pairing token flow
- mutual authenticated TCP handshake
- heartbeat-driven session pump
- daemon listener and outbound connection runtime
- platform sink selection scaffold
- native macOS and Windows input injection backends
- hotkey parsing and normalization helpers
- local capture listener scaffolding for macOS and Windows
- daemon controller-state transitions and active peer selection
- peer-to-peer forwarding of captured local events
- hotkey activation-chord suppression
- capture suppression for self-injected events
- platform-level suppression of self-injected events
- CLI status reporting from daemon runtime snapshots
- local `flky switch` and `flky release` control requests through a file-backed command channel
- packaging scripts for install, release bundles, and CI artifact uploads
- cross-platform cursor/key normalization
- runtime diagnostics for local capture, injection backend, and permission hints
- platform permission probes and OS-specific readiness diagnostics
- session resilience: injection failure no longer crashes the TCP session
- pairing address override via `node.advertised_addr` and `flky pair init --advertised-addr`
- remote SwitchRequest propagation now transitions the target peer into `controlled-by`
- local capture now emits mouse movement after initializing the first cursor sample
- **Exclusive Capture (macOS)** using `CGEventTap` for local input suppression
- **Automatic Control Resume**: Role persistence across reconnects
- **Reachability Probing (FRP)**: Parallel UDP racing for multi-IP zero-config switching
- **`flky doctor`**: System diagnostic tool for permissions and network

## Partial

- Windows exclusive capture (currently falls back to passive)
- native installers and code signing
- graceful degradation when rdev/enigo are unavailable in non-interactive sessions

## Not Started

- Windows UIPI elevation or manifest for input injection

## Verified

- `cargo build` (macOS and Windows)
- `cargo test` (current workspace passes; see crate-specific verification below)
- `cargo test -p flowkey-input` (15 tests pass, including mouse-move capture regression coverage)
- cross-platform pairing flow (macOS <-> Windows via Tailscale)
- authenticated TCP session establishment (macOS <-> Windows)
- input event capture, serialization, remote delivery, and real mouse movement forwarding
- local and remote SwitchRequest/SwitchRelease state propagation in code path and interactive validation
- see [cross-platform-test-report.md](./cross-platform-test-report.md) for full results

## Known Issues

1. Windows input injection blocked by UIPI when daemon runs outside interactive desktop session
2. `pair init` advertised address may still be unreachable across subnets unless the user sets `node.advertised_addr` or passes `--advertised-addr`
3. Windows firewall blocks port 48571 by default
4. macOS requires manual Accessibility permission grant for input injection

## Recommended Next Steps

1. Run a short manual regression sweep: move, click, drag, wheel, type, hotkey switch, release
2. Document Windows UIPI requirement and firewall setup with exact operator steps
3. Add native installers and platform-specific UX polish
4. Consider privilege/session detection before starting capture hooks on Windows
