//! End to end: a fake Chat with Work server (token endpoint, device
//! authorization, the chat API, and an Action Cable style WebSocket at
//! /local_agent) drives a real daemon over the wire, the way the Rails side
//! does.

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
use cww::config::{Config, Limits, Root, ServerConfig};
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
    /// The owner lets this computer use their chats.
    chat_access: bool,
    /// The next chat call says the token predates chat access.
    stale_scope: bool,
    /// Questions posted to the chat API.
    questions: Vec<String>,
    /// The `model_id` and `attachments` each question asked with.
    asked_with: Vec<Value>,
    /// The server predates choosing a model and the newer chat actions.
    old_api: bool,
    /// Approvals, denials and chat actions, as "what chat id detail".
    decisions: Vec<String>,
    /// Files uploaded: (filename, content type, bytes).
    uploads: Vec<(String, String, String)>,
    /// Images asked for that came with a token or a proof.
    assets_with_credentials: usize,
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
    let method = head
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_string();
    if path.starts_with("/assets/") {
        if header(&head, "authorization").is_some() || !dpop.is_empty() {
            state.lock().unwrap().assets_with_credentials += 1;
        }
        let (status, extra, content_type, body): (u16, &str, &str, Vec<u8>) = match path.as_str() {
            "/assets/providers/slack-0c9450af.svg" => (
                200,
                "",
                "image/svg+xml",
                b"<svg xmlns='http://www.w3.org/2000/svg'/>".to_vec(),
            ),
            "/assets/big.png" => (200, "", "image/png", vec![0; 2 * 1024 * 1024]),
            "/assets/page.svg" => (
                200,
                "",
                "text/html; charset=utf-8",
                b"<html></html>".to_vec(),
            ),
            "/assets/moved.svg" => (
                302,
                "location: https://evil.example/x.svg\r\n",
                "text/plain",
                Vec::new(),
            ),
            _ => (404, "", "text/plain", b"not found".to_vec()),
        };
        let head = format!(
            "HTTP/1.1 {status} X\r\ncontent-type: {content_type}\r\n{extra}content-length: {}\r\nconnection: close\r\n\r\n",
            body.len()
        );
        let _ = stream.write_all(head.as_bytes()).await;
        let _ = stream.write_all(&body).await;
        return;
    }
    let (status, json) = if [
        "/local_agent/chat",
        "/local_agent/models",
        "/local_agent/uploads",
    ]
    .iter()
    .any(|api| path.starts_with(api))
    {
        let authorization = header(&head, "authorization").unwrap_or_default();
        let content_type = header(&head, "content-type").unwrap_or_default();
        chat_api(
            &state,
            &origin,
            ChatRequest {
                method: &method,
                path: &path,
                authorization: &authorization,
                dpop: &dpop,
                content_type: &content_type,
                body: &body,
            },
        )
    } else {
        oauth(&state, &origin, &path, &dpop, &form)
    };
    let nonce = state.lock().unwrap().nonce.clone();
    let body = json.to_string();
    let response = format!(
        "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ndpop-nonce: {nonce}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes()).await.unwrap();
}

fn header(head: &str, name: &str) -> Option<String> {
    head.lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.trim()
            .eq_ignore_ascii_case(name)
            .then(|| v.trim().to_string())
    })
}

struct ChatRequest<'a> {
    method: &'a str,
    path: &'a str,
    authorization: &'a str,
    dpop: &'a str,
    content_type: &'a str,
    body: &'a str,
}

/// The chat API, as Rails serves it to a device allowed to chat.
fn chat_api(state: &Mutex<ServerState>, origin: &str, request: ChatRequest) -> (u16, Value) {
    let ChatRequest {
        method,
        path,
        authorization,
        dpop,
        content_type,
        body,
    } = request;
    let mut st = state.lock().unwrap();
    let key = st.public_key.clone().unwrap_or_default();
    // The device's own token, and a proof bound to it for this request.
    let Ok(claims) = verify_proof(dpop, &key) else {
        return (401, json!({ "error": "invalid_token" }));
    };
    let ath = URL_SAFE_NO_PAD.encode(ring::digest::digest(
        &ring::digest::SHA256,
        ACCESS_TOKEN.as_bytes(),
    ));
    if authorization != format!("DPoP {ACCESS_TOKEN}")
        || claims["htm"] != method
        || claims["htu"] != format!("{origin}{path}")
        || claims["ath"] != ath
    {
        return (401, json!({ "error": "invalid_token" }));
    }
    let approve_url = format!("{origin}/1000001/settings?tab=connectors#computers");
    if path == "/local_agent/chat_access_request" {
        return (
            202,
            json!({ "granted": st.chat_access, "requested": !st.chat_access, "approve_url": approve_url }),
        );
    }
    if !st.chat_access {
        return (
            403,
            json!({ "error": "chat_access_required", "error_description": "Allow this computer to use your chats in Settings",
                    "requested": false, "approve_url": approve_url }),
        );
    }
    if st.stale_scope {
        st.stale_scope = false;
        return (403, json!({ "error": "insufficient_scope" }));
    }
    let old_api = st.old_api;
    let chat = |number: u64, title: &str, state: &str| {
        let mut chat = json!({ "number": number, "title": title, "state": state, "project": null, "mine": true,
                "created_at": "2026-09-25T08:00:00Z", "updated_at": "2026-09-25T08:14:03Z",
                "url": format!("{origin}/1000001/chats/{number}") });
        if !old_api {
            chat["model"] = json!({ "id": 3, "name": "Claude Sonnet" });
            chat["can"] = json!({ "retry": true, "branch": true, "rename": true, "delete": true, "share": true });
        }
        chat
    };
    // Rails without the route: an HTML 404, not the API's JSON.
    let newer = path.starts_with("/local_agent/models")
        || path.starts_with("/local_agent/uploads")
        || path.ends_with("/retry")
        || path.ends_with("/branches")
        || path.ends_with("/share")
        || path.ends_with("/input")
        || (path == "/local_agent/chats/7" && matches!(method, "PATCH" | "DELETE"));
    if old_api && newer {
        return (404, Value::Null);
    }
    let posted: Value = serde_json::from_str(body).unwrap_or_default();
    if let Some(model) = posted.get("model_id")
        && model != "3"
        && model != 3
    {
        return (
            422,
            json!({ "error": "model_unavailable", "error_description": "That model isn't available on your plan" }),
        );
    }
    match (method, path) {
        ("GET", "/local_agent/models") => (
            200,
            json!({ "default_model_id": 3, "models": [
                { "id": 3, "name": "Claude Sonnet", "provider": "anthropic", "description": "Fast and capable", "selectable": true, "reason": null },
                { "id": 4, "name": "Claude Opus", "provider": "anthropic", "description": null, "selectable": false, "reason": "Upgrade to use it" }
            ] }),
        ),
        ("POST", "/local_agent/uploads") => {
            let Some(boundary) = content_type
                .strip_prefix("multipart/form-data; boundary=")
                .map(str::to_string)
            else {
                return (
                    422,
                    json!({ "error": "attachment_refused", "error_description": "Not multipart" }),
                );
            };
            // One part: its headers, a blank line, then the file.
            let part = body
                .split(&format!("--{boundary}"))
                .nth(1)
                .unwrap_or_default();
            let (headers, data) = part.split_once("\r\n\r\n").unwrap_or_default();
            let filename = headers
                .split("filename=\"")
                .nth(1)
                .and_then(|r| r.split('"').next())
                .unwrap_or_default()
                .to_string();
            let file_type = headers
                .lines()
                .find_map(|l| l.strip_prefix("Content-Type: "))
                .unwrap_or_default()
                .to_string();
            let data = data.strip_suffix("\r\n").unwrap_or(data).to_string();
            let size = data.len();
            st.uploads.push((filename.clone(), file_type.clone(), data));
            (
                201,
                json!({ "signed_id": format!("signed-{}", st.uploads.len()), "filename": filename,
                        "byte_size": size, "content_type": file_type }),
            )
        }
        ("POST", "/local_agent/chats/7/tool_calls/12/approval") => {
            st.decisions
                .push(format!("approve 7 12 {}", posted["for_rest_of_chat"]));
            (202, json!({ "chat": chat(7, "Q3 budget", "processing") }))
        }
        ("POST", "/local_agent/chats/7/tool_calls/12/denial") => {
            st.decisions.push(format!("deny 7 12 {}", posted["reason"]));
            (202, json!({ "chat": chat(7, "Q3 budget", "processing") }))
        }
        ("POST", "/local_agent/chats/7/retry") => {
            st.decisions
                .push(format!("retry 7 {}", posted["message_id"]));
            (202, json!({ "chat": chat(7, "Q3 budget", "processing") }))
        }
        ("POST", "/local_agent/chats/7/branches") => {
            st.decisions
                .push(format!("branch 7 {}", posted["message_id"]));
            (
                201,
                json!({ "chat": chat(9, "Q3 budget (branch)", "idle") }),
            )
        }
        ("PATCH", "/local_agent/chats/7") => {
            st.decisions.push(format!("rename 7 {}", posted["title"]));
            (
                200,
                json!({ "chat": chat(7, posted["title"].as_str().unwrap_or_default(), "idle") }),
            )
        }
        ("DELETE", "/local_agent/chats/7") => {
            st.decisions.push("delete 7".into());
            (204, Value::Null)
        }
        ("POST", "/local_agent/chats/7/share") => {
            st.decisions.push("share 7".into());
            let url = format!("{origin}/shared/abc");
            let mut shared = chat(7, "Q3 budget", "idle");
            shared["share"] = json!({ "url": url, "expires_at": "2026-11-01T09:00:00Z" });
            (
                201,
                json!({ "url": url, "expires_at": "2026-11-01T09:00:00Z", "chat": shared }),
            )
        }
        ("DELETE", "/local_agent/chats/7/share") => {
            st.decisions.push("unshare 7".into());
            (200, json!({ "chat": chat(7, "Q3 budget", "idle") }))
        }
        ("POST", "/local_agent/chats/7/tool_calls/32/input") => {
            if posted["input"]["environment"].is_null() && !posted["input"].is_null() {
                return (
                    422,
                    json!({ "error": "invalid", "error_description": "Environment is needed." }),
                );
            }
            st.decisions
                .push(format!("answer 7 32 {}", posted["input"]));
            (202, json!({ "chat": chat(7, "Q3 budget", "processing") }))
        }
        ("DELETE", "/local_agent/chats/7/tool_calls/32/input") => {
            st.decisions.push("decline 7 32".into());
            (202, json!({ "chat": chat(7, "Q3 budget", "processing") }))
        }
        ("GET", "/local_agent/chats") => (
            200,
            json!({ "account": { "name": "Plenty" }, "user": { "name": "Carmine" }, "locked_reason": null,
                    "projects": [], "chats": [chat(7, "Q3 budget", "idle")] }),
        ),
        ("GET", "/local_agent/chats/7") => (
            200,
            json!({ "chat": chat(7, "Q3 budget", "idle"), "locked_reason": null, "entries": [
                { "kind": "user", "id": 1, "content": "What did we budget for Q3?" },
                { "kind": "activity", "id": 2, "title": "Searched Drive", "details": "1 search", "services": ["Drive"],
                  "pending": false, "steps": [{ "summary": "Searched Drive for “q3”", "pending": false, "files": ["Q3 plan.pdf"] }] },
                { "kind": "assistant", "id": 3, "content": "It's **€40k**.", "sources": [{ "title": "Q3 plan.pdf", "url": "https://drive.example/q3" }] }
            ] }),
        ),
        ("POST", "/local_agent/chats") => {
            let body: Value = serde_json::from_str(body).unwrap_or_default();
            st.asked_with.push(json!([
                body["model_id"],
                body["attachments"],
                body["project_id"]
            ]));
            st.questions
                .push(body["content"].as_str().unwrap_or_default().to_string());
            (201, json!({ "chat": chat(8, "Chat #8", "processing") }))
        }
        ("POST", "/local_agent/chats/7/messages") => {
            let body: Value = serde_json::from_str(body).unwrap_or_default();
            st.asked_with.push(json!([
                body["model_id"],
                body["attachments"],
                body["project_id"]
            ]));
            st.questions
                .push(body["content"].as_str().unwrap_or_default().to_string());
            (202, json!({ "chat": chat(7, "Q3 budget", "processing") }))
        }
        _ => (
            404,
            json!({ "error": "not_found", "error_description": "No such chat" }),
        ),
    }
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
            let scopes: Vec<&str> = form["scope"].split(' ').collect();
            assert!(scopes.contains(&"local_agent:serve"), "{scopes:?}");
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
            writable: false,
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
            ..Default::default()
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

/// A CONNECT proxy that wants Basic auth, like a company's.
struct TestProxy {
    port: u16,
    /// `CONNECT` targets it tunnelled, in order.
    tunnels: Arc<Mutex<Vec<String>>>,
    /// Requests it refused for missing or wrong credentials.
    refused: Arc<Mutex<usize>>,
}

impl TestProxy {
    async fn start(user: &str, password: &str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let expected = format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}"))
        );
        let tunnels = Arc::new(Mutex::new(Vec::new()));
        let refused = Arc::new(Mutex::new(0));
        let (t, r) = (Arc::clone(&tunnels), Arc::clone(&refused));
        tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let (t, r, expected) = (Arc::clone(&t), Arc::clone(&r), expected.clone());
                tokio::spawn(async move { tunnel(stream, &expected, &t, &r).await });
            }
        });
        Self {
            port,
            tunnels,
            refused,
        }
    }

    fn url(&self, user: &str, password: &str) -> String {
        format!("http://{user}:{password}@127.0.0.1:{}", self.port)
    }
}

async fn tunnel(
    mut client: TcpStream,
    expected_auth: &str,
    tunnels: &Mutex<Vec<String>>,
    refused: &Mutex<usize>,
) {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if client.read(&mut byte).await.unwrap_or(0) == 0 {
            return;
        }
        head.push(byte[0]);
    }
    let head = String::from_utf8_lossy(&head).to_string();
    let mut words = head.split_whitespace();
    let (method, target) = (
        words.next().unwrap_or_default(),
        words.next().unwrap_or_default(),
    );
    let auth = head.lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.eq_ignore_ascii_case("proxy-authorization")
            .then(|| v.trim().to_string())
    });
    if method != "CONNECT" {
        let _ = client.write_all(b"HTTP/1.1 405 Only CONNECT\r\n\r\n").await;
        return;
    }
    if auth.as_deref() != Some(expected_auth) {
        *refused.lock().unwrap() += 1;
        let _ = client
            .write_all(b"HTTP/1.1 407 Proxy Authentication Required\r\nProxy-Authenticate: Basic realm=\"test\"\r\ncontent-length: 0\r\n\r\n")
            .await;
        return;
    }
    let Ok(mut upstream) = TcpStream::connect(target).await else {
        let _ = client.write_all(b"HTTP/1.1 502 Bad Gateway\r\n\r\n").await;
        return;
    };
    tunnels.lock().unwrap().push(target.to_string());
    client
        .write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
        .await
        .unwrap();
    let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
}

/// Pairing, token refreshes and the WebSocket all go through the proxy set
/// in config.toml, and status shows it without the password.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn goes_through_an_http_proxy() {
    const PASSWORD: &str = "hunter2-proxy";
    let fx = fixture();
    let mut server = FakeServer::start().await;
    let proxy = TestProxy::start("alice", PASSWORD).await;
    let target = server.origin.trim_start_matches("http://").to_string();
    Config {
        proxy: Some(proxy.url("alice", PASSWORD)),
        ..Config::default()
    }
    .save(&fx.paths)
    .unwrap();

    let (paths, origin) = (fx.paths.clone(), server.origin.clone());
    tokio::task::spawn_blocking(move || {
        let options = cww::auth::LoginOptions {
            store: Some(SecretStore::File(paths.secrets_file())),
            ..Default::default()
        };
        cww::auth::login(&paths, &origin, options, |_| {})
    })
    .await
    .unwrap()
    .expect("pairing through the proxy");
    let pairing = proxy.tunnels.lock().unwrap().len();
    assert!(pairing >= 2, "pairing never used the proxy");
    assert!(
        proxy.tunnels.lock().unwrap().iter().all(|t| *t == target),
        "{:?}",
        proxy.tunnels
    );
    assert_eq!(*proxy.refused.lock().unwrap(), 0);

    let shutdown = CancellationToken::new();
    let daemon = tokio::spawn(cww::daemon::run(fx.paths.clone(), shutdown.clone(), false));
    let _ws = tokio::time::timeout(Duration::from_secs(20), server.sockets.recv())
        .await
        .expect("the daemon connects through the proxy")
        .unwrap();
    // At least a token refresh and the WebSocket.
    assert!(proxy.tunnels.lock().unwrap().len() >= pairing + 2);
    assert!(proxy.tunnels.lock().unwrap().iter().all(|t| *t == target));

    let status = control(&fx.paths, ControlRequest::Status).await;
    let shown = format!("alice:***@127.0.0.1:{}", proxy.port);
    assert_eq!(status["proxy"]["source"], "config", "{status}");
    assert!(
        status["proxy"]["url"].as_str().unwrap().contains(&shown),
        "{status}"
    );
    assert!(!status.to_string().contains(PASSWORD), "{status}");

    // The CLI says the same, in both forms.
    let home = fx.paths.config_file();
    let home = home.parent().unwrap().parent().unwrap().to_path_buf();
    for args in [&["status"][..], &["status", "--json"][..]] {
        let (home, args) = (home.clone(), args.to_vec());
        let output = tokio::task::spawn_blocking(move || {
            std::process::Command::new(env!("CARGO_BIN_EXE_cww"))
                .args(args)
                .env("CWW_HOME", home)
                .output()
                .unwrap()
        })
        .await
        .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(output.status.success(), "{stdout}");
        assert!(stdout.contains(&shown), "{stdout}");
        assert!(!stdout.contains(PASSWORD), "{stdout}");
    }

    shutdown.cancel();
    daemon.await.unwrap().unwrap();
}

/// A proxy that refuses the credentials stops pairing with a clear error,
/// and the error doesn't repeat the password.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_proxy_that_refuses_the_credentials_is_reported() {
    let fx = fixture();
    let server = FakeServer::start().await;
    let proxy = TestProxy::start("alice", "right-password").await;
    Config {
        proxy: Some(proxy.url("alice", "wrong-password")),
        ..Config::default()
    }
    .save(&fx.paths)
    .unwrap();
    let (paths, origin) = (fx.paths.clone(), server.origin.clone());
    let err = tokio::task::spawn_blocking(move || {
        let options = cww::auth::LoginOptions {
            store: Some(SecretStore::File(paths.secrets_file())),
            ..Default::default()
        };
        cww::auth::login(&paths, &origin, options, |_| {})
    })
    .await
    .unwrap()
    .unwrap_err();
    let err = format!("{err:#}");
    assert!(err.contains("407"), "{err}");
    assert!(!err.contains("wrong-password"), "{err}");
    assert_eq!(*proxy.refused.lock().unwrap(), 1);

    // The WebSocket's own CONNECT says what went wrong.
    let bad = cww::proxy::Proxy::parse(&proxy.url("alice", "wrong-password"), "config").unwrap();
    let port = server.origin.rsplit(':').next().unwrap().parse().unwrap();
    let err = format!("{:#}", bad.connect("127.0.0.1", port).await.err().unwrap());
    assert!(err.contains("rejected the credentials"), "{err}");
    assert!(!err.contains("wrong-password"), "{err}");
    let good = cww::proxy::Proxy::parse(&proxy.url("alice", "right-password"), "config").unwrap();
    assert!(good.connect("127.0.0.1", port).await.is_ok());
}

async fn control_error(paths: &Paths, request: ControlRequest) -> String {
    let socket = paths.socket_path();
    tokio::task::spawn_blocking(move || control::request(&socket, request))
        .await
        .unwrap()
        .expect_err("the request fails")
        .to_string()
}

/// Follows `watch` on a thread, the way the desktop app does.
struct Watcher {
    lines: std::sync::mpsc::Receiver<Value>,
}

impl Watcher {
    fn start(paths: &Paths) -> Self {
        let socket = paths.socket_path();
        let (tx, lines) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let client = control::Client::connect(&socket).unwrap().unwrap();
            for event in client.subscribe(&[control::Topic::Status]).unwrap() {
                let mut event = event.unwrap();
                if tx.send(event["status"].take()).is_err() {
                    return;
                }
            }
        });
        Self { lines }
    }

    /// Wait for a status line that satisfies `check`.
    async fn until(&mut self, what: &str, check: impl Fn(&Value) -> bool) -> Value {
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut last = Value::Null;
        while Instant::now() < deadline {
            match self.lines.try_recv() {
                Ok(status) if check(&status) => return status,
                Ok(status) => last = status,
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                Err(e) => panic!("watch ended: {e}; last status {last}"),
            }
        }
        panic!("never saw {what}; last status {last}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_settings_app_manages_folders_over_the_control_channel() {
    let fx = fixture();
    // Not paired yet; secrets go to the file store, not the test machine's keychain.
    Config {
        secret_store: Some("file".into()),
        ..Config::default()
    }
    .save(&fx.paths)
    .unwrap();
    let shutdown = CancellationToken::new();
    let daemon = tokio::spawn(cww::daemon::run(fx.paths.clone(), shutdown.clone(), false));
    let deadline = Instant::now() + Duration::from_secs(10);
    while !fx.paths.socket_path().exists() {
        assert!(Instant::now() < deadline, "the daemon never listened");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let mut watch = Watcher::start(&fx.paths);
    let first = watch.until("the first status", |_| true).await;
    assert_eq!(first["connection"]["connection"], "not_paired", "{first}");
    assert_eq!(first["roots"], json!([]));

    // Share a folder; the watch reports it and its first index pass.
    let docs = fx.base.join("work/docs");
    let added = control(
        &fx.paths,
        ControlRequest::RootsAdd {
            path: docs.clone(),
            label: Some("Docs".into()),
            i_know: false,
            writable: false,
            follow_symlinks: false,
        },
    )
    .await;
    assert_eq!(added["root"]["id"], "docs", "{added}");
    let ready = watch
        .until("the index to be ready", |s| {
            s["roots"][0]["index"] == "ready"
        })
        .await;
    assert_eq!(ready["roots"][0]["label"], "Docs");
    // The daemon shows Windows paths without the \\?\ prefix that
    // canonicalize() adds.
    let shown = docs.to_string_lossy().replace(r"\\?\", "");
    assert_eq!(ready["roots"][0]["local_path"], json!(shown));
    assert!(
        ready["roots"][0]["indexed_files"].as_u64().unwrap() >= 2,
        "{ready}"
    );
    assert_eq!(Config::load(&fx.paths).unwrap().roots.len(), 1, "saved");

    // Relabel it, and refuse what `cww roots add` refuses.
    control(
        &fx.paths,
        ControlRequest::RootsLabel {
            root: "docs".into(),
            label: " Work docs ".into(),
        },
    )
    .await;
    watch
        .until("the new label", |s| s["roots"][0]["label"] == "Work docs")
        .await;
    let empty = control_error(
        &fx.paths,
        ControlRequest::RootsLabel {
            root: "docs".into(),
            label: "  ".into(),
        },
    )
    .await;
    assert!(empty.contains("can't be empty"), "{empty}");
    let ssh = fx.base.join(".ssh");
    std::fs::create_dir_all(&ssh).unwrap();
    let denied = control_error(
        &fx.paths,
        ControlRequest::RootsAdd {
            path: ssh,
            label: None,
            i_know: true,
            writable: false,
            follow_symlinks: false,
        },
    )
    .await;
    assert!(denied.contains("deny list"), "{denied}");
    let again = control_error(
        &fx.paths,
        ControlRequest::RootsAdd {
            path: docs.clone(),
            label: None,
            i_know: false,
            writable: false,
            follow_symlinks: false,
        },
    )
    .await;
    assert!(again.contains("already shared"), "{again}");

    // The deny list in effect, for the settings window.
    let deny = control(&fx.paths, ControlRequest::Deny).await;
    let builtin: Vec<&str> = deny["builtin"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p.as_str().unwrap())
        .collect();
    assert!(
        builtin.contains(&".ssh") && builtin.contains(&".env*"),
        "{deny}"
    );
    assert!(
        deny["never_changed"]
            .as_array()
            .unwrap()
            .contains(&json!(".git")),
        "{deny}"
    );
    assert!(
        deny["own_dirs"]
            .as_array()
            .unwrap()
            .contains(&json!(fx.paths.config_dir)),
        "{deny}"
    );

    // Pause and resume show up in the status and the audit log.
    control(&fx.paths, ControlRequest::Pause).await;
    watch.until("paused", |s| s["paused"] == true).await;
    control(&fx.paths, ControlRequest::Resume).await;
    watch.until("resumed", |s| s["paused"] == false).await;
    let log = control(&fx.paths, ControlRequest::AuditTail { lines: Some(2) }).await;
    let events: Vec<&str> = log["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["event"].as_str().unwrap())
        .collect();
    assert_eq!(events, ["paused", "resumed"], "{log}");

    control(
        &fx.paths,
        ControlRequest::RootsRemove {
            root: "docs".into(),
        },
    )
    .await;
    watch.until("no roots", |s| s["roots"] == json!([])).await;
    assert!(Config::load(&fx.paths).unwrap().roots.is_empty());

    shutdown.cancel();
    tokio::time::timeout(Duration::from_secs(10), daemon)
        .await
        .expect("daemon stops")
        .unwrap()
        .unwrap();
}

/// Requests that edit config.toml at once (the app adds a folder while a
/// rename is out, say) all land: none saves over another's change.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_config_edits_all_land() {
    let fx = fixture();
    Config {
        secret_store: Some("file".into()),
        ..Config::default()
    }
    .save(&fx.paths)
    .unwrap();
    let shutdown = CancellationToken::new();
    let daemon = tokio::spawn(cww::daemon::run(fx.paths.clone(), shutdown.clone(), false));
    let deadline = Instant::now() + Duration::from_secs(10);
    while !fx.paths.socket_path().exists() {
        assert!(Instant::now() < deadline, "the daemon never listened");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let add = |name: &str| {
        let path = fx.base.join("work").join(name);
        std::fs::create_dir_all(&path).unwrap();
        control(
            &fx.paths,
            ControlRequest::RootsAdd {
                path,
                label: None,
                i_know: false,
                writable: false,
                follow_symlinks: false,
            },
        )
    };
    add("docs").await;

    let (a, b, c, d, _, _) = tokio::join!(
        add("alpha"),
        add("bravo"),
        add("charlie"),
        add("delta"),
        control(
            &fx.paths,
            ControlRequest::RootsLabel {
                root: "docs".into(),
                label: "Renamed".into(),
            },
        ),
        control(&fx.paths, ControlRequest::Pause),
    );
    for added in [a, b, c, d] {
        assert!(added["root"]["id"].is_string(), "{added}");
    }
    let config = Config::load(&fx.paths).unwrap();
    let mut ids: Vec<&str> = config.roots.iter().map(|r| r.id.as_str()).collect();
    ids.sort_unstable();
    assert_eq!(ids, ["alpha", "bravo", "charlie", "delta", "docs"]);
    assert_eq!(config.root("docs").unwrap().label, "Renamed");
    assert!(config.paused);

    shutdown.cancel();
    tokio::time::timeout(Duration::from_secs(10), daemon)
        .await
        .expect("daemon stops")
        .unwrap()
        .unwrap();
}

/// `cww login --json`, which the desktop app runs to pair: the code first,
/// then the outcome, one JSON object per line, saying what became of the
/// daemon. `--no-daemon` keeps these tests from installing a service.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn login_json_reports_the_code_and_the_pairing() {
    let fx = fixture();
    let mut server = FakeServer::start().await;
    let lines = login_json(&fx, &server.origin).await;
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert_eq!(lines[0]["event"], "code");
    assert_eq!(lines[0]["user_code"], "WDJB-MJHT");
    assert_eq!(lines[0]["device_name"], "Test laptop");
    assert_eq!(
        lines[0]["browser_url"],
        json!(format!("{}/device", server.origin))
    );
    assert_eq!(lines[0]["opened"], false, "--no-browser");
    assert_eq!(lines[1]["event"], "paired");
    assert_eq!(lines[1]["device_id"], "42");
    assert_eq!(lines[1]["daemon"], "not_running");
    assert_eq!(
        Config::load(&fx.paths).unwrap().server.unwrap().device_id,
        "42"
    );

    // Paired again with the daemon running: it picks the pairing up.
    let shutdown = CancellationToken::new();
    let daemon = tokio::spawn(cww::daemon::run(fx.paths.clone(), shutdown.clone(), false));
    tokio::time::timeout(Duration::from_secs(20), server.sockets.recv())
        .await
        .expect("the daemon connects")
        .unwrap();
    let lines = login_json(&fx, &server.origin).await;
    assert_eq!(lines[1]["event"], "paired", "{lines:?}");
    assert_eq!(lines[1]["daemon"], "reloaded", "{lines:?}");
    assert!(lines[1].get("daemon_error").is_none(), "{lines:?}");

    shutdown.cancel();
    tokio::time::timeout(Duration::from_secs(10), daemon)
        .await
        .expect("daemon stops")
        .unwrap()
        .unwrap();
}

async fn login_json(fx: &Fixture, origin: &str) -> Vec<Value> {
    let (base, origin) = (fx.base.join("cww"), origin.to_string());
    let out = tokio::task::spawn_blocking(move || {
        std::process::Command::new(env!("CARGO_BIN_EXE_cww"))
            .args([
                "login",
                "--json",
                "--no-browser",
                "--no-daemon",
                "--server",
                &origin,
                "--name",
                "Test laptop",
            ])
            .env("CWW_HOME", base)
            .env("CWW_SECRET_STORE", "file")
            .output()
            .unwrap()
    })
    .await
    .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
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

/// The terminal's chats go through the daemon: it calls the chat API with
/// its own DPoP-bound token (the TUI never sees one), relays refusals with
/// their codes, refreshes a token that predates chat access, and follows a
/// chat over its socket's chat channel.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn relays_chats_for_the_terminal() {
    use cww::tui::chat::{Chats, Entry, Live};

    let fx = fixture();
    let mut server = FakeServer::start().await;
    pair(&fx, &server);
    let shutdown = CancellationToken::new();
    let daemon = tokio::spawn(cww::daemon::run(fx.paths.clone(), shutdown.clone(), false));
    let mut ws = tokio::time::timeout(Duration::from_secs(20), server.sockets.recv())
        .await
        .expect("the daemon connects")
        .unwrap();
    let chats = Chats::new(&fx.paths.socket_path());

    // Not allowed yet: the TUI learns why and where to allow it.
    let c = chats.clone();
    let refused = tokio::task::spawn_blocking(move || c.list())
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(refused.code, "chat_access_required");
    assert_eq!(
        refused.approve_url.as_deref(),
        Some(
            format!(
                "{}/1000001/settings?tab=connectors#computers",
                server.origin
            )
            .as_str()
        )
    );
    let c = chats.clone();
    let asked = tokio::task::spawn_blocking(move || c.request_access())
        .await
        .unwrap()
        .unwrap();
    assert!(asked.requested && !asked.granted);

    // Allowed, with a token from before: the daemon refreshes and retries.
    {
        let mut st = server.state.lock().unwrap();
        st.chat_access = true;
        st.stale_scope = true;
    }
    let refreshes = |server: &FakeServer| {
        server
            .state
            .lock()
            .unwrap()
            .log
            .iter()
            .filter(|l| l.starts_with("POST /local_agent/token"))
            .count()
    };
    let before = refreshes(&server);
    let c = chats.clone();
    let list = tokio::task::spawn_blocking(move || c.list())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(list.chats[0].number, 7);
    assert_eq!(list.chats[0].title, "Q3 budget");
    assert_eq!(
        refreshes(&server),
        before + 1,
        "one refresh for the new scope"
    );

    let c = chats.clone();
    let transcript = tokio::task::spawn_blocking(move || c.show(7))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(transcript.entries.len(), 3);
    assert!(
        matches!(&transcript.entries[2], Entry::Assistant { content, .. } if content == "It's **€40k**.")
    );

    let c = chats.clone();
    let started = tokio::task::spawn_blocking(move || c.send(None, "What changed this week?"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(started.number, 8);
    let c = chats.clone();
    tokio::task::spawn_blocking(move || c.send(Some(7), "And Q4?"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        server.state.lock().unwrap().questions,
        ["What changed this week?", "And Q4?"]
    );

    // Bad input never reaches the server.
    let c = chats.clone();
    let empty = tokio::task::spawn_blocking(move || c.send(Some(7), "   "))
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(empty.code, "invalid");
    let c = chats.clone();
    let missing = tokio::task::spawn_blocking(move || c.show(99))
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(missing.code, "not_found");

    // Following a chat: the daemon subscribes on its socket and relays updates.
    let (live_tx, mut live_rx) = tokio::sync::mpsc::unbounded_channel();
    let c = chats.clone();
    let follower = tokio::task::spawn_blocking(move || {
        c.follow(
            7,
            |_| {},
            |live| match live {
                Some(live) => live_tx.send(live).is_ok(),
                None => true,
            },
        )
    });
    let identifier = r#"{"channel":"LocalAgent::ChatChannel","chat":"7"}"#;
    let subscribe: Value = serde_json::from_str(&next_text(&mut ws).await.unwrap()).unwrap();
    assert_eq!(subscribe["command"], "subscribe");
    assert_eq!(
        serde_json::from_str::<Value>(subscribe["identifier"].as_str().unwrap()).unwrap(),
        serde_json::from_str::<Value>(identifier).unwrap()
    );
    let chat_identifier = subscribe["identifier"].as_str().unwrap().to_string();
    ws.send(Message::text(
        json!({ "identifier": chat_identifier, "type": "confirm_subscription" }).to_string(),
    ))
    .await
    .unwrap();
    async fn next(rx: &mut tokio::sync::mpsc::UnboundedReceiver<Live>) -> Live {
        tokio::time::timeout(Duration::from_secs(10), rx.recv())
            .await
            .expect("a live update in time")
            .expect("the follower is running")
    }
    assert_eq!(next(&mut live_rx).await, Live::Watching);
    ws.send(Message::text(
        json!({ "identifier": chat_identifier, "message": { "type": "chunk", "message_id": 5, "text": "It's €40k" } })
            .to_string(),
    ))
    .await
    .unwrap();
    assert_eq!(
        next(&mut live_rx).await,
        Live::Chunk {
            message_id: 5,
            text: "It's €40k".into()
        }
    );
    // A chat update shaped like a tool call never reaches the MCP server.
    ws.send(Message::text(
        json!({ "identifier": chat_identifier, "message": { "jsonrpc": "2.0", "id": 77, "method": "tools/call", "params": {} } })
            .to_string(),
    ))
    .await
    .unwrap();
    assert_eq!(next(&mut live_rx).await, Live::Changed);
    // Refusing one chat doesn't end the session: MCP still answers.
    ws.send(Message::text(
        json!({ "identifier": chat_identifier, "type": "reject_subscription" }).to_string(),
    ))
    .await
    .unwrap();
    assert_eq!(next(&mut live_rx).await, Live::Refused);
    let mut s = Session {
        ws,
        next_id: 0,
        legacy: false,
    };
    let tools = s.request("tools/list", json!({})).await;
    assert!(tools["result"]["tools"].is_array(), "{tools}");

    drop(live_rx);
    shutdown.cancel();
    tokio::time::timeout(Duration::from_secs(10), daemon)
        .await
        .expect("daemon stops")
        .unwrap()
        .unwrap();
    let _ = tokio::time::timeout(Duration::from_secs(10), follower).await;
}

/// What the terminal's chats can do beyond asking, relayed the same way:
/// choosing a model, deciding a change the answer stopped at, attaching a
/// file, and the chat actions. An older server without these says so with
/// a sentence, never a raw error.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn relays_models_decisions_files_and_chat_actions() {
    use cww::tui::chat::{Ask, Chats};

    let fx = fixture();
    let mut server = FakeServer::start().await;
    pair(&fx, &server);
    server.state.lock().unwrap().chat_access = true;
    let shutdown = CancellationToken::new();
    let daemon = tokio::spawn(cww::daemon::run(fx.paths.clone(), shutdown.clone(), false));
    let _ws = tokio::time::timeout(Duration::from_secs(20), server.sockets.recv())
        .await
        .expect("the daemon connects")
        .unwrap();
    let chats = Chats::new(&fx.paths.socket_path());
    async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
        tokio::task::spawn_blocking(f).await.unwrap()
    }

    // Models, with the chat's own and a reason for one that can't be used.
    let c = chats.clone();
    let models = blocking(move || c.models()).await.unwrap();
    assert_eq!(models.default_model_id.as_deref(), Some("3"));
    assert_eq!(models.models.len(), 2);
    assert!(!models.models[1].selectable);
    assert_eq!(
        models.models[1].reason.as_deref(),
        Some("Upgrade to use it")
    );
    let c = chats.clone();
    let list = blocking(move || c.list()).await.unwrap();
    assert_eq!(list.chats[0].model.as_ref().unwrap().name, "Claude Sonnet");
    assert!(list.chats[0].can(|can| can.retry));

    // A file, read by the client and uploaded by the daemon.
    let file = fx.base.join("notes.txt");
    std::fs::write(&file, "Q3 notes").unwrap();
    let c = chats.clone();
    let uploaded = blocking(move || c.upload(&file)).await.unwrap();
    assert_eq!(uploaded.signed_id, "signed-1");
    assert_eq!(
        server.state.lock().unwrap().uploads,
        [(
            "notes.txt".to_string(),
            "text/plain".to_string(),
            "Q3 notes".to_string()
        )]
    );

    // Asking with the model and the file; a model that can't be used says why.
    let c = chats.clone();
    blocking(move || {
        c.ask(&Ask {
            chat: None,
            text: "Summarize".into(),
            project: Some(5),
            model: Some("3".into()),
            attachments: vec!["signed-1".into()],
        })
    })
    .await
    .unwrap();
    assert_eq!(
        server.state.lock().unwrap().asked_with,
        [json!(["3", ["signed-1"], 5])]
    );
    let c = chats.clone();
    let refused = blocking(move || {
        c.ask(&Ask {
            chat: Some(7),
            text: "Again".into(),
            model: Some("4".into()),
            ..Ask::default()
        })
    })
    .await
    .unwrap_err();
    assert_eq!(refused.code, "model_unavailable");
    assert_eq!(refused.message, "That model isn't available on your plan");

    // Deciding a change, and the chat actions.
    let c = chats.clone();
    blocking(move || c.approve(7, 12, true)).await.unwrap();
    let c = chats.clone();
    blocking(move || c.deny(7, 12, Some("Use the Web team")))
        .await
        .unwrap();
    let c = chats.clone();
    blocking(move || c.retry(7, None)).await.unwrap();
    let c = chats.clone();
    let branch = blocking(move || c.branch(7, Some(3))).await.unwrap();
    assert_eq!(branch.number, 9);
    let c = chats.clone();
    let renamed = blocking(move || c.rename(7, "Budget")).await.unwrap();
    assert_eq!(renamed.title, "Budget");
    let c = chats.clone();
    let shared = blocking(move || c.share(7)).await.unwrap();
    assert_eq!(shared.url, format!("{}/shared/abc", server.origin));
    assert_eq!(shared.chat.share.unwrap().url, shared.url);
    let c = chats.clone();
    let unshared = blocking(move || c.unshare(7)).await.unwrap();
    assert_eq!(unshared.share, None);

    // Answering a question from a tool's server, or declining it.
    let c = chats.clone();
    let missing = blocking(move || c.answer(7, 32, Some(json!({ "days": "30" }))))
        .await
        .unwrap_err();
    assert_eq!(missing.code, "invalid");
    assert_eq!(missing.message, "Environment is needed.");
    let c = chats.clone();
    blocking(move || {
        c.answer(
            7,
            32,
            Some(json!({ "environment": "production", "days": "30" })),
        )
    })
    .await
    .unwrap();
    let c = chats.clone();
    blocking(move || c.answer(7, 32, None)).await.unwrap();
    let c = chats.clone();
    blocking(move || c.decline(7, 32)).await.unwrap();
    let c = chats.clone();
    blocking(move || c.delete(7)).await.unwrap();
    assert_eq!(
        server.state.lock().unwrap().decisions,
        [
            "approve 7 12 true",
            "deny 7 12 \"Use the Web team\"",
            "retry 7 null",
            "branch 7 3",
            "rename 7 \"Budget\"",
            "share 7",
            "unshare 7",
            "answer 7 32 {\"days\":\"30\",\"environment\":\"production\"}",
            "answer 7 32 null",
            "decline 7 32",
            "delete 7",
        ]
    );

    // A file over the limit never leaves the computer.
    let big = fx.base.join("big.bin");
    std::fs::File::create(&big)
        .unwrap()
        .set_len(cww::tui::chat::MAX_UPLOAD_BYTES + 1)
        .unwrap();
    let c = chats.clone();
    let too_big = blocking(move || c.upload(&big)).await.unwrap_err();
    assert_eq!(too_big.code, "attachment_refused");
    assert_eq!(server.state.lock().unwrap().uploads.len(), 1);

    // An older server: a sentence for each, and chats without a model.
    server.state.lock().unwrap().old_api = true;
    let c = chats.clone();
    let old = blocking(move || c.models()).await.unwrap_err();
    assert_eq!(old.code, "unsupported");
    assert!(
        old.message.contains("doesn't let you pick a model"),
        "{}",
        old.message
    );
    let c = chats.clone();
    let old = blocking(move || c.retry(7, None)).await.unwrap_err();
    assert_eq!(old.code, "unsupported");
    assert!(old.message.contains("can't retry"), "{}", old.message);
    let c = chats.clone();
    let list = blocking(move || c.list()).await.unwrap();
    assert_eq!(list.chats[0].model, None);
    assert_eq!(list.chats[0].can, None);

    shutdown.cancel();
    tokio::time::timeout(Duration::from_secs(10), daemon)
        .await
        .expect("daemon stops")
        .unwrap()
        .unwrap();
}

/// The chat list stays current over the daemon's socket: one subscription
/// to the list's channel however many terminals watch it, its changes
/// relayed as they come, and a refused one asked again by the next watcher.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn keeps_the_chat_list_current_for_the_terminal() {
    use cww::tui::chat::{Chats, ListLive};

    let fx = fixture();
    let mut server = FakeServer::start().await;
    pair(&fx, &server);
    server.state.lock().unwrap().chat_access = true;
    let shutdown = CancellationToken::new();
    let daemon = tokio::spawn(cww::daemon::run(fx.paths.clone(), shutdown.clone(), false));
    let mut ws = tokio::time::timeout(Duration::from_secs(20), server.sockets.recv())
        .await
        .expect("the daemon connects")
        .unwrap();
    let chats = Chats::new(&fx.paths.socket_path());
    let identifier = r#"{"channel":"LocalAgent::ChatsChannel"}"#;

    type Closer = Arc<Mutex<Option<cww::control::Closer>>>;
    let watch = |chats: &Chats| {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let closer: Closer = Arc::default();
        let (c, held) = (chats.clone(), Arc::clone(&closer));
        let task = tokio::task::spawn_blocking(move || {
            c.follow_list(
                |c| *held.lock().unwrap() = c,
                |live| match live {
                    Some(live) => tx.send(live).is_ok(),
                    None => true,
                },
            )
        });
        (rx, task, closer)
    };
    let hang_up = |closer: &Closer| {
        if let Some(c) = closer.lock().unwrap().take() {
            c.close();
        }
    };
    /// The next change. A watcher that came before the tunnel's session
    /// started hears it's offline first.
    async fn next(rx: &mut tokio::sync::mpsc::UnboundedReceiver<ListLive>) -> ListLive {
        loop {
            let live = tokio::time::timeout(Duration::from_secs(10), rx.recv())
                .await
                .expect("a change in time")
                .expect("the watcher is running");
            if live != ListLive::Offline {
                return live;
            }
        }
    }
    async fn subscribed(ws: &mut WebSocketStream<TcpStream>) -> Value {
        serde_json::from_str(&next_text(ws).await.unwrap()).unwrap()
    }

    // The first watcher subscribes; the server refuses (chats were just
    // taken back, say).
    let (mut first, first_task, first_closer) = watch(&chats);
    let subscribe = subscribed(&mut ws).await;
    assert_eq!(subscribe["command"], "subscribe");
    assert_eq!(subscribe["identifier"], identifier);
    ws.send(Message::text(
        json!({ "identifier": identifier, "type": "reject_subscription" }).to_string(),
    ))
    .await
    .unwrap();
    assert_eq!(next(&mut first).await, ListLive::Refused);

    // The next watcher asks again, and this time it's confirmed.
    let (mut second, second_task, second_closer) = watch(&chats);
    assert_eq!(subscribed(&mut ws).await["identifier"], identifier);
    ws.send(Message::text(
        json!({ "identifier": identifier, "type": "confirm_subscription" }).to_string(),
    ))
    .await
    .unwrap();
    assert_eq!(next(&mut first).await, ListLive::Watching);
    assert_eq!(next(&mut second).await, ListLive::Watching);

    // Each change reaches every watcher, and none reaches the MCP server.
    let changes = [
        json!({ "event": "chat", "chat": { "number": 12, "title": "Started on the web", "state": "processing", "updated_at": "2026-10-02T09:05:00Z" } }),
        json!({ "event": "removed", "number": 7 }),
        json!({ "event": "projects", "projects": [{ "id": 7, "name": "Launch", "icon": "folder-simple", "hq": false, "all_access": false, "url": "https://x/projects/4" }] }),
        json!({ "event": "account", "account": { "name": "Plenty UG", "logo": null }, "user": { "name": "Carmine", "avatar": null }, "credits": { "left": 1180, "capacity": 5000, "running_low": false }, "locked_reason": null }),
        json!({ "jsonrpc": "2.0", "id": 77, "method": "tools/call", "params": {} }),
    ];
    for change in &changes {
        ws.send(Message::text(
            json!({ "identifier": identifier, "message": change }).to_string(),
        ))
        .await
        .unwrap();
    }
    for rx in [&mut first, &mut second] {
        let ListLive::Chat(chat) = next(rx).await else {
            panic!("a chat first");
        };
        assert_eq!((chat.number, chat.processing()), (12, true));
        assert_eq!(next(rx).await, ListLive::Removed(7));
        let ListLive::Projects(projects) = next(rx).await else {
            panic!("the projects");
        };
        assert_eq!(projects[0].name, "Launch");
        let ListLive::Account(account) = next(rx).await else {
            panic!("the account");
        };
        assert_eq!(account.credits.unwrap().left, 1180);
    }
    let mut s = Session {
        ws,
        next_id: 0,
        legacy: false,
    };
    let tools = s.request("tools/list", json!({})).await;
    assert!(tools["result"]["tools"].is_array(), "{tools}");

    // The last watcher gone, the daemon unsubscribes at once.
    hang_up(&first_closer);
    hang_up(&second_closer);
    let mut unsubscribed = false;
    for _ in 0..20 {
        let Some(text) = tokio::time::timeout(Duration::from_secs(10), next_text(&mut s.ws))
            .await
            .ok()
            .flatten()
        else {
            break;
        };
        let frame: Value = serde_json::from_str(&text).unwrap();
        if frame["command"] == "unsubscribe" && frame["identifier"] == identifier {
            unsubscribed = true;
            break;
        }
    }
    assert!(unsubscribed, "the daemon leaves the list's channel");

    shutdown.cancel();
    tokio::time::timeout(Duration::from_secs(10), daemon)
        .await
        .expect("daemon stops")
        .unwrap()
        .unwrap();
    let _ = tokio::time::timeout(Duration::from_secs(10), first_task).await;
    let _ = tokio::time::timeout(Duration::from_secs(10), second_task).await;
}

/// Logos and file-type icons come from the paired server through the
/// daemon: only images under /assets/, without the token, small, and never
/// through a redirect.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fetches_the_webs_images_for_clients() {
    use cww::tui::chat::Chats;

    let fx = fixture();
    let mut server = FakeServer::start().await;
    pair(&fx, &server);
    let shutdown = CancellationToken::new();
    let daemon = tokio::spawn(cww::daemon::run(fx.paths.clone(), shutdown.clone(), false));
    let _ws = tokio::time::timeout(Duration::from_secs(20), server.sockets.recv())
        .await
        .expect("the daemon connects")
        .unwrap();
    let chats = Chats::new(&fx.paths.socket_path());
    let fetch = |path: &'static str| {
        let c = chats.clone();
        tokio::task::spawn_blocking(move || c.asset(path))
    };

    let logo = fetch("/assets/providers/slack-0c9450af.svg")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(logo.content_type, "image/svg+xml");
    assert!(logo.bytes.starts_with(b"<svg"));
    for (path, code) in [
        ("/assets/missing.svg", "not_found"),
        ("/assets/big.png", "unreachable"),
        ("/assets/page.svg", "unsupported"),
        ("/assets/moved.svg", "unreachable"),
        ("/local_agent/chats", "invalid"),
        ("/assets/../local_agent/chats", "invalid"),
    ] {
        let failure = fetch(path).await.unwrap().unwrap_err();
        assert_eq!(failure.code, code, "{path}: {}", failure.message);
    }
    // A client that skips the check is refused by the daemon too.
    let refused = control_error(
        &fx.paths,
        ControlRequest::Asset {
            path: "/local_agent/chats".into(),
        },
    )
    .await;
    assert!(refused.contains("isn't an image"), "{refused}");
    {
        let st = server.state.lock().unwrap();
        assert_eq!(st.assets_with_credentials, 0, "images go without the token");
        assert!(
            !st.log.iter().any(|l| l.contains("/local_agent/chats")),
            "nothing but images was asked for: {:?}",
            st.log
        );
    }

    shutdown.cancel();
    tokio::time::timeout(Duration::from_secs(10), daemon)
        .await
        .expect("daemon stops")
        .unwrap()
        .unwrap();
}

/// Pair, sharing `docs` with changes allowed (`writable`) and a read-only
/// `ro` folder next to it.
fn pair_for_changes(fx: &Fixture, server: &FakeServer, writable: bool, limits: Option<Limits>) {
    pair(fx, server);
    let ro = fx.base.join("work/ro");
    std::fs::create_dir_all(&ro).unwrap();
    std::fs::write(ro.join("keep.md"), "keep\n").unwrap();
    let mut config = Config::load(&fx.paths).unwrap();
    config.roots[0].writable = writable;
    config.roots.push(Root {
        id: "ro".into(),
        label: "Read only".into(),
        path: ro,
        follow_symlinks: false,
        writable: false,
    });
    if let Some(limits) = limits {
        config.limits = limits;
    }
    config.save(&fx.paths).unwrap();
}

async fn connect(server: &mut FakeServer) -> Session {
    let ws = tokio::time::timeout(Duration::from_secs(20), server.sockets.recv())
        .await
        .expect("the daemon connects")
        .unwrap();
    Session {
        ws,
        next_id: 0,
        legacy: false,
    }
}

fn tool_names(tools: &Value) -> Vec<String> {
    tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect()
}

/// Every change tool over the wire, and every way out of a shared folder
/// a change could try. The trash lives under the test's own directory
/// (`Paths::under`), as `$XDG_DATA_HOME/Trash` would.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn changes_files_over_the_wire() {
    let fx = fixture();
    let mut server = FakeServer::start().await;
    pair_for_changes(&fx, &server, true, None);
    let docs = fx.base.join("work/docs");
    let outside = fx.base.join("outside");
    let trash = fx.paths.home_trash.clone().unwrap();

    let shutdown = CancellationToken::new();
    let daemon = tokio::spawn(cww::daemon::run(fx.paths.clone(), shutdown.clone(), false));
    let mut s = connect(&mut server).await;

    // The change tools are listed, with annotations the server can trust
    // to tell writes from destructive calls.
    let tools = s.request("tools/list", json!({})).await;
    assert_eq!(
        tool_names(&tools),
        [
            "roots",
            "search",
            "list",
            "read",
            "create",
            "write",
            "edit",
            "mkdir",
            "move",
            "delete",
            "create_document"
        ]
    );
    for tool in tools["result"]["tools"].as_array().unwrap() {
        let name = tool["name"].as_str().unwrap();
        let a = &tool["annotations"];
        assert_eq!(a["openWorldHint"], false, "{name}");
        let read_only = ["roots", "search", "list", "read"].contains(&name);
        assert_eq!(a["readOnlyHint"], read_only, "{name}");
        let destructive = ["write", "move", "delete", "create_document"].contains(&name);
        assert_eq!(a["destructiveHint"], destructive, "{name}");
        assert_eq!(a["idempotentHint"], read_only || name == "mkdir", "{name}");
        assert_eq!(tool["inputSchema"]["additionalProperties"], false, "{name}");
    }
    let meta = |name: &str| {
        tools["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["name"] == name)
            .unwrap()["_meta"]["com.chatwithwork/writeWhen"]
            .clone()
    };
    assert_eq!(meta("write"), json!({ "mode": "append" }));
    assert_eq!(meta("move"), json!({ "replace": false }));
    assert_eq!(meta("delete"), Value::Null);
    assert_eq!(meta("create_document"), json!({ "replace": false }));
    let roots = s.call("roots", json!({})).await;
    assert_eq!(roots["structuredContent"]["roots"][0]["writable"], true);
    assert_eq!(roots["structuredContent"]["roots"][1]["writable"], false);

    // create, then a dry run of a replace with its diff, then the replace.
    let made = s
        .call(
            "create",
            json!({ "path": "docs:plans/q4.md", "content": "Q4: ship\n" }),
        )
        .await;
    assert_eq!(made["isError"], false, "{made}");
    assert_eq!(made["structuredContent"]["effect"], "created");
    assert_eq!(made["structuredContent"]["kind"], "file");
    assert_eq!(
        std::fs::read_to_string(docs.join("plans/q4.md")).unwrap(),
        "Q4: ship\n"
    );
    let preview = s
        .call(
            "write",
            json!({ "path": "docs:plans/q4.md", "content": "Q4: ship it\n", "dry_run": true }),
        )
        .await;
    let p = &preview["structuredContent"];
    assert_eq!(p["dry_run"], true);
    assert!(
        p["diff"]
            .as_str()
            .unwrap()
            .contains("-Q4: ship\n+Q4: ship it\n"),
        "{preview}"
    );
    let sha = p["previous"]["sha256"].as_str().unwrap().to_string();
    let replaced = s
        .call(
            "write",
            json!({ "path": "docs:plans/q4.md", "content": "Q4: ship it\n", "expected_sha256": sha }),
        )
        .await;
    assert_eq!(
        replaced["structuredContent"]["effect"], "replaced",
        "{replaced}"
    );
    assert_eq!(replaced["structuredContent"]["previous"]["in_trash"], true);
    let stale = s
        .call(
            "write",
            json!({ "path": "docs:plans/q4.md", "content": "x", "expected_sha256": sha }),
        )
        .await;
    assert_eq!(error_code(&stale), "conflict");

    // append, edit, mkdir.
    s.call(
        "write",
        json!({ "path": "docs:plans/q4.md", "content": "- hire\n", "mode": "append" }),
    )
    .await;
    let edited = s
        .call(
            "edit",
            json!({ "path": "docs:plans/q4.md", "old_text": "- hire", "new_text": "- hire two" }),
        )
        .await;
    assert_eq!(edited["structuredContent"]["effect"], "edited", "{edited}");
    assert_eq!(
        std::fs::read_to_string(docs.join("plans/q4.md")).unwrap(),
        "Q4: ship it\n- hire two\n"
    );
    let folder = s
        .call(
            "mkdir",
            json!({ "path": "docs:archive/2026", "parents": true }),
        )
        .await;
    assert_eq!(
        folder["structuredContent"]["effect"], "created_folder",
        "{folder}"
    );

    // move with and without replace, then delete.
    let moved = s
        .call(
            "move",
            json!({ "from": "docs:plans/q4.md", "to": "docs:archive/2026/q4.md" }),
        )
        .await;
    assert_eq!(
        moved["structuredContent"]["from"], "docs:plans/q4.md",
        "{moved}"
    );
    assert_eq!(
        moved["structuredContent"]["path"],
        "docs:archive/2026/q4.md"
    );
    std::fs::write(docs.join("plans/q4.md"), "newer\n").unwrap();
    let onto = s
        .call(
            "move",
            json!({ "from": "docs:plans/q4.md", "to": "docs:archive/2026/q4.md" }),
        )
        .await;
    assert_eq!(error_code(&onto), "exists");
    let replace = s
        .call(
            "move",
            json!({ "from": "docs:plans/q4.md", "to": "docs:archive/2026/q4.md", "replace": true }),
        )
        .await;
    assert_eq!(
        replace["structuredContent"]["previous"]["in_trash"], true,
        "{replace}"
    );
    let deleted = s.call("delete", json!({ "path": "docs:archive" })).await;
    assert_eq!(
        deleted["structuredContent"]["effect"], "trashed",
        "{deleted}"
    );
    assert_eq!(deleted["structuredContent"]["kind"], "dir");
    assert!(!docs.join("archive").exists());
    assert_eq!(
        std::fs::read_to_string(if cfg!(target_os = "macos") {
            trash.join("archive/2026/q4.md")
        } else {
            trash.join("files/archive/2026/q4.md")
        })
        .unwrap(),
        "newer\n"
    );
    #[cfg(not(target_os = "macos"))]
    {
        let record = std::fs::read_to_string(trash.join("info/archive.trashinfo")).unwrap();
        assert!(record.starts_with("[Trash Info]\nPath=/"), "{record}");
    }

    // Word and Excel documents, read back through `read`.
    let doc = s
        .call(
            "create_document",
            json!({ "path": "docs:report.docx", "content": "# Report\n\n- **Falcon** ships\n" }),
        )
        .await;
    assert_eq!(doc["structuredContent"]["effect"], "created", "{doc}");
    let text = s.call("read", json!({ "path": "docs:report.docx" })).await;
    assert!(
        text["structuredContent"]["text"]
            .as_str()
            .unwrap()
            .contains("Falcon ships"),
        "{text}"
    );
    let sheet = json!({ "path": "docs:budget.xlsx", "sheets": [
        { "name": "Budget", "header": true, "rows": [["Item", "Cost"], ["Falcon", 10.5], ["=2+2", null]] }
    ] });
    let book = s.call("create_document", sheet.clone()).await;
    assert_eq!(book["structuredContent"]["effect"], "created", "{book}");
    let text = s.call("read", json!({ "path": "docs:budget.xlsx" })).await;
    let text = text["structuredContent"]["text"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        text.contains("Falcon\t10.5") && text.contains("=2+2"),
        "{text}"
    );
    assert_eq!(
        error_code(&s.call("create_document", sheet.clone()).await),
        "exists"
    );
    let mut again = sheet.clone();
    again["replace"] = json!(true);
    let replaced_book = s.call("create_document", again).await;
    assert_eq!(
        replaced_book["structuredContent"]["effect"], "replaced",
        "{replaced_book}"
    );
    assert_eq!(
        replaced_book["structuredContent"]["previous"]["in_trash"],
        true
    );
    for (args, expected) in [
        (
            json!({ "path": "docs:deck.pptx", "content": "x" }),
            "invalid_argument",
        ),
        (
            json!({ "path": "docs:macro.docm", "content": "x" }),
            "invalid_argument",
        ),
        (
            json!({ "path": "docs:a.docx", "sheets": [] }),
            "invalid_argument",
        ),
        (
            json!({ "path": "docs:a.xlsx", "content": "x" }),
            "invalid_argument",
        ),
        (
            json!({ "path": "docs:a.xlsx", "sheets": [{ "name": "a/b", "rows": [] }] }),
            "invalid_argument",
        ),
        (
            json!({ "path": "ro:a.docx", "content": "x" }),
            "not_writable",
        ),
    ] {
        let result = s.call("create_document", args.clone()).await;
        assert_eq!(error_code(&result), expected, "{args}: {result}");
    }

    // Every way out of the folder, or around its rules, is refused.
    for (tool, args, expected) in [
        (
            "create",
            json!({ "path": "docs:../outside/x.txt", "content": "x" }),
            "invalid_path",
        ),
        (
            "create",
            json!({ "path": "/etc/x.txt", "content": "x" }),
            "invalid_path",
        ),
        (
            "create",
            json!({ "path": "docs:linked/x.txt", "content": "x" }),
            "denied",
        ),
        (
            "write",
            json!({ "path": "docs:escape.txt", "content": "x" }),
            "denied",
        ),
        (
            "write",
            json!({ "path": "docs:hardlink.txt", "content": "x" }),
            "denied",
        ),
        (
            "edit",
            json!({ "path": "docs:escape.txt", "old_text": "s", "new_text": "x" }),
            "denied",
        ),
        ("delete", json!({ "path": "docs:linked" }), "denied"),
        ("delete", json!({ "path": "docs:escape.txt" }), "denied"),
        (
            "write",
            json!({ "path": "docs:.env", "content": "x" }),
            "denied",
        ),
        (
            "create",
            json!({ "path": "docs:keys/id_ed25519", "content": "x" }),
            "denied",
        ),
        (
            "create",
            json!({ "path": "docs:.git/hooks/pre-commit", "content": "x" }),
            "denied",
        ),
        (
            "move",
            json!({ "from": "docs:.env", "to": "docs:env.txt" }),
            "denied",
        ),
        (
            "move",
            json!({ "from": "docs:notes.md", "to": "docs:.ssh/notes.md" }),
            "denied",
        ),
        (
            "move",
            json!({ "from": "docs:keys", "to": "docs:keys2" }),
            "denied",
        ),
        (
            "move",
            json!({ "from": "docs:notes.md", "to": "ro:notes.md" }),
            "not_writable",
        ),
        (
            "create",
            json!({ "path": "ro:new.md", "content": "x" }),
            "not_writable",
        ),
        ("delete", json!({ "path": "ro:keep.md" }), "not_writable"),
        (
            "create",
            json!({ "path": "docs:setup.exe", "content": "MZ" }),
            "not_changeable",
        ),
        (
            "create",
            json!({ "path": "docs:report.docx", "content": "x" }),
            "not_changeable",
        ),
        ("delete", json!({ "path": "docs:" }), "invalid_path"),
        (
            "write",
            json!({ "path": "docs:notes.md", "content": "x", "mode": "truncate" }),
            "invalid_argument",
        ),
        (
            "delete",
            json!({ "path": "docs:notes.md", "recursive": true }),
            "invalid_argument",
        ),
    ] {
        let result = s.call(tool, args.clone()).await;
        assert_eq!(error_code(&result), expected, "{tool} {args}: {result}");
    }
    assert_eq!(
        std::fs::read_to_string(outside.join("secret.txt")).unwrap(),
        "supersecret-outside Falcon\n"
    );
    assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 1);
    assert_eq!(
        std::fs::read_to_string(docs.join(".env")).unwrap(),
        "API_TOKEN=supersecret-env\n"
    );
    assert!(docs.join("keys/id_rsa").exists());

    // Nothing the server got names a local path or the trash.
    let everything = format!("{made}{replaced}{moved}{replace}{deleted}");
    assert!(
        !everything.contains(&fx.base.display().to_string()),
        "{everything}"
    );

    // The audit log has each change with its effect, and where the trashed
    // copy went (locally only).
    let audit = std::fs::read_to_string(fx.paths.audit_file()).unwrap();
    let entries: Vec<Value> = audit
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let find = |tool: &str, effect: &str| {
        entries
            .iter()
            .find(|e| e["tool"] == tool && e["effect"] == effect && e["dry_run"] != true)
            .unwrap_or_else(|| panic!("no {tool} {effect} in {audit}"))
            .clone()
    };
    let replaced_entry = find("write", "replaced");
    assert!(
        replaced_entry["trash"]
            .as_str()
            .unwrap()
            .starts_with(&trash.display().to_string())
    );
    assert_eq!(replaced_entry["written"], 12);
    let moved_entry = find("move", "moved");
    assert_eq!(moved_entry["path"], "docs:plans/q4.md");
    assert_eq!(moved_entry["to"], "docs:archive/2026/q4.md");
    assert_eq!(find("delete", "trashed")["path"], "docs:archive");
    assert!(
        entries
            .iter()
            .any(|e| e["tool"] == "write" && e["dry_run"] == true && e["effect"] == "replaced")
    );
    assert!(entries.iter().any(|e| e["tool"] == "create"
        && e["code"] == "not_writable"
        && e["decision"] == "denied"));

    shutdown.cancel();
    tokio::time::timeout(Duration::from_secs(10), daemon)
        .await
        .expect("daemon stops")
        .unwrap()
        .unwrap();
}

/// Changes have their own budget, apart from reads.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn changes_are_rate_limited() {
    let fx = fixture();
    let mut server = FakeServer::start().await;
    let limits = Limits {
        changes_per_minute: 2,
        ..Limits::default()
    };
    pair_for_changes(&fx, &server, true, Some(limits));
    let shutdown = CancellationToken::new();
    let daemon = tokio::spawn(cww::daemon::run(fx.paths.clone(), shutdown.clone(), false));
    let mut s = connect(&mut server).await;
    for n in 0..2 {
        let made = s
            .call(
                "create",
                json!({ "path": format!("docs:{n}.md"), "content": "x" }),
            )
            .await;
        assert_eq!(made["isError"], false, "{made}");
    }
    let third = s
        .call("create", json!({ "path": "docs:2.md", "content": "x" }))
        .await;
    assert_eq!(error_code(&third), "rate_limited");
    assert!(!fx.base.join("work/docs/2.md").exists());
    // Reads go on.
    let read = s.call("read", json!({ "path": "docs:0.md" })).await;
    assert_eq!(read["isError"], false, "{read}");
    shutdown.cancel();
    tokio::time::timeout(Duration::from_secs(10), daemon)
        .await
        .expect("daemon stops")
        .unwrap()
        .unwrap();
}

/// Only this computer turns changes on, over the control channel; the
/// tools appear on the next connection and go away with the switch.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn allowing_changes_is_a_local_switch() {
    let fx = fixture();
    let mut server = FakeServer::start().await;
    pair_for_changes(&fx, &server, false, None);
    let shutdown = CancellationToken::new();
    let daemon = tokio::spawn(cww::daemon::run(fx.paths.clone(), shutdown.clone(), false));
    let mut s = connect(&mut server).await;
    let tools = s.request("tools/list", json!({})).await;
    assert_eq!(tool_names(&tools), ["roots", "search", "list", "read"]);
    let refused = s
        .request(
            "tools/call",
            json!({ "name": "create", "arguments": { "path": "docs:x.md", "content": "x" } }),
        )
        .await;
    assert_eq!(refused["error"]["code"], -32602, "{refused}");

    let allowed = control(
        &fx.paths,
        ControlRequest::RootsWritable {
            root: "docs".into(),
            writable: true,
            i_know: false,
        },
    )
    .await;
    assert_eq!(allowed["root"]["writable"], true, "{allowed}");
    assert!(Config::load(&fx.paths).unwrap().roots[0].writable, "saved");
    let status = control(&fx.paths, ControlRequest::Status).await;
    assert_eq!(status["roots"][0]["writable"], true, "{status}");
    // The daemon reconnects so the server lists the new tools.
    let mut s = connect(&mut server).await;
    let tools = s.request("tools/list", json!({})).await;
    assert!(
        tool_names(&tools).contains(&"delete".to_string()),
        "{tools}"
    );
    let made = s
        .call("create", json!({ "path": "docs:x.md", "content": "x" }))
        .await;
    assert_eq!(made["isError"], false, "{made}");
    let ro = s
        .call("create", json!({ "path": "ro:x.md", "content": "x" }))
        .await;
    assert_eq!(error_code(&ro), "not_writable");

    control(
        &fx.paths,
        ControlRequest::RootsWritable {
            root: "docs".into(),
            writable: false,
            i_know: false,
        },
    )
    .await;
    let mut s = connect(&mut server).await;
    let tools = s.request("tools/list", json!({})).await;
    assert_eq!(tool_names(&tools), ["roots", "search", "list", "read"]);
    let log = control(&fx.paths, ControlRequest::AuditTail { lines: Some(200) }).await;
    let events: Vec<&str> = log["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["event"].as_str().unwrap())
        .collect();
    assert!(
        events.contains(&"root_changes_allowed") && events.contains(&"root_changes_stopped"),
        "{events:?}"
    );
    shutdown.cancel();
    tokio::time::timeout(Duration::from_secs(10), daemon)
        .await
        .expect("daemon stops")
        .unwrap()
        .unwrap();
}
