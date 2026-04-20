mod config;

pub use config::{
    unix_timestamp_now, CaptureMode, Config, DEFAULT_INPUT_COALESCE_WINDOW_MS, NodeConfig,
    PeerConfig, SwitchConfig,
};
