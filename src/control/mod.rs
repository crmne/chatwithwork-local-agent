//! The local control channel, used by the CLI, the TUI and the settings app
//! to talk to the daemon. CONTROL.md documents it for other clients.
//!
//! - **Linux and macOS:** a Unix socket at `$XDG_RUNTIME_DIR/cww/cww.sock`
//!   (mode 0600, in a 0700 directory). Every connection's peer UID must match
//!   the daemon's.
//! - **Windows:** a named pipe, `\\.\pipe\cww-<hash>`, whose DACL grants
//!   access to the current user only. Remote clients are rejected, and each
//!   side checks that the process at the other end runs as the same user.
//!
//! One JSON request per line, one JSON response per line. A `subscribe`
//! request turns the connection into a stream of events.
//!
//! The terminal UI's chats go through here too: the daemon relays them to
//! Chat with Work with its own token, so no client ever holds one.

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

#[cfg(unix)]
use unix as transport;
#[cfg(windows)]
use windows as transport;

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{
    AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader as TokioBufReader,
};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

pub use transport::Listener;

/// Bumped when a request or response changes incompatibly. Additions don't
/// bump it: clients must ignore fields they don't know.
pub const PROTOCOL_VERSION: u32 = 1;

/// Longest request line accepted.
const MAX_REQUEST_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "cmd")]
pub enum ControlRequest {
    /// Protocol and daemon version.
    Hello,
    /// Connection, pairing, pause state, and roots with their index state.
    Status,
    Pause,
    Resume,
    /// Re-read the config file (after `cww roots add`, `cww login`, ...).
    Reload,
    /// Stop the daemon. The service manager may start it again (systemd and
    /// launchd restart it only after a failure; this is a clean exit).
    Shutdown,
    /// Share a folder. Same rules as `cww roots add`.
    RootsAdd {
        path: PathBuf,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        #[serde(default)]
        follow_symlinks: bool,
        #[serde(default)]
        i_know: bool,
    },
    /// Stop sharing a folder, by ID, label or path.
    RootsRemove {
        root: String,
    },
    /// The last `lines` audit entries (default 50, at most 1000), oldest first.
    AuditTail {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        lines: Option<usize>,
    },
    /// Folders worth offering on first run (the Documents folder), with
    /// whether they exist and are already shared. Nothing is shared until
    /// the user confirms with `roots_add`.
    SuggestedRoots,
    /// Stream events on this connection until it closes. With the `chat`
    /// topic, `chat` names the chat to follow, which the daemon follows on
    /// the server for as long as the subscription is open.
    Subscribe {
        #[serde(default = "all_topics")]
        topics: Vec<Topic>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        chat: Option<String>,
    },
    /// The owner's chats, newest first, as the server lists them.
    Chats,
    /// One chat and its transcript.
    Chat {
        chat: String,
    },
    /// Ask a question in `chat`, or in a new chat without one.
    ChatSend {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        chat: Option<String>,
        text: String,
        /// A project for a new chat.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        project: Option<u64>,
    },
    /// Stop the answer being written in `chat`.
    ChatCancel {
        chat: String,
    },
    /// Ask the owner to let this computer use their chats. Nothing is
    /// allowed until they say yes in Chat with Work.
    ChatAccess,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Topic {
    /// Every new audit log entry.
    Audit,
    /// The full `status` result, whenever something in it changes.
    Status,
    /// Updates of the chat named in `subscribe`: streamed answer text,
    /// progress, and "read it again".
    Chat,
}

fn all_topics() -> Vec<Topic> {
    vec![Topic::Audit, Topic::Status]
}

/// Something a subscriber may want to know about.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum ControlEvent {
    Audit {
        entry: Box<crate::audit::AuditEntry>,
    },
    Status {
        status: Value,
    },
    /// One update of a followed chat, as the server sent it, or a note from
    /// the daemon about the subscription (`watching`, `refused`, `offline`).
    Chat {
        chat: String,
        update: Value,
    },
}

impl ControlEvent {
    fn topic(&self) -> Topic {
        match self {
            Self::Audit { .. } => Topic::Audit,
            Self::Status { .. } => Topic::Status,
            Self::Chat { .. } => Topic::Chat,
        }
    }

    fn concerns(&self, followed: Option<&str>) -> bool {
        match self {
            Self::Chat { chat, .. } => followed == Some(chat.as_str()),
            _ => true,
        }
    }
}

/// A failure with a code a client can act on, such as
/// `chat_access_required`. It reaches the client as
/// `{"ok": false, "error": <message>, "code": <code>, ...details}`.
#[derive(Debug, Clone, thiserror::Error)]
#[error("{message}")]
pub struct Refusal {
    pub code: String,
    pub message: String,
    /// More fields for the response, such as `approve_url`.
    pub details: Value,
}

impl Refusal {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_string(),
            message: message.into(),
            details: json!({}),
        }
    }

    pub fn with_details(mut self, details: Value) -> Self {
        self.details = details;
        self
    }
}

/// How often a chat subscription hears from the daemon while nothing
/// happens, so a client that stopped listening is noticed.
const CHAT_HEARTBEAT: Duration = Duration::from_secs(30);

pub trait ControlHandler: Send + Sync + 'static {
    fn handle(&self, request: ControlRequest) -> impl Future<Output = Result<Value>> + Send;

    /// Events for subscribers, if the handler publishes any.
    fn events(&self) -> Option<broadcast::Receiver<ControlEvent>> {
        None
    }

    /// The first `status` event a new subscriber gets.
    fn initial_status(&self) -> impl Future<Output = Option<Value>> + Send {
        async { None }
    }

    /// Start following `chat` for a new subscriber. Every call is matched
    /// by one [`ControlHandler::unwatch_chat`] when the subscription ends.
    fn watch_chat(&self, _chat: &str) -> impl Future<Output = Result<()>> + Send {
        async { bail!("chats are not available") }
    }

    fn unwatch_chat(&self, _chat: &str) {}
}

/// Bind the control endpoint, refusing if another daemon is already
/// listening.
pub fn bind(path: &Path) -> Result<Listener> {
    transport::bind(path)
}

pub async fn serve<H: ControlHandler>(
    mut listener: Listener,
    handler: Arc<H>,
    shutdown: CancellationToken,
) {
    loop {
        let stream = tokio::select! {
            () = shutdown.cancelled() => return,
            accepted = listener.accept() => match accepted {
                Ok(stream) => stream,
                Err(e) => {
                    tracing::warn!("control connection refused: {e:#}");
                    // Don't spin if accepting keeps failing.
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    continue;
                }
            },
        };
        let handler = Arc::clone(&handler);
        let shutdown = shutdown.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_connection(stream, handler, shutdown).await {
                tracing::debug!("control connection: {e:#}");
            }
        });
    }
}

async fn handle_connection<S, H>(
    stream: S,
    handler: Arc<H>,
    shutdown: CancellationToken,
) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Send + Unpin,
    H: ControlHandler,
{
    let (read, mut write) = tokio::io::split(stream);
    let mut lines = TokioBufReader::new(read).lines();
    loop {
        let line = tokio::select! {
            () = shutdown.cancelled() => return Ok(()),
            line = tokio::time::timeout(Duration::from_secs(300), lines.next_line()) => line??,
        };
        let Some(line) = line else { return Ok(()) };
        if line.len() > MAX_REQUEST_BYTES {
            bail!("control request too long");
        }
        let request = match serde_json::from_str::<ControlRequest>(&line) {
            Ok(request) => request,
            Err(e) => {
                write_line(
                    &mut write,
                    &json!({ "ok": false, "error": format!("bad request: {e}") }),
                )
                .await?;
                continue;
            }
        };
        if let ControlRequest::Subscribe { topics, chat } = request {
            return stream_events(&mut write, handler.as_ref(), &topics, chat, shutdown).await;
        }
        let response = match handler.handle(request).await {
            Ok(mut value) => {
                if !value.is_object() {
                    value = json!({ "result": value });
                }
                value["ok"] = json!(true);
                value
            }
            Err(e) => refusal_response(&e),
        };
        write_line(&mut write, &response).await?;
    }
}

fn refusal_response(e: &anyhow::Error) -> Value {
    let mut response = json!({ "ok": false, "error": format!("{e:#}") });
    if let Some(refusal) = e.downcast_ref::<Refusal>() {
        if let Some(details) = refusal.details.as_object() {
            for (key, value) in details {
                response[key] = value.clone();
            }
        }
        response["error"] = json!(refusal.message);
        response["code"] = json!(refusal.code);
    }
    response
}

async fn stream_events<W, H>(
    write: &mut W,
    handler: &H,
    topics: &[Topic],
    chat: Option<String>,
    shutdown: CancellationToken,
) -> Result<()>
where
    W: AsyncWrite + Unpin,
    H: ControlHandler,
{
    let Some(events) = handler.events() else {
        write_line(
            write,
            &json!({ "ok": false, "error": "events are not available" }),
        )
        .await?;
        return Ok(());
    };
    let followed = match (topics.contains(&Topic::Chat), chat) {
        (false, _) => None,
        (true, None) => {
            let refusal = anyhow::Error::new(Refusal::new(
                "bad_request",
                "bad request: the chat topic needs a chat",
            ));
            return write_line(write, &refusal_response(&refusal)).await;
        }
        (true, Some(chat)) => {
            if let Err(e) = handler.watch_chat(&chat).await {
                return write_line(write, &refusal_response(&e)).await;
            }
            Some(chat)
        }
    };
    let result = forward_events(
        write,
        handler,
        topics,
        followed.as_deref(),
        events,
        shutdown,
    )
    .await;
    if let Some(chat) = &followed {
        handler.unwatch_chat(chat);
    }
    result
}

async fn forward_events<W, H>(
    write: &mut W,
    handler: &H,
    topics: &[Topic],
    followed: Option<&str>,
    mut events: broadcast::Receiver<ControlEvent>,
    shutdown: CancellationToken,
) -> Result<()>
where
    W: AsyncWrite + Unpin,
    H: ControlHandler,
{
    write_line(write, &json!({ "ok": true, "topics": topics })).await?;
    if topics.contains(&Topic::Status)
        && let Some(status) = handler.initial_status().await
    {
        write_line(
            write,
            &serde_json::to_value(ControlEvent::Status { status })?,
        )
        .await?;
    }
    let mut heartbeat = tokio::time::interval(CHAT_HEARTBEAT);
    heartbeat.tick().await;
    loop {
        let event = tokio::select! {
            () = shutdown.cancelled() => return Ok(()),
            event = events.recv() => event,
            _ = heartbeat.tick(), if followed.is_some() => {
                write_line(write, &json!({ "event": "heartbeat" })).await?;
                continue;
            }
        };
        match event {
            Ok(event) if topics.contains(&event.topic()) && event.concerns(followed) => {
                write_line(write, &serde_json::to_value(&event)?).await?;
            }
            Ok(_) => {}
            // A slow client missed some events. Tell it, and carry on.
            Err(broadcast::error::RecvError::Lagged(n)) => {
                write_line(write, &json!({ "event": "lagged", "missed": n })).await?;
            }
            Err(broadcast::error::RecvError::Closed) => return Ok(()),
        }
    }
}

async fn write_line<W: AsyncWrite + Unpin>(write: &mut W, value: &Value) -> Result<()> {
    let mut out = serde_json::to_vec(value)?;
    out.push(b'\n');
    write.write_all(&out).await?;
    write.flush().await?;
    Ok(())
}

/// A blocking connection to the daemon, for the CLI and the TUI.
pub struct Client {
    reader: BufReader<transport::ClientStream>,
}

impl Client {
    /// Connect to the daemon. `Ok(None)` means no daemon is listening.
    pub fn connect(path: &Path) -> Result<Option<Self>> {
        Ok(transport::connect(path)?.map(|stream| Self {
            reader: BufReader::new(stream),
        }))
    }

    /// Send one request and wait for its response.
    pub fn call(&mut self, request: &ControlRequest) -> Result<Value> {
        let value = self.call_raw(request)?;
        if value["ok"] != json!(true) {
            bail!(
                "{}",
                value["error"]
                    .as_str()
                    .unwrap_or("the daemon returned an error")
            );
        }
        Ok(value)
    }

    /// Send one request and return its response as it came, `ok` or not,
    /// for callers that act on a refusal's `code`.
    pub fn call_raw(&mut self, request: &ControlRequest) -> Result<Value> {
        self.send(request)?;
        self.next_line()?
            .context("the daemon closed the connection")
    }

    /// Turn this connection into an event stream. Each item is one event
    /// object (`{"event":"audit",...}` or `{"event":"status",...}`).
    pub fn subscribe(mut self, topics: &[Topic]) -> Result<Events> {
        self.call(&ControlRequest::Subscribe {
            topics: topics.to_vec(),
            chat: None,
        })?;
        transport::clear_timeout(self.reader.get_ref());
        Ok(Events { client: self })
    }

    /// Follow one chat: `{"event":"chat",...}` updates and heartbeats. A
    /// refusal comes back as the response's `code`, like other chat calls.
    pub fn follow_chat(mut self, chat: &str) -> Result<std::result::Result<Events, Value>> {
        let response = self.call_raw(&ControlRequest::Subscribe {
            topics: vec![Topic::Chat],
            chat: Some(chat.to_string()),
        })?;
        if response["ok"] != json!(true) {
            return Ok(Err(response));
        }
        transport::clear_timeout(self.reader.get_ref());
        Ok(Ok(Events { client: self }))
    }

    fn send(&mut self, request: &ControlRequest) -> Result<()> {
        let mut line = serde_json::to_vec(request)?;
        line.push(b'\n');
        let stream = self.reader.get_mut();
        stream.write_all(&line)?;
        stream.flush()?;
        Ok(())
    }

    fn next_line(&mut self) -> Result<Option<Value>> {
        let mut line = String::new();
        if self.reader.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        Ok(Some(
            serde_json::from_str(&line).context("bad response from the daemon")?,
        ))
    }
}

/// Events from [`Client::subscribe`]. Ends when the daemon stops.
pub struct Events {
    client: Client,
}

impl Events {
    /// Something that ends this stream from another thread, where the
    /// platform allows it. Otherwise the stream ends at the next event,
    /// at the latest the next heartbeat.
    pub fn closer(&self) -> Option<Closer> {
        #[cfg(unix)]
        {
            self.client.reader.get_ref().try_clone().ok().map(Closer)
        }
        #[cfg(windows)]
        {
            None
        }
    }
}

#[cfg(unix)]
pub struct Closer(std::os::unix::net::UnixStream);

#[cfg(windows)]
pub struct Closer;

impl Closer {
    pub fn close(&self) {
        #[cfg(unix)]
        let _ = self.0.shutdown(std::net::Shutdown::Both);
    }
}

impl Iterator for Events {
    type Item = Result<Value>;

    fn next(&mut self) -> Option<Self::Item> {
        self.client.next_line().transpose()
    }
}

/// Send one request to the running daemon. `Ok(None)` means no daemon is
/// listening.
pub fn request(path: &Path, request: ControlRequest) -> Result<Option<Value>> {
    match Client::connect(path)? {
        Some(mut client) => client.call(&request).map(Some),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Echo {
        events: broadcast::Sender<ControlEvent>,
    }

    impl ControlHandler for Echo {
        async fn handle(&self, request: ControlRequest) -> Result<Value> {
            match request {
                ControlRequest::Pause => bail!("nope"),
                other => Ok(json!({ "got": other })),
            }
        }

        fn events(&self) -> Option<broadcast::Receiver<ControlEvent>> {
            Some(self.events.subscribe())
        }

        async fn initial_status(&self) -> Option<Value> {
            Some(json!({ "paused": false }))
        }
    }

    fn endpoint(tmp: &Path) -> PathBuf {
        crate::paths::Paths::under(tmp).socket_path()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn round_trip_subscribe_and_single_daemon() {
        let tmp = tempfile::tempdir().unwrap();
        let path = endpoint(tmp.path());
        let listener = bind(&path).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        assert!(bind(&path).is_err(), "second daemon refused");

        let (events, _) = broadcast::channel(16);
        let handler = Arc::new(Echo {
            events: events.clone(),
        });
        let shutdown = CancellationToken::new();
        let task = tokio::spawn(serve(listener, handler, shutdown.clone()));

        let p = path.clone();
        let (status, err) = tokio::task::spawn_blocking(move || {
            let mut client = Client::connect(&p).unwrap().unwrap();
            let status = client.call(&ControlRequest::Status).unwrap();
            let err = client.call(&ControlRequest::Pause).unwrap_err();
            (status, err)
        })
        .await
        .unwrap();
        assert_eq!(status["got"]["cmd"], "status");
        assert!(err.to_string().contains("nope"));

        let p = path.clone();
        let subscriber = tokio::task::spawn_blocking(move || {
            let client = Client::connect(&p).unwrap().unwrap();
            let mut events = client.subscribe(&[Topic::Audit, Topic::Status]).unwrap();
            let first = events.next().unwrap().unwrap();
            let second = events.next().unwrap().unwrap();
            (first, second)
        });
        // Wait until the subscriber is registered, then publish.
        while events.receiver_count() == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        events
            .send(ControlEvent::Audit {
                entry: Box::new(crate::audit::AuditEntry::event("connected")),
            })
            .unwrap();
        let (first, second) = subscriber.await.unwrap();
        assert_eq!(first["event"], "status");
        assert_eq!(first["status"]["paused"], false);
        assert_eq!(second["event"], "audit");
        assert_eq!(second["entry"]["event"], "connected");

        shutdown.cancel();
        task.await.unwrap();

        let missing = endpoint(&tmp.path().join("none"));
        assert!(request(&missing, ControlRequest::Status).unwrap().is_none());
    }

    #[test]
    fn requests_have_a_stable_wire_format() {
        let add: ControlRequest =
            serde_json::from_str(r#"{"cmd":"roots_add","path":"/tmp/x","label":"X"}"#).unwrap();
        assert_eq!(
            add,
            ControlRequest::RootsAdd {
                path: "/tmp/x".into(),
                label: Some("X".into()),
                follow_symlinks: false,
                i_know: false,
            }
        );
        let sub: ControlRequest = serde_json::from_str(r#"{"cmd":"subscribe"}"#).unwrap();
        assert_eq!(
            sub,
            ControlRequest::Subscribe {
                topics: vec![Topic::Audit, Topic::Status],
                chat: None,
            }
        );
        let follow: ControlRequest =
            serde_json::from_str(r#"{"cmd":"subscribe","topics":["chat"],"chat":"42"}"#).unwrap();
        assert_eq!(
            follow,
            ControlRequest::Subscribe {
                topics: vec![Topic::Chat],
                chat: Some("42".into()),
            }
        );
        assert_eq!(
            serde_json::to_string(&ControlRequest::ChatSend {
                chat: None,
                text: "hi".into(),
                project: None
            })
            .unwrap(),
            r#"{"cmd":"chat_send","text":"hi"}"#
        );
        assert_eq!(
            serde_json::to_string(&ControlRequest::AuditTail { lines: Some(5) }).unwrap(),
            r#"{"cmd":"audit_tail","lines":5}"#
        );
    }
}
