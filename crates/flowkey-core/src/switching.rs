use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DaemonCommand {
    Switch { peer_id: String },
    Release,
}

impl DaemonCommand {
    pub fn switch(peer_id: impl Into<String>) -> Self {
        Self::Switch {
            peer_id: peer_id.into(),
        }
    }

    pub fn release() -> Self {
        Self::Release
    }

    pub async fn send_to<S>(&self, stream: &mut S) -> Result<()>
    where
        S: AsyncWriteExt + Unpin,
    {
        let payload = bincode::serialize(self).context("failed to serialize daemon command")?;
        let len = payload.len() as u32;
        stream
            .write_u32(len)
            .await
            .context("failed to write command length")?;
        stream
            .write_all(&payload)
            .await
            .context("failed to write command payload")?;
        stream.flush().await.context("failed to flush stream")?;
        Ok(())
    }

    pub async fn read_from<S>(stream: &mut S) -> Result<Self>
    where
        S: AsyncReadExt + Unpin,
    {
        let len = stream
            .read_u32()
            .await
            .context("failed to read command length")?;
        if len > 1024 * 64 {
            return Err(anyhow!("command payload too large: {len} bytes"));
        }

        let mut payload = vec![0u8; len as usize];
        stream
            .read_exact(&mut payload)
            .await
            .context("failed to read command payload")?;

        let command = bincode::deserialize(&payload).context("failed to deserialize command")?;
        Ok(command)
    }

    pub fn load_from_path(path: &Path) -> Result<Self> {
        let raw = fs::read_to_string(path)
            .with_context(|| format!("failed to read daemon command from {}", path.display()))?;
        let command = toml::from_str::<Self>(&raw)
            .with_context(|| format!("failed to parse daemon command from {}", path.display()))?;

        Ok(command)
    }

    pub fn save_to_path(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!(
                    "failed to create daemon command directory {}",
                    parent.display()
                )
            })?;
        }

        let tmp_path = path.with_extension(format!(
            "{}.tmp",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock should be after unix epoch")
                .as_nanos()
        ));
        let raw = toml::to_string_pretty(self).context("failed to serialize daemon command")?;
        fs::write(&tmp_path, raw)
            .with_context(|| format!("failed to write daemon command to {}", tmp_path.display()))?;

        if path.exists() {
            fs::remove_file(path).with_context(|| {
                format!("failed to replace daemon command at {}", path.display())
            })?;
        }

        fs::rename(&tmp_path, path)
            .with_context(|| format!("failed to move daemon command to {}", path.display()))?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::DaemonCommand;

    #[test]
    fn command_round_trips_through_toml() {
        let command = DaemonCommand::switch("office-pc");
        let encoded = toml::to_string(&command).expect("command should serialize");
        let decoded: DaemonCommand = toml::from_str(&encoded).expect("command should deserialize");

        assert_eq!(decoded, command);
    }

    #[test]
    fn command_save_and_reload_preserves_variant() {
        let command = DaemonCommand::release();
        let path = std::env::temp_dir().join(format!(
            "flowkey-command-test-{}-{}.toml",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock should be after epoch")
                .as_nanos()
        ));

        command
            .save_to_path(&path)
            .expect("command should save to temp path");
        let reloaded = DaemonCommand::load_from_path(&path).expect("command should reload");
        fs::remove_file(&path).expect("temp command should be removable");

        assert_eq!(reloaded, command);
    }
}
