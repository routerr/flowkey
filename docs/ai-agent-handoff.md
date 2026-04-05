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

- native installers, code signing, and deeper platform UX polish

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

1. add native installers for macOS and Windows
2. improve platform-specific user experience details

## Suggested Ownership Split

- one worker for macOS input capture/injection
- one worker for Windows input capture/injection
- one worker for daemon/session policy and reconnect handling

## Notes

- The repository already passes `cargo test`.
- The network/auth stack is a solid base; do not discard it.
- The platform sink abstraction is the best place to hook the next real OS-specific work.
- self-injected loopback suppression now shares one filter across capture and injection paths.
