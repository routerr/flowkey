use anyhow::{anyhow, Context, Result};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use flowkey_protocol::message::Message;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameHeader {
    pub payload_len: u32,
    pub message_type: u8,
}

pub async fn write_message(stream: &mut TcpStream, message: &Message) -> Result<()> {
    let payload = bincode::serialize(message).context("failed to serialize message")?;
    let header = FrameHeader {
        payload_len: payload
            .len()
            .try_into()
            .context("message payload too large")?,
        message_type: message_type(message),
    };

    stream
        .write_u32(header.payload_len)
        .await
        .context("failed to write payload length")?;
    stream
        .write_u8(header.message_type)
        .await
        .context("failed to write message type")?;
    stream
        .write_all(&payload)
        .await
        .context("failed to write payload")?;

    Ok(())
}

pub async fn read_message(stream: &mut TcpStream) -> Result<Message> {
    let payload_len = stream
        .read_u32()
        .await
        .context("failed to read payload length")?;
    let message_type = stream
        .read_u8()
        .await
        .context("failed to read message type")?;
    let mut payload = vec![0; payload_len as usize];
    stream
        .read_exact(&mut payload)
        .await
        .context("failed to read message payload")?;

    let message = bincode::deserialize::<Message>(&payload).context("failed to decode message")?;
    let expected_type = message_type_for_decoded_message(&message);

    if message_type != expected_type {
        return Err(anyhow!("message type header does not match payload"));
    }

    Ok(message)
}

fn message_type(message: &Message) -> u8 {
    match message {
        Message::Hello(_) => 0x01,
        Message::HelloAck(_) => 0x02,
        Message::AuthChallenge(_) => 0x03,
        Message::AuthResponse(_) => 0x04,
        Message::AuthResult(_) => 0x05,
        Message::SwitchRequest { .. } => 0x06,
        Message::SwitchRelease { .. } => 0x07,
        Message::InputEvent { .. } => 0x08,
        Message::Heartbeat => 0x09,
        Message::Error { .. } => 0x0A,
    }
}

fn message_type_for_decoded_message(message: &Message) -> u8 {
    message_type(message)
}

#[cfg(test)]
mod tests {
    use tokio::net::{TcpListener, TcpStream};

    use super::{read_message, write_message};
    use flowkey_protocol::message::{HelloPayload, Message, PROTOCOL_VERSION};

    #[tokio::test]
    async fn message_round_trips_over_frame_io() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener should bind");
        let addr = listener.local_addr().expect("listener should have addr");

        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("server should accept");
            read_message(&mut stream).await.expect("server should read")
        });

        let mut client = TcpStream::connect(addr)
            .await
            .expect("client should connect");
        let message = Message::Hello(HelloPayload {
            version: PROTOCOL_VERSION,
            node_id: "macbook-air".to_string(),
            node_name: "MacBook Air".to_string(),
        });
        write_message(&mut client, &message)
            .await
            .expect("client should write");

        let received = server.await.expect("task should complete");
        assert_eq!(received, message);
    }
}
