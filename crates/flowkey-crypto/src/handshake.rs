use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use ed25519_dalek::{Signature, Signer};
use rand::distributions::Alphanumeric;
use rand::{thread_rng, Rng};
use serde::{Deserialize, Serialize};

use crate::identity::{signing_key_from_base64, NodeIdentity};

const PAIR_TOKEN_PREFIX: &str = "v1.pair";
const DEFAULT_EXPIRY_SECS: u64 = 600;
const SIGNING_CONTEXT: &[u8] = b"flowkey-pairing-offer-v1";
const SESSION_SIGNING_CONTEXT: &[u8] = b"flowkey-session-auth-v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandshakeOffer {
    pub version: u16,
    pub short_code: String,
    pub node: NodeIdentity,
    pub expires_at_epoch_secs: u64,
    pub nonce: String,
    pub signature: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct UnsignedHandshakeOffer {
    version: u16,
    short_code: String,
    node: NodeIdentity,
    expires_at_epoch_secs: u64,
    nonce: String,
}

impl HandshakeOffer {
    pub fn new(node: NodeIdentity, encoded_private_key: &str) -> Result<Self> {
        let unsigned = UnsignedHandshakeOffer {
            version: 1,
            short_code: random_fragment(8).to_uppercase(),
            node,
            expires_at_epoch_secs: unix_timestamp_now() + DEFAULT_EXPIRY_SECS,
            nonce: random_fragment(24),
        };

        let signing_key = signing_key_from_base64(encoded_private_key)?;
        let signature = signing_key.sign(&unsigned.signing_bytes()?);

        Ok(Self {
            version: unsigned.version,
            short_code: unsigned.short_code,
            node: unsigned.node,
            expires_at_epoch_secs: unsigned.expires_at_epoch_secs,
            nonce: unsigned.nonce,
            signature: URL_SAFE_NO_PAD.encode(signature.to_bytes()),
        })
    }

    pub fn to_token(&self) -> Result<String> {
        let payload = serde_json::to_vec(self).context("failed to serialize pairing offer")?;
        let encoded = URL_SAFE_NO_PAD.encode(payload);

        Ok(format!(
            "{PAIR_TOKEN_PREFIX}.{}.{}",
            self.short_code, encoded
        ))
    }

    pub fn from_token(token: &str) -> Result<Self> {
        let mut segments = token.split('.');

        let prefix_a = segments
            .next()
            .ok_or_else(|| anyhow!("missing token prefix"))?;
        let prefix_b = segments
            .next()
            .ok_or_else(|| anyhow!("missing token marker"))?;
        let short_code = segments
            .next()
            .ok_or_else(|| anyhow!("missing short code"))?;
        let payload = segments
            .next()
            .ok_or_else(|| anyhow!("missing token payload"))?;

        if prefix_a != "v1" || prefix_b != "pair" {
            return Err(anyhow!("unsupported token prefix"));
        }

        if segments.next().is_some() {
            return Err(anyhow!("unexpected extra token segments"));
        }

        let decoded = URL_SAFE_NO_PAD
            .decode(payload)
            .context("failed to decode token payload")?;
        let offer =
            serde_json::from_slice::<Self>(&decoded).context("failed to parse token payload")?;

        if offer.short_code != short_code {
            return Err(anyhow!("short code does not match payload"));
        }

        if offer.is_expired() {
            return Err(anyhow!("pairing token has expired"));
        }

        offer.verify_signature()?;

        Ok(offer)
    }

    pub fn is_expired(&self) -> bool {
        unix_timestamp_now() > self.expires_at_epoch_secs
    }

    pub fn verify_signature(&self) -> Result<()> {
        let verifying_key = self.node.verifying_key()?;
        let signature_bytes = URL_SAFE_NO_PAD
            .decode(&self.signature)
            .context("failed to decode offer signature")?;
        let signature = Signature::try_from(signature_bytes.as_slice())
            .context("failed to parse offer signature")?;

        verifying_key
            .verify_strict(&self.unsigned().signing_bytes()?, &signature)
            .context("pairing offer signature verification failed")
    }

    fn unsigned(&self) -> UnsignedHandshakeOffer {
        UnsignedHandshakeOffer {
            version: self.version,
            short_code: self.short_code.clone(),
            node: self.node.clone(),
            expires_at_epoch_secs: self.expires_at_epoch_secs,
            nonce: self.nonce.clone(),
        }
    }
}

impl UnsignedHandshakeOffer {
    fn signing_bytes(&self) -> Result<Vec<u8>> {
        let mut bytes = SIGNING_CONTEXT.to_vec();
        bytes.push(b':');
        bytes.extend(serde_json::to_vec(self).context("failed to serialize unsigned offer")?);
        Ok(bytes)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionChallenge {
    pub session_id: String,
    pub challenger_node_id: String,
    pub nonce: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionResponse {
    pub session_id: String,
    pub responder_node_id: String,
    pub signature: String,
}

impl SessionChallenge {
    pub fn new(challenger_node_id: impl Into<String>) -> Self {
        Self {
            session_id: random_fragment(16),
            challenger_node_id: challenger_node_id.into(),
            nonce: random_fragment(24),
        }
    }

    pub fn signing_bytes(&self, responder_node_id: &str) -> Result<Vec<u8>> {
        signing_payload_for_session(
            &self.session_id,
            &self.challenger_node_id,
            &self.nonce,
            responder_node_id,
        )
    }

    pub fn sign_response(
        &self,
        responder_node_id: &str,
        encoded_private_key: &str,
    ) -> Result<SessionResponse> {
        let signing_key = signing_key_from_base64(encoded_private_key)?;
        let signature = signing_key.sign(&self.signing_bytes(responder_node_id)?);

        Ok(SessionResponse {
            session_id: self.session_id.clone(),
            responder_node_id: responder_node_id.to_string(),
            signature: URL_SAFE_NO_PAD.encode(signature.to_bytes()),
        })
    }

    pub fn verify_response(&self, response: &SessionResponse, peer: &NodeIdentity) -> Result<()> {
        if response.session_id != self.session_id {
            return Err(anyhow!("session id mismatch"));
        }

        if response.responder_node_id != peer.node_id {
            return Err(anyhow!("responder node id mismatch"));
        }

        let verifying_key = peer.verifying_key()?;
        let signature_bytes = URL_SAFE_NO_PAD
            .decode(&response.signature)
            .context("failed to decode session signature")?;
        let signature = Signature::try_from(signature_bytes.as_slice())
            .context("failed to parse session signature")?;

        verifying_key
            .verify_strict(
                &self.signing_bytes(&response.responder_node_id)?,
                &signature,
            )
            .context("session auth signature verification failed")
    }
}

pub fn signing_payload_for_session(
    session_id: &str,
    challenger_node_id: &str,
    nonce: &str,
    responder_node_id: &str,
) -> Result<Vec<u8>> {
    let mut bytes = SESSION_SIGNING_CONTEXT.to_vec();
    bytes.push(b':');
    bytes.extend(
        serde_json::to_vec(&(session_id, challenger_node_id, nonce, responder_node_id))
            .context("failed to serialize session signing payload")?,
    );
    Ok(bytes)
}

fn random_fragment(len: usize) -> String {
    thread_rng()
        .sample_iter(&Alphanumeric)
        .take(len)
        .map(char::from)
        .collect()
}

fn unix_timestamp_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock should be after unix epoch")
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::{signing_payload_for_session, HandshakeOffer, SessionChallenge};
    use crate::identity::NodeIdentity;
    use base64::engine::general_purpose::STANDARD_NO_PAD;
    use base64::Engine;
    use ed25519_dalek::SigningKey;

    #[test]
    fn pairing_offer_round_trips_through_token() {
        let signing_key = SigningKey::generate(&mut rand::rngs::OsRng);
        let offer = HandshakeOffer::new(
            NodeIdentity {
                node_id: "macbook-air".to_string(),
                node_name: "MacBook Air".to_string(),
                listen_addr: "192.168.1.10:48571".to_string(),
                public_key: STANDARD_NO_PAD.encode(signing_key.verifying_key().to_bytes()),
            },
            &STANDARD_NO_PAD.encode(signing_key.to_bytes()),
        )
        .expect("offer should sign");

        let token = offer.to_token().expect("token should serialize");
        let parsed = HandshakeOffer::from_token(&token).expect("token should parse");

        assert_eq!(parsed.node.node_id, "macbook-air");
        assert_eq!(parsed.node.listen_addr, "192.168.1.10:48571");
        assert_eq!(parsed.short_code, offer.short_code);
    }

    #[test]
    fn session_challenge_round_trips_with_valid_signature() {
        let signing_key = SigningKey::generate(&mut rand::rngs::OsRng);
        let peer = NodeIdentity {
            node_id: "office-pc".to_string(),
            node_name: "Office PC".to_string(),
            listen_addr: "192.168.1.25:48571".to_string(),
            public_key: STANDARD_NO_PAD.encode(signing_key.verifying_key().to_bytes()),
        };
        let challenge = SessionChallenge::new("macbook-air");
        let response = challenge
            .sign_response(
                &peer.node_id,
                &STANDARD_NO_PAD.encode(signing_key.to_bytes()),
            )
            .expect("response should sign");

        challenge
            .verify_response(&response, &peer)
            .expect("response should verify");
    }

    #[test]
    fn session_signing_payload_is_deterministic() {
        let one = signing_payload_for_session("s", "a", "b", "c").expect("payload should build");
        let two = signing_payload_for_session("s", "a", "b", "c").expect("payload should build");
        assert_eq!(one, two);
    }
}
