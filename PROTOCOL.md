# Chat with Work Local Agent: tunnel protocol, version 1

This document describes everything that goes over the wire between the Local Agent daemon (`cww`) and a Chat with Work server: pairing, tokens, the WebSocket tunnel, and the MCP messages inside it. A server implementation (the Rails app, or a self-hosted one) can be checked against it line by line.

The key words MUST, SHOULD and MAY are used as in RFC 2119.

## Contents

1. [Roles and overview](#1-roles-and-overview)
2. [Server URL](#2-server-url)
3. [Device key and DPoP proofs](#3-device-key-and-dpop-proofs)
4. [Pairing (RFC 8628)](#4-pairing-rfc-8628)
5. [Token endpoint](#5-token-endpoint)
6. [The WebSocket tunnel](#6-the-websocket-tunnel)
7. [MCP inside the tunnel](#7-mcp-inside-the-tunnel)
8. [Tools](#8-tools)
9. [Errors](#9-errors)
10. [Limits](#10-limits)
11. [Decisions this spec makes](#11-decisions-this-spec-makes)
12. [Chats for the terminal UI](#12-chats-for-the-terminal-ui)

## 1. Roles and overview

- The **daemon** runs on the user's computer. It is an MCP *server* that exposes four read-only tools, and seven change tools while at least one shared folder allows changes (section 8.7).
- The **server** is Chat with Work. It is the MCP *client*: the model loop runs there and calls the tools.
- The daemon opens the only connection, outbound, to `wss://<server>/local_agent`. The server never connects to the daemon.
- Over that socket the server sends JSON-RPC requests and the daemon answers them. The daemon never sends requests: no sampling, no elicitation, no roots.

```
cww login        POST /local_agent/device_authorizations   (DPoP)
                 user approves at /device
                 POST /local_agent/token  device_code grant (DPoP) -> device_id + refresh_token

cww daemon run   POST /local_agent/token  refresh_token grant (DPoP) -> access_token (10 min)
                 GET  /local_agent  Upgrade: websocket
                      Authorization: DPoP <access_token>
                      DPoP: <proof bound to the token>
                 <- MCP requests / -> MCP responses, until the socket closes
                 reconnect with backoff, refreshing the token when needed
```

## 2. Server URL

The user gives the server as an origin, for example `https://chatwithwork.com` (the default). The daemon rejects URLs with a path, query, fragment or credentials.

- `https://` is required. Plain `http://` is accepted only when the host is `localhost` or a loopback IP, for development.
- HTTP endpoints are `<origin>/local_agent/device_authorizations` and `<origin>/local_agent/token`.
- The WebSocket URL is `wss://<host[:port]>/local_agent` (`ws://` for the loopback exception).
- TLS uses rustls. The daemon trusts the operating system's certificate store (macOS keychains, the Windows certificate store, the distribution's CA bundle on Linux, or `SSL_CERT_FILE`/`SSL_CERT_DIR` when set) plus Mozilla's root certificates, so servers behind private CAs work. The daemon never follows redirects: a 3xx from any endpoint is an error.

## 3. Device key and DPoP proofs

At pairing the daemon creates an **Ed25519** key pair. The private key never leaves the machine (it lives in the OS keychain, or in a 0600 file when no keychain is available). The public key is sent as **`public_key`**: the raw 32-byte key, base64url without padding. This is the same value as the JWK `x` parameter.

Every call to the token endpoints and the WebSocket upgrade carries a **`DPoP`** header with a proof as defined by [RFC 9449](https://www.rfc-editor.org/rfc/rfc9449): a compact JWS signed with the device key.

**Proof header:**

```json
{ "typ": "dpop+jwt", "alg": "EdDSA", "jwk": { "kty": "OKP", "crv": "Ed25519", "x": "<public_key>" } }
```

**Proof claims:**

| Claim | Value |
|---|---|
| `jti` | 16 random bytes, base64url. Unique per proof. |
| `htm` | `POST` for the token endpoints, `GET` for the WebSocket upgrade. |
| `htu` | The full HTTP(S) URL of the request, without query: `https://<host>/local_agent/token`, `https://<host>/local_agent/device_authorizations`, or `https://<host>/local_agent` for the upgrade (the HTTPS form, even though the socket URL is `wss://`). |
| `iat` | Current Unix time in seconds. |
| `nonce` | The latest `DPoP-Nonce` the server sent, if the daemon has one. |
| `ath` | Only on the WebSocket upgrade: base64url(SHA-256(access token)). |

**The server MUST verify** every proof: signature, `typ`, `alg`, that `jwk.x` equals the device's registered public key (at pairing: equals the `public_key` form field), `htm`, `htu`, `iat` within a small window (60 seconds is suggested), `jti` not seen before within that window, `ath` on the upgrade, and `nonce` when the server requires nonces.

**Nonces.** The server SHOULD send a `DPoP-Nonce` response header on every token endpoint response (success or error) and on a rejected upgrade. The daemon puts the latest nonce it has seen into every proof. When a proof has no nonce or a stale one, the token endpoint answers `400 {"error":"use_dpop_nonce"}` with a fresh `DPoP-Nonce` header, and the daemon retries once. The daemon's very first request carries no nonce.

The JWK thumbprint ([RFC 7638](https://www.rfc-editor.org/rfc/rfc7638)) is `base64url(SHA-256('{"crv":"Ed25519","kty":"OKP","x":"<public_key>"}'))`. `cww login` prints it, so the approval page MAY show it for comparison. Servers that issue self-contained access tokens SHOULD bind them with `cnf.jkt` set to this thumbprint.

## 4. Pairing (RFC 8628)

### 4.1 Device authorization request

```
POST /local_agent/device_authorizations
Content-Type: application/x-www-form-urlencoded
Accept: application/json
DPoP: <proof, htm=POST, htu=https://<host>/local_agent/device_authorizations>

client_id=cww
&scope=local_agent:serve local_agent:chat
&public_key=<base64url Ed25519 public key>
&name=<device name, defaults to the hostname>
&platform=<linux | macos | windows>
&client_version=<cww version, e.g. 0.1.0>
```

**Success: `200 OK`**

```json
{
  "device_code": "…",
  "user_code": "WDJB-MJHT",
  "verification_uri": "https://chatwithwork.com/device",
  "verification_uri_complete": "https://chatwithwork.com/device?user_code=WDJB-MJHT",
  "expires_in": 900,
  "interval": 5
}
```

`verification_uri_complete` is optional and `interval` defaults to 5. Any other status is an error with an OAuth error body (see [section 9](#9-errors)).

The server stores the public key with the pending authorization, together with the name, platform and version it shows on the approval page. The approval page (`/device`) is server-side UI and not part of this protocol. The design calls for re-authentication, showing the device name, OS, IP and approximate location, and an email to the user after approval.

### 4.2 Polling

The daemon waits `interval` seconds, then polls until it is approved, denied, or `expires_in` passes:

```
POST /local_agent/token
Content-Type: application/x-www-form-urlencoded
DPoP: <proof, htm=POST, htu=https://<host>/local_agent/token>

grant_type=urn:ietf:params:oauth:grant-type:device_code
&device_code=<device_code>
&client_id=cww
```

The proof MUST be signed by the key registered in 4.1.

| Response | Daemon behavior |
|---|---|
| `400 {"error":"authorization_pending"}` | Keep polling. |
| `400 {"error":"slow_down"}` | Add 5 seconds to the interval, keep polling. |
| `400 {"error":"access_denied"}` | Stop: "pairing was denied". |
| `400 {"error":"expired_token"}` | Stop: "the pairing code expired". |
| `200` token response (below) | Paired. |

**Approval: `200 OK`**

```json
{
  "access_token": "…",
  "token_type": "DPoP",
  "expires_in": 600,
  "scope": "local_agent:serve",
  "refresh_token": "…",
  "device_id": "42"
}
```

`device_id` and `refresh_token` are REQUIRED here. `device_id` is opaque to the daemon; a JSON string or number is accepted. The daemon stores the refresh token and the key in the secret store, and the origin and device ID in `~/.config/cww/config.toml`.

## 5. Token endpoint

Before connecting, the daemon exchanges its refresh credential for a short-lived access token:

```
POST /local_agent/token
Content-Type: application/x-www-form-urlencoded
DPoP: <proof, htm=POST, htu=https://<host>/local_agent/token>

grant_type=refresh_token
&refresh_token=<refresh_token>
&client_id=cww
```

**Success: `200 OK`**

```json
{
  "access_token": "…",
  "token_type": "DPoP",
  "expires_in": 600,
  "scope": "local_agent:serve",
  "refresh_token": "…"
}
```

- The access token is opaque to the daemon. The design calls for a signed token with a 10-minute lifetime, `aud` equal to the server origin, `scope=local_agent:serve`, and binding to the device key (`cnf.jkt`). That scope MUST NOT grant access to any other API. While the owner lets the computer use their chats, the scope also carries `local_agent:chat`, which opens the chat API of section 12 and nothing else.
- `refresh_token` in the response is OPTIONAL. If present and different, the daemon replaces the stored one (rotation). Servers that rotate SHOULD accept the previous refresh token for a short grace period, because the daemon may crash between receiving and storing it.
- The daemon caches the access token in memory and refreshes it 60 seconds before `expires_in` runs out, or after the server rejects it.
- The refresh request MUST be rejected with `400 {"error":"invalid_grant"}` once the device is revoked. On `invalid_grant`, `unauthorized_client` or `access_denied`, the daemon marks itself **revoked**, closes the tunnel, stops reconnecting, and waits for the user to run `cww login`. Any other failure (network error, 5xx, other error codes) is retried with backoff.

## 6. The WebSocket tunnel

### 6.1 Upgrade request

```
GET /local_agent HTTP/1.1
Host: chatwithwork.com
Upgrade: websocket
Connection: Upgrade
Sec-WebSocket-Version: 13
Sec-WebSocket-Key: …
Sec-WebSocket-Protocol: actioncable-v1-json, mcp
Authorization: DPoP <access_token>
DPoP: <proof, htm=GET, htu=https://<host>/local_agent, ath=…, nonce=…>
Origin: https://chatwithwork.com
User-Agent: cww/0.1.0
```

- The access token is only ever sent in the `Authorization` header, never in the URL.
- `Origin` is the server's own origin, so Action Cable's same-origin check (`allow_same_origin_as_host`) accepts it.
- Behind an HTTP proxy, the daemon first opens a tunnel with `CONNECT <host>:<port>` and runs TLS and this request inside it, as it does for the pairing and token requests. The server sees the same bytes either way.

**The server MUST**, before accepting the upgrade: validate the access token (signature, expiry, audience, scope `local_agent:serve`, device not revoked), validate the DPoP proof as in section 3, including `ath`, and check that the proof key is the one the token is bound to.

**Rejection.** Respond `401` (or `403`) instead of `101`, with a fresh `DPoP-Nonce` header. The daemon retries once right away with the new nonce, or with a freshly refreshed token if no nonce came back. After that it falls back to the normal backoff.

### 6.2 Subprotocols and framing

The daemon offers two subprotocols, and the server MUST select one in `Sec-WebSocket-Protocol`:

| Subprotocol | Framing |
|---|---|
| `actioncable-v1-json` | Action Cable (what Rails speaks). If the server selects no subprotocol, the daemon assumes this one. |
| `mcp` | Bare JSON-RPC: every text frame is exactly one JSON-RPC message, with no envelope. |

Any other selection is an error, and the daemon disconnects.

**Action Cable framing.** The channel identifier is this exact string:

```
{"channel":"LocalAgent::Channel"}
```

1. The server MAY send `{"type":"welcome"}`.
2. The daemon sends `{"command":"subscribe","identifier":"{\"channel\":\"LocalAgent::Channel\"}"}`.
3. The server answers `{"identifier":"…","type":"confirm_subscription"}`, or `reject_subscription`, which the daemon treats as unauthorized (see 6.4). Only this channel's rejection counts: a refused chat channel (section 12) is just a chat that can't be followed.
4. **Server to daemon:** each JSON-RPC message is the `message` of a channel transmission:
   `{"identifier":"{\"channel\":\"LocalAgent::Channel\"}","message":{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{…}}}`.
   `message` SHOULD be a JSON object. A string containing serialized JSON is also accepted. Frames for other identifiers are ignored, except the chat channels of section 12, whose messages go to the terminal UI and never to the MCP server.
5. **Daemon to server:** each JSON-RPC message is sent as a `message` command whose `data` is the serialized JSON-RPC message:
   `{"command":"message","identifier":"{\"channel\":\"LocalAgent::Channel\"}","data":"{\"jsonrpc\":\"2.0\",\"id\":7,\"result\":{…}}"}`.
   JSON-RPC messages never contain an `action` key, so Rails dispatches them to `LocalAgent::Channel#receive(data)`, where `data` is the parsed JSON-RPC message.
6. `{"type":"ping"}` frames are accepted and ignored (they count as traffic). `{"type":"disconnect","reason":…,"reconnect":…}` is handled as in 6.4.

In Rails terms: authenticate in the `LocalAgent` connection's `connect` from the request headers. `transmit(jsonrpc_hash)` sends a request to the daemon. `receive(data)` gets each response.

### 6.3 Heartbeats and liveness

- The daemon sends a WebSocket **ping** frame every 20 seconds. The server's WebSocket stack answers with a pong, which is automatic in Rails and most libraries.
- If the daemon receives no frame at all (pong, Action Cable ping, or message) for 60 seconds, it treats the connection as dead and reconnects. Action Cable's 3-second pings keep it alive. With `mcp` framing, pongs do.
- The server SHOULD treat the device as offline as soon as the socket closes, and fail pending tool calls fast rather than waiting out their deadline.

### 6.4 Closing, revocation and reconnects

| Server action | Daemon behavior |
|---|---|
| Close code **4001** ("re-authenticate") | Drop the cached access token, refresh it, reconnect. |
| Close code **4003** ("revoked") | Drop the cached token and try to refresh. The refresh gets `invalid_grant`, so the daemon becomes revoked and stops. |
| Action Cable `disconnect` with `reconnect:false`, or with `reason:"unauthorized"`, or `reject_subscription` | Same as 4001. |
| Any other close, network loss, or idle timeout | Reconnect with backoff, reusing the cached token if it is still valid. |

To revoke a device, the server sets `revoked_at`, closes the device's socket with **4003**, and rejects every later refresh with `invalid_grant`. The socket's lifetime is **not** tied to the access token's lifetime: a token is checked at upgrade time only, and the server ends sessions by closing the socket. A server that wants periodic re-authentication MAY close with 4001 at any time.

**Backoff** is full-jitter exponential: a random delay in `[0.25 s, min(60 s, 1 s × 2^attempt)]`, reset after a connection that lasted at least 60 seconds.

### 6.5 Message size

- A JSON-RPC message MUST NOT exceed **262 144 bytes (256 KiB)** in either direction, measured on the serialized JSON-RPC message (the `data` string or `message` object, not the Action Cable envelope).
- WebSocket frames and messages are capped at `2 × 256 KiB + 16 KiB`, because Action Cable string-encodes `data` and can double its size. A larger frame ends the connection.
- A request larger than 256 KiB (but inside the frame cap) gets JSON-RPC error `-32600` "message too large".
- The daemon shrinks tool results to fit: `read` halves its chunk (and moves `next_offset` back), `list` halves its page (and adjusts `next_cursor`), and `search` drops trailing hits. Any other oversized response is replaced by error `-32603` "response too large".

## 7. MCP inside the tunnel

### 7.1 Versions and sessions

Each WebSocket connection is a **new MCP session**. The first JSON-RPC message the server sends on a connection chooses the mode, and rmcp (the official Rust MCP SDK) implements both:

- **Stateless, MCP `2026-07-28` (preferred).** There is no handshake. Every request's `params._meta` MUST carry:

  ```json
  "_meta": {
    "io.modelcontextprotocol/protocolVersion": "2026-07-28",
    "io.modelcontextprotocol/clientCapabilities": {},
    "io.modelcontextprotocol/clientInfo": { "name": "chatwithwork", "version": "…" }
  }
  ```

  `clientInfo` is optional. A request without the two required keys gets `-32602`. `server/discover` returns the supported versions and capabilities.
- **Legacy handshake, MCP `2025-11-25` and earlier.** The first message is `initialize`. The daemon answers with the negotiated version, and later requests need no `_meta`. The server SHOULD send `notifications/initialized`. This exists because RubyLLM's MCP client falls back to it.

In stateless mode `ping` is not available (`-32601`). Liveness comes from WebSocket pings (6.3).

### 7.2 Chat ID

Every `tools/call` SHOULD carry the Chat with Work chat ID in its request metadata:

```json
"_meta": { "com.chatwithwork/chatId": "1234", … }
```

A string or a number is accepted. The daemon uses it for the per-chat read budget (section 10) and writes it to the local audit log. Calls without it share one budget.

### 7.3 Methods

| Method | Answer |
|---|---|
| `server/discover` | `supportedVersions`, `capabilities: {"tools":{}}`, and `_meta["io.modelcontextprotocol/serverInfo"] = {"name":"cww","version":…}`. |
| `initialize` | Legacy handshake (7.1). `serverInfo.name` is `cww`, and the result carries instructions. |
| `tools/list` | The four read-only tools (section 8), then the change tools while a shared folder allows changes (section 8.7), in one page. The list only changes when the user turns changes on or off for a folder, and the daemon then reconnects, so a server that lists tools on each new connection is always current. |
| `tools/call` | See section 8. |
| `notifications/initialized`, `notifications/cancelled` | Accepted. Cancelling abandons a call in progress. |
| `resources/*`, `prompts/*`, `completion/complete`, `logging/setLevel`, anything else | `-32601` method not found. |

The daemon ignores JSON-RPC responses from the server, since it never sends requests, and ignores unknown notifications.

### 7.4 Tool naming on the server

The daemon's tool names are `roots`, `search`, `list` and `read`, and the change tools `create`, `write`, `edit`, `mkdir`, `move`, `delete` and `create_document`, with no prefix. The server namespaces them per device, for example `local_<device_id>_read`, following its existing `mcp_<id>_` convention.

## 8. Tools

The read-only tools (8.3 to 8.6) are annotated `readOnlyHint: true`, `destructiveHint: false`, `idempotentHint: true`, `openWorldHint: false`. The change tools carry their own annotations (8.7). Unknown argument keys are rejected with `invalid_argument`.

### 8.1 Paths

Every path is **`<root_id>:<relative path>`**. The daemon rejects the following before touching the filesystem:

| Rule | Error |
|---|---|
| No `:`, or a root ID outside `[a-z0-9][a-z0-9_-]{1,31}` (2 to 32 characters; `C:` never parses as a root) | `invalid_path` |
| Absolute paths (leading `/`), a leading `~`, or a relative part starting with `/` | `invalid_path` |
| `..` in any component | `invalid_path` |
| NUL bytes or backslashes anywhere | `invalid_path` |
| Control characters (newlines, escapes), direction marks and isolates (U+061C, U+200E, U+200F, U+202A to U+202E, U+2066 to U+2069), zero-width characters (U+200B to U+200D, U+2060 to U+2064, U+FEFF), or line and paragraph separators anywhere: they could forge or hide part of an audit log line. A file whose name has one can't be read or changed by path | `invalid_path` |
| Longer than 4096 bytes, or a component longer than 255 bytes | `invalid_path` |
| Well-formed, but no shared root has this ID | `unknown_root` |

`.` components and empty components (`a//b`) are ignored. `<root_id>:` on its own means the root folder. Paths the daemon returns are always normalized in this form. Absolute local paths are never sent.

Resolution then follows the rules in README.md ("Threat model"): symlinks are not followed, the path must stay inside the root, the deny list applies, and only regular files (for `read`) and directories (for `list`) are served.

### 8.2 Results

A successful call returns:

```json
{
  "content": [ { "type": "text", "text": "<the same JSON as structuredContent, serialized>" } ],
  "structuredContent": { … },
  "isError": false
}
```

(`resultType: "complete"` is added in `2026-07-28` sessions.) Servers SHOULD read `structuredContent`.

Timestamps are RFC 3339 in UTC, for example `2026-09-24T12:00:00Z`. Sizes are bytes. **Offsets and character counts are Unicode scalar values**, which is what Ruby's `String#length` counts, not bytes or UTF-16 units.

### 8.3 `roots`

Arguments: none (`{}`).

```json
{
  "roots": [
    { "id": "work-docs", "label": "Work docs", "available": true, "writable": false, "index": "ready", "indexed_files": 1532 }
  ]
}
```

`writable` is true when the user allowed changes in the folder; the change tools only work there. `index` is `pending`, `indexing`, `ready`, `error` or `disabled`. `available` is false when the folder is missing, for example on an unmounted drive. Labels are chosen by the user and are the only description of a root the server gets.

### 8.4 `search`

| Argument | Type | Notes |
|---|---|---|
| `query` | string, required | 1 to 1000 characters. BM25 over file names (boosted ×2) and extracted text. |
| `root` | string | Only search this root ID. |
| `path_glob` | string | Without `/`, it matches the file name (`*.pdf`). With `/`, it matches the relative path (`plans/**/*.md`). Case-insensitive. |
| `modified_after` | string | RFC 3339 time or `YYYY-MM-DD` (UTC midnight). |
| `limit` | integer | 1 to 20, default 20. |

```json
{
  "hits": [
    {
      "path": "work-docs:plans/q3.md",
      "title": "q3.md",
      "modified": "2026-09-20T08:14:03Z",
      "size": 5120,
      "snippet": "The quarterly budget for Project Falcon is approved…"
    }
  ],
  "engine": "index"
}
```

`engine` is `index`, `grep`, or `index+grep`. When the index returns nothing, or a searched root hasn't finished its first index pass, the daemon also runs a live grep: a case-insensitive literal match of `query`, in text files only, with a 5-second time budget. Snippets are at most 300 characters. Files that match the deny list, or that the daemon refuses to open (hard links, special files), are never indexed or returned.

### 8.5 `list`

| Argument | Type | Notes |
|---|---|---|
| `path` | string, required | A directory: `<root_id>:` or `<root_id>:dir/sub`. |
| `cursor` | string | The `next_cursor` of the previous page. Opaque. |

```json
{
  "path": "work-docs:plans",
  "entries": [
    { "name": "q3.md", "path": "work-docs:plans/q3.md", "kind": "file", "size": 5120, "modified": "2026-09-20T08:14:03Z" },
    { "name": "archive", "path": "work-docs:plans/archive", "kind": "dir", "modified": "2026-09-01T10:00:00Z" }
  ],
  "next_cursor": "200"
}
```

`kind` is `file`, `dir`, `symlink` (listed, never followed), or `other`. `size` appears only for files. Entries are sorted by name, at most 200 per page. `next_cursor` is `null` on the last page. Denied entries and names that aren't valid UTF-8 are left out.

### 8.6 `read`

| Argument | Type | Notes |
|---|---|---|
| `path` | string, required | A file. |
| `offset` | integer | Character offset, default 0. |
| `max_chars` | integer | 1 to 32000, default 32000. It is lowered further by the remaining hourly budget (section 10). |

```json
{
  "path": "work-docs:plans/q3.pdf",
  "text": "…",
  "offset": 0,
  "next_offset": 32000,
  "total_chars": 81234,
  "modified": "2026-09-20T08:14:03Z",
  "size": 482113
}
```

`next_offset` is `null` at the end of the file. PDF, DOCX, PPTX (slides marked `--- Slide N ---`), and XLSX/XLS/ODS (one `## Sheet` section per sheet, tab-separated rows) are converted to text. Other files are read as UTF-8 (invalid bytes are replaced) unless they look binary, which gives `unsupported`. Files over 64 MiB give `too_large`.

### 8.7 Changes

The user allows changes per shared folder, on their computer (`writable = true` in `config.toml`, `cww roots allow-changes`, or a switch in the terminal UI and the desktop app). Nothing the server sends can turn it on. While no folder allows changes, `tools/list` has only the four read-only tools and a call to a change tool is an unknown tool (`-32602`). While one does, these seven are listed after them:

| Tool | Does | `readOnlyHint` | `destructiveHint` | `idempotentHint` | `_meta["com.chatwithwork/writeWhen"]` |
|---|---|---|---|---|---|
| `create` | New text file; fails if the path exists | false | false | false | |
| `write` | Replace a text file's content, or append to it; creates it if missing | false | **true** | false | `{"mode": "append"}` |
| `edit` | Replace one exact span of a text file | false | false | false | |
| `mkdir` | New folder, `parents` optional | false | false | true | |
| `move` | Move or rename a file or folder; `replace` optional | false | **true** | false | `{"replace": false}` |
| `delete` | Move a file or folder to the system trash | false | **true** | false | |
| `create_document` | New Word (`.docx`) or Excel (`.xlsx`) file; `replace` optional | false | **true** | false | `{"replace": false}` |

All of them are `openWorldHint: false`.

**`writeWhen`.** MCP annotations can't vary with arguments, so a tool that is destructive only with some arguments is annotated `destructiveHint: true` (the worst case) and names, in `_meta["com.chatwithwork/writeWhen"]`, the argument values that make a call a plain write: the call is not destructive when every argument listed has the value given, where a missing argument counts as its default (`mode` defaults to `"replace"`, `replace` to `false`). So `write` with `mode: "append"`, `move` without `replace` (or with `replace: false`), and `create_document` without `replace` are writes; `write` without `mode` (a replace), `move` or `create_document` with `replace: true`, and every `delete` are destructive. A server that ignores the hint treats them all as destructive, which is safe.

**Rules every change follows**, in the daemon, whatever the server decided:

- The folder must allow changes (`not_writable`), and so must any folder shared on its own inside it (a folder shared read-only stays read-only inside one that allows changes), and the path follows 8.1. The shared folder itself can't be changed, moved or deleted (`invalid_path`).
- Folders are resolved on handles as reads are, but **never through a symlink or junction**, even in a folder that follows links for reads, and the change happens relative to the open folder handle. Symlinks, files with more than one hard link, FIFOs, sockets and devices are never changed, moved or deleted (`denied`); a symlink is never followed.
- The deny list applies to every path a change touches, sources and destinations, and to everything inside a folder that is moved or deleted (`denied`). So does a second list of paths that can be read but are **never changed**: every entry of the user's home folder whose name starts with a dot, version control internals (`.git`, `.hg`, `.svn`, `.bzr`, `.jj`, and any folder holding Git's `HEAD`, `objects` and `refs`), shell startup files (`.bashrc`, `.zshrc`, `.profile`, ...), Git and terminal settings (`.gitconfig`, `.config/git`, `.tmux.conf`), desktop session and window manager settings (`.xinitrc`, `.xprofile`, `.config/hypr`, `.config/i3`, `.config/sway`, ...), autostart and launch agents, editor, hook, interpreter and tool settings that run commands (`.vscode`, `.idea`, Vim and Emacs settings, `.husky`, `.pre-commit-config.yaml`, `mise.toml`, `*.pth`, `.envrc`, `.direnv`, `.cargo`, ...), and `desktop.ini` and `autorun.inf` (`denied`). The list is the daemon's; it may grow, and a server should only show the refusal.
- **No programs are made:** no name with an extension that runs or launches something on any of the three systems (`.exe`, `.bat`, `.ps1`, `.lnk`, `.url`, `.desktop`, `.app`, `.command`, `.jar`, `.so`, Office files with macros, documents that make Windows or Office run or fetch things such as `.rtf`, `.mht`, `.slk`, `.iqy`, `.rdp` and `.theme`, and more; also `.js` on Windows), no text with an Office processing instruction (`<?mso-application?>`), and no file ever gets an execute bit. Every file the daemon makes carries the system's mark for downloads (Mark-of-the-Web on Windows, quarantine on macOS). Existing executable files, files marked read-only, and files owned by another user are never changed (`not_changeable` or `denied`); executables and read-only files can still go to the trash. New names can't contain control characters, direction marks, `\ / : * ? " < > |`, start with a space or end with a dot or a space, or be a Windows device name (`invalid_path`).
- **Text tools write text.** `create`, `write` and `edit` take UTF-8 without NUL characters, refuse names of documents and media that text would corrupt (`.pdf`, `.docx`, `.xlsx`, `.zip`, images, ...; `not_changeable`, pointing `.docx` and `.xlsx` to `create_document`), and only change existing files that are UTF-8 text (`not_changeable`). `edit` and `append` keep the file's line endings and byte order mark.
- **Nothing is deleted for good.** A replaced, appended or edited file's previous version, a deleted file or folder, and a file a `move` or `create_document` replaces all go to the **system trash** on the user's computer: the freedesktop.org trash on Linux (`$XDG_DATA_HOME/Trash`, or `$topdir/.Trash/$uid` or `$topdir/.Trash-$uid` for other filesystems, with a `.trashinfo` record so file managers can restore it), the Trash on macOS (`~/.Trash`, or `.Trashes/$uid` on other volumes), and the Recycle Bin on Windows (with the `$I` record Restore uses). When the trash can't take an item (a network drive, a removable drive on Windows, a mount inside the folder), the change is refused with `trash_unavailable` and nothing changes.
- **Atomic.** New content is written to a hidden file beside the target and flushed to disk. A new file is renamed into place without replacing anything (`renameat2(RENAME_NOREPLACE)`, `renamex_np(RENAME_EXCL)`, or a handle rename without `ReplaceIfExists`). A replaced file is swapped with its new version in one step where the filesystem can (Linux, macOS), keeping its group, extended attributes and ACLs, and the old version then goes to the trash; elsewhere (Windows, which keeps the access list) the old version goes to the trash first. A crash leaves the old file, the new file, or the old one in the trash or beside the file under a hidden name; never half a file. A move to another filesystem copies, then moves the original to the trash.
- **Budgets** (section 10): changes per minute and per day and bytes written per hour, separate from the read budgets, and a size cap per file. Dry runs and refused calls don't count against them.
- Changes run one at a time, and every one is in the local audit log with its effect, the paths before and after, the bytes written, and where the old version went in the trash.

**Dry runs.** Every change tool takes `dry_run: true`: the daemon runs every check above against the files as they are now and answers what the call would do (`"dry_run": true`), without changing anything or counting against the change budgets. For `write` and `edit` the answer includes a unified diff (`diff`, at most 32 KiB) and the file's current SHA-256 (`previous.sha256`). The diff shows the file as it is, so it counts against the read budgets (section 10) like a `read` of as many characters: it is cut to what the budget allows (ending with a line that says so), and once the budget is used up, a dry run of `write` or `edit` is refused with `rate_limited`, as a read would be. A server that gets `rate_limited` for its own preview can show the card without the diff (with `old_text` and `new_text` for `edit`). A server can make a dry run itself, with the model's arguments plus `dry_run: true`, to fill the approval card, and then pass the SHA-256 back as `expected_sha256` when the user approves, so `write` and `edit` fail with `conflict` if the file changed in between. Dry runs are logged locally like other calls.

**Results.** A change answers:

```json
{
  "path": "work-docs:plans/q4.md",
  "effect": "replaced",
  "dry_run": false,
  "kind": "file",
  "size": 2100,
  "sha256": "9f2c…",
  "previous": { "size": 2048, "modified": "2026-09-20T08:14:03Z", "sha256": "4b1a…", "in_trash": true }
}
```

| Field | Present | Meaning |
|---|---|---|
| `path` | always | The path the change produced: the destination of a move. |
| `effect` | always | `created`, `replaced`, `appended`, `edited`, `created_folder`, `moved`, `trashed`, or `unchanged` (`mkdir` of a folder that exists). |
| `dry_run` | always | True when nothing changed. |
| `kind` | always | `file` or `dir`. |
| `from` | `move` | Where it came from. |
| `size`, `sha256` | files written | The new file's size and SHA-256. |
| `previous` | replaced, edited, appended, deleted, or replaced by a move | What was there: `size` (for a folder, the bytes it holds), `modified`, `sha256` (text changes only), and `in_trash`, true once it is in the trash. |
| `diff` | dry runs of `write` and `edit` | A unified diff, cut at 32 KiB. |
| `entries` | folders moved or deleted, `mkdir` with `parents` | How many files and folders a folder holds, or how many folders were made. |

Where the trash is on the computer is never sent; only the local audit log has it.

### 8.8 `create`

| Argument | Type | Notes |
|---|---|---|
| `path` | string, required | The new file. Its folder must exist (`not_found` says to use `mkdir`). |
| `content` | string, required | UTF-8 text. |
| `dry_run` | boolean | |

Fails with `exists` if anything is at the path. Effect `created`.

### 8.9 `write`

| Argument | Type | Notes |
|---|---|---|
| `path` | string, required | |
| `content` | string, required | The new text, or the text to add. |
| `mode` | `"replace"` (default) or `"append"` | |
| `expected_sha256` | string | Hex SHA-256 the file's current content must have (`conflict` otherwise, or when the file is gone). |
| `dry_run` | boolean | |

Effect `replaced` or `appended`, or `created` when the file didn't exist. The previous version goes to the trash.

### 8.10 `edit`

| Argument | Type | Notes |
|---|---|---|
| `path` | string, required | An existing text file. |
| `old_text` | string, required | Must appear exactly once. Not empty. At most 8 KiB and at most half the file, unless it is 64 bytes or less. |
| `new_text` | string, required | Different from `old_text`. |
| `expected_sha256` | string | As for `write`. |
| `dry_run` | boolean | |

`invalid_argument` when `old_text` isn't found ("Read the file again and quote the text exactly") or appears more than once ("appears N times; include more of the surrounding text"), and when it is larger than the limits above ("Use write to replace the file's content"): an edit is a write, which a server may let through for the rest of a chat, so replacing most of a file takes `write`, which is destructive and always asks. When the file uses Windows line endings and `old_text` has none, `\n` in both texts matches `\r\n`. Effect `edited`; the previous version goes to the trash.

### 8.11 `mkdir`

| Argument | Type | Notes |
|---|---|---|
| `path` | string, required | |
| `parents` | boolean | Also make missing folders on the way (`entries` says how many were made). |
| `dry_run` | boolean | |

Effect `created_folder`, or `unchanged` when the folder exists. `exists` when a file is in the way.

### 8.12 `move`

| Argument | Type | Notes |
|---|---|---|
| `from` | string, required | A file or folder. |
| `to` | string, required | The new path, name included; in the same shared folder or another that allows changes. |
| `replace` | boolean | Default false. With true, a **file** already at `to` goes to the trash first; a file can only replace a file. |
| `dry_run` | boolean | |

`exists` when something is at `to` and `replace` isn't true. A folder can't move into itself, and a folder holding a shared folder or anything on the deny list can't move (`denied`), nor one holding more than the entry limit (`too_large`). Executables, read-only files and other users' files don't move. Between filesystems the item is copied (regular files and folders only, up to the size cap; `not_changeable` for folders with links, special files or programs), then the original goes to the trash. Effect `moved`, with `from`; `previous` describes a replaced file.

### 8.13 `delete`

| Argument | Type | Notes |
|---|---|---|
| `path` | string, required | A file or folder. |
| `dry_run` | boolean | |

Moves it to the system trash, where the user can restore it. Effect `trashed`; `entries` counts what a folder held. The same refusals as a move's source apply, except that executables and read-only files may go.

### 8.14 `create_document`

| Argument | Type | Notes |
|---|---|---|
| `path` | string, required | Ends in `.docx` or `.xlsx`; anything else is `invalid_argument` (PowerPoint and macro formats aren't made). |
| `content` | string | For `.docx`, required: the document in Markdown. Headings, paragraphs, bold, italic, strikethrough, inline code, bulleted and numbered lists (nested), quotes, code blocks and tables. Links become their text with the address after it in parentheses, never a live link; images become nothing. |
| `sheets` | array | For `.xlsx`, required: `[{"name": "Budget", "rows": [["Item", "Cost"], ["Falcon", 10.5]], "header": true}]`. Names have 1 to 31 characters, none of `[ ] : * ? / \`, unique ignoring case. Cells are strings, numbers, booleans, or `null` for empty; never formulas (a string starting with `=` stays text). `header` bolds and freezes the first row. |
| `replace` | boolean | Default false. With true, an existing file at the path goes to the trash and a new one is written: existing documents are never edited in place. |
| `dry_run` | boolean | |

Effect `created`, or `replaced`. `exists` without `replace`.

## 9. Errors

**OAuth endpoints** return `{"error": "<code>", "error_description": "<optional>"}` with a 4xx status, using the codes above plus RFC 6749 codes. `use_dpop_nonce` triggers one retry. `invalid_grant`, `unauthorized_client` and `access_denied` on refresh mean the device is revoked.

**JSON-RPC errors** are for requests that can't be routed:

| Code | When |
|---|---|
| `-32600` | Request larger than 256 KiB. |
| `-32601` | Unknown or unsupported method. |
| `-32602` | Unknown tool name, missing `_meta` in a stateless session, or malformed params. |
| `-32603` | Response too large, or internal failure. |

**Tool errors** are for everything that happens inside a known tool. They come back as a normal result so the model can read them:

```json
{
  "content": [ { "type": "text", "text": "denied: this path is on the deny list (.env*)" } ],
  "structuredContent": { "error": { "code": "denied", "message": "this path is on the deny list (.env*)" } },
  "isError": true
}
```

| `code` | Meaning |
|---|---|
| `invalid_argument` | Missing, malformed, unknown, or out-of-range arguments. |
| `invalid_path` | The path breaks a rule in 8.1. |
| `unknown_root` | No shared root has that ID. |
| `outside_root` | Resolution would leave the root. |
| `denied` | Deny list, a symlink (not followed), a file with more than one hard link, or a FIFO, socket or device. |
| `not_found` | No such file or directory, or the root folder is missing right now. |
| `not_a_file` / `not_a_directory` | Wrong kind of entry for `read` / `list`, or for a change. |
| `too_large` | The file is over the size limit, or a folder holds more than a change may move. |
| `unsupported` | The file can't be turned into text (binary, encrypted or damaged). |
| `rate_limited` | A daemon-side rate or volume limit was hit, for reads or for changes. Try again later. |
| `paused` | The user ran `cww pause`. |
| `not_writable` | A change in a shared folder that doesn't allow changes. The message names the folder's label and says the person can allow changes on their computer. |
| `exists` | Something is already at the path (or at `to`), and the call doesn't replace it. |
| `not_changeable` | A kind of file the daemon never makes or changes: a program or launcher, an executable file, a binary file as text, an Office file as text. |
| `trash_unavailable` | The system trash can't take the old version or the item, so nothing changed. |
| `conflict` | The file changed since the caller looked (`expected_sha256`, or during the change). |
| `internal` | Unexpected failure. |

Change errors are sentences, for example `not_writable: Changes aren't allowed in the shared folder “Work docs”. The person can turn on Allow changes for it in the Local Agent on their computer.` or `trash_unavailable: The system trash can't take this item, so nothing was changed. cww never deletes a file for good.` `not_writable`, `not_changeable`, `denied`, `invalid_path`, `unknown_root`, `outside_root`, `rate_limited` and `paused` are logged as refusals, the others as errors.

Messages are written for the model and the user. They never include absolute paths.

## 10. Limits

These are enforced by the daemon whatever the server does. The defaults can be changed only in the local config file, under `[limits]`.

| Limit | Default |
|---|---|
| Tool calls per rolling minute (all chats) | 120 |
| Characters per `read` call | 32 000 |
| `read` characters per chat per rolling hour | 256 000 |
| `read` characters per rolling hour, all chats | 2 000 000 |
| Search hits per call / snippet length | 20 / 300 characters |
| `list` entries per page | 200 |
| Largest file `read` opens | 64 MiB |
| Largest file indexed | 32 MiB |
| JSON-RPC message size | 256 KiB |
| Live grep time budget | 5 seconds |
| Changes per rolling minute / day (all chats) | 30 / 500 |
| Bytes changes may write per rolling hour | 50 MiB |
| Largest file a change creates, edits or copies | 10 MiB |
| Files and folders one move or delete of a folder may carry | 1000 |

## 11. Decisions this spec makes

`docs/local-agent.md` left these points open or ambiguous. This is what the daemon does:

1. **Framing.** The design says both "JSON-RPC frames on a WebSocket" and "terminated by an Action Cable channel". The daemon speaks Action Cable (`actioncable-v1-json`, channel `LocalAgent::Channel`) by default, with bare JSON-RPC (`mcp`) as a negotiated alternative for relays and other servers.
2. **Tool names** are `roots`, `search`, `list` and `read`, without the `local_` prefix the design's table shows. The server adds `local_<device_id>_`, which would otherwise produce `local_7_local_read`.
3. **DPoP.** Standard RFC 9449 proofs with `alg: EdDSA` on every token call and on the upgrade, with `ath` on the upgrade. "Signs a server nonce" is implemented as the RFC 9449 `DPoP-Nonce` mechanism, not as a custom challenge frame.
4. **`htu` for the upgrade** is the `https://` form of the socket URL, which is what Rails sees as `request.url`.
5. **Form-encoded OAuth requests** (as in RFC 6749 and 8628) with JSON responses. The device metadata travels as extra form fields.
6. **The token only gates the upgrade.** An open socket is not tied to the 10-minute token lifetime. The server revokes by closing with 4003 and refusing refreshes, and can force re-authentication with 4001.
7. **Chat ID** travels in request `_meta` under `com.chatwithwork/chatId` and is optional.
8. **The size cap** is measured on the JSON-RPC message, not the Action Cable envelope. Frames get 2× headroom.
9. **Results** carry both `structuredContent` and the same JSON as a text block. Tool-level problems are `isError` results with a stable `code`, and JSON-RPC errors are kept for routing failures.
10. **Root labels and IDs are visible to the server.** Open question 7 in the design is answered in favor of labels. Absolute paths are never visible.
11. **Hard links** are refused by default: a file with a link count above 1 is `denied`. A hard link inside a root can point at data from outside it, and after the fact it can't be told apart from a legitimate one.
12. **Hidden files** (dot-files) and git-ignored files are not indexed or grepped, but can still be listed and read if they aren't on the deny list.
13. **Offsets** count Unicode scalar values.
14. **Revocation handling.** `invalid_grant`, `unauthorized_client` and `access_denied` on refresh stop the daemon's reconnect loop until `cww login`. Everything else is retried.
15. **One server per daemon.** Open question 1 in the design is answered as one pairing at a time. `cww login` again replaces it.
16. **Chats for the terminal** use the device's own token with a second scope, not a separate grant, and the daemon makes every call, so the terminal UI never holds a token. Answers stream over the tunnel's socket (section 12) rather than Server-Sent Events.
17. **Changes are opt-in per folder, on the computer.** Approval of each change happens on the server, and the daemon can't tell a real approval from a forged one, so the daemon also makes every change recoverable (the system trash), bounded (budgets and caps), contained (handle-based resolution, the deny lists, the sandbox's write rights for exactly those folders) and visible (the audit log). See README.md, "Threat model".

## 12. Chats for the terminal UI

`cww tui` shows the owner's chats and asks questions. It never talks to the server: it asks the daemon over the control socket (CONTROL.md), and the daemon calls the server as the device.

**Permission.** Chats are a second permission, apart from answering tools. `cww login` asks for `local_agent:chat` in its scope (`--no-chats` leaves it out), and the approval page lets the user decline it. A computer paired without it asks on first use with `POST /local_agent/chat_access_request`, and the owner answers in the server's settings. The owner can take chats back at any time without unpairing; the server then refuses the chat API and closes the socket with 4001, so followed chats stop.

**Requests.** JSON over HTTPS, each with `Authorization: DPoP <access token>` and a `DPoP` proof for that method and URL with `ath`, exactly as the upgrade in 6.1. Errors are `{"error": <code>, "error_description": <sentence>, ...}`.

| Request | Answers |
|---|---|
| `GET /local_agent/chats` | `chats` (newest first: `number`, `title`, `state`, `project`, `mine`, `updated_at`, `url`, `model`, `can`), `projects` (`id`, `name`, `icon`, `hq`, `all_access`, `url`), `account`, `user`, `credits` (`left`, `capacity`, `running_low`), `locked_reason`, `pins` (`projects`, `chats`), `links` (`new_chat`, `chats`, `projects`, `settings`: `tab`, `label`, `icon`, `url`) |
| `GET /local_agent/chats/<number>` | `chat`, `locked_reason`, `entries` (`user` with `attachments`, `activity` with title, details, `pending`, steps with file names and `waiting`, `assistant` with Markdown and `sources`, `notice`), `approvals`, and `questions` |
| `GET /local_agent/models` | `{"default_model_id", "models": [{"id","name","provider","description","rate","selectable","reason"}]}` |
| `POST /local_agent/chats` `{"content":…}` | `201 {"chat":…}`: a new chat with its first question |
| `POST /local_agent/chats/<number>/messages` `{"content":…}` | `202 {"chat":…}`, or `409 chat_busy` while an answer is written |
| `POST /local_agent/chats/<number>/cancellation` | `202`: stops the answer |
| `POST /local_agent/chats/<number>/tool_calls/<id>/approval` `{"for_rest_of_chat":…}` | `202 {"chat":…}` |
| `POST /local_agent/chats/<number>/tool_calls/<id>/denial` `{"reason":…}` | `202 {"chat":…}` |
| `POST /local_agent/chats/<number>/tool_calls/<id>/input` `{"input":{…}}` | `202 {"chat":…}`: answers a question (no `input` for a page to visit) |
| `DELETE /local_agent/chats/<number>/tool_calls/<id>/input` | `202 {"chat":…}`: declines it |
| `POST /local_agent/uploads` (multipart, one `file`) | `{"signed_id","filename","byte_size","content_type"}`, or `422 attachment_refused` |
| `POST /local_agent/chats/<number>/retry` `{"message_id":…}` | `202 {"chat":…}`, or `409 chat_busy` |
| `POST /local_agent/chats/<number>/branches` `{"message_id":…}` | `201 {"chat":…}`: the new chat |
| `PATCH /local_agent/chats/<number>` `{"title":…}` | `{"chat":…}` |
| `DELETE /local_agent/chats/<number>` | `204` |
| `POST /local_agent/chats/<number>/share` | `201 {"url","expires_at","chat"}` |
| `DELETE /local_agent/chats/<number>/share` | `{"chat":…}`: stops sharing |
| `POST /local_agent/chat_access_request` | `202 {"granted","requested","approve_url"}` |
| `GET /assets/<path>` (no token, no proof) | The image a logo or icon names, served to anyone: only images, at most 1 MiB, never through a redirect |

Both ways of asking take an optional `model_id` (from `/local_agent/models`) and `attachments`, a list of `signed_id`s from uploads; a new chat also takes `project_id`. An unknown or unselectable model fails with `422 model_unavailable`. `message_id` is optional for retry and branch (the latest question or answer without one). Every chat's JSON carries `model` (`{"id","name"}` or null), `share` (`{"url","expires_at"}` or null) and `can` (`retry`, `branch`, `rename`, `delete`, `share`); a client treats a missing `can` as allowing none of them. `approvals` lists the changes the answer stopped at: `id`, `service`, `effect`, and either `decidable: true` with `summary`, `details` (`label`, `value`) and `allow_for_rest_of_chat`, or `decidable: false` with `waiting_for`. `questions` lists what a tool's server asked the person (MCP elicitation): `id`, `service`, and either `decidable: true` with `message`, `kind` (`form` with `fields`, or `url` with `url` and `host`) and a `note`, or `decidable: false` with `waiting_for`; a missing required field is `422 invalid`. Only the person who drove that turn decides, only while the call waits (`409 already_decided` otherwise). The upload is `multipart/form-data` and can take longer than other calls; files are at most 25 MB.

`403 chat_access_required` (with `requested` and `approve_url`) means the owner hasn't allowed chats. `403 insufficient_scope` means the token predates the permission: the daemon refreshes the token and retries once. `401` is handled the same way. Other codes (`locked`, `rate_limited`, `invalid`, `not_found`, `forbidden`) are passed to the terminal as they are. An answer that isn't this JSON means the server has no chat API, or not that call yet (an older server without models, uploads or the chat actions), and the terminal says which.

**Images.** Wherever the web shows a logo or a file-type icon, the JSON gives `{"path": "/assets/…", "monochrome": …}`: `logo` on a chat's `model`, on each model, on approvals, questions and activity steps, `logos` beside an activity's `services`, and `icon` on attachments, uploads and sources. When a client asks the daemon for one, the daemon fetches that path from the paired server's origin, without the token or a proof (the server serves `/assets/` to anyone), and through the proxy when there is one. It only asks for paths under `/assets/` made of letters, digits, `-`, `_`, `.` and `/`, never follows a redirect, and only takes an image of at most 1 MiB.

**Streaming.** With Action Cable framing, the daemon follows a chat by subscribing to one more channel on the same socket:

```
{"command":"subscribe","identifier":"{\"channel\":\"LocalAgent::ChatChannel\",\"chat\":\"42\"}"}
```

The server confirms it only for a device allowed to chat and a chat its owner may watch, and then sends updates as the channel's `message`: `{"type":"chunk","message_id":5,"text":"…"}` (answer text as it's written), `{"type":"progress","text":"…"}` (what the running step does), and `{"type":"changed"}` (anything else: read the chat again). The daemon subscribes once per chat however many terminals follow it, unsubscribes when the last one stops, and subscribes again after a reconnect. A refused chat channel never ends the session. With `mcp` framing there are no chat channels.

**The chat list.** While a terminal shows the list, the daemon also subscribes, once, to the list's channel, which takes no parameters:

```
{"command":"subscribe","identifier":"{\"channel\":\"LocalAgent::ChatsChannel\"}"}
```

The server confirms it for a device allowed to chat whose owner is an active member of its organization (never a guest), and rejects it otherwise. After the confirmation the terminal lists the chats once, then applies each `message` on that identifier: `{"event":"chat","chat":{…}}` (a chat started, renamed, starting or stopping being answered, switching model, getting or losing its public link, or moving in or out of a project: the same JSON as the list's, inserted by `number` or replaced, sorted by `updated_at`), `{"event":"removed","number":42}` (deleted, or out of the owner's sight), `{"event":"projects","projects":[…]}` (the whole list again), and `{"event":"account","account":{…},"user":{…},"credits":{…},"locked_reason":…}` (the list's header again). When the owner leaves a project, the server closes the socket instead; the daemon reconnects, subscribes again, and the terminal lists again on the confirmation. A rejected list channel never ends the session, and the next terminal to watch the list asks again. The daemon unsubscribes when the last terminal stops watching. A server without the channel never confirms it, and the terminal reads the list after each change it makes, as before.
