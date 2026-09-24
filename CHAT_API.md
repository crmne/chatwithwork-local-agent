# Chat API for the terminal: a proposal

Status: **proposal.** Nothing here exists yet. Chat with Work only has its web UI, so the chat pane in `cww tui` says chat isn't available and points at the browser. This is the smallest HTTP API that would let it chat for real. The TUI is written against it already: `src/tui/chat.rs` defines a `ChatBackend` trait whose methods map onto the endpoints below. `list_chats` and `messages` are the two `GET`s, `send` is a `POST` followed by the event stream until the reply ends, and `cancel` is the cancellation.

## Contents

1. [Authentication](#1-authentication)
2. [Conventions](#2-conventions)
3. [Endpoints](#3-endpoints)
4. [The reply stream](#4-the-reply-stream)
5. [Tool activity from this computer](#5-tool-activity-from-this-computer)
6. [Errors](#6-errors)
7. [What's left out](#7-whats-left-out)

## 1. Authentication

**Recommendation: a separate grant with its own scope, `chats`, bound to the same device key.**

Today the device key and refresh token only get `local_agent:serve` tokens, and PROTOCOL.md section 5 says that scope must not open any other API. That's what makes pairing cheap to approve: a stolen key can answer tool calls for folders you chose, nothing more. Letting the same grant read your chats would change that quietly. Chats hold quotes from Drive, Slack, Dropbox and everything else you've connected, so a key that can read them is worth far more to steal.

So chat access is asked for on its own, when you first use chat in the TUI (or with `cww login --chat`):

- **Device flow, as in PROTOCOL.md section 4,** with `scope=chats` and the existing `public_key`. The approval page says "Chat from the terminal on carmine-mbp" and shows the same key fingerprint, so it reads as a second permission for a computer you already know, not a new computer.
- **Its own refresh token,** stored in the keychain next to the daemon's (key `chats_refresh_token`). The TUI holds it and makes the HTTP calls itself. The daemon never gets it, and its network surface stays one outbound WebSocket.
- **DPoP-bound access tokens** (10 minutes, `scope=chats`, `cnf.jkt` = the device key's thumbprint), refreshed at `POST /local_agent/token` exactly as in PROTOCOL.md section 5. Every API request carries `Authorization: DPoP <token>` and a `DPoP` proof with that request's `htm`, `htu` and `ath`, so a leaked token is useless without the key.
- **Revocable on its own** under Settings ▸ Computers ("Chat from the terminal: revoke"). Revoking the computer revokes both grants.

A single grant with both scopes would be simpler, but it breaks the promise that pairing only lets the server call four read-only tools. A separate token without the device key (a pasted personal access token) would skip the key-in-keychain and DPoP protections the daemon already has. The separate `chats` grant keeps both.

## 2. Conventions

- Base path `/api/v1`. JSON in and out, UTF-8, `Content-Type: application/json`.
- IDs are strings. Times are RFC 3339 in UTC.
- Lists are newest first and paginate with an opaque cursor: pass `cursor` from the previous page's `next_cursor`; `null` means there's no more. `limit` defaults to 30, at most 100.
- Unknown fields may appear later. Clients ignore them.
- Message content is Markdown, as the web UI renders it. Citations come as a separate list.

## 3. Endpoints

| Method and path | Does |
|---|---|
| `GET /api/v1/chats` | List chats |
| `POST /api/v1/chats` | Start a chat with a first message |
| `GET /api/v1/chats/:id/messages` | A chat's messages |
| `POST /api/v1/chats/:id/messages` | Send a message in a chat |
| `GET /api/v1/chats/:id/events` | The reply as it's written (SSE, section 4) |
| `POST /api/v1/chats/:id/cancellation` | Stop the reply being written |

### List chats

```
GET /api/v1/chats?limit=30&cursor=…
```

```json
{
  "chats": [
    {
      "id": "101",
      "title": "Q3 budget",
      "created_at": "2026-09-24T16:02:11Z",
      "updated_at": "2026-09-25T08:14:03Z",
      "processing": false,
      "url": "https://chatwithwork.com/chats/101"
    }
  ],
  "next_cursor": "eyJ1cGRhdGVkX2F0Ijo…"
}
```

`title` is `null` until `SummarizeChatTitleJob` names the chat. `processing` is true while a reply is being written, so a client can attach to its stream.

### Start a chat

```
POST /api/v1/chats
{"message": {"content": "What did we budget for Q3?"}, "model": "gemini-3.1-flash-lite"}
```

`model` is optional and defaults to the user's default model. Answers `201 Created`:

```json
{
  "chat": { "id": "102", "title": null, "created_at": "…", "updated_at": "…", "processing": true, "url": "…" },
  "message": { "id": "5001", "role": "user", "content": "What did we budget for Q3?", "created_at": "…" },
  "events_url": "/api/v1/chats/102/events"
}
```

### A chat's messages

```
GET /api/v1/chats/:id/messages?limit=30&cursor=…
```

```json
{
  "messages": [
    {
      "id": "5002",
      "role": "assistant",
      "content": "The Q3 budget is €40k [1].",
      "created_at": "2026-09-25T08:14:09Z",
      "model": "gemini-3.1-flash-lite",
      "tools": [
        {
          "id": "tc_77",
          "service": "local",
          "tool": "search",
          "summary": "search \"q3 budget\"",
          "status": "ok",
          "device": { "id": "42", "name": "carmine-mbp" },
          "request_id": "7"
        }
      ],
      "citations": [
        { "index": 1, "title": "plans/q3.md", "service": "local", "ref": "work-docs:plans/q3.md" }
      ]
    },
    { "id": "5001", "role": "user", "content": "What did we budget for Q3?", "created_at": "…", "tools": [], "citations": [] }
  ],
  "next_cursor": null
}
```

Only `user` and `assistant` messages appear. Tool calls and results are folded into the answer they led to, as `tools`, the same way the web UI folds them into one activity line. `status` is `running`, `ok`, `empty`, `denied` or `error`.

### Send a message

```
POST /api/v1/chats/:id/messages
{"content": "And Q4?", "model": "gemini-3.1-flash-lite"}
```

Answers `202 Accepted` with `{"message": {...}, "events_url": "..."}`, like starting a chat. The reply is written in the background (`WorkAssistantJob`); read it from the events stream. If a reply is already being written, the answer is `409 chat_busy`.

### Stop a reply

```
POST /api/v1/chats/:id/cancellation
```

Answers `202 Accepted`. The stream then ends with `reply.cancelled`. Cancelling when nothing is running is also `202`.

## 4. The reply stream

```
GET /api/v1/chats/:id/events
Accept: text/event-stream
Last-Event-ID: 5002:17
```

Server-Sent Events. Each event has an `id` (`<message id>:<sequence>`) so a dropped connection resumes with `Last-Event-ID` and misses nothing. Without it, the stream starts with the reply in progress, if any, replayed from its first event. The server sends a comment line (`: keepalive`) every 15 seconds, and closes the stream 30 seconds after the last reply ends; the client opens it again after its next message.

| Event | Data |
|---|---|
| `reply.started` | `{"message_id": "5002", "model": "…"}` |
| `text.delta` | `{"message_id": "5002", "delta": "The Q3 budget"}` |
| `tool.started` | `{"message_id": "5002", "tool": { "id": "tc_77", "service": "local", "tool": "search", "summary": "search \"q3 budget\"", "status": "running", "device": {...}, "request_id": "7" }}` |
| `tool.finished` | Same shape, with the final `status` and an updated `summary` ("search \"q3 budget\" · 3 hits"). |
| `reply.finished` | `{"message_id": "5002", "content": "<the whole answer>", "citations": [...], "credits": 1}` |
| `reply.failed` | `{"message_id": "5002", "error": {"code": "provider_unavailable", "message": "…"}}` |
| `reply.cancelled` | `{"message_id": "5002"}` |
| `chat.titled` | `{"title": "Q3 budget"}` |

```
id: 5002:1
event: reply.started
data: {"message_id":"5002","model":"gemini-3.1-flash-lite"}

id: 5002:2
event: tool.started
data: {"message_id":"5002","tool":{"id":"tc_77","service":"local","tool":"search","summary":"search \"q3 budget\"","status":"running","device":{"id":"42","name":"carmine-mbp"},"request_id":"7"}}

id: 5002:3
event: text.delta
data: {"message_id":"5002","delta":"The Q3 budget is "}
```

`reply.finished` carries the whole answer, so a client that missed deltas still ends up with the right text. In the TUI these become `ChatEvent::Started`, `TextDelta`, `Tool`, `Done` and `Error`.

On the server this could be `ActionController::Live` reading a per-chat event feed that `Message::Broadcastable` writes next to its Turbo Stream broadcasts. It already knows every event here (a message created, a chunk appended, a tool call started and finished); it only renders them as HTML today.

## 5. Tool activity from this computer

When the assistant calls a tool on this computer, the step says so: `service` is `local`, `device` names the computer, and `request_id` is the JSON-RPC ID of the call in the tunnel. The daemon's audit log records the same `chat_id` and `request_id` for every call (CONTROL.md, "Audit entries").

That lets the TUI join the two. A step from this computer is shown as "This computer", and its local decision comes from the audit entry: a call the daemon refused shows the deny-list reason it logged, which the server only knows as `denied`. The server never learns anything new from this; the join happens locally.

Steps for other services (`drive`, `dropbox`, `onedrive`, `slack`, `basecamp`, `mcp_<server id>`) have no `device` or `request_id`.

## 6. Errors

Errors are JSON with an HTTP status:

```json
{"error": {"code": "chat_busy", "message": "A reply is still being written in this chat."}}
```

| Status | `code` | When |
|---|---|---|
| 400 | `use_dpop_nonce` | The proof needs the nonce from the `DPoP-Nonce` header. Retry once with it. |
| 401 | `invalid_token` | Expired or revoked token (`WWW-Authenticate: DPoP error="invalid_token"`). Refresh, then retry once. If the refresh fails with `invalid_grant`, chat was revoked: ask the user to approve it again. |
| 403 | `insufficient_scope` | A token without `chats`. |
| 403 | `usage_limit` | Out of credits for this period. `message` is the sentence the web UI shows; `url` points at billing. |
| 403 | `model_unavailable` | The chosen model isn't available to this user. |
| 404 | `not_found` | No such chat, or not this user's. |
| 409 | `chat_busy` | A reply is already being written. |
| 422 | `invalid` | Empty content, content over the size limit, a field of the wrong type. `details` lists them. |
| 429 | `rate_limited` | Too many requests. `Retry-After` says when to try again. |
| 5xx | `unavailable` | Retry with backoff. |

Failures while writing a reply (a provider outage, a lost connector) arrive as `reply.failed` on the stream, not as HTTP errors.

## 7. What's left out

Kept out to stay small, and easy to add later without a version bump: attachments, branching and retrying messages, share links, deleting and renaming chats, search across chats, and model listing (clients can send a slug they know, and a wrong one gets `model_unavailable`).
