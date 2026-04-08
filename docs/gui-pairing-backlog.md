# flowkey GUI & Zero-Copy Pairing: Implementation Backlog

This document provides a structured roadmap for building the cross-platform management GUI and the automated pairing protocol.

## 1. Status Summary

- **Phase 1: Zero-Copy Protocol Engine**: ✅ **COMPLETED**
  - SHA3-based SAS code generation implemented.
  - mDNS advertisement extended with `is_pairing` and `pairing_port`.
  - Automated pairing handshake (Propose/Acknowledge) implemented in `flowkey-net`.
- **Phase 2: GUI Foundation & System Tray**: ✅ **COMPLETED**
  - Tauri project scaffolded in `crates/flowkey-gui`.
  - System Tray with menu items implemented.
  - IPC commands for pairing and discovery implemented.
- **Phase 3: Management Dashboard UI (Frontend)**: ✅ **COMPLETED**
  - Frontend environment setup (Vite/React/TS).
  - Discovery Radar UI implemented.
  - visual Zero-Copy Pairing flow with 6-digit SAS implemented.
- **Phase 4: Integration & Lifecycle Polish**: ⏳ **IN PROGRESS**
  - Status Bridge & Events. ⏳
  - Auto-start & Permissions Visualizer. ⏳
  - Accept Toggle Control. ⏳

---

## 2. Remaining Work Backlog

### Phase 4: Integration & Lifecycle Polish
**Goal**: Finalize background behaviors and production readiness.

#### Task 4.1: Status Bridge & Events
*   **Subtasks**:
    *   Implement an event stream from Rust to JS to update `DaemonStatus` (connected peer, heartbeat status, etc.).
    *   Refactor the tray menu to dynamically show the active peer.
*   **Acceptance Criteria**: UI reflects the daemon state in real-time.

#### Task 4.2: Auto-start & Permissions Visualizer
*   **Subtasks**:
    *   Integrate `tauri-plugin-autostart`.
    *   Add a "Doctor" tab in the GUI to show permission status (Accessibility, etc.).
*   **Acceptance Criteria**: Users can easily diagnose setup issues from the GUI.

#### Task 4.3: Accept Toggle Control
*   **Subtasks**:
    *   Implement a "Remote Control Mode" toggle.
    *   If enabled, allow remote peers to take control without a local hotkey press.
*   **Acceptance Criteria**: Device A can take control of Device B with one click.
