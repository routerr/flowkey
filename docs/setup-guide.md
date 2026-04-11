# flowkey Setup Guide

This guide covers the platform-specific requirements for running `flowkey` on macOS and Windows.

## Interactive Setup

To get started quickly, run the interactive setup command on both devices:
```bash
flky setup
```
This will guide you through:
1.  **Naming your device**: Choose a recognizable name for each machine.
2.  **Configuring the hotkey**: Select or define a custom key combination to switch control.
3.  **Choosing Capture Mode**: 
    -   **Passive** (Recommended): Captures input events while passing them through to the local system.
    -   **Exclusive**: Intercepts and suppresses input events locally when control is switched to a remote peer. This requires higher permissions but provides better isolation.
4.  **Pairing**: Exchange pairing tokens between devices to establish mutual trust.

## macOS Setup

macOS requires explicit user permission for observing and injecting input.

### 1. Accessibility Permission
Needed for injecting input (controlling the macOS machine from a remote peer).
- Open **System Settings**.
- Navigate to **Privacy & Security** > **Accessibility**.
- Click the **+** button or toggle the switch for `flky` (the daemon).

### 2. Input Monitoring Permission
Needed for capturing input (using the macOS machine to control a remote peer).
- Open **System Settings**.
- Navigate to **Privacy & Security** > **Input Monitoring**.
- Click the **+** button or toggle the switch for `flky`.

---

## Windows Setup

Windows requires specific conditions for the daemon to interact with the desktop session.

### 1. Interactive Session
The daemon must be run from an **interactive signed-in desktop session**.
- **DO NOT** run it via SSH or as a background service (unless configured specifically to interact with the desktop).
- Start `flky daemon` from a standard command prompt or PowerShell window while logged in.

### 2. Windows Firewall
By default, Windows Firewall blocks incoming TCP connections.
- You must allow incoming traffic on port **48571**.
- Run this in an Administrator PowerShell to add the rule:
  ```powershell
  New-NetFirewallRule -DisplayName "flowkey" -Direction Inbound -Action Allow -Protocol TCP -LocalPort 48571
  ```

### 3. UIPI (User Interface Privilege Isolation)
If you cannot control certain applications (e.g., Task Manager or elevated installers), you may need to run `flky daemon` as an **Administrator**.

For release builds that request `uiAccess=true`, the binary must be code-signed and installed under `C:\Program Files\...` or another trusted location. Unsigned local debug builds still run, but Windows will ignore the `uiAccess` request.

---

## Diagnostics

If you're having trouble, run the built-in diagnostic tool:
```bash
flky doctor
```
This will check for common permission and network issues.
