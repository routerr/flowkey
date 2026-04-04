# Key Mouse Sharer V1 CLI and Protocol Spec

## Scope

This document defines a minimal V1 CLI and a matching peer-to-peer daemon protocol for:

- same-LAN communication
- paired peers only
- logged-in desktop sessions only
- keyboard and mouse forwarding only

## Terminology

- `node`: one running installation of the daemon on a machine
- `peer`: another trusted node
- `controller`: the node currently capturing and sending input
- `controlled`: the node currently receiving and injecting input
- `session`: one active encrypted connection between two paired nodes

## CLI Spec

## Command Summary

```text
kms daemon
kms pair init
kms pair accept <token>
kms peers list
kms switch <peer-id>
kms release
kms status
```

## `kms daemon`

Starts the background daemon in the foreground terminal session.

### Behavior

- loads config
- checks local identity
- opens listen socket
- starts protocol loop
- initializes platform capture/injection components
- prints concise startup diagnostics

### Example

```bash
kms daemon
```

## `kms pair init`

Creates a pairing offer token for another machine.

### Behavior

- ensures local node identity exists
- creates a short-lived pairing offer
- prints a copyable token

### Example

```bash
kms pair init
```

### Example Output

```text
Pairing token:
v1.pair.E3K9-9W2M.192.168.1.10:48571.base64payload
```

## `kms pair accept <token>`

Accepts a pairing token from another machine and stores trust.

### Behavior

- validates token format and expiry
- records peer address and public key
- marks peer as trusted
- optionally prints a local confirmation code for symmetric trust if required

### Example

```bash
kms pair accept v1.pair.E3K9-9W2M.192.168.1.10:48571.base64payload
```

## `kms peers list`

Lists known peers.

### Example Output

```text
ID          NAME         ADDRESS             TRUSTED
office-pc   Office PC    192.168.1.25:48571 yes
```

## `kms switch <peer-id>`

Requests that the local daemon enter controller mode toward the selected peer.

### Behavior

- validates that the peer is trusted and authenticated
- writes a local control request into the config directory
- the daemon polls that request file and applies the switch while it is running

This command is primarily useful for scripting and diagnostics. The normal UX is still the configured hotkey.

## `kms release`

Returns control to the local machine and forces release of tracked remote input state.

### Behavior

- writes a local release request into the config directory
- the daemon polls that request file and clears controller mode while it is running

## `kms status`

Prints concise runtime status.

### Example Output

```text
node: macbook-air
listen: 0.0.0.0:48571
state: connected-idle
peer: office-pc
trusted: yes
session: healthy
capture: enabled
inject: native
note: macOS requires Accessibility permission for input control
note: macOS requires Input Monitoring permission for global capture
```

## Config File Spec

Suggested location rules:

- macOS: `~/Library/Application Support/kms/config.toml`
- Windows: `%AppData%/kms/config.toml`
- control requests live beside the config as `control.toml`
- daemon status snapshots live beside the config as `status.toml`

Suggested shape:

```toml
[node]
id = "macbook-air"
name = "MacBook Air"
listen_addr = "0.0.0.0:48571"
private_key = "base64:..."

[switch]
hotkey = "Ctrl+Alt+Shift+K"

[[peers]]
id = "office-pc"
name = "Office PC"
addr = "192.168.1.25:48571"
public_key = "base64:..."
trusted = true
```

## Pairing Flow Spec

## Goals

- easy to perform from terminal
- no account system
- no centralized server
- resistant to accidental pairing with the wrong LAN host

## Recommended V1 Flow

### Step 1: Offer Creation

Machine A runs:

```bash
kms pair init
```

This produces a token containing:

- protocol version
- short expiry
- node id
- display name
- listen address
- public key
- nonce
- integrity protection

### Step 2: Offer Acceptance

Machine B runs:

```bash
kms pair accept <token>
```

Machine B:

- validates token
- stores A as trusted
- optionally attempts first connection immediately

### Step 3: Mutual Trust Completion

For the simplest V1, require pairing to be performed in both directions:

1. A creates token and B accepts
2. B creates token and A accepts

This keeps trust explicit and avoids overcomplicating the pairing handshake.

## Pair Token Format

Human-pastable, single line:

```text
v1.pair.<short-code>.<payload>
```

Where payload decodes to a structure like:

```json
{
  "version": 1,
  "node_id": "macbook-air",
  "name": "MacBook Air",
  "listen_addr": "192.168.1.10:48571",
  "public_key": "base64...",
  "nonce": "base64...",
  "expires_at": "2026-04-05T12:30:00Z",
  "mac": "base64..."
}
```

The short code is for user confirmation in logs and CLI output, not primary trust.

## Network Protocol Spec

## Transport

Recommended V1 transport:

- `TCP`
- one persistent connection per peer
- encrypted after handshake

## Framing

Each frame:

- 4 bytes: payload length, big-endian
- 1 byte: message type
- N bytes: serialized payload

This is enough for V1 and easy to debug.

## Versioning

Every handshake message includes:

- protocol version
- node identity

If versions mismatch incompatibly, the connection is rejected.

## Connection Lifecycle

1. TCP connect
2. protocol hello exchange
3. identity validation
4. session key establishment
5. encrypted message loop
6. heartbeat until disconnect

Current implementation status:

- steps 1 to 4 are implemented as an authenticated handshake flow
- step 5 is currently plaintext TCP framing, not encrypted
- step 6 is implemented with periodic heartbeats and timeout detection

## Message Types

Suggested minimal set:

```text
0x01 HELLO
0x02 HELLO_ACK
0x03 AUTH_CHALLENGE
0x04 AUTH_RESPONSE
0x05 AUTH_RESULT
0x06 SWITCH_REQUEST
0x07 SWITCH_RELEASE
0x08 INPUT_EVENT
0x09 HEARTBEAT
0x0A ERROR
```

## Message Definitions

## `HELLO`

Sent immediately after connect.

Fields:

- protocol version
- node id
- node name
- public key

Implemented shape:

- `Message::Hello(HelloPayload { version, node_id, node_name })`

## `HELLO_ACK`

Fields:

- protocol version
- node id
- node name
- public key

Implemented shape:

- `Message::HelloAck(HelloPayload { version, node_id, node_name })`

## `AUTH_CHALLENGE`

Fields:

- session id
- challenger node id
- random nonce

Implemented shape:

- `Message::AuthChallenge(AuthChallengePayload { session_id, challenger_node_id, nonce })`

## `AUTH_RESPONSE`

Fields:

- session id
- responder node id
- Ed25519 signature over the challenge payload

Implemented shape:

- `Message::AuthResponse(AuthResponsePayload { session_id, responder_node_id, signature })`

## `AUTH_RESULT`

Fields:

- success flag
- authenticated peer id when successful
- error code if rejected

Implemented shape:

- `Message::AuthResult(AuthResultPayload { ok, peer_id, error })`

## `SWITCH_REQUEST`

Sent by the node that wants to become controller for this session.

Fields:

- target peer id
- request id

## `SWITCH_RELEASE`

Sent when releasing control.

Fields:

- request id

## `INPUT_EVENT`

Carries one normalized input event.

Fields:

- sequence number
- event payload

V1 can send one input event per frame. Batching may be added later only if profiling shows it is needed.

## `HEARTBEAT`

Fields:

- monotonic sender timestamp

## `ERROR`

Fields:

- error code
- short message

## Input Event Payload Spec

Suggested event variants:

```text
KEY_DOWN
KEY_UP
MOUSE_MOVE
MOUSE_BUTTON_DOWN
MOUSE_BUTTON_UP
MOUSE_WHEEL
```

Suggested payload shapes:

### `KEY_DOWN` / `KEY_UP`

- key code
- modifier bitmask

### `MOUSE_MOVE`

- dx
- dy

### `MOUSE_BUTTON_DOWN` / `MOUSE_BUTTON_UP`

- button id

### `MOUSE_WHEEL`

- delta_x
- delta_y

Current implementation detail:

- events are routed through `InputEventSink`
- platform injector implementations are logging/no-op adapters for now
- real OS injection is still pending

## Sequence and Ordering Rules

- input events use strictly increasing per-session sequence numbers
- receiver ignores duplicate or stale event sequence numbers
- TCP already preserves order, but explicit sequence numbers help diagnostics and future proofing

## Recovery Rules

The receiver must force-release tracked input state when:

- session disconnects
- `SWITCH_RELEASE` is received
- invalid session transition occurs
- heartbeat timeout occurs

Tracked state includes:

- pressed keys
- pressed mouse buttons
- active modifiers

## Security Rules

V1 minimum rules:

- reject unknown public keys
- reject expired pairing tokens
- encrypt session traffic
- do not allow unauthenticated input events
- do not allow control before successful auth

Out of scope for V1:

- internet relay security model
- user accounts
- centralized revocation

## Error Codes

Suggested minimal set:

```text
100 unsupported_version
101 untrusted_peer
102 auth_failed
103 invalid_state
104 invalid_frame
105 session_expired
106 permission_denied
107 platform_error
```

## Minimal Example Session

```text
A -> B HELLO
B -> A HELLO_ACK
B -> A AUTH_CHALLENGE
A -> B AUTH_RESPONSE
B -> A AUTH_RESULT(success)
A -> B SWITCH_REQUEST
A -> B INPUT_EVENT(KEY_DOWN)
A -> B INPUT_EVENT(KEY_UP)
A -> B INPUT_EVENT(MOUSE_MOVE)
A -> B SWITCH_RELEASE
```

## Recommended V1 Defaults

- default port: `48571`
- heartbeat interval: `2s`
- heartbeat timeout: `6s`
- pairing token expiry: `10m`

These values are simple starting points and should be tuned after real testing.

## Summary

The V1 protocol should stay intentionally small:

- explicit pairing
- persistent trusted session
- simple framed messages
- one-event input forwarding
- strong disconnect recovery

That is enough to ship a useful LAN-only CLI-first product without locking the project into an overly complex design.
