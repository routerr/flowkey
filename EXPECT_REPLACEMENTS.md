# Fix-Expect-Calls: Replacement Summary

## Overview
Replaced all `.expect()` panic calls with `map_err()` + logging in production critical paths to prevent daemon crashes from mutex poisoning.

## Files Modified

### 1. `crates/flowkey-daemon/src/bootstrap.rs`
**Total expect() replacements: 2**

**Imports updated:**
- Added `anyhow` to `use anyhow::{anyhow, Context, Result}`

**Changes:**
1. **Line 70-77**: RuntimeSnapshot creation
   - Pattern: `map_err() + error!() logging + anyhow!()`
   - On error: Returns error and logs "daemon state unavailable"

2. **Line 305-310**: Daemon runtime final state reporting
   - Pattern: `map_err() + error!() logging + anyhow!()`
   - On error: Returns error and logs "daemon state unavailable"

### 2. `crates/flowkey-daemon/src/session_flow.rs`
**Total expect() replacements: 10**

**Imports updated:**
- Added `use anyhow::anyhow`
- Updated `use tracing::{error, info, warn}`

**Changes by function:**

1. **on_remote_switch() callback (lines 45-55, 64-72)**
   - Lock result mutex: `match self.runtime.lock()`
   - On error: Logs error, returns "daemon state unavailable"
   - Graceful fallback with `DaemonState::Disconnected`

2. **on_remote_release() callback (lines 92-137)**
   - Same pattern as on_remote_switch
   - Locks daemon runtime safely with match

3. **setup_and_run_session() (lines 157-220)**
   - Line 157-164: Mark authenticated call
   - Line 175-182: Toggle controller call
   - Line 191-201: Session sender registration
   - All use match-based error handling with graceful returns
   - Early returns with 0 Duration on critical failures

4. **cleanup_session() (lines 285-328)**
   - Line 285-292: Remove sender from registry
   - Line 322-328: Mark disconnected
   - Returns early on mutex poisoning

5. **mark_lost_session() (lines 343-361)**
   - Line 343-350: Remove sender from registry
   - Line 355-361: Mark disconnected
   - Returns early on mutex poisoning

### 3. `crates/flowkey-net/src/connection.rs`
**Total expect() replacements: 4**

**Imports updated:**
- Updated `use tracing::{error, info, trace, warn}`

**Changes by method:**

1. **queue_mouse_move() (line 173-180)**
   - Coalescer lock: `match self.coalescer.lock()`
   - On error: Logs error, marks channel closed, returns error

2. **queue_scroll() (line 233-243)**
   - Same pattern as queue_mouse_move

3. **send_immediate_input() (line 284-291)**
   - Same pattern as queue_mouse_move

4. **send_control_command() (line 312-328)**
   - Same pattern as queue_mouse_move

5. **spawn_flush_worker() (lines 398-436)**
   - Three separate lock operations:
     - Initial coalescer lock (398-404)
     - Condvar wait (412-418)
     - Wait with timeout (429-435)
   - All errors logged and break 'worker to exit gracefully

## Error Handling Pattern

### Common pattern for production paths:
```rust
// Before
let mut runtime = self.runtime
    .lock()
    .expect("daemon runtime mutex should not be poisoned");

// After
let mut runtime = match self.runtime.lock() {
    Ok(runtime) => runtime,
    Err(e) => {
        error!("daemon runtime mutex poisoned: {}", e);
        warn!(peer = %peer_id, "failed to update state due to mutex poisoning");
        return;  // or return default value
    }
};
```

## Error Recovery Behavior

1. **Daemon callbacks (on_remote_switch, on_remote_release):**
   - Log error at ERROR level
   - Return with default error state
   - Allow session to continue with safe defaults

2. **Setup/cleanup functions:**
   - Log error at ERROR level  
   - Warn with context (peer_id, operation)
   - Return early with safe defaults (Duration::from_secs(0))
   - Prevent cascading failures

3. **Input coalescer locks:**
   - Log error at ERROR level
   - Mark channel as closed to prevent further operations
   - Return error to caller
   - Worker threads break out of loop on poisoning

## Test Code
Test methods in `crates/flowkey-core/src/daemon.rs` retain `.expect()` calls:
- Lines 232, 240, 269, 287, 306 (daemon.rs tests)
- These are acceptable as they are test-only code paths

## Benefits

1. **Daemon Availability:** No more daemon crashes from mutex poisoning
2. **Graceful Degradation:** Failed operations return recoverable errors
3. **Observability:** All errors logged with context
4. **Production Safety:** Critical paths have defensive error handling
5. **Maintained Functionality:** Sessions continue under degraded conditions

## Validation

All replacements follow the pattern of:
1. Using `match` on `lock()` result
2. Logging errors with tracing::error!()
3. Including contextual information (peer_id, operation)
4. Returning safe defaults instead of panicking
5. Allowing graceful recovery from mutex poisoning
