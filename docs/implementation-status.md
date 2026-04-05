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

## Partial

- daemon runtime state management and reconnect recovery
- disconnect cleanup
- reconnect/session resume policy
- native installers and code signing

## Not Started

## Verified

- `cargo build`
- `cargo test`

## Recommended Next Step

Add native installers and platform-specific UX polish beyond the new install, bundle, and release automation helpers.
