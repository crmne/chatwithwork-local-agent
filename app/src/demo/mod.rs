//! `--demo`: a stand-in daemon with sample folders, activity, chats and a
//! paired account, speaking the real control protocol on a private socket.
//! For screenshots, for trying the app without pairing a computer, and for
//! the window's tests, which also read the requests it received. Built only
//! with the `demo` feature, and in tests.

mod assets;
mod chats;

/// Northwind's projects, as the demo's list sends them.
#[cfg(test)]
pub fn projects() -> Vec<Value> {
    chats::projects()
}

/// One more project, invite-only.
#[cfg(test)]
pub fn project(id: u64, name: &str, icon: &str) -> Value {
    chats::project(id, name, icon, false, false)
}

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
    /// Every change to the list so far, as `chats` subscribers get them.
    list_events: Vec<Value>,
    /// The chats as the list's subscribers last heard of them.
    listed: Vec<Value>,
    /// Refuse the `chats` topic, as a daemon from before live lists does.
    old_daemon: bool,
    /// The organization's credits left, of 5,000.
    credits_left: u64,
    /// The projects the person is on.
    projects: Vec<Value>,
    /// Answers don't stream on their own (tests hold them still).
    held: bool,
    /// Chats whose answer is streaming now.
    streaming: HashSet<u64>,
    /// Chats asked to stop.
    cancelled: HashSet<u64>,
    next_id: u64,
    /// Files uploaded for questions, by `signed_id`.
    uploads: Vec<Value>,
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

    /// Refuse the `chats` topic, as a daemon from before live lists does.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn act_old(&self) {
        self.demo.change(|s| s.old_daemon = true);
    }

    /// A chat started on the web: it shows up being answered.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn start_elsewhere(&self, number: u64, title: &str, question: &str) {
        self.demo.change(|s| {
            let mut summary = chats::summary(number, title, "processing", ago(0));
            summary["url"] = json!(format!("https://chatwithwork.com/northwind/chats/{number}"));
            s.chats.insert(
                0,
                chats::Chat::new(
                    summary,
                    vec![json!({ "kind": "user", "id": number * 10 + 1, "content": question })],
                ),
            );
        });
        self.demo.sync_list();
    }

    /// Change a chat as someone elsewhere would: a new title, say.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn edit_elsewhere(&self, number: u64, edit: impl FnOnce(&mut Value)) {
        self.demo.change(|s| {
            if let Some(chat) = s
                .chats
                .iter_mut()
                .find(|c| c.summary["number"] == json!(number))
            {
                edit(&mut chat.summary);
                chat.summary["updated_at"] = json!(ago(0));
            }
        });
        self.demo.sync_list();
    }

    /// A chat deleted on the web, or moved out of sight.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn delete_elsewhere(&self, number: u64) {
        self.demo
            .change(|s| s.chats.retain(|c| c.summary["number"] != json!(number)));
        self.demo.sync_list();
    }

    /// The credits move elsewhere: the list's header says so.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn spend(&self, left: u64) {
        self.demo.change(|s| {
            s.credits_left = left;
            s.list_events.push(chats::account_update(left));
        });
    }

    /// The projects change elsewhere: joined, renamed, left.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn set_projects(&self, projects: Vec<Value>) {
        self.demo.change(|s| {
            s.projects = projects.clone();
            s.list_events
                .push(json!({ "event": "projects", "projects": projects }));
        });
    }

    /// What happens elsewhere while `--demo` runs: a chat started on the
    /// web shows up, is answered and renamed, and the credits run low.
    #[cfg_attr(test, allow(dead_code))]
    pub fn script_live_events(&self) {
        let me = Server {
            paths: self.paths.clone(),
            demo: Arc::clone(&self.demo),
        };
        std::thread::spawn(move || {
            let pause = |s| std::thread::sleep(Duration::from_secs(s));
            pause(20);
            me.start_elsewhere(
                30,
                "Draft the Q4 hiring plan",
                "Draft the Q4 hiring plan from the headcount sheet.",
            );
            pause(6);
            me.finish(30, 302, "Here's a first draft of the Q4 hiring plan.");
            pause(4);
            me.edit_elsewhere(30, |c| c["title"] = json!("Q4 hiring plan"));
            pause(10);
            me.spend(410);
        });
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
        listed: chats::sample().into_iter().map(|c| c.summary).collect(),
        chats: chats::sample(),
        chat_events: Vec::new(),
        list_events: Vec::new(),
        old_daemon: false,
        credits_left: 1250,
        projects: chats::projects(),
        held: false,
        streaming: HashSet::new(),
        cancelled: HashSet::new(),
        next_id: 1000,
        uploads: Vec::new(),
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
        listed: chats::sample().into_iter().map(|c| c.summary).collect(),
        chats: chats::sample(),
        chat_events: Vec::new(),
        list_events: Vec::new(),
        old_daemon: false,
        credits_left: 1250,
        projects: chats::projects(),
        held: false,
        streaming: HashSet::new(),
        cancelled: HashSet::new(),
        next_id: 1000,
        uploads: Vec::new(),
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
            "chats" | "chat" | "chat_send" | "chat_cancel" | "chat_access" | "models"
            | "chat_upload" | "chat_approve" | "chat_deny" | "chat_answer" | "chat_decline"
            | "chat_retry" | "chat_branch" | "chat_rename" | "chat_delete" | "chat_share"
            | "chat_unshare" => {
                let answer = self.chat_request(cmd, request);
                self.sync_list();
                answer
            }
            "asset" => assets::serve(request["path"].as_str().unwrap_or_default()),
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
            if request["cmd"] == "subscribe"
                && request["topics"]
                    .as_array()
                    .is_some_and(|t| t.iter().any(|t| t == "chats"))
            {
                self.state
                    .lock()
                    .expect("demo lock")
                    .requests
                    .push(request.clone());
                return self.follow_list(reader.get_mut());
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
                let mut answer = chats::list_header(state.credits_left);
                answer["ok"] = json!(true);
                answer["chats"] = json!(list);
                answer["projects"] = json!(state.projects);
                answer
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
                        "approvals": chat.approvals,
                        "questions": chat.questions,
                    }),
                    None => json!({ "ok": false, "code": "not_found", "error": "No such chat" }),
                }
            }
            "models" => {
                let mut models = chats::models();
                models["ok"] = json!(true);
                models
            }
            "chat_upload" => self.upload(request),
            "chat_approve" | "chat_deny" | "chat_answer" | "chat_decline" => {
                let Some(number) = number else {
                    return json!({ "ok": false, "code": "bad_request", "error": "No chat" });
                };
                self.decide(cmd, number, request)
            }
            "chat_retry" | "chat_branch" | "chat_rename" | "chat_delete" | "chat_share"
            | "chat_unshare" => {
                let Some(number) = number else {
                    return json!({ "ok": false, "code": "bad_request", "error": "No chat" });
                };
                self.act(cmd, number, request)
            }
            "chat_send" => {
                let text = request["text"].as_str().unwrap_or_default().to_string();
                if let Some(model) = request["model"].as_str() {
                    let models = chats::models();
                    let found = models["models"]
                        .as_array()
                        .and_then(|m| m.iter().find(|m| model_id(m) == model).cloned());
                    match found {
                        Some(m) if m["selectable"] == json!(false) => {
                            return json!({ "ok": false, "code": "model_unavailable", "error": m["reason"] });
                        }
                        None => {
                            return json!({ "ok": false, "code": "model_unavailable",
                                           "error": "That model isn't available. Pick another one." });
                        }
                        Some(_) => {}
                    }
                }
                let model = request["model"].as_str().and_then(|id| {
                    chats::models()["models"]
                        .as_array()
                        .and_then(|m| m.iter().find(|m| model_id(m) == id).cloned())
                        .map(|m| json!({ "id": m["id"], "name": m["name"], "logo": m["logo"] }))
                });
                let attachments: Vec<Value> = {
                    let state = self.state.lock().expect("demo lock");
                    request["attachments"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|id| state.uploads.iter().find(|u| u["signed_id"] == *id))
                        .map(|u| json!({ "filename": u["filename"], "byte_size": u["byte_size"], "content_type": u["content_type"], "icon": u["icon"] }))
                        .collect()
                };
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
                            let mut summary =
                                chats::summary(n, &chats::title_for(&text), "idle", ago(0));
                            // Started in a project: it's the project's.
                            if let Some(project) = state.projects.iter().find(|p| {
                                request["project"].is_u64() && p["id"] == request["project"]
                            }) {
                                summary["project"] =
                                    json!({ "id": project["id"], "name": project["name"] });
                                summary["can"]["share"] = json!(false);
                            }
                            state.chats.insert(0, chats::Chat::new(summary, Vec::new()));
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
                    let mut entry = json!({ "kind": "user", "id": id, "content": text });
                    if !attachments.is_empty() {
                        entry["attachments"] = json!(attachments);
                    }
                    chat.entries.push(entry);
                    if let Some(model) = model {
                        chat.summary["model"] = model;
                    }
                    chat.approvals.clear();
                    chat.questions.clear();
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

    /// `chat_upload`: what the server takes, refused with its sentences.
    fn upload(&self, request: &Value) -> Value {
        let name = request["filename"].as_str().unwrap_or_default().to_string();
        if name.is_empty() {
            return json!({ "ok": false, "code": "attachment_refused", "error": "Choose a file to attach" });
        }
        let lower = name.to_lowercase();
        if [".svg", ".svgz", ".swf"].iter().any(|e| lower.ends_with(e)) {
            return json!({ "ok": false, "code": "attachment_refused", "error": format!("{name} has a blocked file type") });
        }
        if lower.ends_with(".exe") {
            return json!({ "ok": false, "code": "attachment_refused", "error": format!("{name} is not a supported file type") });
        }
        let size = request["data"].as_str().map_or(0, |d| d.len() * 3 / 4);
        let mut state = self.state.lock().expect("demo lock");
        let n = state.uploads.len() + 1;
        let uploaded = json!({
            "signed_id": format!("demo-upload-{n}"),
            "filename": name,
            "byte_size": size,
            "content_type": request["content_type"].as_str().unwrap_or("application/octet-stream"),
            "icon": assets::file_icon(request["content_type"].as_str().unwrap_or_default()),
        });
        state.uploads.push(uploaded.clone());
        let mut answer = uploaded;
        answer["ok"] = json!(true);
        answer
    }

    /// Approve, deny, answer or decline what chat `number` waits for: the
    /// step settles and the answer carries on.
    fn decide(self: &Arc<Self>, cmd: &str, number: u64, request: &Value) -> Value {
        let tool_call = request["tool_call"]
            .as_str()
            .and_then(|t| t.parse::<u64>().ok());
        let reply = {
            let mut state = self.state.lock().expect("demo lock");
            let Some(chat) = state
                .chats
                .iter_mut()
                .find(|c| c.summary["number"] == json!(number))
            else {
                return json!({ "ok": false, "code": "not_found", "error": "No such chat" });
            };
            let approval = matches!(cmd, "chat_approve" | "chat_deny");
            let list = if approval {
                &mut chat.approvals
            } else {
                &mut chat.questions
            };
            let Some(at) = list.iter().position(|a| a["id"].as_u64() == tool_call) else {
                return json!({ "ok": false, "code": "not_found", "error": "Nothing waits for that here." });
            };
            if cmd == "chat_answer" {
                let question = &list[at];
                let input = &request["input"];
                for field in question["fields"].as_array().into_iter().flatten() {
                    let value = &input[field["name"].as_str().unwrap_or_default()];
                    let missing = value.is_null() || value == &json!("");
                    if field["required"] == json!(true) && missing {
                        let title = field["title"].as_str().unwrap_or_default();
                        return json!({ "ok": false, "code": "invalid", "error": format!("{title} is needed.") });
                    }
                }
            }
            list.remove(at);
            for entry in &mut chat.entries {
                for step in entry["steps"].as_array_mut().into_iter().flatten() {
                    if step["waiting"] == json!(true) {
                        step["waiting"] = json!(false);
                    }
                }
            }
            chat.summary["state"] = json!("processing");
            chat.summary["updated_at"] = json!(ago(0));
            match cmd {
                "chat_approve" => "Posted to #launch. The team can see it now.".to_string(),
                "chat_deny" => match request["reason"].as_str() {
                    Some(reason) => format!("I didn't post it. You said: {reason}"),
                    None => "I didn't post it.".to_string(),
                },
                "chat_decline" => "Understood, I'll leave it there.".to_string(),
                _ => "Thanks, carrying on with that.".to_string(),
            }
        };
        let summary = self.summary_of(number);
        self.changed(number);
        let id = {
            let mut state = self.state.lock().expect("demo lock");
            state.next_id += 10;
            state.next_id
        };
        if !self.state.lock().expect("demo lock").held {
            self.stream(
                number,
                vec![(
                    Duration::from_millis(600),
                    chats::Step::Finish { id, content: reply },
                )],
            );
        }
        json!({ "ok": true, "chat": summary })
    }

    fn summary_of(&self, number: u64) -> Value {
        let state = self.state.lock().expect("demo lock");
        state
            .chats
            .iter()
            .find(|c| c.summary["number"] == json!(number))
            .map(|c| c.summary.clone())
            .unwrap_or(Value::Null)
    }

    /// Retry, branch, rename, delete, share or stop sharing chat `number`.
    fn act(self: &Arc<Self>, cmd: &str, number: u64, request: &Value) -> Value {
        let message = request["message"]
            .as_str()
            .and_then(|m| m.parse::<u64>().ok());
        let mut state = self.state.lock().expect("demo lock");
        let Some(at) = state
            .chats
            .iter()
            .position(|c| c.summary["number"] == json!(number))
        else {
            return json!({ "ok": false, "code": "not_found", "error": "No such chat" });
        };
        let can = |what: &str| state.chats[at].summary["can"][what] == json!(true);
        let forbidden = |what: &str| {
            json!({ "ok": false, "code": "forbidden",
            "error": format!("Only the person who started a chat can {what} it.") })
        };
        match cmd {
            "chat_retry" => {
                if !can("retry") {
                    return json!({ "ok": false, "code": "forbidden",
                        "error": "Only the person who asked can retry this. Ask again to use your own sources." });
                }
                let chat = &mut state.chats[at];
                if chat.summary["state"] == "processing" {
                    return json!({ "ok": false, "code": "chat_busy", "error": "Hold on, I'm still working on the current reply." });
                }
                // Everything after the question goes.
                let keep = match message
                    .and_then(|m| chat.entries.iter().position(|e| e["id"] == json!(m)))
                {
                    Some(i) if chat.entries[i]["kind"] == "user" => i + 1,
                    Some(i) => chat.entries[..i]
                        .iter()
                        .rposition(|e| e["kind"] == "user")
                        .map_or(0, |u| u + 1),
                    None => chat
                        .entries
                        .iter()
                        .rposition(|e| e["kind"] == "user")
                        .map_or(0, |u| u + 1),
                };
                chat.entries.truncate(keep);
                chat.approvals.clear();
                chat.questions.clear();
                chat.summary["state"] = json!("processing");
                let summary = chat.summary.clone();
                let held = state.held;
                state.next_id += 10;
                let id = state.next_id;
                drop(state);
                self.changed(number);
                if !held {
                    self.stream(number, chats::canned_stream(id + 1, id + 2));
                }
                json!({ "ok": true, "chat": summary })
            }
            "chat_branch" => {
                let chat = &state.chats[at];
                let upto = message
                    .and_then(|m| chat.entries.iter().position(|e| e["id"] == json!(m)))
                    .map_or(chat.entries.len(), |i| i + 1);
                let entries = chat.entries[..upto].to_vec();
                let title = format!(
                    "Branch of {}",
                    chat.summary["title"].as_str().unwrap_or_default()
                );
                let n = state
                    .chats
                    .iter()
                    .filter_map(|c| c.summary["number"].as_u64())
                    .max()
                    .unwrap_or(0)
                    + 1;
                let summary = chats::summary(n, &title, "idle", ago(0));
                state
                    .chats
                    .insert(0, chats::Chat::new(summary.clone(), entries));
                drop(state);
                json!({ "ok": true, "chat": summary })
            }
            "chat_rename" => {
                if !can("rename") {
                    return forbidden("change");
                }
                let title = request["title"]
                    .as_str()
                    .unwrap_or_default()
                    .trim()
                    .to_string();
                if title.is_empty() {
                    return json!({ "ok": false, "code": "invalid", "error": "Give the chat a title" });
                }
                state.chats[at].summary["title"] = json!(title);
                let summary = state.chats[at].summary.clone();
                drop(state);
                self.changed(number);
                json!({ "ok": true, "chat": summary })
            }
            "chat_delete" => {
                if !can("delete") {
                    return forbidden("change");
                }
                state.chats.remove(at);
                json!({ "ok": true })
            }
            "chat_share" => {
                if !can("share") {
                    return forbidden("share");
                }
                let link = json!({ "url": chats::SHARE_URL, "expires_at": chats::expires() });
                state.chats[at].summary["share"] = link.clone();
                json!({ "ok": true, "url": link["url"], "expires_at": link["expires_at"], "chat": state.chats[at].summary })
            }
            _ => {
                state.chats[at].summary["share"] = Value::Null;
                json!({ "ok": true, "chat": state.chats[at].summary })
            }
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
        self.sync_list();
    }

    /// Tell the list's subscribers about every chat that started, changed
    /// or went since they last heard, as the server's list channel does.
    fn sync_list(&self) {
        self.change(|s| {
            let mut events = Vec::new();
            for chat in &s.chats {
                let number = &chat.summary["number"];
                let before = s.listed.iter().find(|c| &c["number"] == number);
                if before != Some(&chat.summary) {
                    events.push(json!({ "event": "chat", "chat": chat.summary }));
                }
            }
            for gone in s.listed.iter().filter(|c| {
                !s.chats
                    .iter()
                    .any(|chat| chat.summary["number"] == c["number"])
            }) {
                events.push(json!({ "event": "removed", "number": gone["number"] }));
            }
            s.listed = s.chats.iter().map(|c| c.summary.clone()).collect();
            s.list_events.extend(events);
        });
    }

    /// A `chats` subscription: "watching", then every change to the list
    /// until the client goes away, as the daemon relays the server's.
    fn follow_list(self: &Arc<Self>, stream: &mut impl Write) {
        if self.state.lock().expect("demo lock").old_daemon {
            let _ = send(
                stream,
                &json!({ "ok": false, "error": "bad request: unknown variant `chats`, expected `status` or `audit`" }),
            );
            return;
        }
        if send(stream, &json!({ "ok": true, "topics": ["chats"] })).is_err() {
            return;
        }
        let mut seen = self.state.lock().expect("demo lock").list_events.len();
        let watching = json!({ "event": "chats", "update": { "event": "watching" } });
        if send(stream, &watching).is_err() {
            return;
        }
        loop {
            let events = {
                let mut state = self.state.lock().expect("demo lock");
                while state.list_events.len() == seen {
                    state = self.changed.wait(state).expect("demo lock");
                }
                let events = state.list_events[seen..].to_vec();
                seen = state.list_events.len();
                events
            };
            for update in events {
                if send(stream, &json!({ "event": "chats", "update": update })).is_err() {
                    return;
                }
            }
        }
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
                            "logos": [assets::logo("drive"), assets::logo("slack")],
                            "steps": [
                                { "summary": "Searched Drive for “budget owner”", "pending": false, "files": ["Budget 2026.xlsx"], "logo": assets::logo("drive") },
                                { "summary": "Reading Budget 2026.xlsx…", "pending": true, "files": [], "logo": assets::logo("drive") },
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
                self.sync_list();
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

/// A model's ID as the control API passes it: a string.
fn model_id(model: &Value) -> String {
    match &model["id"] {
        Value::String(id) => id.clone(),
        other => other.to_string(),
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
