use anyhow::{anyhow, Context, Result};
use base64::engine::general_purpose::STANDARD_NO_PAD;
use base64::Engine;
use ed25519_dalek::{SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeIdentity {
    pub node_id: String,
    pub node_name: String,
    pub listen_addr: String,
    pub public_key: String,
}

impl NodeIdentity {
    pub fn verifying_key(&self) -> Result<VerifyingKey> {
        let bytes = STANDARD_NO_PAD
            .decode(&self.public_key)
            .context("failed to decode public key")?;
        let key_bytes: [u8; 32] = bytes
            .try_into()
            .map_err(|_| anyhow!("invalid public key length"))?;

        VerifyingKey::from_bytes(&key_bytes).context("failed to parse public key")
    }
}

pub fn signing_key_from_base64(encoded_private_key: &str) -> Result<SigningKey> {
    let bytes = STANDARD_NO_PAD
        .decode(encoded_private_key)
        .context("failed to decode private key")?;
    let key_bytes: [u8; 32] = bytes
        .try_into()
        .map_err(|_| anyhow!("invalid private key length"))?;

    Ok(SigningKey::from_bytes(&key_bytes))
}
