//! End to end: a fake Chat with Work server (token endpoint, device
//! authorization, and an Action Cable style WebSocket at /local_agent) drives
//! a real daemon over the wire, the way the Rails side will.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::http::{HeaderValue, StatusCode};
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_util::sync::CancellationToken;

use cww::auth::key::{DeviceKey, verify_proof};
use cww::auth::secrets::{DEVICE_KEY, REFRESH_TOKEN, SecretStore};
use cww::config::{Config, Root, ServerConfig};
use cww::control::{self, ControlRequest};
use cww::paths::Paths;

const IDENTIFIER: &str = r#"{"channel":"LocalAgent::Channel"}"#;
const ACCESS_TOKEN: &str = "access-1";

/// What the fake server knows and has seen.
#[derive(Default)]
struct ServerState {
    /// Device public key (JWK `x`) accepted for proofs.
    public_key: Option<String>,
    refresh_tokens: Vec<String>,
    revoked: bool,
    nonce: String,
    /// Device-code polls answered so far.
    polls: usize,
    /// Requests seen, as "METHOD path" strings.
    log: Vec<String>,
}

struct FakeServer {
    origin: String,
    state: Arc<Mutex<ServerState>>,
    sockets: mpsc::Receiver<WebSocketStream<TcpStream>>,
}

impl FakeServer {
    async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let state = Arc::new(Mutex::new(ServerState {
            nonce: "nonce-1".into(),
            refresh_tokens: vec!["refresh-1".into()],
            ..ServerState::default()
        }));
        let (tx, sockets) = mpsc::channel(4);
        let (st, org) = (Arc::clone(&state), origin.clone());
        tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let (st, org, tx) = (Arc::clone(&st), org.clone(), tx.clone());
                tokio::spawn(async move { handle(stream, st, org, tx).await });
            }
        });
        Self {
            origin,
            state,
            sockets,
        }
    }
}

async fn handle(
    stream: TcpStream,
    state: Arc<Mutex<ServerState>>,
    origin: String,
    sockets: mpsc::Sender<WebSocketStream<TcpStream>>,
) {
    // Peek at the request line to route without consuming the upgrade.
    let mut buf = [0u8; 2048];
    let n = loop {
        let n = stream.peek(&mut buf).await.unwrap();
        if n == 0 || buf[..n].windows(4).any(|w| w == b"\r\n\r\n") {
            break n;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    };
    let head = String::from_utf8_lossy(&buf[..n]).to_string();
    let request_line = head.lines().next().unwrap_or_default().to_string();
    state.lock().unwrap().log.push(request_line.clone());
    if request_line.starts_with("GET /local_agent ") {
        websocket(stream, state, origin, sockets).await;
    } else {
        http(stream, state, origin).await;
    }
}

/// The WebSocket upgrade: check the DPoP-bound token, pick Action Cable.
// tungstenite's callback signature fixes the error type.
#[allow(clippy::result_large_err)]
async fn websocket(
    stream: TcpStream,
    state: Arc<Mutex<ServerState>>,
    origin: String,
    sockets: mpsc::Sender<WebSocketStream<TcpStream>>,
) {
    let check = |req: &Request, mut resp: Response| -> Result<Response, ErrorResponse> {
        let st = state.lock().unwrap();
        let reject = |why: &str| {
            let mut r = ErrorResponse::new(Some(why.to_string()));
            *r.status_mut() = StatusCode::UNAUTHORIZED;
            r.headers_mut()
                .insert("DPoP-Nonce", HeaderValue::from_str(&st.nonce).unwrap());
            r
        };
        let header = |name: &str| req.headers().get(name).and_then(|v| v.to_str().ok());
        if header("Authorization") != Some(&format!("DPoP {ACCESS_TOKEN}")) || st.revoked {
            return Err(reject("bad token"));
        }
        let Some(key) = &st.public_key else {
            return Err(reject("no key"));
        };
        let claims = verify_proof(header("DPoP").unwrap_or_default(), key)
            .map_err(|_| reject("bad proof"))?;
        let ath = URL_SAFE_NO_PAD.encode(ring::digest::digest(
            &ring::digest::SHA256,
            ACCESS_TOKEN.as_bytes(),
        ));
        if claims["htm"] != "GET"
            || claims["htu"] != format!("{origin}/local_agent")
            || claims["ath"] != ath
            || claims["nonce"] != st.nonce.as_str()
        {
            return Err(reject("proof claims"));
        }
        assert_eq!(header("Origin"), Some(origin.as_str()));
        let offered = header("Sec-WebSocket-Protocol").unwrap_or_default();
        assert!(offered.contains("actioncable-v1-json"), "{offered}");
        resp.headers_mut().insert(
            "Sec-WebSocket-Protocol",
            HeaderValue::from_static("actioncable-v1-json"),
        );
        Ok(resp)
    };
    let Ok(mut ws) = tokio_tungstenite::accept_hdr_async(stream, check).await else {
        return;
    };
    ws.send(Message::text(json!({ "type": "welcome" }).to_string()))
        .await
        .unwrap();
    // The daemon subscribes to the channel first.
    let subscribe = next_text(&mut ws).await.expect("subscribe command");
    let subscribe: Value = serde_json::from_str(&subscribe).unwrap();
    assert_eq!(subscribe["command"], "subscribe");
    assert_eq!(subscribe["identifier"], IDENTIFIER);
    ws.send(Message::text(
        json!({ "identifier": IDENTIFIER, "type": "confirm_subscription" }).to_string(),
    ))
    .await
    .unwrap();
    let _ = sockets.send(ws).await;
}

async fn next_text(ws: &mut WebSocketStream<TcpStream>) -> Option<String> {
    while let Some(msg) = ws.next().await {
        match msg.ok()? {
            Message::Text(t) => return Some(t.to_string()),
            Message::Close(_) => return None,
            _ => {}
        }
    }
    None
}

/// Minimal HTTP/1.1 for the OAuth endpoints.
async fn http(mut stream: TcpStream, state: Arc<Mutex<ServerState>>, origin: String) {
    let mut data = Vec::new();
    let mut buf = [0u8; 4096];
    let (head, body) = loop {
        let n = stream.read(&mut buf).await.unwrap();
        data.extend_from_slice(&buf[..n]);
        if let Some(pos) = data.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&data[..pos]).to_string();
            let len: usize = head
                .lines()
                .find_map(|l| {
                    let (k, v) = l.split_once(':')?;
                    k.eq_ignore_ascii_case("content-length")
                        .then(|| v.trim().parse().ok())?
                })
                .unwrap_or(0);
            while data.len() < pos + 4 + len {
                let n = stream.read(&mut buf).await.unwrap();
                data.extend_from_slice(&buf[..n]);
            }
            break (
                head,
                String::from_utf8_lossy(&data[pos + 4..pos + 4 + len]).to_string(),
            );
        }
        if n == 0 {
            return;
        }
    };
    let path = head
        .split_whitespace()
        .nth(1)
        .unwrap_or_default()
        .to_string();
    let dpop = head
        .lines()
        .find_map(|l| {
            l.strip_prefix("dpop: ")
                .or_else(|| l.strip_prefix("DPoP: "))
        })
        .unwrap_or_default()
        .to_string();
    let form: HashMap<String, String> = url::form_urlencoded::parse(body.as_bytes())
        .into_owned()
        .collect();
    let (status, json) = oauth(&state, &origin, &path, &dpop, &form);
    let nonce = state.lock().unwrap().nonce.clone();
    let body = json.to_string();
    let response = format!(
        "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ndpop-nonce: {nonce}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes()).await.unwrap();
}

fn oauth(
    state: &Mutex<ServerState>,
    origin: &str,
    path: &str,
    dpop: &str,
    form: &HashMap<String, String>,
) -> (u16, Value) {
    let mut st = state.lock().unwrap();
    // Pairing registers the key; everything else must be signed by it.
    let key = if path == "/local_agent/device_authorizations" {
        form.get("public_key").cloned().unwrap_or_default()
    } else {
        st.public_key.clone().unwrap_or_default()
    };
    let Ok(claims) = verify_proof(dpop, &key) else {
        return (400, json!({ "error": "invalid_dpop_proof" }));
    };
    assert_eq!(claims["htm"], "POST");
    assert_eq!(claims["htu"], format!("{origin}{path}"));
    if claims["nonce"] != st.nonce.as_str() {
        return (400, json!({ "error": "use_dpop_nonce" }));
    }
    match (path, form.get("grant_type").map(String::as_str)) {
        ("/local_agent/device_authorizations", _) => {
            assert_eq!(form["scope"], "local_agent:serve");
            assert!(!form["name"].is_empty() && !form["platform"].is_empty());
            st.public_key = Some(key);
            (
                200,
                json!({
                    "device_code": "device-code-1",
                    "user_code": "WDJB-MJHT",
                    "verification_uri": format!("{origin}/device"),
                    "expires_in": 60,
                    "interval": 1,
                }),
            )
        }
        ("/local_agent/token", Some("urn:ietf:params:oauth:grant-type:device_code")) => {
            assert_eq!(form["device_code"], "device-code-1");
            st.polls += 1;
            if st.polls == 1 {
                return (400, json!({ "error": "authorization_pending" }));
            }
            (
                200,
                json!({
                    "access_token": ACCESS_TOKEN,
                    "token_type": "DPoP",
                    "expires_in": 600,
                    "scope": "local_agent:serve",
                    "refresh_token": "refresh-1",
                    "device_id": "42",
                }),
            )
        }
        ("/local_agent/token", Some("refresh_token")) => {
            let given = form.get("refresh_token").cloned().unwrap_or_default();
            if st.revoked || !st.refresh_tokens.contains(&given) {
                return (400, json!({ "error": "invalid_grant" }));
            }
            // Rotate the refresh credential on every use.
            let next = format!("refresh-{}", st.refresh_tokens.len() + 1);
            st.refresh_tokens = vec![next.clone()];
            (
                200,
                json!({
                    "access_token": ACCESS_TOKEN,
                    "token_type": "DPoP",
                    "expires_in": 600,
                    "scope": "local_agent:serve",
                    "refresh_token": next,
                }),
            )
        }
        _ => (404, json!({ "error": "not_found" })),
    }
}

/// The server side of one connected daemon.
struct Session {
    ws: WebSocketStream<TcpStream>,
    next_id: u64,
    /// Negotiated with `initialize` (MCP 2025-11-25) instead of stateless.
    legacy: bool,
}

impl Session {
    /// Send a stateless MCP 2026-07-28 request and wait for its response.
    async fn request(&mut self, method: &str, mut params: Value) -> Value {
        self.next_id += 1;
        let id = self.next_id;
        params["_meta"] = if self.legacy {
            json!({ "com.chatwithwork/chatId": "chat-7" })
        } else {
            json!({
                "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                "io.modelcontextprotocol/clientInfo": { "name": "chatwithwork", "version": "test" },
                "io.modelcontextprotocol/clientCapabilities": {},
                "com.chatwithwork/chatId": "chat-7",
            })
        };
        self.send_raw(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))
            .await;
        self.response(id).await
    }

    async fn send_raw(&mut self, message: Value) {
        self.ws
            .send(Message::text(
                json!({ "identifier": IDENTIFIER, "message": message }).to_string(),
            ))
            .await
            .unwrap();
    }

    async fn response(&mut self, id: u64) -> Value {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let text = tokio::time::timeout_at(deadline.into(), next_text(&mut self.ws))
                .await
                .expect("response in time")
                .expect("socket open");
            let frame: Value = serde_json::from_str(&text).unwrap();
            assert_eq!(frame["command"], "message", "{frame}");
            assert_eq!(frame["identifier"], IDENTIFIER);
            let message: Value = serde_json::from_str(frame["data"].as_str().unwrap()).unwrap();
            if message["id"] == json!(id) {
                return message;
            }
        }
    }

    async fn call(&mut self, tool: &str, arguments: Value) -> Value {
        let response = self
            .request(
                "tools/call",
                json!({ "name": tool, "arguments": arguments }),
            )
            .await;
        assert!(response.get("error").is_none(), "{response}");
        response["result"].clone()
    }
}

fn error_code(result: &Value) -> &str {
    assert_eq!(result["isError"], true, "expected a tool error: {result}");
    result["structuredContent"]["error"]["code"]
        .as_str()
        .unwrap()
}

struct Fixture {
    _tmp: tempfile::TempDir,
    base: PathBuf,
    paths: Paths,
}

fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path().canonicalize().unwrap();
    let root = base.join("work/docs");
    let outside = base.join("outside");
    std::fs::create_dir_all(root.join("plans")).unwrap();
    std::fs::create_dir_all(root.join("keys")).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(
        root.join("notes.md"),
        "# Notes\nThe quarterly budget for Project Falcon is approved.\n",
    )
    .unwrap();
    std::fs::write(
        root.join("plans/q3.txt"),
        "Falcon launch moves to October.\n",
    )
    .unwrap();
    std::fs::write(root.join(".env"), "API_TOKEN=supersecret-env\n").unwrap();
    std::fs::write(root.join("keys/id_rsa"), "supersecret-key Falcon\n").unwrap();
    std::fs::write(outside.join("secret.txt"), "supersecret-outside Falcon\n").unwrap();
    link_file(&outside.join("secret.txt"), &root.join("escape.txt"));
    link_dir(&outside, &root.join("linked"));
    std::fs::hard_link(outside.join("secret.txt"), root.join("hardlink.txt")).unwrap();
    let paths = Paths::under(&base.join("cww"));
    Fixture {
        _tmp: tmp,
        base,
        paths,
    }
}

fn pair(fixture: &Fixture, server: &FakeServer) {
    let key = DeviceKey::generate().unwrap();
    server.state.lock().unwrap().public_key = Some(key.public_key());
    let store = SecretStore::File(fixture.paths.secrets_file());
    store.set(DEVICE_KEY, &key.to_stored()).unwrap();
    store.set(REFRESH_TOKEN, "refresh-1").unwrap();
    let config = Config {
        server: Some(ServerConfig {
            url: server.origin.clone(),
            device_id: "42".into(),
        }),
        secret_store: Some("file".into()),
        roots: vec![Root {
            id: "docs".into(),
            label: "Work docs".into(),
            path: fixture.base.join("work/docs"),
            follow_symlinks: false,
        }],
        ..Config::default()
    };
    config.save(&fixture.paths).unwrap();
}

async fn control(paths: &Paths, request: ControlRequest) -> Value {
    let socket = paths.socket_path();
    tokio::task::spawn_blocking(move || control::request(&socket, request))
        .await
        .unwrap()
        .unwrap()
        .expect("daemon running")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn serves_tools_and_enforces_policy_over_the_wire() {
    let fx = fixture();
    let mut server = FakeServer::start().await;
    pair(&fx, &server);

    let shutdown = CancellationToken::new();
    let daemon = tokio::spawn(cww::daemon::run(fx.paths.clone(), shutdown.clone(), false));
    let ws = tokio::time::timeout(Duration::from_secs(20), server.sockets.recv())
        .await
        .expect("the daemon connects")
        .unwrap();
    let mut s = Session {
        ws,
        next_id: 0,
        legacy: false,
    };
    // The first token request had no nonce and was told to retry with one.
    let log = server.state.lock().unwrap().log.clone();
    assert!(
        log.iter()
            .filter(|l| l.starts_with("POST /local_agent/token"))
            .count()
            >= 2,
        "{log:?}"
    );

    // Stateless discovery and the tool list.
    let discover = s.request("server/discover", json!({})).await;
    assert!(
        discover["result"]["supportedVersions"]
            .as_array()
            .unwrap()
            .contains(&json!("2026-07-28")),
        "{discover}"
    );
    let tools = s.request("tools/list", json!({})).await;
    let tools = tools["result"]["tools"].as_array().unwrap().clone();
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["roots", "search", "list", "read"]);
    for tool in &tools {
        assert_eq!(tool["annotations"]["readOnlyHint"], true);
        assert_eq!(tool["annotations"]["openWorldHint"], false);
    }

    // Roots: IDs and labels only.
    let roots = s.call("roots", json!({})).await;
    assert_eq!(roots["structuredContent"]["roots"][0]["id"], "docs");
    assert_eq!(roots["structuredContent"]["roots"][0]["label"], "Work docs");

    // Wait for the first index pass so search uses the index.
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let roots = s.call("roots", json!({})).await;
        if roots["structuredContent"]["roots"][0]["index"] == "ready" {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "index never became ready: {roots}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // A normal read and a paged read.
    let read = s.call("read", json!({ "path": "docs:notes.md" })).await;
    assert_eq!(read["isError"], false);
    assert!(
        read["structuredContent"]["text"]
            .as_str()
            .unwrap()
            .contains("Project Falcon")
    );
    let page = s
        .call(
            "read",
            json!({ "path": "docs:notes.md", "offset": 2, "max_chars": 5 }),
        )
        .await;
    assert_eq!(page["structuredContent"]["text"], "Notes");
    assert_eq!(page["structuredContent"]["next_offset"], 7);

    // Search finds the files inside the root and nothing denied or outside.
    let search = s.call("search", json!({ "query": "Falcon" })).await;
    let hits: Vec<&str> = search["structuredContent"]["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["path"].as_str().unwrap())
        .collect();
    assert!(
        hits.contains(&"docs:notes.md") && hits.contains(&"docs:plans/q3.txt"),
        "{search}"
    );
    assert_eq!(hits.len(), 2, "{search}");
    assert!(
        search["structuredContent"]["engine"]
            .as_str()
            .unwrap()
            .starts_with("index")
    );
    let glob = s
        .call("search", json!({ "query": "Falcon", "path_glob": "*.txt" }))
        .await;
    assert_eq!(
        glob["structuredContent"]["hits"].as_array().unwrap().len(),
        1
    );
    // Grep fallback: a substring the tokenizer can't find.
    let grep = s.call("search", json!({ "query": "udget for Proj" })).await;
    assert_eq!(
        grep["structuredContent"]["hits"][0]["path"], "docs:notes.md",
        "{grep}"
    );

    // Listing hides denied entries and never follows the symlinked folder.
    let list = s.call("list", json!({ "path": "docs:" })).await;
    let names: Vec<&str> = list["structuredContent"]["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"notes.md") && names.contains(&"plans"));
    assert!(!names.contains(&".env"));
    assert_eq!(
        error_code(&s.call("list", json!({ "path": "docs:linked" })).await),
        "denied"
    );

    // Reads outside the roots.
    for (path, code) in [
        ("docs:../outside/secret.txt", "invalid_path"),
        ("docs:plans/../../../outside/secret.txt", "invalid_path"),
        ("/etc/passwd", "invalid_path"),
        ("~/.ssh/id_ed25519", "invalid_path"),
        ("C:\\Windows\\win.ini", "invalid_path"),
        ("docs:notes.md\u{0}.txt", "invalid_path"),
        ("elsewhere:secret.txt", "unknown_root"),
    ] {
        let result = s.call("read", json!({ "path": path })).await;
        assert_eq!(error_code(&result), code, "{path}: {result}");
    }
    // Denied secrets inside the root.
    for path in ["docs:.env", "docs:keys/id_rsa"] {
        assert_eq!(
            error_code(&s.call("read", json!({ "path": path })).await),
            "denied",
            "{path}"
        );
    }
    // Symlink escapes and a hard link to a file outside the root.
    for path in [
        "docs:escape.txt",
        "docs:linked/secret.txt",
        "docs:hardlink.txt",
    ] {
        assert_eq!(
            error_code(&s.call("read", json!({ "path": path })).await),
            "denied",
            "{path}"
        );
    }

    // Bad arguments and unknown tools.
    assert_eq!(
        error_code(
            &s.call("read", json!({ "path": "docs:notes.md", "mode": "w" }))
                .await
        ),
        "invalid_argument"
    );
    let unknown = s
        .request(
            "tools/call",
            json!({ "name": "write", "arguments": { "path": "docs:x" } }),
        )
        .await;
    assert_eq!(unknown["error"]["code"], -32602, "{unknown}");
    let bogus = s.request("resources/delete_everything", json!({})).await;
    assert_eq!(bogus["error"]["code"], -32601, "{bogus}");
    for method in ["resources/list", "prompts/list", "resources/templates/list"] {
        let refused = s.request(method, json!({})).await;
        assert_eq!(refused["error"]["code"], -32601, "{method}: {refused}");
    }
    // Stateless sessions need the per-request _meta.
    s.send_raw(json!({ "jsonrpc": "2.0", "id": 900, "method": "tools/list", "params": {} }))
        .await;
    assert_eq!(s.response(900).await["error"]["code"], -32602);

    // Pause from the CLI side: calls are refused, then answered again.
    control(&fx.paths, ControlRequest::Pause).await;
    assert_eq!(error_code(&s.call("roots", json!({})).await), "paused");
    control(&fx.paths, ControlRequest::Resume).await;
    assert_eq!(s.call("roots", json!({})).await["isError"], false);

    // Nothing that left the machine mentions local paths or denied content.
    // (Checked on everything the server received in this session.)
    let status = control(&fx.paths, ControlRequest::Status).await;
    assert_eq!(status["connection"]["connection"], "connected");
    assert_eq!(
        status["roots"][0]["local_path"],
        fx.base.join("work/docs").display().to_string()
    );

    // The audit log recorded allowed and denied calls with the chat ID.
    let audit = std::fs::read_to_string(fx.paths.audit_file()).unwrap();
    let entries: Vec<Value> = audit
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert!(entries.iter().any(|e| e["event"] == "connected"));
    assert!(
        entries
            .iter()
            .any(|e| e["tool"] == "read" && e["decision"] == "allowed" && e["chat_id"] == "chat-7")
    );
    assert!(
        entries
            .iter()
            .any(|e| e["path"] == "docs:escape.txt" && e["decision"] == "denied")
    );
    assert!(
        entries
            .iter()
            .any(|e| e["path"] == "docs:.env" && e["code"] == "denied")
    );
    assert!(
        entries
            .iter()
            .any(|e| e["tool"] == "write" && e["code"] == "unknown_tool")
    );

    // Revoke: the server closes the socket and rejects the refresh credential.
    server.state.lock().unwrap().revoked = true;
    s.ws.close(Some(CloseFrame {
        code: CloseCode::Library(4003),
        reason: "revoked".into(),
    }))
    .await
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let status = control(&fx.paths, ControlRequest::Status).await;
        if status["connection"]["connection"] == "revoked" {
            break;
        }
        assert!(Instant::now() < deadline, "never saw revoked: {status}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    shutdown.cancel();
    tokio::time::timeout(Duration::from_secs(10), daemon)
        .await
        .expect("daemon stops")
        .unwrap()
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn nothing_sensitive_leaves_the_machine() {
    let fx = fixture();
    let mut server = FakeServer::start().await;
    pair(&fx, &server);
    let shutdown = CancellationToken::new();
    let daemon = tokio::spawn(cww::daemon::run(fx.paths.clone(), shutdown.clone(), false));
    let ws = tokio::time::timeout(Duration::from_secs(20), server.sockets.recv())
        .await
        .unwrap()
        .unwrap();
    let mut s = Session {
        ws,
        next_id: 0,
        legacy: true,
    };
    // RubyLLM falls back to the 2025-11-25 handshake; that works too.
    let init = s
        .request(
            "initialize",
            json!({
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": { "name": "chatwithwork", "version": "test" },
            }),
        )
        .await;
    assert_eq!(init["result"]["protocolVersion"], "2025-11-25", "{init}");
    assert_eq!(init["result"]["serverInfo"]["name"], "cww");
    s.send_raw(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }))
        .await;

    let mut outputs = Vec::new();
    for (tool, args) in [
        ("roots", json!({})),
        ("list", json!({ "path": "docs:" })),
        ("list", json!({ "path": "docs:keys" })),
        ("search", json!({ "query": "supersecret" })),
        ("search", json!({ "query": "API_TOKEN" })),
        ("read", json!({ "path": "docs:.env" })),
        ("read", json!({ "path": "docs:escape.txt" })),
        ("read", json!({ "path": "docs:hardlink.txt" })),
        ("read", json!({ "path": "docs:linked/secret.txt" })),
    ] {
        outputs.push(s.call(tool, args).await.to_string());
    }
    // Hard-linked files are not even findable by name.
    let by_name = s.call("search", json!({ "query": "hardlink" })).await;
    assert_eq!(by_name["structuredContent"]["hits"], json!([]), "{by_name}");

    let everything = outputs.join("\n");
    assert!(!everything.contains("supersecret"), "{everything}");
    assert!(
        !everything.contains(&fx.base.display().to_string()),
        "{everything}"
    );
    assert!(!everything.contains("id_rsa"), "{everything}");

    shutdown.cancel();
    daemon.await.unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pairs_with_the_device_flow() {
    let fx = fixture();
    let server = FakeServer::start().await;
    let (paths, origin) = (fx.paths.clone(), server.origin.clone());
    let mut said = Vec::new();
    let paired = tokio::task::spawn_blocking(move || {
        let options = cww::auth::LoginOptions {
            name: Some("Test laptop".into()),
            store: Some(SecretStore::File(paths.secrets_file())),
        };
        let result = cww::auth::login(&paths, &origin, options, |line| said.push(line.to_string()));
        (result, said)
    })
    .await
    .unwrap();
    let (result, said) = paired;
    let paired = result.unwrap();
    assert_eq!(paired.device_id, "42");
    assert_eq!(paired.url, server.origin);
    assert!(said.iter().any(|l| l.contains("WDJB-MJHT")), "{said:?}");

    let config = Config::load(&fx.paths).unwrap();
    assert_eq!(config.server.unwrap().device_id, "42");
    assert_eq!(config.secret_store.as_deref(), Some("file"));
    let store = SecretStore::File(fx.paths.secrets_file());
    let key = DeviceKey::from_stored(&store.get(DEVICE_KEY).unwrap().unwrap()).unwrap();
    assert_eq!(
        Some(key.public_key()),
        server.state.lock().unwrap().public_key
    );
    assert_eq!(
        store.get(REFRESH_TOKEN).unwrap().as_deref(),
        Some("refresh-1")
    );
    assert_mode(&fx.paths.secrets_file(), 0o600);
    assert_mode(&fx.paths.config_file(), 0o600);
}

#[cfg(unix)]
fn assert_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
        mode,
        "{}",
        path.display()
    );
}

/// Windows has no modes; the profile folder's ACL keeps files private.
#[cfg(windows)]
fn assert_mode(path: &Path, _mode: u32) {
    assert!(path.exists(), "{}", path.display());
}

#[cfg(unix)]
fn link_file(target: &Path, link: &Path) {
    std::os::unix::fs::symlink(target, link).unwrap();
}

#[cfg(unix)]
fn link_dir(target: &Path, link: &Path) {
    std::os::unix::fs::symlink(target, link).unwrap();
}

/// A file symlink needs Developer Mode on Windows. Without it, a junction to
/// the folder around the target makes the same escape attempt.
#[cfg(windows)]
fn link_file(target: &Path, link: &Path) {
    if std::os::windows::fs::symlink_file(target, link).is_err() {
        link_dir(target.parent().unwrap(), link);
    }
}

/// Junctions need no privileges.
#[cfg(windows)]
fn link_dir(target: &Path, link: &Path) {
    let status = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(link)
        .arg(target)
        .output()
        .unwrap()
        .status;
    assert!(status.success(), "mklink /J failed");
}
