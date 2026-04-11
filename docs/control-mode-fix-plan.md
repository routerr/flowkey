# Control Mode: Analysis & Fix Plan

> Audience: AI coding agents executing this plan in follow-up sessions.
> Status: Draft. Not yet implemented. Exploration only — no code changes made.
> Date: 2026-04-12

## 1. User-Reported Symptoms

1. The GUI "Control" button (per-peer, in Trusted Peers list) never works on either platform.
2. The hotkey `Ctrl+Shift+Alt+K` only triggers on macOS. Windows never toggles control via hotkey.
3. Mac → Windows control works when initiated via the Mac hotkey.
4. The "Release Control" button never works.
5. Desired behavior:
   - Control mode must be **explicit**. While controlling a remote, local keyboard/mouse should drive **only** the remote (fully consumed locally).
   - Control must be releasable from the **remote (controlled) device** — either by a UI button or by the same `Ctrl+Shift+Alt+K` toggle.

## 2. Architecture Recap (read before editing)

- `flowkey-gui` (Tauri app) spawns the daemon **in-process** via `flowkey_daemon::spawn_supervised` (`crates/flowkey-gui/src/main.rs:322-331`, `crates/flowkey-daemon/src/supervisor.rs:41-45`).
- Daemon boots `spawn_hotkey_watcher` (local input capture + hotkey handling) and `spawn_control_watcher` (IPC channel from GUI) in `crates/flowkey-daemon/src/bootstrap.rs:112-130`.
- GUI → daemon control commands travel over:
  - macOS: Unix socket at `Config::control_path().with_extension("sock")`
  - Windows: Named pipe at `config.control_pipe_name()`
  - See `crates/flowkey-gui/src/main.rs:179-238` and `crates/flowkey-daemon/src/control_ipc.rs:22-172`.
- State machine lives in `crates/flowkey-core/src/daemon.rs` (`DaemonState`, `DaemonRuntime`).
- Status is serialized as a kebab-case string (`crates/flowkey-core/src/status.rs:65-79`): `disconnected`, `connected-idle`, `controlling`, `controlled-by`, `recovering`.
- Local input capture abstractions:
  - Trait: `InputCapture` in `crates/flowkey-input/src/capture.rs:21-29`.
  - Cross-platform fallback (rdev-based, **passive only**): `LocalInputCapture` in the same file.
  - macOS exclusive implementation (CGEventTap): `crates/flowkey-platform-macos/src/capture.rs` (`MacosCapture`).
  - Windows: **no platform-specific capture exists**. `crates/flowkey-platform-windows/src/hotkey.rs` only wraps `HotkeyBinding::parse`; it does not hook anything.
- Capture mode config: `CaptureMode::Passive | Exclusive`, default `Passive` (`crates/flowkey-config/src/config.rs:36-47`).

## 3. Root-Cause Analysis

### RC1 — Windows hotkey never triggers (and Windows can never run exclusive capture)

**File references:**
- `crates/flowkey-daemon/src/platform.rs:254-267` selects `LocalInputCapture` on Windows.
- `crates/flowkey-input/src/capture.rs:165-167` — `LocalInputCapture::set_suppression_enabled` is a no-op by design ("LocalInputCapture is passive and does not support suppression").
- `crates/flowkey-input/src/capture.rs:174-196` uses `rdev::listen`.

**Why it fails:**
- `rdev::listen` on Windows is known to have quirks with modifier tracking under global hooks, and — more importantly — it is purely observational. Events cannot be consumed at the OS level.
- So even if the hotkey *is* detected, (a) the hotkey key sequence is still delivered to whatever app has focus, and (b) during "Controlling" state local input cannot be dropped, so the local machine and remote machine both receive input.
- There is no replacement for `MacosCapture` on Windows; the daemon has no reliable way to register or consume a hotkey there.

### RC2 — "Control" and "Release Control" buttons appear to do nothing

**Surface-level wiring is correct.** `switchToPeer` → Tauri command `switch_to_peer` → writes `DaemonCommand::Switch` over the IPC stream (`crates/flowkey-gui/src/main.rs:179-207`). The daemon's `handle_control_command` (`crates/flowkey-daemon/src/control_ipc.rs:198-289`) releases any prior session, calls `select_active_peer`, then `toggle_controller`. Structurally this should work.

**But these silent-failure paths exist:**

1. **IPC errors are swallowed and fallback to file path**
   `crates/flowkey-gui/src/main.rs:180-207` unconditionally falls through to `cmd.save_to_path(&control_path)` if the socket/pipe path errors out. On macOS/Windows that fallback path is **not watched** by `spawn_control_watcher` (only the `#[cfg(not(any(target_os = "macos", target_os = "windows")))]` branch reads files — `control_ipc.rs:138-170`). Result: a broken IPC returns `Ok(())` to the UI but the daemon never sees the command.

2. **`select_active_peer` rejects any peer that isn't already authenticated**
   `crates/flowkey-core/src/daemon.rs:110-135`. If the user clicks "Control" on a trusted peer whose session is not currently alive (never connected, recovering, lost), the command errors inside the daemon with `"active peer must already be authenticated"`, but the error is only logged (`control_ipc.rs:66` `warn!`) — never propagated back through the IPC stream to the GUI.

3. **Release banner visibility is too narrow**
   `crates/flowkey-gui/frontend/src/App.tsx:247-252`:
   ```tsx
   {status?.state.startsWith('controlling') && (
     <div className="active-control-banner">
       <button onClick={releaseControl}>Release Control</button>
   ```
   The banner (and therefore the only Release button) only renders when *this* node is in `controlling` state. A node in `controlled-by` state sees **no** release affordance — this is the biggest reason users feel "the release button never works".

4. **Release from the controlled side has no wire protocol**
   Even if we show a button on the controlled side, there is currently no protocol message to tell the *controller's* daemon to exit its `Controlling` state. `DaemonCommand::Release` only operates locally; it does not flow across the network session.

### RC3 — "Explicit" exclusive control is macOS-only and not on by default

- `crates/flowkey-config/src/config.rs:43-47` defaults `CaptureMode` to `Passive`.
- macOS `raw_callback` already supports event swallowing when exclusive + suppression enabled (`crates/flowkey-platform-macos/src/capture.rs:230-244`, via `cg_event.set_type(CGEventType::Null)`), gated by the `suppression_enabled` atomic that the hotkey watcher toggles on entering `Controlling` (`crates/flowkey-daemon/src/platform.rs:126-133`).
- However, the **IPC path (Control button, Release button)** never calls `capture.set_suppression_enabled(...)`. The `suppression_state: Arc<AtomicBool>` held by `control_ipc.rs` is a parallel flag that the capture never reads. So even on macOS, pressing the Control button in the UI enters "Controlling" state without flipping the tap into exclusive mode.
- Windows has no exclusive-mode mechanism at all.

### RC4 — Controlled side has no hotkey/UI escape hatch

- Hotkey watcher only reacts to `CaptureSignal::HotkeyPressed` and calls `toggle_controller`. On the controlled side, `toggle_controller` from `ControlledBy` currently transitions to `ConnectedIdle` (`daemon.rs:156-159`) — good — but this relies on the capture loop actually receiving the hotkey, which on Windows it doesn't, and on macOS the exclusive tap may be suppressing the wrong side's events.
- There is no cross-node protocol message that the controlled side can send to request the controller to release.

## 4. Fix Plan

Phases are ordered from lowest-risk/highest-leverage to largest engineering effort. Each phase leaves the tree compilable and testable.

### Phase A — Minimal usability fixes (low risk)

**Goal:** Make the existing happy path visible and honest; make Release work from both sides (at least locally).

- **A1. Surface IPC errors to the GUI.**
  File: `crates/flowkey-gui/src/main.rs:179-238` (`switch_to_peer`, `release_control`).
  - Remove the silent `save_to_path` fallback on macOS/Windows; instead return `Err` when IPC fails so the React layer can display it via `setError`.
  - Keep the file-based fallback only for non-macOS/non-Windows builds.
  - Verify: break the socket path temporarily, click Control, confirm error bar displays.

- **A2. Show Release banner in BOTH `controlling` and `controlled-by` states.**
  File: `crates/flowkey-gui/frontend/src/App.tsx:247-252`.
  - Change the conditional to `status?.state === 'controlling' || status?.state === 'controlled-by'`.
  - Show distinct copy per role ("Currently controlling X" vs. "Currently controlled by X").
  - Wire the button to `release_control` for both roles. (For `controlled-by`, the local daemon releasing is a best-effort until Phase C lands; it will at least move local state to `ConnectedIdle` and stop accepting injected input via the protocol layer once A3 ships.)

- **A3. Local release must stop accepting injected input.**
  File: `crates/flowkey-daemon/src/session_flow.rs` (`DaemonSessionCallback`), `crates/flowkey-daemon/src/control_ipc.rs:257-289` (Release handler).
  - After `runtime.release_control()` on a `ControlledBy` transition, the session callback must reject further `InputEvent` frames from the peer until the peer re-initiates.
  - The existing `accept_remote_control` config flag gates *initial* acceptance; we need a runtime-scoped gate that flips when the controlled side releases.
  - Verify with a unit test on the session callback (mock peer pushing input after release).

- **A4. Flip `CaptureMode` default to `Exclusive`.**
  File: `crates/flowkey-config/src/config.rs:43-47` and existing-config migration paths at lines 309 and 330.
  - Existing users keep their saved value; only the generated default changes.
  - This makes macOS explicit by default. Windows still has no effect until Phase B.

- **A5. IPC path should flip capture suppression too.**
  File: `crates/flowkey-daemon/src/control_ipc.rs` — thread the capture handle (or a `SetSuppression` callback) into `spawn_control_watcher`, or expose `suppression_state` to the capture directly.
  Simpler option: have `MacosCapture` read `suppression_state` directly (the `Arc<AtomicBool>` is already passed in at `platform.rs:248`). Verify that the atomic flipped in `control_ipc.rs:231` is the same `Arc` the tap sees. If so, this is already wired and we only need to confirm the tap is in `exclusive=true` mode (requires A4 to default to `Exclusive`).

### Phase B — Windows exclusive capture + reliable hotkey (largest change)

**Goal:** Create a Windows analogue of `MacosCapture` so the hotkey and exclusive mode both work.

- **B1. New file `crates/flowkey-platform-windows/src/capture.rs`.**
  - Define `WindowsCapture` implementing `flowkey_input::capture::InputCapture`.
  - Use `SetWindowsHookExW(WH_KEYBOARD_LL, ...)` and `SetWindowsHookExW(WH_MOUSE_LL, ...)`.
  - Run a dedicated thread that owns a message loop (`GetMessageW` / `DispatchMessageW`). The hook callback is a C-ABI `extern "system" fn` that forwards into a Rust closure via a thread-local `TapContext` (mirror `MacosCapture::TapContext`).
  - Track modifier state from the hook's `KBDLLHOOKSTRUCT.vkCode` plus `GetAsyncKeyState` as a fallback.
  - Reuse the shared `HotkeyTracker`, `CaptureState` from `flowkey-input`.
  - When `exclusive == true && suppression_enabled.load()`, return a **non-zero** value from the hook proc (`return 1;`) to drop the event; otherwise return `CallNextHookEx(...)`.
  - Provide `capture_restart_counter()` so diagnostics keep working.

- **B2. Wire it into `create_platform_input_capture` on Windows.**
  File: `crates/flowkey-daemon/src/platform.rs:254-267`.
  - Replace `flowkey_input::capture::LocalInputCapture::with_loopback(...)` with `flowkey_platform_windows::capture::WindowsCapture::with_loopback(binding, loopback, exclusive, suppression_state)`.
  - Mirror the note-emitting pattern used for macOS so diagnostics show "exclusive capture mode enabled" on Windows.

- **B3. UAC / interactive session guardrails.**
  File: `crates/flowkey-daemon/src/bootstrap.rs:49-56` already warns when `!permissions.user_session`. Extend the runtime note so the GUI surfaces "hotkey disabled: daemon is not in an interactive desktop session" explicitly.

- **B4. Tests.**
  - Unit test for the Windows modifier-tracker logic (pure Rust, no OS calls).
  - Manual test matrix documented in the PR description: hotkey from Windows when idle, while controlled, while controlling; explicit exclusive drop.

### Phase C — Cross-node "release from remote" protocol

**Goal:** The controlled device can tell the controlling device to release, so the "explicit mode" feels symmetric.

- **C1. New protocol message.**
  File: `crates/flowkey-protocol/src/message.rs`.
  - Add `SessionCommand::RequestRelease { request_id }` (or similar name — match existing kebab-case).
  - Bump any version/handshake check if the protocol has one (see `flowkey-crypto/src/handshake.rs` and `flowkey-protocol/src/lib.rs`).

- **C2. Sender path on the controlled side.**
  File: `crates/flowkey-daemon/src/control_ipc.rs` Release handler.
  - When local state is `ControlledBy` at the time of the release, additionally send `SessionSender::send_request_release(...)` to the active peer so the controller's daemon exits `Controlling`.
  - Alternatively, add a separate `DaemonCommand::RequestRemoteRelease` so the GUI can invoke it explicitly without also tearing down local state prematurely.

- **C3. Receiver path on the controlling side.**
  File: `crates/flowkey-net/src/connection.rs` (session loop) and `crates/flowkey-daemon/src/session_flow.rs`.
  - On receiving `RequestRelease`, call `runtime.release_control()`, notify the peer with the existing `send_release` flow, flip suppression off.

- **C4. UI wire-up.**
  File: `crates/flowkey-gui/frontend/src/App.tsx`.
  - On `controlled-by`, the Release button invokes a new Tauri command `request_remote_release` (or reuses `release_control` if Phase C collapses the two commands).

### Phase D — Hardening (optional, after B ships)

- Add an integration test that boots two in-process daemons on loopback and exercises:
  1. macOS-side hotkey → controlling → release via hotkey.
  2. GUI Control button → controlling → GUI Release button.
  3. `controlled-by` side presses hotkey → controller exits `Controlling` (requires C).
- Consider replacing the `Arc<AtomicBool>` suppression_state threading with a proper `CaptureController` handle that all code paths (hotkey, IPC, session) can call.

## 5. Files the implementer will touch (quick index)

| Phase | File | Purpose |
|-------|------|---------|
| A1 | `crates/flowkey-gui/src/main.rs` | Return IPC errors to UI |
| A2 | `crates/flowkey-gui/frontend/src/App.tsx` | Banner for both roles |
| A3 | `crates/flowkey-daemon/src/session_flow.rs`, `control_ipc.rs` | Runtime remote-input gate |
| A4 | `crates/flowkey-config/src/config.rs` | Default `Exclusive` |
| A5 | `crates/flowkey-daemon/src/control_ipc.rs`, `platform.rs` | Verify suppression atomic is shared |
| B1 | `crates/flowkey-platform-windows/src/capture.rs` (new) | WindowsCapture impl |
| B2 | `crates/flowkey-daemon/src/platform.rs` | Use WindowsCapture |
| B3 | `crates/flowkey-daemon/src/bootstrap.rs` | Surface session warning |
| C1 | `crates/flowkey-protocol/src/message.rs` | `RequestRelease` message |
| C2 | `crates/flowkey-daemon/src/control_ipc.rs` | Send from controlled side |
| C3 | `crates/flowkey-net/src/connection.rs`, `session_flow.rs` | Handle on controller |
| C4 | `crates/flowkey-gui/frontend/src/App.tsx` | Invoke new command |

## 6. Verification checklist

For each phase, after implementation:

- [ ] `cargo fmt --all`
- [ ] `cargo clippy --all-targets -- -D warnings`
- [ ] `cargo test --workspace`
- [ ] macOS smoke test: hotkey toggle, UI Control button, UI Release button, exclusive local suppression.
- [ ] Windows smoke test (Phase B onward): same four behaviors.
- [ ] Cross-platform: initiate from each side, release from each side.
- [ ] Confirm `daemon.log` shows `hotkey switched daemon role` and `daemon control request applied` for both hotkey and IPC paths.

## 7. Out of scope (explicit non-goals)

- Rewriting the network/session layer.
- Changing the pairing / discovery flow.
- Adding a new input-injection backend on either platform.
- Touching `daemon.log` rotation or logging infrastructure.
