# flowkey Optimization: Stability & Smoothness Analysis

This document outlines the technical analysis of the current `flowkey` codebase and provides a structured, agent-friendly backlog to improve operational smoothness and connection stability.

---

## 1. Technical Analysis & Inspection Results

### 1.1 Protocol Efficiency (The "JSON Overhead")
*   **Observation**: Currently, `crates/flowkey-net/src/frame.rs` uses `serde_json` to serialize every `InputEvent`.
*   **Impact**: High-frequency events (mouse moves at 1000Hz) generate significant string manipulation overhead and larger-than-necessary packets (approx. 100-200 bytes per move).
*   **Recommendation**: Transition to a compact binary format like `Bincode`.

### 1.2 Network Smoothness (The "TCP Jitter" & "Head-of-Line Blocking")
*   **Observation**: The system uses a single TCP stream with `unbounded_channel` for the session.
*   **Impact**: If a single packet is lost on Wi-Fi, the entire input stream stalls (Head-of-Line Blocking). Unbounded channels can grow indefinitely if the network is slower than the input rate, leading to "input lag" that catches up all at once.
*   **Recommendation**: Implement bounded channels with a "Last-Event-Wins" dropping policy for mouse movements and add capture-time timestamps for jitter compensation.

### 1.3 Command Latency (The "150ms Polling Loop")
*   **Observation**: `crates/flowkey-core/src/switching.rs` and `flowkey-daemon/src/bootstrap.rs` poll a TOML file every 150ms to receive commands from the CLI.
*   **Impact**: Artificial delay when switching devices via `flky switch`.
*   **Recommendation**: Replace file polling with a Local Domain Socket (macOS) or Named Pipe (Windows).

### 1.4 State Robustness (The "UIPI & Mutex" Issues)
*   **Observation**: Windows input injection is currently blocked by elevated (Admin) windows if the daemon is unprivileged. The `DaemonRuntime` uses a single large `Mutex` for all state.
*   **Impact**: Inconsistent control on Windows; potential UI stutter during status snapshotting.
*   **Recommendation**: Add a Windows manifest for UIPI access and refactor the `Mutex` into smaller, task-specific guards.

---

## 2. AI-Agent Friendly Work Backlog

### Phase 1: High-Efficiency Protocol & Timestamps
**Goal**: Reduce serialization overhead and enable jitter detection.

#### Task 1.1: Binary Protocol Migration
*   **Subtasks**:
    *   Add `bincode` to `crates/flowkey-protocol/Cargo.toml`.
    *   Update `crates/flowkey-net/src/frame.rs` to use `bincode` instead of `serde_json`.
*   **Reference Files**: `crates/flowkey-net/src/frame.rs`, `crates/flowkey-protocol/src/message.rs`
*   **Code Change Proposal**:
    ```rust
    // crates/flowkey-net/src/frame.rs
    pub async fn write_message(stream: &mut TcpStream, message: &Message) -> Result<()> {
        let payload = bincode::serialize(message)?; // Replacement
        // ... header logic stays similar but uses binary length
    }
    ```
*   **Acceptance Criteria**:
    *   `cargo test -p flowkey-net` passes.
    *   Packet size for `MouseMove` is reduced by >60%.

#### Task 1.2: Capture Timestamps
*   **Subtasks**:
    *   Add `timestamp_us: u64` to `InputEvent` in `flowkey-protocol`.
    *   Populate `timestamp_us` in `CaptureState::translate_event` using `SystemTime`.
*   **Reference Files**: `crates/flowkey-protocol/src/input.rs`, `crates/flowkey-input/src/capture.rs`
*   **Acceptance Criteria**: All captured events contain a non-zero microsecond timestamp.

---

### Phase 2: Network Robustness & Congestion Control
**Goal**: Prevent "Lag Spikes" and memory growth during network congestion.

#### Task 2.1: Bounded Channels & Event Dropping
*   **Subtasks**:
    *   Change `session_channel` in `flowkey-net` to use `tokio::sync::mpsc::channel(100)`.
    *   Update `SessionSender::send_input` to drop redundant `MouseMove` events if the channel is full.
*   **Reference Files**: `crates/flowkey-net/src/connection.rs`
*   **Code Change Proposal**:
    ```rust
    // Logic in SessionSender
    pub fn send_input(&self, event: InputEvent) -> Result<()> {
        if matches!(event, InputEvent::MouseMove { .. }) {
            self.sender.try_send(SessionCommand::Input(event)).ok(); // Non-blocking, drop if full
        } else {
            self.sender.blocking_send(SessionCommand::Input(event))?; // Critical events must wait
        }
        Ok(())
    }
    ```
*   **Acceptance Criteria**: The daemon does not increase memory usage during simulated 50% packet loss.

#### Task 2.2: Mouse Event Batching
*   **Subtasks**:
    *   Implement a 5-10ms "coalescing" window in the capture loop that combines multiple small `MouseMove` deltas into one larger delta before sending.
*   **Acceptance Criteria**: Average packet rate during fast mouse movement stays below 150 packets/sec without losing total distance moved.

---

### Phase 3: Operational Smoothness (IPC & Mutex)
**Goal**: Instant CLI response and lock-free status reporting.

#### Task 3.1: IPC for Daemon Control
*   **Subtasks**:
    *   Replace `spawn_control_watcher` in `bootstrap.rs` with a UDS/Named Pipe listener.
    *   Update `flowkey-cli` to send commands over the socket instead of writing TOML files.
*   **Reference Files**: `crates/flowkey-daemon/src/bootstrap.rs`, `crates/flowkey-cli/src/main.rs`
*   **Acceptance Criteria**: `flky switch` command responds in <10ms instead of 150ms.

#### Task 3.2: Atomic Status Reporting
*   **Subtasks**:
    *   Replace the `Mutex<DaemonRuntime>` snapshot pattern with `ArcSwap` or atomic fields for frequently read status (e.g., `state`, `active_peer_id`).
*   **Acceptance Criteria**: `flky status` does not block if the network thread is busy.

---

### Phase 4: Platform-Specific Polish
**Goal**: Resolve OS-level injection and capture edge cases.

#### Task 4.1: Windows UIPI & Manifest
*   **Subtasks**:
    *   Add an `app.manifest` to `flowkey-cli` requesting `uiAccess="true"`.
    *   Sign the binary (required for `uiAccess`).
*   **Acceptance Criteria**: The daemon can control a Task Manager window running as Administrator.

#### Task 4.2: MacOS Input Monitoring Performance
*   **Subtasks**:
    *   Optimize `MacosCapture` to avoid `lock().unwrap()` inside the `CGEventTap` callback by using a `LockFreeQueue` (e.g., `crossbeam-channel`).
*   **Acceptance Criteria**: Zero "Event tap timed out" messages in system logs during heavy CPU load.
