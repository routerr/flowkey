use serde::{Deserialize, Serialize};

use crate::input::InputEvent;

pub const PROTOCOL_VERSION: u16 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HelloPayload {
    pub version: u16,
    pub node_id: String,
    pub node_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthChallengePayload {
    pub session_id: String,
    pub challenger_node_id: String,
    pub nonce: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthResponsePayload {
    pub session_id: String,
    pub responder_node_id: String,
    pub signature: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthResultPayload {
    pub ok: bool,
    pub peer_id: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Message {
    Hello(HelloPayload),
    HelloAck(HelloPayload),
    AuthChallenge(AuthChallengePayload),
    AuthResponse(AuthResponsePayload),
    AuthResult(AuthResultPayload),
    SwitchRequest { peer_id: String, request_id: String },
    SwitchRelease { request_id: String },
    InputEvent { sequence: u64, event: InputEvent },
    Heartbeat,
    Error { code: u16, message: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PairingMessage {
    /// Sent by the initiator to propose a pairing.
    Propose {
        node_id: String,
        node_name: String,
        public_key: String,
    },
    /// Sent by the responder to acknowledge the proposal and share their identity.
    Acknowledge {
        node_id: String,
        node_name: String,
        public_key: String,
    },
    /// Sent by either side to confirm the SAS code matches and they accept the pairing.
    Accept,
    /// Sent by either side to reject the pairing.
    Reject,
}

pub fn generate_sas_code(pubkey_a: &str, pubkey_b: &str) -> String {
    use sha3::{Digest, Sha3_256};
    let mut hasher = Sha3_256::new();
    // Sort keys to ensure same order on both sides regardless of who initiated.
    let mut keys = [pubkey_a, pubkey_b];
    keys.sort();
    hasher.update(keys[0].as_bytes());
    hasher.update(keys[1].as_bytes());
    let result = hasher.finalize();

    // Use the first 8 bytes to derive a 6-digit code.
    // u64 ensures we have plenty of entropy before the modulo.
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&result[0..8]);
    let val = u64::from_be_bytes(bytes);
    format!("{:06}", val % 1_000_000)
}

#[cfg(test)]
mod tests {
    use super::{generate_sas_code, HelloPayload, Message, PROTOCOL_VERSION};
    use crate::input::{InputEvent, Modifiers};

    #[test]
    fn sas_code_is_deterministic_and_order_independent() {
        let key_a = "pubkey_a_1234567890";
        let key_b = "pubkey_b_0987654321";

        let code1 = generate_sas_code(key_a, key_b);
        let code2 = generate_sas_code(key_b, key_a);

        assert_eq!(code1.len(), 6);
        assert_eq!(code1, code2);
        // Verify it looks like a number
        assert!(code1.chars().all(|c| c.is_ascii_digit()));
    }

    #[test]
    fn message_round_trips_through_toml() {
        let message = Message::InputEvent {
            sequence: 42,
            event: InputEvent::KeyDown {
                code: "KeyK".to_string(),
                modifiers: Modifiers {
                    shift: false,
                    control: true,
                    alt: true,
                    meta: false,
                },
                timestamp_us: 0,
            },
        };

        let encoded = toml::to_string(&message).expect("message should serialize");
        let decoded: Message = toml::from_str(&encoded).expect("message should deserialize");

        assert_eq!(decoded, message);
    }

    #[test]
    fn hello_messages_use_same_protocol_version() {
        let hello = Message::Hello(HelloPayload {
            version: PROTOCOL_VERSION,
            node_id: "macbook-air".to_string(),
            node_name: "MacBook Air".to_string(),
        });

        match hello {
            Message::Hello(payload) => assert_eq!(payload.version, 1),
            _ => panic!("expected hello"),
        }
    }
}
