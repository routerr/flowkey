mod config;

pub use config::{
    unix_timestamp_now, CaptureMode, Config, NodeConfig, PeerConfig, SwitchConfig,
    DEFAULT_INPUT_COALESCE_WINDOW_MS,
};
