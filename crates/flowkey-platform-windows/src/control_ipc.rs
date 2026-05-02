use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeClient};

pub use tokio::net::windows::named_pipe::NamedPipeClient as ControlStream;

/// Attempt to connect to the daemon control pipe.
///
/// Returns `Some(pipe)` if successful, `None` if connection failed.
pub async fn connect_to_control_pipe(pipe_name: &str) -> Option<NamedPipeClient> {
    ClientOptions::new().open(pipe_name).ok()
}
