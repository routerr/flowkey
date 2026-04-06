# flowkey

flowkey is a CLI-first Rust workspace for a LAN-only keyboard and mouse sharing tool targeting macOS and Windows.

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
    ├── protocol.md
    └── roadmap.md
```

## Design Docs

- [Architecture](./docs/architecture.md)
- [Roadmap](./docs/roadmap.md)
- [Protocol](./docs/protocol.md)
- [Setup Guide](./docs/setup-guide.md)

## Current Status

This repo is now past the initial scaffold and has a working cross-platform control path under the expected operator conditions (interactive Windows desktop session, macOS terminal with Accessibility/Input Monitoring permission):

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
- `flky switch` and `flky release` now queue local daemon control requests through a file-backed command channel
- remote switch propagation now drives the target peer into `controlled-by`
- local capture now emits mouse movement correctly after the first observed cursor sample
- **Exclusive Capture (macOS)**: Local input is now suppressed while controlling a peer using `CGEventTap`.
- **Automatic Control Resume**: Daemon now automatically restores control role after network interruptions.
- **Reachability Probing (FRP)**: Zero-config switching using UDP racing to select the best IP among multiple interfaces.
- **Diagnostics**: `flky doctor` command added to diagnose permissions and network setup.
- release bundles and checksum files can be generated locally or by tagged GitHub Actions releases

The remaining gaps are mostly platform hardening and UX polish rather than missing core protocol wiring.

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

This installs the `flky` binary.

On Windows:

```powershell
.\\scripts\\install.ps1
```

If you prefer the direct Cargo command, use:

```bash
cargo install --path crates/flowkey-cli --locked --force
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
flky --help
```

### Start the Placeholder Daemon

```bash
flky daemon
```

## Config

By default, the CLI looks for config at:

- macOS: `~/Library/Application Support/flowkey/config.toml`
- Windows: `%AppData%/flowkey/config.toml`

The daemon also uses sibling files in the same directory for runtime state:

- `control.toml` for queued `flky switch` and `flky release` requests
- `status.toml` for the live daemon snapshot used by `flky status`

Optional pairing override:

- `node.advertised_addr` can be set in `config.toml` when the auto-detected address is not reachable from the peer
- `flky pair init --advertised-addr <ip:port>` overrides both auto-detection and config for a single pairing token

You can override that path with:

```bash
FLKY_CONFIG=/absolute/path/to/config.toml cargo run -p flowkey-cli -- status
```

Current behavior:

- if the config file exists, it is loaded
- if it does not exist, the CLI falls back to built-in defaults

Running `flky pair init` or `flky daemon` will create and persist a config automatically if one does not exist yet.

Example override in `config.toml`:

```toml
[node]
advertised_addr = "100.79.183.18:48571"
```

## Commands

The CLI surface is still small, but the core commands are wired and `flky status` now reads the daemon runtime snapshot:

```text
flky daemon
flky setup
flky pair init
flky pair accept <token>
flky discover
flky peers list
flky switch <peer-id>
flky release
flky status
flky doctor
```

`flky switch` and `flky release` write a command file into the local config directory, and the daemon watches that file while it is running.

The daemon runtime now tracks authenticated peers, active peer selection, controller mode transitions, active-session forwarding for captured local input, hotkey chord suppression, shared cursor/key normalization, and local control commands on supported platforms.

## Pairing Flow

The easiest way to pair devices is using the interactive wizard:

```bash
flky setup
```

This will guide you through setting your device name, discovering peers on the network, and exchanging pairing tokens.

Alternatively, you can use the manual CLI flow:

1. On machine A:

```bash
cargo run -p flowkey-cli -- pair init
```

2. Copy the printed token to machine B and run:

```bash
cargo run -p flowkey-cli -- pair accept '<token>'
```

3. Repeat in the opposite direction if you want explicit mutual trust on both machines.

4. Inspect stored peers with:

```bash
cargo run -p flowkey-cli -- peers
```

If both daemons are already running on the same LAN, you can discover nearby nodes first:

```bash
cargo run -p flowkey-cli -- discover
```

What is real now:

- each node gets a persisted Ed25519 keypair
- `pair init` creates a signed pairing offer and advertises a LAN-reachable listen address
- `pair accept` verifies the signature before storing trust
- `flowkey-net` has a working TCP hello plus mutual signed challenge/response handshake path
- the daemon now binds a listener, accepts trusted peers, tracks authenticated sessions, and forwards captured local input to the active peer session
- `flky switch` and `flky release` write a local command file that the daemon consumes and applies

What is still not done yet:

- native input injection still depends on the local OS permission model
- Windows must run the daemon from the signed-in desktop session, not via SSH or `Start-Process`
- Windows Firewall may need an inbound rule for TCP port `48571`
- macOS still requires Accessibility and Input Monitoring permission grants for full operation

## Platform Notes

- Windows: start `flky daemon` from the interactive desktop session. If injection still fails, run it at the same privilege level as the target apps.
- Windows: if peers cannot connect, open TCP port `48571` in Windows Firewall.
- macOS: grant Accessibility in `System Settings > Privacy & Security > Accessibility` and Input Monitoring in `System Settings > Privacy & Security > Input Monitoring`.

- replay-resistant online authentication beyond token expiry
- remaining session recovery edge cases

What now also works:

- self-injected input is recorded and filtered back out of the local capture stream
- remote-control forwarding no longer loops the daemon's own injected events back into the network path
- the control file is kept alongside the config and status snapshots in the platform-specific app data directory
- release artifacts are created per platform as tar.gz, dmg, or zip bundles with SHA-256 checksums

## Implementation Notes

- `flowkey-core` owns state transitions and recovery logic
- `flowkey-protocol` owns the wire contract
- `flowkey-net` will own connection and heartbeat behavior
- `flowkey-platform-macos` and `flowkey-platform-windows` will own platform-specific capture and injection
- `flowkey-cli` should stay thin

## Suggested Next Steps

1. Implement config loading in `flowkey-config`
2. Define protocol messages in `flowkey-protocol`
3. Wire daemon state in `flowkey-core`
4. Add pairing primitives in `flowkey-crypto`
5. Build one-platform end-to-end proof before full cross-platform polish
