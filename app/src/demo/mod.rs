//! `--demo`: a stand-in daemon with sample folders, activity, chats and a
//! paired account, speaking the real control protocol on a private socket.
//! For screenshots, for trying the app without pairing a computer, and for
//! the window's tests, which also read the requests it received. Built only
//! with the `demo` feature, and in tests.

mod chats;

use std::collections::HashSet;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use serde_json::{Value, json};

use crate::paths::Paths;

struct State {
    status: Value,
    log: Vec<Value>,
    generation: u64,
    requests: Vec<Value>,
    chats: Vec<chats::Chat>,
    /// Every chat update so far, as followers get them.
    chat_events: Vec<Value>,
    /// Answers don't stream on their own (tests hold them still).
    held: bool,
    /// Chats whose answer is streaming now.
    streaming: HashSet<u64>,
    /// Chats asked to stop.
    cancelled: HashSet<u64>,
    next_id: u64,
}

struct Demo {
    state: Mutex<State>,
    changed: Condvar,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scenario {
    /// Paired, two folders, a day of activity.
    Sample,
    /// Running, but not paired and sharing nothing: a first run.
    Fresh,
}

/// A running stand-in daemon.
pub struct Server {
    pub paths: Paths,
    demo: Arc<Demo>,
}

impl Server {
    /// Pairing and logging out, without a Chat with Work server.
    pub fn account(&self) -> Arc<dyn crate::pairing::Account> {
        Arc::new(Account(Arc::clone(&self.demo)))
    }

    /// Every request received so far, except `status` and `watch`.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn requests(&self) -> Vec<Value> {
        self.demo.state.lock().expect("demo lock").requests.clone()
    }

    /// Hold answers still: nothing streams until released, so tests see
    /// one moment of it.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn hold_streams(&self, held: bool) {
        self.demo.change(|s| s.held = held);
    }

    /// Finish a held answer in chat `chat`.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn finish(&self, chat: u64, message: u64, content: &str) {
        self.demo.apply(
            chat,
            &chats::Step::Finish {
                id: message,
                content: content.to_string(),
            },
        );
    }
}

fn ago(seconds: i64) -> String {
    (jiff::Timestamp::now() - jiff::SignedDuration::from_secs(seconds)).to_string()
}

fn fresh() -> State {
    let home = crate::paths::home_dir().unwrap_or_else(|| PathBuf::from("/home/me"));
    let status = json!({
        "ok": true,
        "version": "0.1.0",
        "pid": 4242,
        "connection": { "connection": "not_paired", "since": ago(5) },
        "server": null,
        "device_id": null,
        "paused": false,
        "roots": [],
        "pairing": null,
        "config_file": home.join(".config/cww/config.toml"),
        "audit_file": home.join(".local/state/cww/audit.jsonl"),
    });
    State {
        status,
        log: vec![json!({ "ts": ago(5), "event": "started", "detail": "cww 0.1.0" })],
        generation: 0,
        requests: Vec::new(),
        chats: chats::sample(),
        chat_events: Vec::new(),
        held: false,
        streaming: HashSet::new(),
        cancelled: HashSet::new(),
        next_id: 1000,
    }
}

fn sample() -> State {
    let home = crate::paths::home_dir().unwrap_or_else(|| PathBuf::from("/home/me"));
    let tool = |secs: i64, tool: &str, path: Option<&str>, query: Option<&str>, decision: &str| {
        let mut e = json!({ "ts": ago(secs), "event": "tool", "tool": tool, "decision": decision, "chat_id": "1842" });
        if let Some(path) = path {
            e["path"] = json!(path);
        }
        if let Some(query) = query {
            e["query"] = json!(query);
            e["results"] = json!(6);
        }
        if decision == "allowed" && tool == "read" {
            e["bytes"] = json!(18_432);
        }
        if decision == "denied" {
            e["code"] = json!("denied");
            e["reason"] = json!("this path is on the deny list (.env*)");
        }
        e
    };
    let log = vec![
        json!({ "ts": ago(26 * 3600), "event": "started", "detail": "cww 0.1.0" }),
        json!({ "ts": ago(26 * 3600 - 2), "event": "connected", "detail": "wss://chatwithwork.com/local_agent as device 42" }),
        tool(
            25 * 3600,
            "search",
            None,
            Some("vendor contract renewal"),
            "allowed",
        ),
        tool(
            25 * 3600 - 30,
            "read",
            Some("documents:Contracts/Acme renewal 2026.pdf"),
            None,
            "allowed",
        ),
        json!({ "ts": ago(3 * 3600), "event": "paused" }),
        json!({ "ts": ago(3 * 3600 - 600), "event": "resumed" }),
        tool(40 * 60, "roots", None, None, "allowed"),
        tool(39 * 60, "search", None, Some("Q3 roadmap"), "allowed"),
        tool(
            38 * 60,
            "list",
            Some("work-projects:Falcon"),
            None,
            "allowed",
        ),
        tool(
            37 * 60,
            "read",
            Some("work-projects:Falcon/.env"),
            None,
            "denied",
        ),
        tool(
            36 * 60,
            "read",
            Some("documents:Plans/Q3 roadmap.md"),
            None,
            "allowed",
        ),
        tool(
            2 * 60,
            "read",
            Some("documents:Plans/Budget 2027.xlsx"),
            None,
            "allowed",
        ),
    ];
    let status = json!({
        "ok": true,
        "version": "0.1.0",
        "pid": 4242,
        "connection": { "connection": "connected", "since": ago(26 * 3600 - 2) },
        "server": "https://chatwithwork.com",
        "device_id": "42",
        "paused": false,
        "roots": [
            { "id": "documents", "label": "Documents", "available": true, "index": "ready",
              "indexed_files": 1532, "local_path": home.join("Documents") },
            { "id": "work-projects", "label": "Work projects", "available": true, "index": "indexing",
              "indexed_files": 214, "local_path": home.join("Work/Projects") },
        ],
        "pairing": null,
        "config_file": home.join(".config/cww/config.toml"),
        "audit_file": home.join(".local/state/cww/audit.jsonl"),
    });
    State {
        status,
        log,
        generation: 0,
        requests: Vec::new(),
        chats: chats::sample(),
        chat_events: Vec::new(),
        held: false,
        streaming: HashSet::new(),
        cancelled: HashSet::new(),
        next_id: 1000,
    }
}

fn deny() -> Value {
    json!({
        "ok": true,
        "builtin": [".ssh", ".gnupg", ".aws", ".azure", ".config/gcloud", ".kube",
            ".docker/config.json", ".netrc", ".npmrc", ".pypirc", ".git-credentials",
            ".config/gh/hosts.yml", ".env*", "*.pem", "*.key", "*.p12", "*.pfx", "id_*",
            "*.kdbx", "*.1pux", ".password-store", ".local/share/keyrings", "Library/Keychains",
            "*.keychain", "*.keychain-db", "Library/Mail", "Library/Messages", ".mozilla",
            ".config/google-chrome", "Library/Safari", "Library/Cookies", "/etc/shadow"],
        "extra": ["*.secret", "Clients/Confidential"],
        "removed": [],
        "own_dirs": [],
        "allow_hardlinks": false,
        "config_file": crate::paths::home_dir().map(|h| h.join(".config/cww/config.toml")),
    })
}

impl Demo {
    fn change(&self, edit: impl FnOnce(&mut State)) {
        let mut state = self.state.lock().expect("demo lock");
        edit(&mut state);
        state.generation += 1;
        self.changed.notify_all();
    }

    fn event(state: &mut State, event: &str) {
        state.log.push(json!({ "ts": ago(0), "event": event }));
    }

    fn answer(self: &Arc<Self>, request: &Value) -> Value {
        let cmd = request["cmd"].as_str().unwrap_or_default();
        if cmd != "status" {
            self.state
                .lock()
                .expect("demo lock")
                .requests
                .push(request.clone());
        }
        match cmd {
            "status" => self.state.lock().expect("demo lock").status.clone(),
            "pause" | "resume" => {
                let paused = cmd == "pause";
                self.change(|s| {
                    s.status["paused"] = json!(paused);
                    Self::event(s, if paused { "paused" } else { "resumed" });
                });
                json!({ "ok": true, "paused": paused })
            }
            "roots_add" => {
                let path = PathBuf::from(request["path"].as_str().unwrap_or_default());
                let label = request["label"]
                    .as_str()
                    .map(str::to_string)
                    .or_else(|| path.file_name().map(|n| n.to_string_lossy().into_owned()))
                    .unwrap_or_else(|| "Folder".into());
                let id: String = label
                    .to_lowercase()
                    .chars()
                    .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
                    .collect();
                let root = json!({ "id": id, "label": label, "available": true, "index": "indexing",
                                   "indexed_files": 0, "local_path": path });
                self.change(|s| {
                    s.status["roots"]
                        .as_array_mut()
                        .expect("roots")
                        .push(root.clone());
                    Self::event(s, "reloaded");
                });
                let me = Arc::clone(self);
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_secs(3));
                    me.change(|s| {
                        for r in s.status["roots"].as_array_mut().expect("roots") {
                            if r["id"] == json!(id) {
                                r["index"] = json!("ready");
                                r["indexed_files"] = json!(87);
                            }
                        }
                    });
                });
                json!({ "ok": true, "root": root })
            }
            "roots_remove" | "roots_label" => {
                let which = request["root"].as_str().unwrap_or_default().to_string();
                let label = request["label"].as_str().map(str::to_string);
                self.change(|s| {
                    let roots = s.status["roots"].as_array_mut().expect("roots");
                    match &label {
                        Some(label) => {
                            for r in roots.iter_mut().filter(|r| r["id"] == json!(which)) {
                                r["label"] = json!(label);
                            }
                        }
                        None => roots.retain(|r| r["id"] != json!(which)),
                    }
                    Self::event(s, "reloaded");
                });
                json!({ "ok": true })
            }
            "chats" | "chat" | "chat_send" | "chat_cancel" | "chat_access" => {
                self.chat_request(cmd, request)
            }
            "deny" => deny(),
            "audit_tail" => {
                let n = request["lines"].as_u64().unwrap_or(50) as usize;
                let log = &self.state.lock().expect("demo lock").log;
                json!({ "ok": true, "entries": log[log.len().saturating_sub(n)..] })
            }
            _ => json!({ "ok": false, "error": format!("the demo doesn't do {cmd:?}") }),
        }
    }

    fn serve(self: Arc<Self>, stream: impl Read + Write) {
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        while reader.read_line(&mut line).is_ok_and(|n| n > 0) {
            let request: Value = serde_json::from_str(&line).unwrap_or_default();
            line.clear();
            if request["cmd"] == "subscribe"
                && request["topics"]
                    .as_array()
                    .is_some_and(|t| t.iter().any(|t| t == "chat"))
            {
                self.state
                    .lock()
                    .expect("demo lock")
                    .requests
                    .push(request.clone());
                return self.follow(
                    reader.get_mut(),
                    request["chat"].as_str().unwrap_or_default(),
                );
            }
            if request["cmd"] == "subscribe" {
                self.state
                    .lock()
                    .expect("demo lock")
                    .requests
                    .push(request.clone());
                let topics = json!(["status", "audit"]);
                if send(reader.get_mut(), &json!({ "ok": true, "topics": topics })).is_err() {
                    return;
                }
                // A status event now and after each change, and an audit
                // event for each new log entry, as the daemon sends them.
                let mut seen = u64::MAX;
                let mut logged = self.state.lock().expect("demo lock").log.len();
                loop {
                    let (status, entries) = {
                        let mut state = self.state.lock().expect("demo lock");
                        while state.generation == seen {
                            state = self.changed.wait(state).expect("demo lock");
                        }
                        seen = state.generation;
                        let entries = state.log[logged.min(state.log.len())..].to_vec();
                        logged = state.log.len();
                        (state.status.clone(), entries)
                    };
                    for entry in entries {
                        if send(
                            reader.get_mut(),
                            &json!({ "event": "audit", "entry": entry }),
                        )
                        .is_err()
                        {
                            return;
                        }
                    }
                    let mut status = status;
                    if let Some(map) = status.as_object_mut() {
                        map.remove("ok");
                    }
                    if send(
                        reader.get_mut(),
                        &json!({ "event": "status", "status": status }),
                    )
                    .is_err()
                    {
                        return;
                    }
                }
            }
            if send(reader.get_mut(), &self.answer(&request)).is_err() {
                return;
            }
        }
    }
}

impl Demo {
    /// The chat API (CONTROL.md, "Chats"), for a paired sample.
    fn chat_request(self: &Arc<Self>, cmd: &str, request: &Value) -> Value {
        let paired = !self.state.lock().expect("demo lock").status["server"].is_null();
        if !paired {
            return json!({ "ok": false, "code": "not_paired", "error": "This computer isn't paired." });
        }
        let number = request["chat"].as_str().and_then(|c| c.parse::<u64>().ok());
        match cmd {
            "chats" => {
                let state = self.state.lock().expect("demo lock");
                let list: Vec<Value> = state.chats.iter().map(|c| c.summary.clone()).collect();
                json!({
                    "ok": true,
                    "chats": list,
                    "projects": [],
                    "account": { "name": chats::ACCOUNT },
                    "user": { "name": chats::USER },
                    "locked_reason": null,
                })
            }
            "chat" => {
                let state = self.state.lock().expect("demo lock");
                match state
                    .chats
                    .iter()
                    .find(|c| Some(c.summary["number"].as_u64().unwrap_or(0)) == number)
                {
                    Some(chat) => json!({
                        "ok": true,
                        "chat": chat.summary,
                        "locked_reason": null,
                        "entries": chat.entries,
                    }),
                    None => json!({ "ok": false, "code": "not_found", "error": "No such chat" }),
                }
            }
            "chat_send" => {
                let text = request["text"].as_str().unwrap_or_default().to_string();
                let (number, summary) = {
                    let mut state = self.state.lock().expect("demo lock");
                    let id = state.next_id;
                    state.next_id += 10;
                    let number = match number {
                        Some(n) => n,
                        None => {
                            let n = state
                                .chats
                                .iter()
                                .filter_map(|c| c.summary["number"].as_u64())
                                .max()
                                .unwrap_or(0)
                                + 1;
                            state.chats.insert(
                                0,
                                chats::Chat {
                                    summary: json!({
                                        "number": n, "title": chats::title_for(&text), "state": "idle",
                                        "project": null, "mine": true, "updated_at": ago(0),
                                        "url": format!("https://chatwithwork.com/northwind/chats/{n}"),
                                    }),
                                    entries: Vec::new(),
                                },
                            );
                            n
                        }
                    };
                    let Some(chat) = state
                        .chats
                        .iter_mut()
                        .find(|c| c.summary["number"] == json!(number))
                    else {
                        return json!({ "ok": false, "code": "not_found", "error": "No such chat" });
                    };
                    if chat.summary["state"] == "processing" {
                        return json!({ "ok": false, "code": "chat_busy", "error": "An answer is being written in this chat." });
                    }
                    chat.entries
                        .push(json!({ "kind": "user", "id": id, "content": text }));
                    chat.summary["state"] = json!("processing");
                    chat.summary["updated_at"] = json!(ago(0));
                    (number, chat.summary.clone())
                };
                self.changed(number);
                let held = self.state.lock().expect("demo lock").held;
                if !held {
                    let id = self.state.lock().expect("demo lock").next_id;
                    self.stream(number, chats::canned_stream(id + 1, id + 2));
                }
                json!({ "ok": true, "chat": summary })
            }
            "chat_cancel" => {
                let Some(number) = number else {
                    return json!({ "ok": false, "code": "bad_request", "error": "No chat" });
                };
                let streaming = {
                    let mut state = self.state.lock().expect("demo lock");
                    state.cancelled.insert(number);
                    state.streaming.contains(&number)
                };
                if !streaming {
                    self.settle(number);
                }
                json!({ "ok": true })
            }
            _ => json!({ "ok": true, "granted": true, "requested": false }),
        }
    }

    /// Tell followers that `chat` changed.
    fn changed(&self, chat: u64) {
        self.chat_event(chat, json!({ "type": "changed" }));
    }

    fn chat_event(&self, chat: u64, update: Value) {
        self.change(|s| {
            s.chat_events
                .push(json!({ "event": "chat", "chat": chat.to_string(), "update": update }));
        });
    }

    /// Stop a chat's answer where it is.
    fn settle(&self, chat: u64) {
        self.change(|s| {
            if let Some(c) = s
                .chats
                .iter_mut()
                .find(|c| c.summary["number"] == json!(chat))
            {
                c.summary["state"] = json!("idle");
                for entry in &mut c.entries {
                    if entry["kind"] == "activity" {
                        entry["pending"] = json!(false);
                    }
                }
            }
            s.cancelled.remove(&chat);
        });
        self.changed(chat);
    }

    /// Play `steps` into `chat` on a thread of their own.
    fn stream(self: &Arc<Self>, chat: u64, steps: Vec<(Duration, chats::Step)>) {
        let me = Arc::clone(self);
        if !me.state.lock().expect("demo lock").streaming.insert(chat) {
            return;
        }
        std::thread::spawn(move || {
            for (pause, step) in steps {
                std::thread::sleep(pause);
                let cancelled = me
                    .state
                    .lock()
                    .expect("demo lock")
                    .cancelled
                    .contains(&chat);
                if cancelled {
                    break;
                }
                me.apply(chat, &step);
            }
            me.change(|s| {
                s.streaming.remove(&chat);
            });
            if me
                .state
                .lock()
                .expect("demo lock")
                .cancelled
                .contains(&chat)
            {
                me.settle(chat);
            }
        });
    }

    /// One step of a streamed answer, as the server would write it.
    fn apply(&self, chat: u64, step: &chats::Step) {
        use chats::Step;
        match step {
            Step::Progress(text) => {
                self.chat_event(chat, json!({ "type": "progress", "text": text }))
            }
            Step::Chunk { id, text } => {
                self.change(|s| {
                    if let Some(c) = s
                        .chats
                        .iter_mut()
                        .find(|c| c.summary["number"] == json!(chat))
                        && !c.entries.iter().any(|e| e["id"] == json!(id))
                    {
                        c.entries.push(
                            json!({ "kind": "assistant", "id": id, "content": "", "sources": [] }),
                        );
                    }
                });
                self.chat_event(
                    chat,
                    json!({ "type": "chunk", "message_id": id, "text": text }),
                );
            }
            Step::Activity {
                id,
                pending,
                progress,
            } => {
                self.change(|s| {
                    let Some(c) = s.chats.iter_mut().find(|c| c.summary["number"] == json!(chat)) else {
                        return;
                    };
                    match c.entries.iter_mut().find(|e| e["id"] == json!(id)) {
                        Some(entry) => {
                            entry["pending"] = json!(pending);
                            if !pending {
                                let title = entry["title"].as_str().unwrap_or_default().replace("Searching", "Searched");
                                entry["title"] = json!(title);
                                entry["progress"] = Value::Null;
                                if let Some(steps) = entry["steps"].as_array_mut() {
                                    for step in steps.iter_mut() {
                                        step["pending"] = json!(false);
                                        let summary = step["summary"].as_str().unwrap_or_default().replace("Reading", "Read").replace('…', "");
                                        step["summary"] = json!(summary);
                                    }
                                }
                                let reads = entry["steps"].as_array().map_or(0, |s| s.iter().filter(|s| s["summary"].as_str().is_some_and(|t| t.starts_with("Read"))).count());
                                let searches = entry["steps"].as_array().map_or(0, |s| s.len() - reads);
                                let mut details = format!("{searches} search{}", if searches == 1 { "" } else { "es" });
                                if reads > 0 {
                                    details.push_str(&format!(", read {reads} file{}", if reads == 1 { "" } else { "s" }));
                                }
                                entry["details"] = json!(details);
                            }
                        }
                        None => c.entries.push(json!({
                            "kind": "activity", "id": id,
                            "title": "Searching Drive and Slack", "details": "1 search",
                            "progress": progress, "services": ["Drive", "Slack"], "pending": pending,
                            "steps": [
                                { "summary": "Searched Drive for “budget owner”", "pending": false, "files": ["Budget 2026.xlsx"] },
                                { "summary": "Reading Budget 2026.xlsx…", "pending": true, "files": [] },
                            ],
                        })),
                    }
                });
                self.changed(chat);
            }
            Step::Finish { id, content } => {
                self.change(|s| {
                    if let Some(c) = s.chats.iter_mut().find(|c| c.summary["number"] == json!(chat)) {
                        c.summary["state"] = json!("idle");
                        c.summary["updated_at"] = json!(ago(0));
                        match c.entries.iter_mut().find(|e| e["id"] == json!(id)) {
                            Some(entry) => entry["content"] = json!(content),
                            None => c.entries.push(json!({ "kind": "assistant", "id": id, "content": content, "sources": [] })),
                        }
                    }
                });
                self.changed(chat);
            }
        }
    }

    /// A `chat` subscription: "watching", then every update to the chat
    /// until the client goes away, as the daemon relays them.
    fn follow(self: &Arc<Self>, stream: &mut impl Write, chat: &str) {
        let Ok(number) = chat.parse::<u64>() else {
            let _ = send(
                stream,
                &json!({ "ok": false, "code": "bad_request", "error": "not a chat number" }),
            );
            return;
        };
        if send(stream, &json!({ "ok": true, "topics": ["chat"] })).is_err() {
            return;
        }
        let watching = json!({ "event": "chat", "chat": chat, "update": { "type": "watching" } });
        if send(stream, &watching).is_err() {
            return;
        }
        let (mut seen, held, catch_up) = {
            let state = self.state.lock().expect("demo lock");
            let processing = state.chats.iter().any(|c| {
                c.summary["number"] == json!(number) && c.summary["state"] == "processing"
            });
            (
                state.chat_events.len(),
                state.held,
                number == 12 && processing,
            )
        };
        // Chat 12 was being answered before it was opened: what's written
        // so far arrives first, and the rest streams unless held.
        if catch_up {
            let chunk = json!({ "event": "chat", "chat": chat,
                                "update": { "type": "chunk", "message_id": 123, "text": chats::stream_so_far() } });
            if send(stream, &chunk).is_err() {
                return;
            }
            if !held {
                self.stream(12, chats::rest_of_stream());
            }
        }
        loop {
            let events = {
                let mut state = self.state.lock().expect("demo lock");
                while state.chat_events.len() == seen {
                    state = self.changed.wait(state).expect("demo lock");
                }
                let events = state.chat_events[seen..].to_vec();
                seen = state.chat_events.len();
                events
            };
            for event in events.into_iter().filter(|e| e["chat"] == json!(chat)) {
                if send(stream, &event).is_err() {
                    return;
                }
            }
        }
    }
}

fn send(stream: &mut impl Write, value: &Value) -> std::io::Result<()> {
    let mut out = serde_json::to_vec(value)?;
    out.push(b'\n');
    stream.write_all(&out)?;
    stream.flush()
}

/// Start a stand-in daemon in a fresh temporary directory; its paths point
/// the app at it.
pub fn start(scenario: Scenario) -> anyhow::Result<Server> {
    static STARTED: AtomicUsize = AtomicUsize::new(0);
    let n = STARTED.fetch_add(1, Ordering::SeqCst);
    let base = std::env::temp_dir().join(format!("cww-demo-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let paths = Paths::under(&base);
    std::fs::create_dir_all(&paths.daemon.config_dir)?;
    std::fs::create_dir_all(&paths.daemon.runtime_dir)?;
    if scenario == Scenario::Sample {
        // A paired sample has been through the welcome page.
        crate::ui::AppState { onboarded: true }.save(&paths);
    }
    let demo = Arc::new(Demo {
        state: Mutex::new(match scenario {
            Scenario::Sample => sample(),
            Scenario::Fresh => fresh(),
        }),
        changed: Condvar::new(),
    });
    // The daemon's own listener, so the endpoint, its permissions and, on
    // Windows, the pipe's owner are exactly what the app will meet.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()?;
    let mut listener = {
        let _guard = runtime.enter();
        cww::control::bind(&paths.socket_path())?
    };
    let serving = Arc::clone(&demo);
    std::thread::spawn(move || {
        runtime.block_on(async move {
            loop {
                match listener.accept().await {
                    Ok(stream) => {
                        let demo = Arc::clone(&serving);
                        tokio::task::spawn_blocking(move || {
                            demo.serve(tokio_util::io::SyncIoBridge::new(stream));
                        });
                    }
                    Err(e) => log::warn!("demo: {e:#}"),
                }
            }
        });
    });
    Ok(Server { paths, demo })
}

/// Pairs instantly-ish and records what it was asked, in place of
/// `cww login --json`.
struct Account(Arc<Demo>);

impl crate::pairing::Account for Account {
    fn pair(
        &self,
        server: Option<String>,
        report: crate::pairing::Report,
    ) -> crate::pairing::Cancel {
        use crate::pairing::{Code, PairEvent};
        use std::sync::atomic::{AtomicBool, Ordering};

        let demo = Arc::clone(&self.0);
        demo.change(|s| s.requests.push(json!({ "cmd": "pair", "server": server })));
        let cancelled = Arc::new(AtomicBool::new(false));
        let stop = Arc::clone(&cancelled);
        std::thread::spawn(move || {
            report(PairEvent::Code(Code {
                user_code: "WDJB-MJHT".into(),
                verification_uri: "https://chatwithwork.com/device".into(),
                browser_url: Some("https://chatwithwork.com/device?user_code=WDJB-MJHT".into()),
                device_name: "carmine-laptop".into(),
                fingerprint: "3sQ2kX9vY0a1Lr8TqZ4mN7pC6bW5eD2fH1gJ0kA9sE".into(),
            }));
            // Long enough for a test to cancel it first.
            for _ in 0..40 {
                std::thread::sleep(Duration::from_millis(100));
                if stop.load(Ordering::SeqCst) {
                    return;
                }
            }
            demo.change(|s| {
                s.status["server"] = json!("https://chatwithwork.com");
                s.status["device_id"] = json!("42");
                s.status["paired"] = json!(true);
                s.status["connection"] = json!({ "connection": "connected", "since": ago(0) });
            });
            report(PairEvent::Paired { daemon_error: None });
        });
        let demo = Arc::clone(&self.0);
        Box::new(move || {
            cancelled.store(true, Ordering::SeqCst);
            demo.change(|s| s.requests.push(json!({ "cmd": "pair_cancel" })));
        })
    }

    fn logout(&self) -> Result<(), String> {
        self.0.change(|s| {
            s.requests.push(json!({ "cmd": "logout" }));
            s.status["server"] = Value::Null;
            s.status["device_id"] = Value::Null;
            s.status["paired"] = json!(false);
            s.status["connection"] = json!({ "connection": "not_paired", "since": ago(0) });
            Demo::event(s, "logged_out");
        });
        Ok(())
    }
}
