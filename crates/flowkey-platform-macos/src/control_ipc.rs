use std::path::Path;
use tokio::net::UnixStream;

pub use tokio::net::UnixStream as ControlStream;

/// Attempt to connect to the daemon control socket.
///
/// Returns `Some(stream)` if successful, `None` if socket doesn't exist or connection failed.
pub async fn connect_to_control_socket(socket_path: &Path) -> Option<UnixStream> {
    if !socket_path.exists() {
        return None;
    }
    UnixStream::connect(socket_path).await.ok()
}


