use tokio::net::windows::named_pipe::{ClientOptions, ClientPipe};

pub use tokio::net::windows::named_pipe::ClientPipe as ControlStream;

/// Attempt to connect to the daemon control pipe.
///
/// Returns `Some(pipe)` if successful, `None` if connection failed.
pub async fn connect_to_control_pipe(pipe_name: &str) -> Option<ClientPipe> {
    ClientOptions::new().open(pipe_name).ok()
}

