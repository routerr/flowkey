# Diagnosis & Fix Plan: Keyboard Not Reaching Mac When Windows Controls Mac

_Created: 2026-04-21_
_Scope: flowkey controller-from-Windows → host-on-Mac keyboard path_

## 1. Context

When a Windows machine controls a remote Mac (mouse + keyboard takeover), the **mouse** works correctly — the Mac cursor mirrors the Windows motion. But the **keyboard does not work at all**:

- Pressing any key on Windows while controlling Mac produces no effect on Mac.
- The key still triggers its normal effect locally on Windows (e.g. `Ctrl+Alt+Del` opens the Windows Task Manager).

### Important caveat about the `Ctrl+Alt+Del` evidence

`Ctrl+Alt+Del` is the Secure Attention Sequence (SAS) on Windows. It is handled by the kernel and Winlogon, **not** by any user-mode input path. No user-mode hook — legitimate or otherwise — can intercept it. So the fact that `Ctrl+Alt+Del` triggers Task Manager is **not** evidence that our keyboard hook failed to install. To judge local suppression correctly, we need to use regular keys: letters, digits, `Enter`, `ArrowLeft`, `Esc`, etc.

The real, actionable symptom is therefore: **arbitrary keystrokes typed on Windows while in the "Controlling" state do not reach the Mac at all**. The focused Mac application does not see any characters, arrow movement, Enter, etc.

### Desired end state

While Windows is controlling Mac, every keystroke on Windows should be:

1. Captured by our low-level keyboard hook on Windows.
2. Serialized as an `InputEvent::KeyDown` / `InputEvent::KeyUp` and sent over the authenticated TCP session.
3. Received on the Mac daemon and injected via CGEvent at HID level into the currently focused app.
4. *Not* be observable locally on Windows (no characters appearing in local Windows apps, no arrow movement on Windows, etc.) — with the unavoidable exception of SAS-class keystrokes.

## 2. Environment & reproduction

- Mac: build with `./build.sh`; run `dist/flowkey.app`. Grant Accessibility + Input Monitoring in System Settings.
- Windows: build with `./build.sh` under MSYS2 UCRT64 bash; install the MSI in `dist/` as admin; launch the installed `flowkey` via the desktop shortcut **as admin**.
- Pair both nodes, confirm "connected" and that diagnostics report `input_injection_backend = native` on both sides.
- Enter control with the hotkey `Ctrl+Alt+Shift+K`. Try to type in a text field on Mac.

## 3. Pipeline map (what code runs for each keystroke)

### Controller side (Windows)

1. `crates/flowkey-platform-windows/src/capture.rs`
   - `WindowsExclusiveCapture::start` spawns `spawn_grab_thread` (line 165). That thread calls `rdev::grab` with a closure.
   - Inside the closure (lines 179–280) every keyboard / mouse event flows through:
     - `state.translate(event, &mut tracker, loopback.as_ref())` at line 196.
     - On `CaptureSignal::Input(input)` (line 208) the event is sent into the mpsc channel (line 234).
     - When `suppression_enabled` is true, the closure returns `None` (suppress locally, lines 235–268). Otherwise it returns `Some(event)` (pass through, line 270).
2. `crates/flowkey-input/src/capture.rs`
   - `CaptureState::translate` (line 263) invokes `translate_event` (line 287), which dispatches keyboard variants to `translate_key_event` (line 331) and builds `InputEvent::KeyDown { code: String, modifiers, timestamp_us }`.
   - A `keyboard_trace` `debug!` event ("captured keyboard event") is emitted here (line 350).
3. `crates/flowkey-input/src/normalize.rs`
   - `normalize_key_code(rdev::Key)` (line 33) maps rdev keys to the protocol string codes (`"KeyA"`, `"Enter"`, `"ArrowLeft"`, …). **Returns `None` for `rdev::Key::Unknown(_)` — which silently suppresses that key**, because `translate_key_event` then returns `None`.
4. `crates/flowkey-daemon/src/platform.rs:166`
   - The daemon thread `capture.wait()`s on the channel. On `CaptureSignal::Input(event)` it looks up the active peer id and calls `sender.send_input(event.clone())` on that peer's `SessionSender` (line 190). If the daemon is not in `Controlling` state, the event is silently dropped (line 178).
5. `crates/flowkey-net/src/connection.rs`
   - `SessionSender::send_input` (line 73): `KeyDown`/`KeyUp` take the `_` arm → `send_immediate_input` (line 268), which `try_send`s on the command channel.
   - The session loop at line 842 receives `SessionCommand::Input(event)`, bumps the sequence, and writes `Message::InputEvent { sequence, event }` on the stream (line 846).

### Host side (Mac)

1. `crates/flowkey-net/src/connection.rs:893`
   - `Message::InputEvent` case — logs `tracing::trace!("received input event")` (line 894), then calls `route_input_event(held_keys, sink, &event)`. **Errors are only `warn!`-logged at line 896 and the event is silently dropped.**
2. `crates/flowkey-input/src/native_injector.rs:96`
   - `handle_input_event`. The `KeyDown`/`KeyUp` arms (lines 99, 129) call `platform::post_key_event(self, code, pressed)` on macOS.
3. `crates/flowkey-input/src/native_injector/macos.rs:180`
   - `post_key_event` looks up the protocol string via `key_code_to_macos_virtual` (table at line 262).
   - If the code is not in the table, it **falls back** to `enigo::Key::Unicode` and logs a `warn!` ("macOS keyboard injection fell back to enigo/unicode path", line 186). For any multi-character unmapped code it returns `Err(...)`.
   - For mapped codes, it builds a `CGEvent::new_keyboard_event`, sets modifier flags, and `event.post(CGEventTapLocation::HID)` (line 237). A `keyboard_trace` `debug!` ("posting macOS keyboard CGEvent") is emitted here (line 225).

### Existing `keyboard_trace` instrumentation (useful for Phase 1)

- `flowkey-input/src/capture.rs:350` — "captured keyboard event" (Windows side, after rdev → protocol translation).
- `flowkey-platform-windows/src/capture.rs:220` — "forwarding keyboard event from Windows capture" (Windows side, just before `sender.send`).
- `flowkey-input/src/native_injector.rs:107` / `:134` — "injecting keyboard event into macOS sink" (Mac side, on the sink boundary).
- `flowkey-input/src/native_injector/macos.rs:225` — "posting macOS keyboard CGEvent" (Mac side, just before `event.post`).
- `flowkey-net/src/connection.rs:894` — `trace!("received input event")` (Mac side, inside the session read loop).

### The observability problem

`flowkey-gui/src/main.rs:305` sets the default `EnvFilter` to:

```
info,flowkey_daemon=debug,flowkey_net=info
```

This means:

- `keyboard_trace` events (which use target `keyboard_trace` but live in `flowkey_input` / `flowkey-platform-windows` / `flowkey-input::native_injector::macos`) are emitted at `debug!`. With the current filter they are **only enabled inside `flowkey_daemon`**. They are therefore **dropped** for all four of the events listed above.
- `flowkey-net` is at `info`, so the `trace!("received input event")` on the Mac side is **also dropped**.

In other words, the existing instrumentation is invisible with the default GUI filter. We must widen the filter before we can diagnose anything from logs.

## 4. Diagnostic decision table

Once tracing is enabled, cross-check which of the existing log events appear on each side when a test key (e.g. `A`, `Enter`, `ArrowLeft`) is pressed:

| # | Windows: "captured keyboard event" | Windows: "forwarding keyboard event from Windows capture" | Mac: "received input event" | Mac: "posting macOS keyboard CGEvent" | Most likely cause |
| --- | --- | --- | --- | --- | --- |
| 1 | no | no | no | no | The rdev WH_KEYBOARD_LL hook isn't firing / isn't installed in the elevated GUI process (Phase 2A). |
| 2 | yes | no | no | no | `normalize_key_code` returned `None` for this rdev key, or the hotkey tracker classified it as `Suppressed` (Phase 2B). |
| 3 | yes | yes | no | no | Event captured but never made it onto the wire — channel full, peer not active, or outbound session not connected (Phase 2C). |
| 4 | yes | yes | yes | no | Transport works; macOS injection mapping / fallback failed (Phase 2D). |
| 5 | yes | yes | yes | yes | The CGEvent was posted but the OS didn't deliver it to the focused app (Phase 2E). |

Regardless of which row we land on, also confirm:
- The `warn!(peer, event)` log that we'll enrich in Phase 1 (step 2) does NOT fire on Mac. If it does, Mac's `sink.handle` is raising an error we can read verbatim.
- Mouse-move events still flow (count matching "forwarding …" and CGEvent mouse traces) so we can compare timing / rates against keyboard.

## 5. Phase 1 — Make the pipeline observable (safe, always-ship)

These are small, minimally invasive changes that make the existing traces visible and the failure modes loud. They should be landed first regardless of the underlying root cause.

### 1. Widen the tracing filter in the GUI entry point

**File:** `crates/flowkey-gui/src/main.rs` — `init_tracing` near line 305.

**Change the default filter from:**

```rust
EnvFilter::new("info,flowkey_daemon=debug,flowkey_net=info")
```

**to:**

```rust
EnvFilter::new("info,flowkey_daemon=debug,flowkey_net=debug,keyboard_trace=trace")
```

`keyboard_trace` is a **target** attached to each keyboard-related `tracing::debug!`/`warn!` — enabling it here means every event with that target is visible regardless of the crate it's emitted from.

### 2. Promote and enrich the receive-side logs

**File:** `crates/flowkey-net/src/connection.rs` near line 893.

**Change:**

```rust
Message::InputEvent { sequence, event } => {
    tracing::trace!(peer = %peer_id, sequence, event = ?event, "received input event");
    if let Err(error) = route_input_event(held_keys, sink, &event) {
        warn!(peer = %peer_id, %error, "input injection failed, continuing session");
    }
}
```

**to:**

```rust
Message::InputEvent { sequence, event } => {
    tracing::debug!(peer = %peer_id, sequence, event = ?event, "received input event");
    if let Err(error) = route_input_event(held_keys, sink, &event) {
        warn!(peer = %peer_id, event = ?event, %error, "input injection failed, continuing session");
    }
}
```

Rationale:
- `trace!` was being filtered out even at `debug` level for `flowkey_net`. Promoting to `debug!` makes it visible under the filter from step 1.
- The injection-failure branch currently swallows *which* event failed. Adding `event = ?event` makes it obvious whether only keyboard fails or also mouse.

### 3. Rebuild on both sides and reproduce

```sh
# Mac
./build.sh
open dist/flowkey.app

# Windows (MSYS2 UCRT64 bash)
./build.sh
# then install the MSI from dist/ as admin, launch from desktop shortcut as admin
```

Pair, hotkey to enter Controlling mode, and type a deterministic sequence on Windows while a Mac text field is focused. E.g.:

```
hello<Enter>world<ArrowLeft><ArrowLeft>X<Esc>
```

Collect logs:

- Windows: `%APPDATA%\flowkey\logs\flowkey.log`
- Mac: `~/Library/Application Support/flowkey/logs/flowkey.log`

(Both are resolved by `Config::log_dir()` in `flowkey-config/src/config.rs:290`.)

Match the events against section 4's decision table and proceed to the corresponding Phase 2 branch.

## 6. Phase 2 — Targeted fixes (pick the branch the logs point at)

Only apply the branch corresponding to the row in the decision table that matches what the logs show. Each branch below is self-contained; fixing more than necessary adds risk.

### 2A. Row 1 — rdev keyboard hook is not firing

`rdev 0.5.3` `src/windows/grab.rs` calls `set_key_hook(raw_callback)` then `set_mouse_hook(raw_callback)` and stores **both** into the same `static mut HOOK: HHOOK`. The second call overwrites the keyboard handle — the keyboard hook is still installed at the OS level but its handle is leaked. This is usually benign (low-level hooks don't need their handle to fire) but it opens two real failure modes:

1. **LowLevelHooksTimeout**: Windows disables any WH_KEYBOARD_LL hook that takes longer than `HKLM\Control Panel\Desktop\LowLevelHooksTimeout` milliseconds (300 ms default) to return. Our callback in `WindowsExclusiveCapture` holds `grab_tracker` and `grab_state` mutexes for the entire body (including logging and sender sends). If any lock contends or a `sender.send` blocks even momentarily, the keyboard hook may be silently disabled while the mouse hook (whose events are cheaper to process per-event) survives.
2. **Hook never installs in this launch mode**: Admin launches via the MSI shortcut can end up in a desktop/session combination where a naive `SetWindowsHookExA` returns NULL for WH_KEYBOARD_LL but succeeds for WH_MOUSE_LL.

Fix candidates, apply in the order listed, stop after the one that works:

1. **Reduce work inside the grab callback.** In `flowkey-platform-windows/src/capture.rs:179–280`, refactor so that:
   - `grab_tracker.lock()` and `grab_state.lock()` are taken, the event is translated, the signal is computed, and the locks are dropped — *before* any `sender.send` and *before* the suppression logic.
   - The `keyboard_trace` `debug!` call (lines 220–232) moves out of the lock scope as well.
   Concretely, scope the locks in a small block whose result is the `Option<CaptureSignal>` and any baseline updates needed for mouse recentering, then match on that outside.
2. **Install the keyboard hook ourselves.** Replace rdev's install of WH_KEYBOARD_LL with a direct `SetWindowsHookExW(WH_KEYBOARD_LL, cb, null_mut(), 0)` call via `windows-sys`, logging `GetLastError()` on NULL return. Keep rdev for mouse only. This both:
   - Avoids the shared-`HOOK` bug in rdev, and
   - Gives a definitive log line telling us whether the hook installed.
   A minimal shim can live next to `spawn_grab_thread` and feed events into the same mpsc channel used today.
3. **Heartbeat the grab thread.** Emit a `keyboard_trace` `debug!` once every N seconds from the thread that owns `GetMessageA`, to confirm the message pump is alive. If the pump dies (e.g. the thread panicked), no keyboard *or* mouse events will flow — but the pump dying after both hooks were installed can look like "mouse kept working" if the pump dies mid-session.

### 2B. Row 2 — rdev `Key::Unknown` / hotkey tracker suppression

If "captured keyboard event" never appears for a specific key, one of:

1. `normalize_key_code` returned `None` — the rdev variant is `Key::Unknown(scan_code)` or a variant not in the table.
2. `HotkeyTracker::process` classified the event as `HotkeyOutcome::Suppressed` because it thinks it's consuming the activation chord. If the user's hotkey is `Ctrl+Alt+Shift+K`, holding any of those three modifiers while typing should be fine (tracker only suppresses once per activation until all chord components are released), but a mis-released modifier can leave `suppress_remaining > 0`.

Fixes:

1. In `flowkey-input/src/capture.rs:translate_key_event` (line 331), add an explicit `warn!(target: "keyboard_trace", physical_key = ?key, "rdev key not mapped, dropping")` on the `None` path *before* returning `None`. Run the repro and read the warn to identify the missing variants. Add them to `flowkey-input/src/normalize.rs` and the corresponding injection tables (`normalize` → protocol, then `key_code_to_macos_virtual` / `parse_key_code` on the inject side).
2. If the `HotkeyTracker` path is guilty, surface a trace from `CaptureState::translate` when it returns `HotkeyOutcome::Suppressed`. Then decide whether the tracker state is correct (it is, for the activation release window) or whether a stuck modifier needs the recovery path already in `held_keys`.

### 2C. Row 3 — captured but never written to the wire

If "forwarding keyboard event from Windows capture" appears on Windows but `"received input event"` never appears on Mac, the gap is between the mpsc channel in `flowkey-daemon` and the TCP write in `flowkey-net`.

Check:

1. `flowkey-daemon/src/platform.rs:167–180` — `active_peer_id` is computed under a lock. If the daemon is briefly NOT in `Controlling { .. }` when the event arrives (e.g. the state toggled back after the hotkey chord released), the event is silently dropped. Add a `warn!` on this fall-through so we see it.
2. `flowkey-net/src/connection.rs:282–292` — `try_send` on a full channel logs `warn!("session channel full; dropping input event")` and returns `Ok(())` (drops silently). Promote this to include a counter so we know whether drops are happening under load. If they are, widen the channel bound or back-pressure sensibly (keyboard shouldn't be dropped even under heavy mouse traffic).

### 2D. Row 4 — received but injection fails on Mac

If "received input event" appears on Mac but "posting macOS keyboard CGEvent" does not, the flow is in `NativeInputSink::handle_input_event` → `post_key_event` and hitting either the fallback or an error. Likely causes and fixes:

1. **`key_code_to_macos_virtual` returning `None`** (table at `flowkey-input/src/native_injector/macos.rs:262`). Add the missing entries. Existing mapping covers the vast majority of printable keys, modifiers, function keys, arrows, numpad, and navigation — but anything we're sending that's not in that table will silently degrade.
2. **Silent fallback to enigo Unicode**. In `post_key_event` (macos.rs:185–207) the `None` branch falls back to `enigo::Key::Unicode` for 1-char codes. This is *almost always wrong* — our protocol uses Keyboard-Event-`code` semantics (physical key), not the character being produced. Change this fallback to an `error!` that returns `Err` so we notice unmapped codes immediately and can fix the table instead of emitting typos via Unicode.
3. **`route_input_event` returning `Err`**. With the Phase 1 step-2 enrichment we'll see `peer / event / error` directly in the log. If the error is from `enigo.key(...)` or `CGEvent::new_keyboard_event`, treat it as a signal to add a missing keycode mapping (#1) or to fix permissions (section 2E).

### 2E. Row 5 — CGEvent is posted but nothing happens on Mac

If the Mac log shows "posting macOS keyboard CGEvent" but no effect is visible in the focused Mac app:

1. **Permissions.** Reconfirm Accessibility + Input Monitoring are both granted for the launched `flowkey.app`. The on-disk path of the `.app` matters — if Mac moved/reinstalled the binary, the permission entry tracks the old path. Quarantine attribute: run `xattr -d com.apple.quarantine dist/flowkey.app` if downloaded, then re-grant.
2. **Event tap ordering.** We post at `CGEventTapLocation::HID` from `HIDSystemState`. For keyboard specifically, the host's own `CGEventTap` (installed by the capture thread in exclusive mode) *will* see the injected event. The loopback suppressor (`crates/flowkey-input/src/loopback.rs`) is supposed to filter it. If loopback matching fails — e.g. because modifiers on capture don't match modifiers on inject — the tap could decide to "suppress" its own injected event in a weird state machine. Add trace logs inside `loopback.should_suppress` for keyboard events and confirm match behavior.
3. **Try posting at Session level for keyboard only.** Mouse must stay at HID (see the macos.rs comments at line 162). For keyboard, `CGEventTapLocation::Session` bypasses the host's own tap and delivers straight to the focused session — at the cost of slightly worse ordering with other HID events. This was tried and reverted once (see the note at macos.rs:213); if Phase 1 proves events reach `post_key_event`, revisit with a standalone reproducer before committing.

## 7. Files likely to change (Phase 1 only — everything in Phase 2 is conditional)

- `crates/flowkey-gui/src/main.rs` — widen the `EnvFilter` default.
- `crates/flowkey-net/src/connection.rs` — promote `trace!("received input event")` to `debug!`, and enrich the `warn!` on `route_input_event` failure to include the event.

No test changes are needed for Phase 1; existing tests in `flowkey-input`, `flowkey-platform-windows`, `flowkey-net` already cover the happy paths and regression risk is near zero.

## 8. Verification plan

1. **Build on both sides.** `./build.sh` on each host, install the MSI on Windows as admin, reinstall the `.app` on Mac with Accessibility + Input Monitoring granted.
2. **Reconnect & confirm state.** Pair; confirm both sides show "connected", `input_injection_backend = native`, and agree on the hotkey binding.
3. **Enter control.** Press `Ctrl+Alt+Shift+K` on Windows. Confirm the daemon status flips to `Controlling` on Windows.
4. **Deterministic typing test.** Focus TextEdit on Mac, then type on Windows:
   ```
   hello<Enter>world<ArrowLeft><ArrowLeft>X<Esc>
   ```
   Expected (after the fix from whichever Phase 2 branch applies): TextEdit on Mac shows `hello\nworlXd` (or similar, depending on where the cursor lands), and **none** of those keystrokes produce any visible effect on Windows itself. Remember: `Ctrl+Alt+Del` is **not** a valid verification key — SAS is kernel-handled and will always trigger Windows regardless of our hook.
5. **Log cross-check.** In both log files, count lines matching `"forwarding keyboard event from Windows capture"` (Windows) and `"posting macOS keyboard CGEvent"` (Mac). They should match, with the correct `code` and `pressed` fields, within a small window.
6. **Negative test.** Release control (repeat hotkey). Type the same sequence on Windows; Windows should respond locally, Mac should *not*.
7. **Regression tests.** Run:
   ```sh
   cargo test -p flowkey-input
   cargo test -p flowkey-platform-windows
   cargo test -p flowkey-net
   ```
   All should pass. No new tests are required for Phase 1.
8. **Stability sanity.** Keep the Controlling session active for several minutes while alternating heavy mouse and keyboard input to ensure neither hook is silently disabled by `LowLevelHooksTimeout`. If either symptom returns mid-session, Phase 2A fix candidate #1 (shrink the grab-callback lock scope) is likely needed regardless of which row the initial log evidence pointed at.
