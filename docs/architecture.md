# flowkey V1 Architecture

## Scope

This document defines the V1 architecture for a dual-platform keyboard and mouse sharing tool with these constraints:

- Platforms: `macOS` and `Windows`
- Network: `same local network`, including standard Wi-Fi
- UX: `CLI only`
- Switching: `specific keyboard shortcut only`
- Session model: `both devices already logged in`
- Features in V1: `keyboard + mouse only`
- Deferred: clipboard sync, file transfer, screen-edge switching, login-screen control

## Product Goal

Run a small daemon on each machine so one machine can become the current input source and forward its keyboard and mouse events to the paired peer over the LAN with low enough latency for normal desktop use.

## Non-Goals

- Remote control over the public internet
- Secure desktop or login-window control
- Multi-peer routing
- Auto-discovery as a hard dependency
- GUI setup
- Clipboard synchronization
- File transfer

## Design Priorities

1. Low latency over LAN/Wi-Fi
2. Reliable key/button up/down state tracking
3. Clear platform boundaries
4. Simple trust model for V1
5. Recovery from disconnects without stuck modifiers

## Current Implementation State

Implemented now:

- persistent node identity and trusted peer config
- signed pairing token generation and verification
- authenticated TCP handshake
- heartbeat-driven session loop
- daemon listener plus outbound session bootstrap
- platform sink selection scaffold
- native macOS and Windows input injection backends
- hotkey parsing and local capture listener scaffolding
- daemon controller-state transitions and active peer selection
- peer-to-peer event forwarding while in controller mode
- hotkey chord suppression during role switching
- self-injected event loopback suppression while controlling a peer
- CLI status reporting from daemon runtime snapshots
- cross-platform cursor/key normalization for capture and injection paths
- optional LAN discovery advertisement and browsing via mDNS

## High-Level Model

Each machine runs the same Rust binary in daemon mode.

At any moment, a daemon is in one of three logical states:

- `idle`: not actively capturing or forwarding input
- `controller`: captures local input and forwards it to the peer
- `controlled`: receives input from the peer and injects it locally

The active role flips when the local user presses the configured hotkey. In the current implementation, controller mode does not grab or block the controller machine's own local OS behavior; it listens passively and forwards events to the peer while the local machine still reacts normally.

## Exclusive Controller Mode

Goal: support a mode where the controller machine stops reacting locally and only the remote peer receives the input stream.

Implementation:
- **macOS**: Fully implemented using `CGEventTap` (via `rdev::grab`). Local input is suppressed while in `Controlling` mode, while the configured hotkey remains functional for emergency release.
- **Windows**: Fully implemented using low-level keyboard and mouse hooks (`WH_KEYBOARD_LL` and `WH_MOUSE_LL` via `rdev::grab`). Input is intercepted and swallowed when suppression is enabled.

Architecture components:
- `InputCapture` trait with `set_suppression_enabled(bool)`
- Shared `suppression_state` (AtomicBool) managed by the daemon and shared with the capture backend.
- Low-latency event-driven loop using `wait()` instead of polling.
- `HotkeySuppressed` signal: Ensures modifier keys (Ctrl, Alt, Shift, Meta) are correctly released on the local machine when switching control, preventing "stuck" keys.
- **Resilient Cleanup**: Automatic restoration of local input control upon network disconnection or session failure.

## Reachability Probing (FRP)

To handle machines with multiple active interfaces (Ethernet, Wi-Fi, VPN), flowkey uses the **Flowkey Reachability Probe (FRP)** protocol:
1. **Discovery**: mDNS TXT records include all non-loopback IP addresses.
2. **Race**: The client sends UDP probes to all candidate IPs in parallel.
3. **Winner**: The first IP to return a valid UDP pong wins the "race" and is used for the TCP session.

This ensures the fastest and most reliable network path is always chosen without manual configuration.

## Recommended Rust Workspace Layout

```text
flowkey/
├── Cargo.toml
├── crates/
│   ├── flowkey-cli/
│   ├── flowkey-core/
│   ├── flowkey-config/
│   ├── flowkey-crypto/
│   ├── flowkey-net/
│   ├── flowkey-protocol/
│   ├── flowkey-daemon/
│   ├── flowkey-input/
│   ├── flowkey-platform-macos/
│   └── flowkey-platform-windows/
└── docs/
    ├── architecture.md
    ├── roadmap.md
    └── protocol.md
```

## Crate Responsibilities

### `flowkey-cli`

User-facing command handling:

- `pair`
- `daemon`
- `status`
- `switch`
- `list-peers`
- `logs` or log config wiring

This crate should stay thin and delegate to library crates.

### `flowkey-core`

Shared application state and orchestration:

- daemon state machine
- active peer selection
- switch ownership logic
- input/session safety rules
- reconnect handling
- sticky-key recovery

This crate is the heart of the app.

### `flowkey-config`

Configuration and persistence:

- peer definitions
- keybinding config
- local node identity
- trusted public keys
- config file parsing

Use a simple file format such as TOML for V1.

### `flowkey-crypto`

Authentication and encryption helpers:

- node key generation
- key serialization
- challenge/response pairing primitives
- session key derivation

For V1, public-key identity plus encrypted transport is enough.

### `flowkey-net`

Persistent LAN connection management:

- listener/client roles
- framing
- heartbeat
- reconnect
- backpressure handling
- send queue prioritization

Use one long-lived connection per peer.

### `flowkey-protocol`

Wire-level types:

- handshake messages
- control messages
- input event messages
- acknowledgement or keepalive messages
- serialization/deserialization

This crate must remain very stable and well tested.

### `flowkey-daemon`

Daemon bootstrap and process lifecycle:

- config loading
- listener startup
- protocol loop startup
- platform-specific component wiring
- shutdown

### `flowkey-input`

Cross-platform input abstractions:

- shared event enums
- controller/controlled interfaces
- switch-trigger interfaces
- safety helpers for modifier release

This crate should define traits, not force fake cross-platform uniformity.

### `flowkey-platform-macos`

macOS-specific implementation:

- global hotkey registration or event tap filtering
- local input capture
- input injection
- permission checks
- permission diagnostics

### `flowkey-platform-windows`

Windows-specific implementation:

- low-level hooks or raw input capture
- hotkey registration
- input injection
- permission/session diagnostics

## Core Runtime Components

## 1. Daemon State Machine

Suggested internal states:

- `Disconnected`
- `ConnectedIdle`
- `Controlling { peer_id }`
- `ControlledBy { peer_id }`
- `Recovering { intended_role: Option<Role> }`

Important transitions:

- `Disconnected -> ConnectedIdle`
- `ConnectedIdle -> Controlling`
- `ConnectedIdle -> ControlledBy`
- `Controlling -> ConnectedIdle`
- `ControlledBy -> ConnectedIdle`
- `Any -> Recovering -> ConnectedIdle`

`Recovering` tracks the `intended_role` during disconnects. When the peer re-authenticates, the daemon automatically resumes the intended role (e.g., automatically sending a `SwitchRequest` if it was the controller).

## 2. Input Capture Pipeline

When in `controller` mode:

1. capture local hardware events
2. normalize to protocol event types
3. timestamp locally for diagnostics
4. enqueue to the network sender
5. suppress local forwarding only where necessary

V1 should prefer:

- key down/up
- mouse move deltas
- mouse button down/up
- wheel scroll

Avoid higher-level text events as the primary transport abstraction.

## 3. Input Injection Pipeline

When in `controlled` mode:

1. receive framed message
2. validate session and ordering
3. translate protocol event to platform API
4. inject immediately
5. track pressed modifier/button state locally

The receiver should maintain a pressed-state table so it can release all active keys/buttons if the connection drops.

## 4. Hotkey Switching

V1 switching is explicit and local.

Suggested behavior:

- hotkey pressed on local machine toggles into remote-control mode
- same hotkey pressed again returns control locally
- optional separate hotkeys for “switch to peer A” and “release control”

This avoids ambiguous edge behavior and keeps the UX deterministic.

## 5. Pairing and Trust

V1 trust model:

- each node has a persistent identity keypair
- first pairing uses a short pairing code or explicit trust command
- after pairing, peers trust each other by stored public key

Current implementation detail:

- pairing tokens are signed with the node private key
- session authentication is mutual and challenge/response based
- peer trust is persisted in the config file

V1 does not need a full PKI or account system.

## 6. LAN Transport

Preferred properties:

- persistent connection
- encrypted session
- low overhead framing
- heartbeat every few seconds
- immediate disconnect detection

Transport choices:

- `TCP + Noise-like handshake`: simplest good default
- `QUIC`: viable, but not required for V1

Recommendation for V1: start with `TCP`.

TCP is easier to debug, easier to implement correctly, and typically good enough for LAN input forwarding when packets are small and the connection is persistent.

## Event Model

Use physical-style events where possible.

Suggested Rust enum sketch:

```rust
pub enum InputEvent {
    KeyDown { code: KeyCode, modifiers: Modifiers },
    KeyUp { code: KeyCode, modifiers: Modifiers },
    MouseMove { dx: i32, dy: i32 },
    MouseButtonDown { button: MouseButton },
    MouseButtonUp { button: MouseButton },
    MouseWheel { delta_x: i32, delta_y: i32 },
}
```

Notes:

- `KeyCode` should represent physical or scan-code-like keys where possible
- text generation should remain platform-native side-effect, not wire-native payload
- mouse movement should start as relative deltas in V1

## Configuration Model

Suggested config file:

```toml
[node]
name = "macbook-air"
listen_addr = "0.0.0.0:48571"

[switch]
hotkey = "Ctrl+Alt+Shift+K"

[[peers]]
id = "office-pc"
name = "Office PC"
addr = "192.168.1.25:48571"
public_key = "base64:..."
trusted = true
```

## Logging and Diagnostics

V1 should log:

- daemon startup/shutdown
- permission check failures
- peer connect/disconnect
- role changes
- dropped/invalid frames
- key release recovery events
- hotkey switch events

Do not log raw sensitive typing content in normal mode.

## Platform Notes

## macOS

Expected requirements:

- Accessibility permission for input control
- Input Monitoring permission for observing global input

Key implementation concerns:

- event tap reliability
- filtering out self-injected events where needed
- handling permission denial with clear CLI diagnostics

## Windows

Expected requirements:

- user-session execution
- input injection via standard Win32 APIs

Key implementation concerns:

- integrity-level restrictions
- avoiding duplicate capture of injected events
- robust handling of disconnects and session transitions

## Failure Handling

Required safety behaviors:

- release all tracked modifiers/buttons on disconnect
- release all tracked modifiers/buttons on mode switch
- reject frames from untrusted peers
- reject control messages from non-active sessions
- do not attempt login-screen or secure-desktop injection

## Performance Goals

Informal V1 targets:

- local event capture overhead should be minimal
- network messages should be tiny and fixed-shape where practical
- mean LAN forwarding delay should feel near-instant for typing
- cursor movement should remain smooth on normal home/office Wi-Fi

Measure first before optimizing transport complexity.

## Suggested Dependencies

Potential Rust library choices:

- `tokio` for async runtime
- `serde` for config and protocol support where helpful
- `toml` for config files
- `clap` for CLI
- `tracing` and `tracing-subscriber` for logs
- `x25519-dalek` or similar for key agreement
- `chacha20poly1305` or similar for symmetric encryption

Platform crates may include:

- macOS bindings through `objc2`, `core-graphics`, or direct FFI as needed
- Windows bindings through `windows` crate

## Architecture Summary

The V1 system should be a Rust workspace with:

- one shared daemon core
- one minimal protocol crate
- one config/crypto/network layer
- thin platform-specific crates for capture and injection

The most important implementation rule is to keep platform-specific input handling at the edges and keep the core focused on state, trust, transport, and recovery.

## AI-Assisted Implementation Notes

If this project is implemented with multiple coding agents, use a strict orchestrator-worker pattern:

- one `orchestrator` owns interfaces, milestone order, and merge decisions
- platform workers own `flowkey-platform-macos` and `flowkey-platform-windows` separately
- protocol/network work stays isolated from platform injection work

Recommended rule:

- shared crates define contracts first
- platform crates consume those contracts
- no platform crate should invent its own protocol or recovery semantics

This keeps multi-agent work parallel without causing endless interface churn.
