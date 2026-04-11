# flowkey Work Backlog — 2026-04-11

This backlog is the execution counterpart of
[`review-2026-04-11.md`](./review-2026-04-11.md). Each task is written so that
an AI coding agent can pick it up without re-deriving context: it states the
goal, points at the exact files involved, proposes an edit plan, and defines
measurable acceptance criteria.

Phases are ordered by dependency and risk. Tasks inside a phase may run in
parallel unless a dependency is noted.

Conventions:

- **Goal**: the single outcome this subtask delivers.
- **Reference**: files or docs the agent should read before editing.
- **Edit plan**: concrete, minimal changes the agent should make.
- **Acceptance criteria**: how the agent proves the work is done.
- **Owner hint**: which rules/skills to consult (`rust-reviewer`,
  `tdd-guide`, etc.).

## Progress Update

Completed since this backlog was written:

- Task A1.2 and A1.3: Windows named pipe control channel now exists in the
  daemon and CLI.
- Task A2.1, A2.2, and A2.3: Windows `uiAccess` manifest, interactive-session
  fail-fast, and setup docs are in place.
- Task A3.1, A3.2, and A3.3: supervised daemon lifecycle in the GUI, clean
  shutdown, and loopback poison recovery are implemented.
- Task A4.1: pairing now persists the observed remote address.
- Task B2.1: `HeldKeyTracker` now flushes held keys/buttons deterministically
  through the session cleanup paths.
- Task B2.2: switch-exit and release paths now flush the previous peer
  before changing daemon control state.
- Task B1.1: hot runtime status is mirrored through `ArcSwap`, so status
  writes no longer serialize directly from the big mutex.
- Task B1.2: `bootstrap.rs` is now split into smaller orchestration modules.
- Task B3.1: capture listeners now restart after panic and surface a restart
  counter in status output.
- Task B3.2: macOS event-tap self-heal now re-enables disabled taps.
- Task C1: macOS permission deep-link now opens System Settings from both the
  CLI doctor flow and the GUI banner.
- Task C2: native installer packaging now includes a WiX fragment for the
  Windows firewall rule and macOS signing / notarization hooks.
- Task C3: auto-start and remote-control settings are exposed in the GUI and
  persisted through the autostart plugin / config.
- Task C4: `native_injector.rs` is split so macOS-specific cursor logic now
  lives behind the macOS platform module.
- Task C5: repo hygiene now ignores `.DS_Store` recursively and removes the
  tracked root `.DS_Store` entry from the index.

Current next step:

- No required backlog steps remain; continue with optional future work if
  desired.

---

## Phase A — Stabilize Windows & GUI

**Phase goal**: Eliminate the two silent-failure classes blocking daily use on
Windows (UIPI + slow IPC) and stop the GUI from dying when the daemon panics.
Exit this phase with a daemon that a non-expert user can run via the GUI on
either platform without manual recovery.

### Task A1 — Windows Named Pipe IPC parity

**Status**: part complete. The control channel now uses a Windows named pipe
and macOS UDS, but the abstract `ControlTransport` refactor in A1.1 is still
open if we want to consolidate the code paths later.

**Goal**: Replace the 150ms TOML file-poll loop on Windows with a Named Pipe
listener, achieving the same sub-10ms `flky switch` latency macOS already has
via UDS.

**Reference**:

- `crates/flowkey-daemon/src/bootstrap.rs:544-640` — current
  `spawn_control_watcher` with the `cfg(target_os = "macos")` UDS branch and
  the `cfg(not(target_os = "macos"))` polling fallback.
- `crates/flowkey-cli/src/main.rs` — where `DaemonCommand::save_to_path` is
  invoked today.
- `crates/flowkey-core/src/switching.rs` — `DaemonCommand` definition and
  `send_to` / `read_from` async helpers.
- `docs/optimization-backlog.md` §3.1.

#### Subtask A1.1 — Abstract the control transport

- **Goal**: Introduce a single `ControlTransport` trait (or enum) that hides
  UDS vs Named Pipe vs file-poll behind one surface, so bootstrap and CLI can
  share logic.
- **Edit plan**:
  1. In `flowkey-core/src/switching.rs`, add `ControlEndpoint` with
     `connect()` and `listen()` methods, feature-gated per OS.
  2. Move the existing macOS UDS code from `bootstrap.rs` into the new
     abstraction.
  3. Update `flky switch` / `flky release` in `flowkey-cli/src/main.rs` to
     call `ControlEndpoint::connect().send(cmd)` with a file-backed fallback
     only when the socket/pipe is unavailable.
- **Acceptance criteria**:
  - `cargo build` passes on macOS and Windows.
  - macOS behavior is byte-for-byte identical (UDS still used; file fallback
    only on failure).
  - No call site still references `DaemonCommand::save_to_path` on the hot
    path.

**Status**: open.

#### Subtask A1.2 — Windows Named Pipe server

- **Goal**: Implement a `NamedPipeServer` branch for the daemon that accepts
  `DaemonCommand` frames over `tokio::net::windows::named_pipe`.
- **Edit plan**:
  1. In `bootstrap.rs`, add a `#[cfg(target_os = "windows")]` branch to
     `spawn_control_watcher` that binds
     `\\.\pipe\flowkey-<user>` using
     `named_pipe::ServerOptions::new().create(...)`.
  2. Each accepted client spawns a task that calls
     `DaemonCommand::read_from` and `handle_control_command`, mirroring the
     macOS path.
  3. Handle `ERROR_PIPE_BUSY` by recreating the server instance in a loop.
- **Acceptance criteria**:
  - On Windows, `flky switch <peer-id>` completes in < 10ms measured with a
    timing log around `ControlEndpoint::connect`.
  - Killing the daemon and restarting re-establishes the pipe without
    restarting the CLI.
  - The 150ms polling fallback is removed from the Windows branch.

**Status**: complete.

#### Subtask A1.3 — Windows CLI client

- **Goal**: Make the CLI connect to the Named Pipe when running on Windows.
- **Edit plan**:
  1. In `flowkey-cli/src/main.rs`, mirror the macOS UDS block with a
     `#[cfg(target_os = "windows")]` block that uses
     `named_pipe::ClientOptions::new().open(...)`.
  2. If the pipe is missing, fall back to writing `control.toml`.
- **Acceptance criteria**:
  - `cargo test -p flowkey-cli` passes.
  - Manual: `flky switch` prints "sent via pipe" log line on Windows.

**Status**: complete.

---

### Task A2 — Windows `uiAccess` manifest & fail-fast startup

**Status**: complete.

**Goal**: Allow the Windows daemon to inject into elevated windows (Task
Manager, signed apps) and stop the silent exit when the daemon is launched
outside an interactive desktop session.

**Reference**:

- `docs/cross-platform-test-report.md` §3 and §7 (UIPI findings).
- `crates/flowkey-platform-windows/src/permissions.rs` — current session
  probe.
- `crates/flowkey-cli/build.rs` — embeds Windows resources already.
- Microsoft Learn: *UAC / UIPI and the `uiAccess` attribute*.

#### Subtask A2.1 — Add `app.manifest` with `uiAccess="true"`

- **Goal**: Ship a manifest that requests `asInvoker` + `uiAccess="true"`, and
  embed it into `flky.exe` via `build.rs`.
- **Edit plan**:
  1. Create `crates/flowkey-cli/resources/app.manifest` with
     `requestedExecutionLevel level="asInvoker" uiAccess="true"`.
  2. Extend `build.rs` to call `winres::WindowsResource::set_manifest_file`.
  3. Document in `README.md` that `uiAccess=true` requires the binary to be
     **code-signed** and installed under `Program Files`.
- **Acceptance criteria**:
  - `cargo build --target x86_64-pc-windows-msvc` succeeds and the resulting
    `flky.exe` contains the manifest (verified by `sigcheck -m flky.exe` or
    `mt.exe -inputresource:flky.exe;#1 -out:extracted.manifest`).
  - Unsigned debug builds continue to run (uiAccess silently downgraded) so
    local development is not blocked.

**Status**: complete.

#### Subtask A2.2 — Fail-fast on non-interactive session

- **Goal**: When the daemon is launched via SSH or `Start-Process` and the
  input hooks cannot install, exit with a clear error instead of silently
  dying mid-init.
- **Edit plan**:
  1. In `flowkey-platform-windows/src/permissions.rs`, add
     `probe_interactive_session()` that calls
     `ProcessIdToSessionId(GetCurrentProcessId(), ...)` and checks for
     `WTSActive`.
  2. In `bootstrap.rs` Windows branch, call this probe before starting the
     capture thread. If the session is not interactive, log
     `error!("flowkey daemon must run inside an interactive desktop session; aborting")`
     and return a typed error.
  3. Forward the error into `flky doctor` output.
- **Acceptance criteria**:
  - Running `Start-Process -FilePath flky.exe -ArgumentList daemon` in a
    detached session exits within 500ms with a non-zero exit code and the
    diagnostic message in stderr.
  - `flky doctor` on Windows reports "interactive session: required / not
    detected" when applicable.

**Status**: complete.

#### Subtask A2.3 — Operator docs

- **Goal**: Update README and setup guide with the UIPI requirement, signing
  note, and Firewall rule one-liner.
- **Edit plan**:
  1. Add a `### Windows Setup` subsection to `README.md` with the
     `New-NetFirewallRule` command and the signing caveat.
  2. Link from `docs/setup-guide.md`.
- **Acceptance criteria**:
  - A new reader can follow the README to run the daemon against an elevated
    window without external research.

**Status**: complete.

---

### Task A3 — Supervised daemon inside the GUI

**Status**: complete.

**Goal**: Make the Tauri GUI resilient to daemon panics, give the GUI a proper
handle to the daemon (with shutdown), and formalize the uncommitted
panic/mutex hardening.

**Reference**:

- `crates/flowkey-gui/src/main.rs` — current `setup()` spawns `run_daemon`
  directly without supervision.
- Uncommitted diff in `flowkey-gui/src/main.rs`,
  `flowkey-input/src/capture.rs`, `flowkey-input/src/native_injector.rs`,
  `flowkey-platform-macos/src/capture.rs`.
- `crates/flowkey-daemon/src/bootstrap.rs:run_daemon` signature.

#### Subtask A3.1 — `DaemonHandle` supervisor API

- **Goal**: Export `flowkey_daemon::spawn_supervised(config) -> DaemonHandle`
  that owns a `JoinHandle`, a `CancellationToken`, and a restart counter.
- **Edit plan**:
  1. In `flowkey-daemon/src/lib.rs` (or a new `supervisor.rs`), add
     `DaemonHandle` with `shutdown()` and `is_running()` methods.
  2. Inside the supervisor task, `loop { run_daemon(config.clone()).await }`
     with exponential backoff (1s, 2s, 5s, max 10s) and `tracing::error!`
     on each restart.
  3. Propagate panics via `tokio::task::JoinHandle` and catch them as
     restart-worthy errors.
- **Acceptance criteria**:
  - Unit test in `flowkey-daemon` injects a panic via a test-only hook and
    verifies the supervisor restarts exactly once and then continues.
  - `DaemonHandle::shutdown()` cancels within 200ms.

**Status**: complete.

#### Subtask A3.2 — GUI adopts the supervisor

- **Goal**: Replace the ad-hoc `tauri::async_runtime::spawn(run_daemon(...))`
  in `setup()` with `spawn_supervised`, and clean up on window close.
- **Edit plan**:
  1. In `flowkey-gui/src/main.rs`, store the `DaemonHandle` inside
     `AppState`.
  2. Call `handle.shutdown()` from the Tauri `WindowEvent::CloseRequested`
     and from the `quit` tray menu item.
  3. Keep the panic hook from the uncommitted diff, but write the log file
     under `Config::log_dir()` so Windows is covered too.
- **Acceptance criteria**:
  - Closing the GUI window cleanly shuts down the daemon (verified by
    absence of `flky daemon` child in Activity Monitor / Task Manager).
  - Forcing a panic in the daemon restarts it automatically while the GUI
    stays responsive.

**Status**: complete.

#### Subtask A3.3 — Harden loopback poison handling

- **Goal**: Commit the uncommitted mutex-poison guards, but add warnings and
  recovery so events are not silently dropped.
- **Edit plan**:
  1. In `flowkey-input/src/capture.rs:124`, replace the silent `return None`
     with `tracing::warn!(target: "loopback", "poisoned mutex, recovering")`
     then call `loopback.clear_poison()` (or rebuild a fresh suppressor)
     before returning the translated event.
  2. Apply the same pattern in `native_injector.rs` record paths and
     `flowkey-platform-macos/src/capture.rs`.
- **Acceptance criteria**:
  - A unit test that deliberately poisons the loopback mutex shows a warn
    log and the next event is processed normally.
  - Manual soak: 30 minutes of heavy mouse movement shows zero warn logs.

**Status**: complete.

---

### Task A4 — Persist pairing peer addresses

**Status**: complete.

**Goal**: Stop relying on mDNS to resolve a peer's address after pairing.

**Reference**:

- `crates/flowkey-gui/src/main.rs:78-94` — `confirm_pairing` writes
  `addr: ""`.
- `crates/flowkey-net/src/pairing.rs` — `PairingProposal` carries the remote
  socket info.

#### Subtask A4.1 — Capture remote addr during pairing

- **Goal**: Thread the observed socket address from `initiate_pairing_client`
  and `run_pairing_listener` into the stored `PeerConfig.addr`.
- **Edit plan**:
  1. Extend `PairingProposal` with `observed_addr: SocketAddr`.
  2. Populate it from both the listener-accept and client-connect paths in
     `flowkey-net/src/pairing.rs`.
  3. In `flowkey-gui/src/main.rs::confirm_pairing`, write
     `proposal.observed_addr.to_string()` into the saved peer.
- **Acceptance criteria**:
  - After pairing, `config.toml` contains a non-empty `addr` for the new
    peer.
  - Reconnect works even after disabling mDNS (tested by blocking UDP 5353
    temporarily).

**Status**: complete.

---

## Phase B — Reliability Hardening (roadmap Milestone 9)

**Phase goal**: Meet the stability targets in the review doc §6: lock-free
status, guaranteed modifier release, watchdog-backed capture threads,
coalesced mouse movement, and automated chaos tests.

### Task B1 — Split `DaemonRuntime` state

**Goal**: Remove the single `Mutex<DaemonRuntime>` bottleneck so `flky status`
and the GUI status bridge never contend with the network hot path.

**Reference**:

- `crates/flowkey-daemon/src/bootstrap.rs:51,378,544,642,798,886,915,1113,1144`.
- `crates/flowkey-core/src/daemon.rs`, `status.rs`.
- `docs/optimization-backlog.md` §3.2.

#### Subtask B1.1 — Extract hot state into `ArcSwap`

- **Goal**: Move `state`, `active_peer_id`, and `notes` out of the big mutex
  into `arc_swap::ArcSwap<RuntimeSnapshot>` so readers never block writers.
- **Edit plan**:
  1. Add `arc-swap = "1"` to `flowkey-core/Cargo.toml`.
  2. Introduce `RuntimeSnapshot` in `flowkey-core/src/status.rs` with the
     fields currently serialized by `persist_status_snapshot`.
  3. Replace `Mutex<DaemonRuntime>` reads in `persist_status_snapshot` and
     `print_runtime_notes` with `ArcSwap::load`.
  4. Writers still update through a thinner `Mutex<DaemonRuntimeMut>` that
     only protects the authoritative state, and publish a new snapshot on
     each transition.
- **Acceptance criteria**:
  - `flky status` returns in < 2ms while the daemon is processing 1000
    mouse events/sec (measured with a stress test binary).
  - No test regressions.

**Status**: complete.

#### Subtask B1.2 — Split `bootstrap.rs`

- **Goal**: Reduce `bootstrap.rs` from 1335 lines to ≤ 600 by extracting
  logical sections.
- **Edit plan**:
  1. Move `spawn_control_watcher` + Named Pipe/UDS code into
     `flowkey-daemon/src/control_ipc.rs`.
  2. Move capture selection + permission probes into
     `flowkey-daemon/src/platform.rs`.
  3. Move status snapshot + notes into
     `flowkey-daemon/src/status_writer.rs`.
- **Acceptance criteria**:
  - `bootstrap.rs` is ≤ 600 lines and contains only top-level orchestration.
  - No public API changes; `cargo test` passes.

---

### Task B2 — Forced release on disconnect / switch-exit

**Goal**: Guarantee that every modifier, mouse button, and held key is
released whenever the control path exits, eliminating sticky-key bugs.

**Reference**:

- `crates/flowkey-core/src/recovery.rs` — only 68 lines today.
- `crates/flowkey-input/src/native_injector.rs` — has `release_all` primitives
  per platform, but they are not wired everywhere.
- Roadmap Milestone 4 exit criteria: "disconnect always clears held
  keys/buttons".

#### Subtask B2.1 — `HeldKeyTracker`

- **Goal**: Track every `KeyDown`/`ButtonDown` the daemon has forwarded to a
  peer, and expose `release_all(&mut sink)`.
- **Edit plan**:
  1. Add `HeldKeyTracker` to `flowkey-core/src/recovery.rs` so it records
     input-down state, tracks modifiers, and can synthesize releases in
     reverse order.
  2. Wire it into the forwarding path in
     `flowkey-net/src/connection.rs::route_input_event` and the daemon
     cleanup paths.
- **Acceptance criteria**:
  - Unit tests cover: modifier chord (shift+ctrl), mouse drag, key repeat.
  - `release_all` generates the exact reverse events in deterministic order.

**Status**: complete.

#### Subtask B2.2 — Call `release_all` on all exit paths

- **Goal**: On disconnect, `SwitchRelease`, and hotkey exit, flush held keys
  through the sink before switching state.
- **Edit plan**:
  1. In `flowkey-core/src/recovery.rs::on_disconnect`, call
     `release_all` before updating `state`.
  2. Same in `switching.rs::apply_release` and hotkey exit branch.
- **Acceptance criteria**:
  - Integration test: simulate `LShift` down → disconnect → reconnect →
    verify the remote peer has no pending shift.
  - Manual: holding `Cmd` while hitting the switch hotkey releases `Cmd` on
    the remote side.

**Status**: complete.

---

### Task B3 — Capture thread watchdog

**Goal**: Survive capture-thread crashes and macOS event-tap timeouts without
user intervention.

**Reference**:

- `crates/flowkey-platform-windows/src/capture.rs` — `rdev::grab` /
  `rdev::listen` thread entry points.
- `crates/flowkey-platform-macos/src/capture.rs` — `CGEventTap` and
  `rdev::grab` entry points.

#### Subtask B3.1 — Supervisor task around capture threads

- **Goal**: Wrap each capture thread in a supervisor task that restarts on
  panic with 1s, 2s, 5s backoff.
- **Edit plan**:
  1. In each platform crate, return a `CaptureHandle` that holds a
     `JoinHandle` and a `CancellationToken`.
  2. `bootstrap.rs` owns the handle, watches it via
     `tokio::task::spawn_blocking`, and restarts via `CaptureHandle::spawn`
     on failure.
- **Acceptance criteria**:
  - Injecting a panic into the capture thread (test-only) causes exactly
    one restart followed by normal operation.
  - `flky status` exposes a `capture_restarts: u64` counter.

**Status**: complete.

#### Subtask B3.2 — macOS event-tap self-heal

- **Goal**: Detect `CGEventTapIsEnabled == false` and re-enable the tap.
- **Edit plan**:
  1. In `flowkey-platform-macos/src/capture.rs`, install a notification
     observer on `kCGEventTapDisabledByTimeout` /
     `kCGEventTapDisabledByUserInput`.
  2. On notification, call `CGEventTapEnable(tap, true)` and log.
- **Acceptance criteria**:
  - Under `stress-ng --cpu 4` for 60s, no events are lost and no warn-level
    "event tap timed out" messages remain unhandled.

**Status**: complete.

---

### Task B4 — Mouse move coalescing

**Goal**: Smooth the high-DPI mouse stream without losing total displacement.

**Reference**:

- `crates/flowkey-net/src/connection.rs` — `SessionSender::send_input`.
- `docs/optimization-backlog.md` §2.2.

#### Subtask B4.1 — 8ms coalescing window

- **Goal**: Batch consecutive `MouseMove` events within an 8ms window into a
  single delta before sending.
- **Edit plan**:
  1. Add a small `Coalescer` struct holding the current accumulated delta
     and a `tokio::time::Instant` deadline.
  2. On `send_input(MouseMove { dx, dy })`, add to the accumulator and
     arm a 8ms flush timer; flush eagerly on any non-move event.
  3. Preserve timestamps by using the latest `timestamp_us` seen.
- **Acceptance criteria**:
  - At 1000Hz mouse input, outgoing packet rate stays below 150 pps.
  - Total accumulated delta over 10s matches the original stream within 1px.

**Status**: complete.

---

### Task B5 — Chaos + regression tests

**Goal**: Ship a test harness that exercises reconnects and stuck-key paths
automatically.

**Reference**:

- `crates/flowkey-net/src/connection.rs` — session lifecycle.
- `crates/flowkey-core/src/recovery.rs` — post-B2 state.

#### Subtask B5.1 — In-process reconnect chaos test

- **Goal**: Spin up two daemon runtimes in-process over a loopback TCP socket,
  drop the connection mid-session, verify auto-reconnect and state recovery.
- **Edit plan**:
  1. Add `tests/reconnect_chaos.rs` at the workspace root.
  2. Use `tokio::net::TcpListener` with a controllable proxy that can sever
     the connection on command.
- **Acceptance criteria**:
  - Test passes in CI on macOS and Windows.
  - 3 consecutive disconnect/reconnect cycles leave no held keys.

**Status**: complete.

#### Subtask B5.2 — Sticky-key regression test

- **Goal**: Cover `HeldKeyTracker` + recovery end-to-end.
- **Edit plan**:
  1. Add `tests/sticky_keys.rs` that drives `route_input_event` with a
     shift-drag sequence, forces disconnect, and asserts the generated
     release events.
- **Acceptance criteria**:
  - Test passes and fails loudly if either tracker or recovery regresses.

**Status**: complete.

---

## Phase C — UX & Release Polish

**Phase goal**: Make first-run effortless on both platforms and ship
installable artifacts.

### Task C1 — macOS permission deep-link

**Goal**: Stop asking users to navigate System Settings by hand.

**Reference**:

- `crates/flowkey-platform-macos/src/permissions.rs`.
- Apple docs: `x-apple.systempreferences:` URL scheme.

#### Subtask C1.1 — Deep-link URLs

- **Edit plan**: Add `open_accessibility_pane()` and
  `open_input_monitoring_pane()` that `std::process::Command::new("open")`
  the respective `x-apple.systempreferences:com.apple.preference.security?…`
  URLs.
- **Acceptance criteria**: `flky doctor --open-permissions` opens both panes
  on macOS; no-op on other platforms.

**Status**: complete.

#### Subtask C1.2 — GUI prompts & recheck

- **Edit plan**: In the React dashboard, when a missing-permission status
  event arrives, render a banner with "Open Settings" that calls a new
  Tauri command invoking C1.1 and then re-probes.
- **Acceptance criteria**: Manual run on a clean Mac account reaches full
  functionality without reading any docs.

**Status**: complete.

### Task C1 status

**Status**: complete.

---

### Task C2 — Native installers

**Goal**: Ship `.msi` and notarized `.dmg` artifacts.

**Reference**:

- `scripts/package.sh`, `scripts/package.ps1`.
- Tauri bundler docs for WiX and DMG.

#### Subtask C2.1 — WiX bundle with Firewall rule

- **Edit plan**:
  1. Add WiX configuration under `crates/flowkey-gui/` using
     `tauri-bundler`'s WiX template.
  2. Include a custom action that runs `netsh advfirewall firewall add
     rule name="flowkey" dir=in action=allow protocol=TCP localport=48571`.
  3. Hook it into `.github/workflows/package.yml`.
- **Acceptance criteria**: Installing the `.msi` on a fresh Windows VM
  grants the Firewall rule and leaves a working `flky.exe`.

**Status**: complete.

#### Subtask C2.2 — macOS notarization

- **Edit plan**:
  1. Extend `scripts/package.sh` to sign with Developer ID and submit to
     `notarytool`.
  2. Document the required env vars in `README.md`.
- **Acceptance criteria**: `spctl --assess` on the produced `.dmg` returns
  `accepted`.

**Status**: complete.

### Task C2 status

**Status**: complete.

---

### Task C3 — Auto-start & remote-control toggle

**Goal**: Expose the deferred UX items from the GUI backlog.

**Reference**:

- `docs/gui-pairing-backlog.md` §2.
- `tauri-plugin-autostart` crate.

#### Subtask C3.1 — Auto-start plugin integration

- **Edit plan**: Add `tauri-plugin-autostart` to `flowkey-gui` with a
  settings toggle in the React dashboard and persisted preference.
- **Acceptance criteria**: Toggle survives reboot on both platforms.

**Status**: complete.

#### Subtask C3.2 — Remote-control accept toggle

- **Edit plan**:
  1. Add `config.node.accept_remote_control: bool` to `flowkey-config`.
  2. Gate inbound `SwitchRequest` handling in `bootstrap.rs` on this flag.
  3. Surface the toggle in the GUI settings pane.
- **Acceptance criteria**: With the flag off, a peer's `flky switch`
  returns "rejected" and no state transition occurs.

**Status**: complete.

### Task C3 status

**Status**: complete.

---

### Task C4 — Refactor `native_injector.rs`

**Goal**: Keep `flowkey-input` purely abstract and move OS code into platform
crates.

**Reference**: `crates/flowkey-input/src/native_injector.rs` (1042 lines).

#### Subtask C4.1 — Split per platform

- **Edit plan**:
  1. Move macOS-specific functions into
     `flowkey-platform-macos/src/inject.rs`.
  2. Move Windows-specific functions into
     `flowkey-platform-windows/src/inject.rs`.
  3. `flowkey-input/src/native_injector.rs` becomes a thin facade that
     re-exports per-platform `NativeInputSink`.
- **Acceptance criteria**:
  - `flowkey-input` no longer has `#[cfg(target_os = ...)]` blocks in the
    injector module.
  - `cargo test -p flowkey-input` still passes.

**Status**: complete.

### Task C4 status

**Status**: complete.

---

### Task C5 — Repo hygiene

**Goal**: Remove noise and stale docs.

#### Subtask C5.1 — `.DS_Store` hygiene

- **Edit plan**:
  1. Add `**/.DS_Store` to `.gitignore`.
  2. `git rm --cached .DS_Store` and `crates/.DS_Store` if tracked.
- **Acceptance criteria**: `git status` shows no `.DS_Store` entries.

**Status**: complete.

#### Subtask C5.2 — README refresh

- **Edit plan**: Replace the outdated "Suggested Next Steps" section of
  `README.md` with a pointer to this backlog and remove items 1–5 which are
  already done.
- **Acceptance criteria**: README accurately reflects the current state.

**Status**: complete.

### Task C5 status

**Status**: complete.

---

## Sequencing & Dependencies

```
Phase A   A1 ──► A2 ──► A3 ──► A4      (A1..A4 can run in parallel where noted)
Phase B   B1 ──► B2 ──► B3
                 └────► B4
                        └──► B5        (B5 depends on B2+B3)
Phase C   C1   C2   C3   C4   C5       (all parallelizable once B is stable)
```

Recommended execution order for a single agent: **A1.1 → A1.2 → A1.3 → A3.1
→ A3.2 → A3.3 → A2.1 → A2.2 → A2.3 → A4.1 → B1.1 → B2.1 → B2.2 → B3.1 →
B3.2 → B4.1 → B1.2 → B5.1 → B5.2 → C-tasks in any order → C5**.

## Exit Criteria for V1.0

- All Phase A and Phase B tasks complete.
- `cargo test` green on macOS and Windows CI.
- Manual regression: move / click / drag / wheel / type / hotkey-switch /
  release on both directions without sticky keys.
- A new user can pair, switch, and recover from a Wi-Fi blip without reading
  beyond `README.md` and `flky doctor` output.
