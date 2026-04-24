#!/bin/bash
set -e

cd /Users/raychang/repo/flowkey

# Stage all changes
git add -A

# Create commit with comprehensive message
git commit -m "refactor: comprehensive code quality improvements (8 fixes)

- fix: Replace 31 .expect() panic calls with error recovery (fix-expect-calls)
  * flowkey-daemon/src/bootstrap.rs
  * flowkey-daemon/src/session_flow.rs
  * flowkey-net/src/connection.rs
  * flowkey-daemon/src/status_writer.rs
  * flowkey-daemon/src/platform.rs
  * flowkey-daemon/src/control_ipc.rs
  Prevents daemon crashes from mutex poisoning; enables graceful degradation

- refactor: Extract hardcoded constants to named consts (extract-magic-numbers)
  * Added LOOPBACK_SUPPRESSION_MS = 40
  * Added INITIAL_RECONNECT_BACKOFF_SECS = 1
  * Added MAX_RECONNECT_BACKOFF_SECS = 8
  * Reused DEFAULT_INPUT_COALESCE_WINDOW_MS = 4
  Improves readability and centralized configuration

- feat: Log dropped input events at session end (log-coalescer-drops)
  * flowkey-net/src/connection.rs: Add dropped_inputs() accessor
  * flowkey-daemon/src/session_flow.rs: Log drops at session close
  Provides visibility into input loss for debugging

- refactor: Consolidate platform socket/pipe code (cleanup-platform-code)
  * Created flowkey-platform-macos/src/control_ipc.rs
  * Created flowkey-platform-windows/src/control_ipc.rs
  * Refactored flowkey-cli/src/main.rs to use platform abstractions
  * Refactored flowkey-gui/src/main.rs to use platform abstractions
  Eliminated 4 instances of duplicated platform code

- refactor: Extract mutex lock-read helpers (extract-lock-helpers)
  * Added apply_transition_with_state_snapshot() in session_flow.rs
  * Added read_state() readonly helper
  * Reduced callback code duplication by 60%+
  Improves lock semantics and reduces contention

- refactor: Standardize error handling to anyhow::Result (standardize-error-types)
  * Unified all Result types in connection.rs and session_flow.rs
  * Changed Result<T, String> → anyhow::Result<T>
  * Updated 8+ method signatures and error handling paths
  Enables better error context with .context() chaining

- refactor: Add state invariant validation (add-state-validation)
  * Added validation to mark_authenticated()
  * Added validation to mark_disconnected()
  * Checks for empty peer_ids, duplicate sessions
  Prevents silent state corruption

- refactor: Consolidate callback handlers (consolidate-callbacks)
  * Extracted apply_state_transition_and_log() template
  * Refactored on_remote_switch() to 10 lines (was 50)
  * Refactored on_remote_release() to 10 lines (was 50)
  Eliminates 80+ lines of duplicated code

Co-authored-by: Copilot <223556219+Copilot@users.noreply.github.com>"

# Push to origin
git push origin main

echo "✅ Commit and push successful!"
