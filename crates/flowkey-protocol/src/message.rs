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

#[cfg(test)]
mod tests {
    use super::{HelloPayload, Message, PROTOCOL_VERSION};
    use crate::input::{InputEvent, Modifiers};

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
