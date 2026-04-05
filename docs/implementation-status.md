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

## Partial

- daemon runtime state management and reconnect recovery
- disconnect cleanup
- reconnect/session resume policy
- native installers and code signing
- SwitchRequest/SwitchRelease protocol messages (defined but not sent or handled)

## Not Started

- manual advertised address override for `pair init`
- Windows UIPI elevation or manifest for input injection
- graceful degradation when rdev/enigo unavailable in non-interactive sessions

## Verified

- `cargo build` (macOS and Windows)
- `cargo test` (14 tests pass)
- cross-platform pairing flow (macOS <-> Windows via Tailscale)
- authenticated TCP session establishment (macOS <-> Windows)
- input event capture, serialization, and remote delivery
- see [cross-platform-test-report.md](./cross-platform-test-report.md) for full results

## Known Issues

1. Windows input injection blocked by UIPI when daemon runs outside interactive desktop session
2. SwitchRequest/SwitchRelease not sent to remote peer on state change
3. `pair init` advertised address may be unreachable across subnets
4. Windows firewall blocks port 48571 by default
5. macOS requires manual Accessibility permission grant for input injection
6. Duplicate diagnostic notes accumulate on session reconnect

## Recommended Next Steps

1. Implement SwitchRequest/SwitchRelease send and receive handling
2. Document Windows UIPI requirement and firewall setup
3. Add advertised address override for cross-subnet pairing
4. Add native installers and platform-specific UX polish
