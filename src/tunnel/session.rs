//! One WebSocket session: bridge frames to the rmcp server and back.

use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use rmcp::RoleServer;
use rmcp::model::{ClientJsonRpcMessage, ErrorCode, ErrorData, RequestId, ServerJsonRpcMessage};
use rmcp::transport::Transport;
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{Mutex, mpsc};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_util::sync::CancellationToken;

use super::framing::{Framing, Inbound};
use crate::tools::LocalFiles;

/// How often the daemon pings the server.
pub const PING_EVERY: Duration = Duration::from_secs(20);
/// With no frame from the server for this long, the connection is dead.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(60);

/// Close codes the server may use (see PROTOCOL.md).
pub const CLOSE_REAUTHENTICATE: u16 = 4001;
pub const CLOSE_REVOKED: u16 = 4003;

#[derive(Debug)]
pub enum SessionEnd {
    /// The server closed the socket.
    Closed {
        code: Option<u16>,
        reason: String,
    },
    /// The server said the device is revoked or unauthorized; refresh the
    /// token before reconnecting, and stop if that fails.
    Unauthorized(String),
    IdleTimeout,
    Shutdown,
    Error(String),
}

/// The rmcp transport: messages flow through channels fed by the socket.
struct ChannelTransport {
    tx: mpsc::Sender<ServerJsonRpcMessage>,
    rx: Arc<Mutex<mpsc::Receiver<ClientJsonRpcMessage>>>,
}

#[derive(Debug, thiserror::Error)]
#[error("the WebSocket session ended")]
pub struct SessionClosed;

impl Transport<RoleServer> for ChannelTransport {
    type Error = SessionClosed;

    fn send(
        &mut self,
        item: ServerJsonRpcMessage,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send + 'static {
        let tx = self.tx.clone();
        async move { tx.send(item).await.map_err(|_| SessionClosed) }
    }

    fn receive(&mut self) -> impl Future<Output = Option<ClientJsonRpcMessage>> + Send {
        let rx = Arc::clone(&self.rx);
        async move { rx.lock().await.recv().await }
    }

    async fn close(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

pub struct SessionOptions {
    pub framing: Framing,
    /// Largest JSON-RPC message accepted or sent.
    pub max_message_bytes: usize,
}

pub async fn run_session<S>(
    ws: WebSocketStream<S>,
    server: LocalFiles,
    options: SessionOptions,
    shutdown: CancellationToken,
) -> SessionEnd
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (mut sink, mut stream) = ws.split();
    let (in_tx, in_rx) = mpsc::channel::<ClientJsonRpcMessage>(64);
    let (out_tx, mut out_rx) = mpsc::channel::<ServerJsonRpcMessage>(64);
    let in_rx = Arc::new(Mutex::new(in_rx));
    let session = CancellationToken::new();

    // The MCP side. rmcp ends a service when the first message is not a
    // valid opener (for example a stateless request without `_meta`); start a
    // fresh one so one bad message can't wedge the connection.
    let mcp_task = {
        let out_tx = out_tx.clone();
        let session = session.clone();
        tokio::spawn(async move {
            while !session.is_cancelled() {
                let transport = ChannelTransport {
                    tx: out_tx.clone(),
                    rx: Arc::clone(&in_rx),
                };
                match rmcp::serve_server(server.clone(), transport).await {
                    Ok(running) => {
                        let _ = running.waiting().await;
                    }
                    Err(e) => tracing::debug!("MCP session did not start: {e}"),
                }
                if in_rx.lock().await.is_closed() {
                    break;
                }
            }
        })
    };

    let framing = options.framing;
    for frame in framing.opening_frames() {
        if let Err(e) = sink.send(Message::text(frame)).await {
            mcp_task.abort();
            return SessionEnd::Error(e.to_string());
        }
    }

    let mut last_seen = Instant::now();
    let mut ping = tokio::time::interval(PING_EVERY);
    ping.tick().await;

    let end = loop {
        tokio::select! {
            () = shutdown.cancelled() => {
                let _ = sink
                    .send(Message::Close(Some(CloseFrame {
                        code: CloseCode::Away,
                        reason: "daemon stopping".into(),
                    })))
                    .await;
                break SessionEnd::Shutdown;
            }
            frame = stream.next() => {
                last_seen = Instant::now();
                match frame {
                    None => break SessionEnd::Closed { code: None, reason: "connection lost".into() },
                    Some(Err(e)) => break SessionEnd::Error(e.to_string()),
                    Some(Ok(Message::Close(frame))) => {
                        let code = frame.as_ref().map(|f| u16::from(f.code));
                        let reason = frame.map(|f| f.reason.to_string()).unwrap_or_default();
                        if matches!(code, Some(CLOSE_REVOKED | CLOSE_REAUTHENTICATE)) {
                            break SessionEnd::Unauthorized(reason);
                        }
                        break SessionEnd::Closed { code, reason };
                    }
                    Some(Ok(Message::Text(text))) => {
                        match framing.decode(text.as_str()) {
                            Inbound::Mcp(value) => {
                                if let Some(reply) = accept(value, options.max_message_bytes, &in_tx).await {
                                    let _ = out_tx.send(reply).await;
                                }
                            }
                            Inbound::Rejected => break SessionEnd::Unauthorized("subscription rejected".into()),
                            Inbound::Disconnect { reason, reconnect } => {
                                if !reconnect || reason == "unauthorized" {
                                    break SessionEnd::Unauthorized(reason);
                                }
                                break SessionEnd::Closed { code: None, reason };
                            }
                            Inbound::Welcome | Inbound::Ping | Inbound::Confirmed | Inbound::Ignored => {}
                        }
                    }
                    // Pings are answered by tungstenite; binary frames are not part of the protocol.
                    Some(Ok(_)) => {}
                }
            }
            outgoing = out_rx.recv() => {
                let Some(message) = outgoing else {
                    break SessionEnd::Error("MCP server stopped".into());
                };
                let Some(text) = serialize_capped(message, options.max_message_bytes) else {
                    continue;
                };
                if let Err(e) = sink.send(Message::text(framing.encode(&text))).await {
                    break SessionEnd::Error(e.to_string());
                }
            }
            _ = ping.tick() => {
                if last_seen.elapsed() > IDLE_TIMEOUT {
                    break SessionEnd::IdleTimeout;
                }
                if let Err(e) = sink.send(Message::Ping(Vec::new().into())).await {
                    break SessionEnd::Error(e.to_string());
                }
            }
        }
    };

    session.cancel();
    drop(in_tx);
    mcp_task.abort();
    end
}

/// Validate one inbound JSON-RPC message and hand it to rmcp. Returns an
/// error reply for requests that can't be accepted.
async fn accept(
    value: Value,
    max_bytes: usize,
    in_tx: &mpsc::Sender<ClientJsonRpcMessage>,
) -> Option<ServerJsonRpcMessage> {
    let id = value
        .get("id")
        .cloned()
        .and_then(|id| serde_json::from_value::<RequestId>(id).ok());
    let is_request = value.get("method").is_some() && id.is_some();
    // Responses: the daemon never sends requests, so there is nothing to match.
    value.get("method")?;
    let size = serde_json::to_vec(&value).map_or(usize::MAX, |v| v.len());
    if size > max_bytes {
        return is_request.then(|| {
            error_reply(
                id.clone(),
                ErrorData::new(ErrorCode::INVALID_REQUEST, "message too large", None),
            )
        });
    }
    match serde_json::from_value::<ClientJsonRpcMessage>(value) {
        Ok(message) => {
            let _ = in_tx.send(message).await;
            None
        }
        // Unknown or malformed methods. Notifications are dropped silently.
        Err(_) => is_request.then(|| {
            error_reply(
                id.clone(),
                ErrorData::new(
                    ErrorCode::METHOD_NOT_FOUND,
                    "method not found or invalid params",
                    None,
                ),
            )
        }),
    }
}

fn error_reply(id: Option<RequestId>, error: ErrorData) -> ServerJsonRpcMessage {
    ServerJsonRpcMessage::error(error, id)
}

/// Serialize an outgoing message, replacing responses that exceed the cap
/// with an error so the server isn't left waiting.
fn serialize_capped(message: ServerJsonRpcMessage, max_bytes: usize) -> Option<String> {
    let text = serde_json::to_string(&message).ok()?;
    if text.len() <= max_bytes {
        return Some(text);
    }
    let id = serde_json::from_str::<Value>(&text)
        .ok()
        .and_then(|v| v.get("id").cloned())
        .and_then(|id| serde_json::from_value::<RequestId>(id).ok())?;
    let reply = error_reply(
        Some(id),
        ErrorData::new(ErrorCode::INTERNAL_ERROR, "response too large", None),
    );
    serde_json::to_string(&reply).ok()
}
