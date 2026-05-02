use std::sync::{Arc, OnceLock};

use serde::Serialize;

#[derive(Clone, Debug, Serialize)]
pub struct WindowsInputDebugEvent {
    pub source: &'static str,
    pub kind: &'static str,
    pub detail: String,
    pub timestamp_ms: u128,
}

type DebugSink = Arc<dyn Fn(WindowsInputDebugEvent) + Send + Sync + 'static>;

static DEBUG_SINK: OnceLock<DebugSink> = OnceLock::new();

pub fn set_debug_sink<F>(sink: F)
where
    F: Fn(WindowsInputDebugEvent) + Send + Sync + 'static,
{
    let _ = DEBUG_SINK.set(Arc::new(sink));
}

pub(crate) fn emit(kind: &'static str, detail: impl Into<String>) {
    let Some(sink) = DEBUG_SINK.get() else {
        return;
    };

    let timestamp_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();

    sink(WindowsInputDebugEvent {
        source: "windows-capture",
        kind,
        detail: detail.into(),
        timestamp_ms,
    });
}
