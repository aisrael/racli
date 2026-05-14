use std::sync::Arc;

use jsonrpsee::core::client::ReceivedMessage;
use jsonrpsee::core::client::TransportReceiverT;
use jsonrpsee::core::client::TransportSenderT;
use lsp_types::request::RegisterCapability;
use lsp_types::request::Request;
use lsp_types::request::UnregisterCapability;
use lsp_types::request::WorkDoneProgressCreate;
use serde_json::Value;
use tokio::io::AsyncBufReadExt;
use tokio::io::AsyncRead;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWrite;
use tokio::io::AsyncWriteExt;
use tokio::io::BufReader;
use tokio::sync::Mutex;

/// Error that can occur when reading or sending messages on a transport.
#[derive(thiserror::Error, Debug)]
pub enum TransportError {
    /// Error in I/O operation.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    /// Error in parsing message.
    #[error("parse error: {0}")]
    Parse(String),
}

async fn write_framed<W: AsyncWrite + Unpin>(
    writer: &mut W,
    body: &str,
) -> Result<(), TransportError> {
    let msg_with_header = format!("Content-Length: {}\r\n\r\n{}", body.len(), body);
    writer.write_all(msg_with_header.as_bytes()).await?;
    Ok(())
}

/// Returns the JSON-RPC `id` if `value` is a server→client request whose `method` is one of `methods` (not a response).
fn server_to_client_request_ack_id(value: &Value, methods: &[&str]) -> Option<Value> {
    let obj = value.as_object()?;
    if obj.contains_key("result") || obj.contains_key("error") {
        return None;
    }
    let method = obj.get("method")?.as_str()?;
    if !methods.contains(&method) {
        return None;
    }
    let id = obj.get("id")?;
    if id.is_null() {
        return None;
    }
    Some(id.clone())
}

async fn reply_null_result<W: AsyncWrite + Unpin>(
    writer: &mut W,
    id: Value,
) -> Result<(), TransportError> {
    let response = serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": Value::Null,
    });
    let body =
        serde_json::to_string(&response).map_err(|e| TransportError::Parse(e.to_string()))?;
    write_framed(writer, &body).await?;
    Ok(())
}

async fn read_framed_body<R: AsyncRead + Send + Unpin>(
    reader: &mut BufReader<R>,
) -> Result<Vec<u8>, TransportError> {
    let mut content_length: Option<usize> = None;

    // Parse header part.
    // https://microsoft.github.io/language-server-protocol/specifications/lsp/3.17/specification/#baseProtocol
    let mut line = String::new();
    loop {
        reader.read_line(&mut line).await?;
        match line.as_str() {
            "\r\n" => break,
            line if line.starts_with("Content-Length: ") => {
                let len = &line[16..line.len() - 2];
                let len = len
                    .parse::<usize>()
                    .map_err(|e| TransportError::Parse(e.to_string()))?;
                content_length = Some(len);
            }
            _ => {}
        }
        line.clear();
    }

    let content_length = content_length.ok_or(TransportError::Parse(
        "Content-Length header not found".to_string(),
    ))?;
    let mut buf = vec![0; content_length];
    reader.read_exact(&mut buf).await?;
    Ok(buf)
}

/// Sending end of I/O transport.
pub struct Sender<T>(Arc<Mutex<T>>)
where
    T: AsyncWrite + Send + Unpin + 'static;

impl<T> TransportSenderT for Sender<T>
where
    T: AsyncWrite + Send + Unpin + 'static,
{
    type Error = TransportError;

    fn send(
        &mut self,
        msg: String,
    ) -> impl std::future::Future<Output = Result<(), Self::Error>> + Send {
        let writer = Arc::clone(&self.0);
        async move {
            let mut guard = writer.lock().await;
            write_framed(&mut *guard, &msg).await?;
            Ok(())
        }
    }
}

/// Receiving end of I/O transport.
pub struct Receiver<R, W> {
    reader: Arc<Mutex<BufReader<R>>>,
    writer: Arc<Mutex<W>>,
}

impl<R, W> TransportReceiverT for Receiver<R, W>
where
    R: AsyncRead + Send + Unpin + 'static,
    W: AsyncWrite + Send + Unpin + 'static,
{
    type Error = TransportError;

    fn receive(
        &mut self,
    ) -> impl std::future::Future<Output = Result<ReceivedMessage, Self::Error>> + Send {
        let reader = Arc::clone(&self.reader);
        let writer = Arc::clone(&self.writer);
        async move {
            loop {
                let buf = {
                    let mut guard = reader.lock().await;
                    read_framed_body(&mut *guard).await?
                };

                let value: Value = serde_json::from_slice(&buf)
                    .map_err(|e| TransportError::Parse(e.to_string()))?;

                let ack_methods = [
                    WorkDoneProgressCreate::METHOD,
                    RegisterCapability::METHOD,
                    UnregisterCapability::METHOD,
                ];
                if let Some(id) = server_to_client_request_ack_id(&value, &ack_methods) {
                    let mut guard = writer.lock().await;
                    reply_null_result(&mut *guard, id).await?;
                    continue;
                }

                return Ok(ReceivedMessage::Bytes(buf));
            }
        }
    }
}

/// Create a I/O transport `Sender` and `Receiver` pair.
///
/// `input` is the stream written to the server (e.g. child stdin); `output` is read from the server
/// (e.g. child stdout).
pub fn io_transport<I, O>(input: I, output: O) -> (Sender<I>, Receiver<O, I>)
where
    I: AsyncWrite + Send + Unpin + 'static,
    O: AsyncRead + Send + Unpin + 'static,
{
    let writer = Arc::new(Mutex::new(input));
    let reader = Arc::new(Mutex::new(BufReader::new(output)));
    let sender = Sender(Arc::clone(&writer));
    let receiver = Receiver { reader, writer };
    (sender, receiver)
}

#[cfg(test)]
mod tests {
    use tokio::io::AsyncWriteExt;
    use tokio::io::duplex;

    use super::*;

    fn lsp_frame(body: &str) -> Vec<u8> {
        format!("Content-Length: {}\r\n\r\n{}", body.len(), body).into_bytes()
    }

    async fn read_next_lsp_json<R: AsyncRead + Unpin>(
        reader: &mut BufReader<R>,
    ) -> serde_json::Value {
        let mut content_length = None::<usize>;
        let mut line = String::new();
        loop {
            line.clear();
            reader.read_line(&mut line).await.unwrap();
            if line.as_str() == "\r\n" {
                break;
            }
            if let Some(rest) = line.strip_prefix("Content-Length: ") {
                let len = rest.trim_end_matches("\r\n");
                content_length = Some(len.parse().unwrap());
            }
        }
        let n = content_length.expect("Content-Length header");
        let mut buf = vec![0u8; n];
        reader.read_exact(&mut buf).await.unwrap();
        serde_json::from_slice(&buf).unwrap()
    }

    #[tokio::test]
    async fn work_done_progress_create_replies_null_and_skips_forwarding() {
        let (mut stdin_readable, stdin_writable) = duplex(65_536);
        let (mut stdout_writable, stdout_readable) = duplex(65_536);

        let (_sender, mut receiver) = io_transport(stdin_writable, stdout_readable);

        let create = serde_json::json!({
            "jsonrpc": "2.0",
            "id": "req-7",
            "method": WorkDoneProgressCreate::METHOD,
            "params": { "token": "tok" },
        });
        let other = r#"{"jsonrpc":"2.0","method":"$/cancelRequest","params":{"id":1}}"#;

        let mut inject = lsp_frame(&create.to_string());
        inject.extend_from_slice(&lsp_frame(other));
        stdout_writable.write_all(&inject).await.unwrap();
        drop(stdout_writable);

        let got = receiver.receive().await.unwrap();
        match got {
            ReceivedMessage::Bytes(b) => {
                let v: Value = serde_json::from_slice(&b).unwrap();
                assert_eq!(v["method"], "$/cancelRequest");
            }
            _ => panic!("expected Bytes"),
        }

        let mut stdin_reader = BufReader::new(&mut stdin_readable);
        let ack = read_next_lsp_json(&mut stdin_reader).await;
        assert_eq!(ack["jsonrpc"], "2.0");
        assert_eq!(ack["id"], "req-7");
        assert!(ack["result"].is_null());
    }

    #[tokio::test]
    async fn register_capability_replies_null_and_skips_forwarding() {
        let (mut stdin_readable, stdin_writable) = duplex(65_536);
        let (mut stdout_writable, stdout_readable) = duplex(65_536);

        let (_sender, mut receiver) = io_transport(stdin_writable, stdout_readable);

        let register = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 42,
            "method": RegisterCapability::METHOD,
            "params": { "registrations": [] },
        });
        let other = r#"{"jsonrpc":"2.0","method":"$/cancelRequest","params":{"id":1}}"#;

        let mut inject = lsp_frame(&register.to_string());
        inject.extend_from_slice(&lsp_frame(other));
        stdout_writable.write_all(&inject).await.unwrap();
        drop(stdout_writable);

        let got = receiver.receive().await.unwrap();
        match got {
            ReceivedMessage::Bytes(b) => {
                let v: Value = serde_json::from_slice(&b).unwrap();
                assert_eq!(v["method"], "$/cancelRequest");
            }
            _ => panic!("expected Bytes"),
        }

        let mut stdin_reader = BufReader::new(&mut stdin_readable);
        let ack = read_next_lsp_json(&mut stdin_reader).await;
        assert_eq!(ack["jsonrpc"], "2.0");
        assert_eq!(ack["id"], 42);
        assert!(ack["result"].is_null());
    }
}
