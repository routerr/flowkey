# Key Mouse Sharer V1 Roadmap

## Goal

Ship a CLI-only V1 for macOS and Windows that lets one logged-in machine control another over the same local network using a specific keyboard shortcut to switch control.

## Delivery Strategy

Build vertically, not by over-generalizing first.

Recommended order:

1. protocol and daemon skeleton
2. local switching state machine
3. one-platform end-to-end path
4. second-platform end-to-end path
5. pairing and trust hardening
6. recovery and packaging

## Current Status

Completed:

- workspace bootstrap
- config loading and saving
- CLI skeleton
- node identity generation
- signed pairing token generation and verification
- authenticated TCP handshake
- heartbeat session pump
- daemon listener and outbound connection runtime
- native macOS and Windows input injection backends
- hotkey parsing and local capture listener scaffolding
- controller-state transitions with active peer selection
- peer-to-peer event forwarding from local capture
- hotkey activation-chord suppression
- CLI status reporting from daemon runtime snapshots
- cross-platform cursor/key normalization
- platform permission probes and richer OS-specific diagnostics

Partially implemented:

- daemon state machine and session recovery
- durable reconnect strategy

## Proposed Repository Layout

```text
key-mouse-sharer/
├── Cargo.toml
├── crates/
│   ├── kms-cli/
│   │   └── src/main.rs
│   ├── kms-core/
│   │   ├── src/lib.rs
│   │   ├── src/daemon.rs
│   │   ├── src/session.rs
│   │   ├── src/switching.rs
│   │   └── src/recovery.rs
│   ├── kms-config/
│   │   ├── src/lib.rs
│   │   └── src/config.rs
│   ├── kms-crypto/
│   │   ├── src/lib.rs
│   │   ├── src/identity.rs
│   │   └── src/handshake.rs
│   ├── kms-net/
│   │   ├── src/lib.rs
│   │   ├── src/connection.rs
│   │   ├── src/frame.rs
│   │   └── src/heartbeat.rs
│   ├── kms-protocol/
│   │   ├── src/lib.rs
│   │   ├── src/message.rs
│   │   └── src/input.rs
│   ├── kms-daemon/
│   │   ├── src/lib.rs
│   │   └── src/bootstrap.rs
│   ├── kms-input/
│   │   ├── src/lib.rs
│   │   ├── src/event.rs
│   │   ├── src/capture.rs
│   │   └── src/inject.rs
│   ├── kms-platform-macos/
│   │   ├── src/lib.rs
│   │   ├── src/capture.rs
│   │   ├── src/inject.rs
│   │   ├── src/hotkey.rs
│   │   └── src/permissions.rs
│   └── kms-platform-windows/
│       ├── src/lib.rs
│       ├── src/capture.rs
│       ├── src/inject.rs
│       ├── src/hotkey.rs
│       └── src/permissions.rs
└── docs/
    ├── architecture.md
    ├── roadmap.md
    └── protocol.md
```

## Milestone 0: Workspace Bootstrap

### Outcome

The repo builds as a Rust workspace and has a runnable placeholder CLI.

### Files

- `Cargo.toml`
- `crates/kms-cli/src/main.rs`
- `crates/kms-core/src/lib.rs`
- `crates/kms-daemon/src/lib.rs`
- `crates/kms-protocol/src/lib.rs`
- `crates/kms-config/src/lib.rs`

### Tasks

- create workspace manifest
- add crate skeletons
- wire basic logging
- add `kms daemon --help`
- add config file location rules

### Exit Criteria

- `cargo build` passes
- `kms --help` works
- workspace structure is stable enough for later milestones

### Status

Done

## Milestone 1: Protocol and Config Foundation

### Outcome

The app can load config and serialize/deserialize the minimal protocol.

### Files

- `crates/kms-config/src/config.rs`
- `crates/kms-protocol/src/message.rs`
- `crates/kms-protocol/src/input.rs`
- `crates/kms-net/src/frame.rs`
- `docs/protocol.md`

### Tasks

- define node config and peer config
- define message enums
- define frame boundaries and versioning
- add protocol round-trip tests

### Exit Criteria

- protocol unit tests pass
- config parser loads a sample file
- framed messages can be encoded and decoded

### Status

Done

## Milestone 2: Identity and Pairing

### Outcome

Two nodes can establish trust and persist peer identity.

### Files

- `crates/kms-crypto/src/identity.rs`
- `crates/kms-crypto/src/handshake.rs`
- `crates/kms-cli/src/main.rs`
- `crates/kms-config/src/config.rs`

### Tasks

- generate persistent node keypair
- implement `kms pair init`
- implement `kms pair accept`
- store trusted peer public keys
- define short pairing code or explicit copy-paste trust token flow

### Exit Criteria

- two nodes can be paired without manual file editing
- trusted peer config survives restart
- untrusted peer is rejected

### Status

Done

## Milestone 3: Network Session and Heartbeats

### Outcome

Two paired nodes can keep a stable encrypted session over LAN.

### Files

- `crates/kms-net/src/connection.rs`
- `crates/kms-net/src/heartbeat.rs`
- `crates/kms-core/src/session.rs`
- `crates/kms-daemon/src/bootstrap.rs`

### Tasks

- listener/client startup
- transport handshake
- encrypted session establishment
- heartbeat and timeout detection
- reconnect logic

### Exit Criteria

- daemon reconnects after brief Wi-Fi interruption
- broken connection transitions to safe idle state
- logs show clear connection lifecycle

### Status

Done in first-pass form

## Milestone 4: Core Switching State Machine

### Outcome

The daemon can enter controller/controlled/idle roles safely.

### Files

- `crates/kms-core/src/daemon.rs`
- `crates/kms-core/src/session.rs`
- `crates/kms-core/src/switching.rs`
- `crates/kms-core/src/recovery.rs`

### Tasks

- implement state transitions
- active peer selection
- switch ownership logic
- forced key/button release on transitions

### Exit Criteria

- state transitions are unit-tested
- invalid transitions are rejected
- disconnect always clears held keys/buttons

### Status

Partial

## Milestone 5: Windows End-to-End Path

### Outcome

Windows can capture, send, receive, and inject input end to end.

### Files

- `crates/kms-platform-windows/src/capture.rs`
- `crates/kms-platform-windows/src/inject.rs`
- `crates/kms-platform-windows/src/hotkey.rs`
- `crates/kms-platform-windows/src/permissions.rs`
- `crates/kms-input/src/capture.rs`
- `crates/kms-input/src/inject.rs`

### Tasks

- implement global hotkey
- implement keyboard capture
- implement mouse capture
- implement keyboard injection
- implement mouse injection
- suppress self-injected events with a shared loopback filter

### Exit Criteria

- Windows-to-Windows control works on LAN
- hotkey can enter and exit control mode
- no persistent sticky modifiers after disconnect

### Status

Not started

## Milestone 6: macOS End-to-End Path

### Outcome

macOS can capture, send, receive, and inject input end to end.

### Files

- `crates/kms-platform-macos/src/capture.rs`
- `crates/kms-platform-macos/src/inject.rs`
- `crates/kms-platform-macos/src/hotkey.rs`
- `crates/kms-platform-macos/src/permissions.rs`

### Tasks

- implement permission checks
- implement global capture
- implement injection
- implement local hotkey handling
- diagnose denied permissions cleanly

### Exit Criteria

- macOS-to-macOS control works on LAN
- daemon clearly reports missing permissions
- input injection works in normal logged-in desktop sessions

### Status

Not started

## Milestone 7: Cross-Platform Interop

### Outcome

macOS and Windows can control each other.

### Files

- `crates/kms-protocol/src/input.rs`
- `crates/kms-platform-macos/src/inject.rs`
- `crates/kms-platform-windows/src/inject.rs`
- `crates/kms-core/src/recovery.rs`

### Tasks

- verify shared key code model
- verify modifier mapping
- test mouse delta behavior across platforms
- handle layout-sensitive edge cases conservatively

### Exit Criteria

- macOS controls Windows
- Windows controls macOS
- standard modifiers and mouse buttons behave predictably

### Status

Not started

## Milestone 8: CLI UX and Ops Polish

### Outcome

The tool is easy to run and diagnose from the terminal.

### Files

- `crates/kms-cli/src/main.rs`
- `crates/kms-config/src/config.rs`
- `docs/architecture.md`
- `docs/protocol.md`
- `README.md`

### Tasks

- improve command help
- add config examples
- add status output
- add logs/output guidance
- document permissions and setup steps

### Exit Criteria

- a new user can pair two devices from docs only
- common setup errors are diagnosable

### Status

Partial
- CLI output is concise and useful

## Milestone 9: Reliability Hardening

### Outcome

V1 survives real-world Wi-Fi and session edge cases.

### Files

- `crates/kms-core/src/recovery.rs`
- `crates/kms-net/src/connection.rs`
- `crates/kms-platform-macos/src/capture.rs`
- `crates/kms-platform-windows/src/capture.rs`
- `tests/` integration test files when introduced

### Tasks

- add reconnect chaos testing
- add forced modifier release tests
- tune heartbeat timeouts
- verify hotkey behavior while controlling peer
- add structured logs around failure paths

### Exit Criteria

- brief Wi-Fi blips recover without restart
- no recurring stuck key bug in common scenarios
- logs are sufficient for bug reports

### Status

Not started

## Next AI Pass

Recommended next implementation focus:

1. platform-specific UX cleanup
2. end-to-end onboarding and docs hardening
3. native installers and code signing

## Recommended Testing Strategy

## Unit Tests

- protocol encode/decode
- config parsing
- state machine transitions
- pairing token parsing
- recovery logic

## Integration Tests

- handshake establishment
- reconnect and timeout behavior
- trusted vs untrusted peer handling

## Manual Platform Tests

- keyboard typing
- modifier combinations
- mouse movement
- left/right/middle click
- wheel scrolling
- hotkey enter/exit
- disconnect during held modifier

## Suggested First Implementation Slice

The fastest path to confidence is:

1. workspace bootstrap
2. protocol
3. pairing
4. session management
5. Windows-only end-to-end proof
6. macOS support
7. cross-platform validation

This gives a vertical demo early and avoids spending too long on abstractions before input injection is proven.

## Future Work After V1

Explicitly defer these until after the core path is stable:

- clipboard sync
- file transfer
- screen-edge switching
- multi-peer selection
- auto-discovery
- GUI companion
- internet relay mode
- multi-monitor mapping

## Summary

The roadmap is intentionally biased toward proving:

- trust
- transport
- switching
- platform injection

Once those are stable, the rest of the product can expand safely.

## Agent Execution Plan

This project is well suited to a small orchestrated agent workflow because several workstreams are related but still separable.

Recommended pattern:

- `orchestrator`: owns architecture, protocol compatibility, merge decisions, and milestone sequencing
- `worker 1`: owns protocol, config, crypto, and session transport
- `worker 2`: owns Windows capture/injection and Windows diagnostics
- `worker 3`: owns macOS capture/injection and macOS permission handling
- `reviewer`: validates interface consistency, recovery logic, and regression risks

Suggested ownership boundaries:

- `worker 1`
  - `crates/kms-protocol/`
  - `crates/kms-config/`
  - `crates/kms-crypto/`
  - `crates/kms-net/`
- `worker 2`
  - `crates/kms-platform-windows/`
  - Windows-facing parts of `crates/kms-input/`
- `worker 3`
  - `crates/kms-platform-macos/`
  - macOS-facing parts of `crates/kms-input/`
- `orchestrator`
  - `crates/kms-core/`
  - `crates/kms-daemon/`
  - `crates/kms-cli/`
  - docs and integration decisions

Rules to avoid multi-agent drift:

- protocol crate is the shared contract and must not change casually
- only the orchestrator merges cross-cutting interface changes
- workers should not rewrite each other's files unless explicitly reassigned
- recovery semantics for disconnects and stuck modifiers must be reviewed centrally

## Model Routing Guidance

Use stronger models only where the task genuinely needs them.

Recommended default routing:

- `cheap/fast model`
  - file reads
  - boilerplate crate scaffolding
  - doc formatting
  - routine serialization code
  - straightforward tests
- `mid-tier model`
  - standard Rust implementation work
  - protocol and config design
  - CLI command wiring
  - integration of worker outputs
- `premium model`
  - architecture changes
  - OS input edge cases
  - tricky reconnect or stuck-key debugging
  - security-sensitive trust and handshake decisions

In practice:

- default most sub-agents to a cheaper or mid-tier model
- keep the orchestrator on the strongest model available when resolving platform or protocol ambiguity
- escalate only after a concrete blocker, failed attempt, or high-stakes design choice
