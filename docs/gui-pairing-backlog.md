# flowkey GUI & Zero-Copy Pairing: Implementation Backlog

This document provides a structured roadmap for building the cross-platform management GUI and the automated pairing protocol.

## 1. Status Summary

- **Phase 1: Zero-Copy Protocol Engine**: ✅ **COMPLETED**
  - SHA3-based SAS code generation implemented.
  - mDNS advertisement extended with `is_pairing` and `pairing_port`.
  - Automated pairing handshake (Propose/Acknowledge) implemented in `flowkey-net`.
- **Phase 2: GUI Foundation & System Tray**: ⏳ **IN PROGRESS**
  - Tauri project scaffolded in `crates/flowkey-gui`. ✅
  - System Tray with menu items implemented. ✅
  - IPC commands for pairing and discovery implemented. ✅
  - Frontend (React/TS) environment setup. ⏳

---

## 2. Remaining Work Backlog

### Phase 3: Management Dashboard UI (Frontend)
**Goal**: Provide a visual "Radar" for pairing and peer management.

#### Task 3.1: Frontend Environment Setup
*   **Subtasks**:
    *   Initialize a Vite/React/TS project in `crates/flowkey-gui/frontend`.
    *   Configure `tauri.conf.json` to point to the frontend dev server.
*   **Acceptance Criteria**: Running the app shows a React-rendered window.

#### Task 3.2: Discovery Radar View
*   **Subtasks**:
    *   Implement a "Searching for devices..." view using `get_discovered_peers`.
    *   Poll or listen for discovery events to update the list in real-time.
*   **Acceptance Criteria**: LAN peers appear in the UI automatically.

#### Task 3.3: One-Click Pairing Flow
*   **Subtasks**:
    *   "Connect" button triggers `connect_to_peer` or `enter_pairing_mode`.
    *   Display the 6-digit SAS code prominently for visual verification.
    *   "Confirm" button triggers `confirm_pairing`.
*   **Acceptance Criteria**: Users can pair two devices without manual token entry.

---

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
