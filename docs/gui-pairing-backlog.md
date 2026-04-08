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
- **Phase 4: Integration & Lifecycle Polish**: ✅ **COMPLETED**
  - Status Bridge & Events: Real-time daemon status updates.
  - Management Features: Peer removal and control switching.
  - Production UX: Dashboard, diagnostics, and control banners.

---

## 2. Future Work (Optional)

### Auto-start & Native Installers
- Integrate `tauri-plugin-autostart`.
- Create `.dmg` (macOS) and `.msi` (Windows) installers using `tauri bundle`.

### Remote Control Mode Toggle
- Add a "Remote Control Mode" switch in the UI to allow remote peers to take control without a local hotkey press (Protocol logic already exists).
