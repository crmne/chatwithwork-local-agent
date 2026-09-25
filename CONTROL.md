# Chat with Work Local Agent: control API, version 1

The daemon (`cww daemon run`) listens on a local control channel. The `cww` command line, the `cww` terminal UI and the settings app all use it to read the daemon's state, pause it, manage shared folders and follow the audit log. This document is the contract for those clients. The tunnel to Chat with Work is a different protocol; see [PROTOCOL.md](PROTOCOL.md).

Nothing here is reachable from the network or from other users on the machine.

## Contents

1. [Where it listens](#1-where-it-listens)
2. [Who may connect](#2-who-may-connect)
3. [Framing](#3-framing)
4. [Requests](#4-requests)
5. [Events](#5-events)
6. [When the daemon isn't running](#6-when-the-daemon-isnt-running)
7. [Versioning](#7-versioning)
8. [Using it from Rust](#8-using-it-from-rust)

## 1. Where it listens

| Platform | Endpoint |
|---|---|
| Linux | Unix socket `$XDG_RUNTIME_DIR/cww/cww.sock`, or `~/.local/state/cww/cww.sock` without `XDG_RUNTIME_DIR` |
| macOS | Unix socket `~/.local/state/cww/cww.sock` (macOS has no `XDG_RUNTIME_DIR`) |
| Windows | Named pipe `\\.\pipe\cww-<hash>`, where `<hash>` is the first 8 bytes, in hex, of SHA-256 of the lowercased runtime directory (`%LOCALAPPDATA%\cww\run`) |

With `CWW_HOME=/dir`, the socket is `/dir/run/cww.sock` (on Windows, the pipe hash is taken from `\dir\run`). When a Unix socket path would be longer than 100 bytes, the daemon uses `/tmp/cww-<uid>-<hash>/cww.sock` (or the same name under a short `XDG_RUNTIME_DIR`) instead.

Clients shouldn't hard-code any of this. `cww status --json` doesn't print the endpoint, but the Rust function `cww::paths::Paths::from_env()?.socket_path()` computes it exactly as the daemon does, and other languages can follow the table above.

## 2. Who may connect

- **Linux and macOS:** the socket is mode `0600` inside a `0700` directory. The daemon also checks the peer's UID (`SO_PEERCRED` / `getpeereid`) and drops connections from any other user.
- **Windows:** the pipe is created owned by the current user and with a protected DACL that grants access to that SID only (`O:<SID>D:P(A;;GA;;;<SID>)`), and remote clients are rejected. The daemon identifies each client by impersonating it at identification level (`ImpersonateNamedPipeClient`) and refuses other users. Clients SHOULD check that the pipe is owned by their own SID (`GetSecurityInfo`, `OWNER_SECURITY_INFORMATION`) before sending anything, and SHOULD open the pipe with `SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION` so the server can't impersonate them. The Rust client does both.
- Only one daemon can listen at a time. A second one fails with "another cww daemon is already running".

There is no authentication beyond this: any process running as the user can drive the daemon, just as it could edit `config.toml`.

## 3. Framing

- UTF-8 JSON, one object per line, each line ending in `\n`. A request line is at most 16 KiB.
- The client sends a request object with a `cmd` field. The daemon answers with exactly one response line, in order. A connection can carry any number of requests.
- A successful response has `"ok": true` plus the fields listed below. A failure is `{"ok": false, "error": "<message for a person>"}`.
- A malformed request gets `{"ok": false, "error": "bad request: ..."}` and the connection stays open.
- The daemon closes a connection after 5 minutes without a request (subscriptions excepted).
- `subscribe` is different: after its response, the connection only carries events (section 5) until either side closes it.

```
→ {"cmd":"status"}
← {"ok":true,"protocol":1,"version":"0.1.0","connection":{...},"roots":[...],...}
→ {"cmd":"pause"}
← {"ok":true,"paused":true}
```

## 4. Requests

### `hello`

```json
{"cmd": "hello"}
```

```json
{"ok": true, "protocol": 1, "version": "0.1.0", "platform": "macos", "pid": 4242}
```

`platform` is `linux`, `macos` or `windows`. Clients should check `protocol` first (section 7).

### `status`

```json
{"cmd": "status"}
```

```json
{
  "ok": true,
  "protocol": 1,
  "version": "0.1.0",
  "platform": "linux",
  "pid": 4242,
  "paired": true,
  "server": "https://chatwithwork.com",
  "device_id": "42",
  "proxy": { "url": "http://alice:***@proxy.example:3128", "source": "config" },
  "paused": false,
  "sandbox": { "kind": "landlock", "state": "enforced" },
  "connection": {
    "connection": "connected",
    "since": "2026-09-25T08:14:03Z",
    "last_error": "connection reset by peer"
  },
  "config_file": "/home/carmine/.config/cww/config.toml",
  "audit_file": "/home/carmine/.local/state/cww/audit.jsonl",
  "roots": [
    {
      "id": "work-docs",
      "label": "Work docs",
      "available": true,
      "index": "ready",
      "indexed_files": 1532,
      "local_path": "/home/carmine/Documents/Work"
    }
  ]
}
```

- `connection.connection` is `not_paired`, `connecting`, `connected`, `offline` or `revoked`. `since` is when it last changed. `last_error` is the most recent problem and may be absent.
- `server` and `device_id` are `null` until the computer is paired.
- `proxy` is the HTTP proxy the daemon reaches the server through, or `null` for a direct connection (or when the setting is invalid; `connection.last_error` says why). `url` never contains the password, which is replaced by `***`. `source` is `config` or the environment variable it came from, such as `HTTPS_PROXY`.
- `sandbox.kind` is `landlock`, `seatbelt` or `none`; `sandbox.state` is `enforced`, `partial` (an older kernel enforces some of the rules), `off` or `unavailable`, with a `detail` when it isn't enforced.
- `roots[].index` is `pending`, `indexing`, `ready`, `error` or `disabled`. During the first pass, `indexed_files` counts files seen so far. `available` is false when the folder is missing, for example on an unmounted drive.
- `local_path` is the absolute path. It is shown to the local user only and never sent to Chat with Work.

### `pause` and `resume`

```json
{"cmd": "pause"}
```

```json
{"ok": true, "paused": true}
```

While paused, every tool call from Chat with Work is refused with the `paused` error. The state is saved in `config.toml` and survives restarts. `resume` answers `{"ok": true, "paused": false}`.

### `reload`

```json
{"cmd": "reload"}
```

Re-reads `config.toml`: roots, deny list, limits and pairing. Answers `{"ok": true}`. Use it after editing the file by hand; the other requests reload on their own.

### `roots_add`

```json
{"cmd": "roots_add", "path": "/home/carmine/Documents", "label": "Documents"}
```

| Field | Type | Notes |
|---|---|---|
| `path` | string, required | An absolute path to an existing folder. It is resolved to its canonical form. |
| `label` | string | Shown to Chat with Work. Defaults to the folder name. 80 characters at most. |
| `follow_symlinks` | bool | Default false. Follow links that stay inside the folder. |
| `i_know` | bool | Default false. Allow `/`, the home folder, whole drives and system folders. Clients should only set it after asking the user. |

```json
{
  "ok": true,
  "root": { "id": "documents", "label": "Documents", "path": "/home/carmine/Documents", "follow_symlinks": false }
}
```

The rules are the same as `cww roots add`. Errors include: the folder doesn't exist, is already shared, matches the deny list, or is too broad (the message says why and mentions `--i-know`; a GUI should phrase its own confirmation). The daemon saves `config.toml`, starts indexing the folder, and writes a `root_added` audit entry.

### `roots_remove`

```json
{"cmd": "roots_remove", "root": "documents"}
```

`root` is an ID, a path, or a label that matches exactly one root. Answers with the removed `root`. Its documents are dropped from the index, and a `root_removed` audit entry is written.

### `suggested_roots`

```json
{"cmd": "suggested_roots"}
```

```json
{
  "ok": true,
  "suggestions": [
    { "path": "/Users/carmine/Documents", "label": "Documents", "exists": true, "shared": false }
  ]
}
```

What to offer on first run: the user's Documents folder (the XDG user directory on Linux, `~/Documents` on macOS, the Documents known folder on Windows, which may be redirected to OneDrive). **Nothing is shared by this request.** Clients must ask the user and send `roots_add` only after an explicit yes.

### `audit_tail`

```json
{"cmd": "audit_tail", "lines": 50}
```

```json
{
  "ok": true,
  "entries": [
    { "ts": "2026-09-25T08:14:03Z", "event": "tool", "tool": "search", "query": "budget", "decision": "allowed", "results": 3, "bytes": 1840, "chat_id": "1234", "request_id": "7" }
  ]
}
```

The last `lines` entries (default 50, at most 1000), oldest first. See [Audit entries](#audit-entries).

### `shutdown`

```json
{"cmd": "shutdown"}
```

Answers `{"ok": true, "stopping": true}`, then the daemon exits cleanly. systemd and launchd restart the daemon only after a failure, and the Windows Scheduled Task only at the next logon, so this really stops it. `cww daemon stop` sends it.

### `subscribe`

```json
{"cmd": "subscribe", "topics": ["audit", "status"]}
```

`topics` defaults to both. The response is `{"ok": true, "topics": [...]}`, and from then on the connection carries events. Send nothing else on it.

## 5. Events

Each event is one line with an `event` field.

**`status`**: the whole `status` result (without `ok`), sent once right after subscribing and again whenever anything in it changes: connection state, pause, roots, index progress. Changes are coalesced over about 150 ms, so a burst produces one event.

```json
{"event": "status", "status": { "protocol": 1, "paused": false, "connection": {...}, "roots": [...], ... }}
```

**`audit`**: one audit entry, as it is written to the log.

```json
{"event": "audit", "entry": { "ts": "2026-09-25T08:14:05Z", "event": "tool", "tool": "read", "path": "work-docs:plans/q3.md", "decision": "denied", "code": "denied", "reason": "this path is on the deny list (.env*)" }}
```

**`lagged`**: the client read too slowly and `missed` events were dropped. Ask for `status` or `audit_tail` on another connection to catch up.

```json
{"event": "lagged", "missed": 12}
```

The daemon never polls to produce events, and a client that waits on a subscription uses no CPU while nothing happens.

### Audit entries

| Field | Present | Meaning |
|---|---|---|
| `ts` | always | RFC 3339, UTC. |
| `event` | always | `tool` for a tool call. Daemon events: `started`, `stopped`, `connected`, `disconnected`, `revoked`, `paused`, `resumed`, `reloaded`, `root_added`, `root_removed`, `shutdown_requested`, `restarting` (to widen the sandbox for a new folder). |
| `tool` | tool calls | `roots`, `search`, `list`, `read`, or an unknown name the server tried. |
| `decision` | tool calls | `allowed`, `denied` or `error`. |
| `path`, `query` | when given | The tool path (`root:relative`) or search query, truncated. |
| `code`, `reason` | refusals and errors | A PROTOCOL.md error code and its message. |
| `results`, `bytes` | answered calls | Hits or entries returned, and bytes sent to the server. |
| `duration_ms` | tool calls | How long the daemon took to answer, in milliseconds (one decimal). |
| `chat_id`, `request_id` | tool calls | The Chat with Work chat and JSON-RPC request. |
| `detail` | daemon events | A short description, such as the server URL on `connected`. |

Unknown fields may appear later; ignore them.

## 6. When the daemon isn't running

Connecting fails: `ENOENT` or `ECONNREFUSED` on Unix, "file not found" for the pipe on Windows. Clients should then:

- Show that the daemon is stopped, and offer to start it: `cww daemon install` registers and starts the per-user service (systemd, launchd or a Scheduled Task).
- Read `config.toml` directly for roots and pairing if they need them. `cww roots add|remove`, `cww pause|resume` and `cww login|logout` all work without a daemon; they edit the file and the daemon picks it up when it starts.
- Read the audit log file directly for history.

## 7. Versioning

`protocol` is 1. It changes only when an existing request, response field or event changes meaning or disappears. New requests, fields and events are added without a bump, so clients must ignore what they don't know, and should check `hello` before relying on a request added later. An unknown `cmd` gets `{"ok": false, "error": "bad request: unknown variant ..."}`.

## 8. Using it from Rust

The `cww` crate exposes a blocking client:

```rust
use cww::control::{Client, ControlRequest, Topic};
use cww::paths::Paths;

let socket = Paths::from_env()?.socket_path();
let Some(mut client) = Client::connect(&socket)? else {
    println!("the daemon is not running");
    return Ok(());
};
let status = client.call(&ControlRequest::Status)?;
println!("{}", status["connection"]["connection"]);

// A second connection for live events.
let events = Client::connect(&socket)?.unwrap().subscribe(&[Topic::Audit, Topic::Status])?;
for event in events {
    println!("{}", event?);
}
```

From a shell, for debugging:

```sh
printf '{"cmd":"status"}\n' | nc -U "$XDG_RUNTIME_DIR/cww/cww.sock"
```
