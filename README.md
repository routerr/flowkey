# key-mouse-sharer

CLI-first Rust workspace for a LAN-only keyboard and mouse sharing tool targeting macOS and Windows.

## V1 Scope

- same local network, including normal Wi-Fi
- both machines already logged in
- explicit hotkey switching
- keyboard and mouse only
- CLI-only operation

Deferred until later:

- clipboard sync
- file transfer
- screen-edge switching
- login-screen control
- public internet support

## Repository Layout

```text
.
├── Cargo.toml
├── scripts/
├── crates/
│   ├── kms-cli/
│   ├── kms-core/
│   ├── kms-config/
│   ├── kms-crypto/
│   ├── kms-net/
│   ├── kms-protocol/
│   ├── kms-daemon/
│   ├── kms-input/
│   ├── kms-platform-macos/
│   └── kms-platform-windows/
└── docs/
    ├── architecture.md
    ├── protocol.md
    └── roadmap.md
```

## Design Docs

- [Architecture](./docs/architecture.md)
- [Roadmap](./docs/roadmap.md)
- [Protocol](./docs/protocol.md)

## Current Status

This repo is now past the initial scaffold and has a working trust and transport path plus native injection backends:

- Rust workspace created
- crate boundaries defined
- basic CLI entrypoint added
- config and pairing flow implemented
- authenticated TCP session path implemented
- native macOS and Windows input injection backends added
- global hotkey parsing and local capture listener scaffolding added
- daemon hotkey watcher now flips controller state on supported platforms
- captured local input now forwards to the active peer session on supported platforms
- hotkey activation chords are suppressed so the switch shortcut does not leak into the forwarded stream
- `kms switch` and `kms release` now queue local daemon control requests through a file-backed command channel
- release bundles and checksum files can be generated locally or by tagged GitHub Actions releases

The code is intentionally thin right now. The goal is to provide a clean foundation for incremental implementation rather than pretend the hard platform-specific behavior already exists.

## Getting Started

### Prerequisites

- Rust stable toolchain
- Cargo

### Build

```bash
cargo build
```

### Install

To install the CLI into Cargo's bin directory:

```bash
./scripts/install.sh
```

On Windows:

```powershell
.\\scripts\\install.ps1
```

If you prefer the direct Cargo command, use:

```bash
cargo install --path crates/kms-cli --locked --force
```

To build a portable release bundle for the current platform:

```bash
./scripts/package.sh
```

On Windows:

```powershell
.\\scripts\\package.ps1
```

Tagged releases are also packaged automatically by `.github/workflows/package.yml`.

Release outputs are currently:

- Linux: `tar.gz` bundle plus SHA-256 checksum
- macOS: `.dmg` app bundle plus SHA-256 checksum
- Windows: `.zip` bundle with `install.ps1` plus SHA-256 checksum

### Run Help

```bash
cargo run -p kms-cli -- --help
```

### Start the Placeholder Daemon

```bash
cargo run -p kms-cli -- daemon
```

## Config

By default, the CLI looks for config at:

- macOS: `~/Library/Application Support/kms/config.toml`
- Windows: `%AppData%/kms/config.toml`

The daemon also uses sibling files in the same directory for runtime state:

- `control.toml` for queued `kms switch` and `kms release` requests
- `status.toml` for the live daemon snapshot used by `kms status`

You can override that path with:

```bash
KMS_CONFIG=/absolute/path/to/config.toml cargo run -p kms-cli -- status
```

Current behavior:

- if the config file exists, it is loaded
- if it does not exist, the CLI falls back to built-in defaults

Running `kms pair init` or `kms daemon` will create and persist a config automatically if one does not exist yet.

## Commands

The CLI surface is still small, but the core commands are wired and `kms status` now reads the daemon runtime snapshot:

```text
kms daemon
kms pair init
kms pair accept <token>
kms peers list
kms switch <peer-id>
kms release
kms status
```

`kms switch` and `kms release` write a command file into the local config directory, and the daemon watches that file while it is running.

The daemon runtime now tracks authenticated peers, active peer selection, controller mode transitions, active-session forwarding for captured local input, hotkey chord suppression, shared cursor/key normalization, and local control commands on supported platforms.

## Pairing Flow

Current pairing is a simple local trust flow:

1. On machine A:

```bash
cargo run -p kms-cli -- pair init
```

2. Copy the printed token to machine B and run:

```bash
cargo run -p kms-cli -- pair accept '<token>'
```

3. Repeat in the opposite direction if you want explicit mutual trust on both machines.

4. Inspect stored peers with:

```bash
cargo run -p kms-cli -- peers
```

What is real now:

- each node gets a persisted Ed25519 keypair
- `pair init` creates a signed pairing offer
- `pair accept` verifies the signature before storing trust
- `kms-net` has a working TCP hello plus mutual signed challenge/response handshake path
- the daemon now binds a listener, accepts trusted peers, tracks authenticated sessions, and forwards captured local input to the active peer session
- `kms switch` and `kms release` write a local command file that the daemon consumes and applies

What is still not done yet:

- replay-resistant online authentication beyond token expiry
- remaining session recovery edge cases

What now also works:

- self-injected input is recorded and filtered back out of the local capture stream
- remote-control forwarding no longer loops the daemon's own injected events back into the network path
- the control file is kept alongside the config and status snapshots in the platform-specific app data directory
- release artifacts are created per platform as tar.gz, dmg, or zip bundles with SHA-256 checksums

## Implementation Notes

- `kms-core` owns state transitions and recovery logic
- `kms-protocol` owns the wire contract
- `kms-net` will own connection and heartbeat behavior
- `kms-platform-macos` and `kms-platform-windows` will own platform-specific capture and injection
- `kms-cli` should stay thin

## Suggested Next Steps

1. Implement config loading in `kms-config`
2. Define protocol messages in `kms-protocol`
3. Wire daemon state in `kms-core`
4. Add pairing primitives in `kms-crypto`
5. Build one-platform end-to-end proof before full cross-platform polish
