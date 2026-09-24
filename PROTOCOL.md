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

## 1. Roles and overview

- The **daemon** runs on the user's computer. It is an MCP *server* that exposes four read-only tools.
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
- TLS uses rustls with the Mozilla (webpki) root store. The daemon never follows redirects: a 3xx from any endpoint is an error.

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
&scope=local_agent:serve
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

- The access token is opaque to the daemon. The design calls for a signed token with a 10-minute lifetime, `aud` equal to the server origin, `scope=local_agent:serve`, and binding to the device key (`cnf.jkt`). That scope MUST NOT grant access to any other API.
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
3. The server answers `{"identifier":"…","type":"confirm_subscription"}`, or `reject_subscription`, which the daemon treats as unauthorized (see 6.4).
4. **Server to daemon:** each JSON-RPC message is the `message` of a channel transmission:
   `{"identifier":"{\"channel\":\"LocalAgent::Channel\"}","message":{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{…}}}`.
   `message` SHOULD be a JSON object. A string containing serialized JSON is also accepted. Frames for other identifiers are ignored.
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
| `tools/list` | The four tools (section 8), in one page. |
| `tools/call` | See section 8. |
| `notifications/initialized`, `notifications/cancelled` | Accepted. Cancelling abandons a call in progress. |
| `resources/*`, `prompts/*`, `completion/complete`, `logging/setLevel`, anything else | `-32601` method not found. |

The daemon ignores JSON-RPC responses from the server, since it never sends requests, and ignores unknown notifications.

### 7.4 Tool naming on the server

The daemon's tool names are `roots`, `search`, `list` and `read`, with no prefix. The server namespaces them per device, for example `local_<device_id>_read`, following its existing `mcp_<id>_` convention.

## 8. Tools

Every tool is annotated `readOnlyHint: true`, `destructiveHint: false`, `idempotentHint: true`, `openWorldHint: false`. Unknown argument keys are rejected with `invalid_argument`.

### 8.1 Paths

Every path is **`<root_id>:<relative path>`**. The daemon rejects the following before touching the filesystem:

| Rule | Error |
|---|---|
| No `:`, or a root ID outside `[a-z0-9][a-z0-9_-]{1,31}` (2 to 32 characters; `C:` never parses as a root) | `invalid_path` |
| Absolute paths (leading `/`), a leading `~`, or a relative part starting with `/` | `invalid_path` |
| `..` in any component | `invalid_path` |
| NUL bytes or backslashes anywhere | `invalid_path` |
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
    { "id": "work-docs", "label": "Work docs", "available": true, "index": "ready", "indexed_files": 1532 }
  ]
}
```

`index` is `pending`, `indexing`, `ready`, `error` or `disabled`. `available` is false when the folder is missing, for example on an unmounted drive. Labels are chosen by the user and are the only description of a root the server gets.

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
| `not_a_file` / `not_a_directory` | Wrong kind of entry for `read` / `list`. |
| `too_large` | The file is over the size limit. |
| `unsupported` | The file can't be turned into text (binary, encrypted or damaged). |
| `rate_limited` | A daemon-side rate or volume limit was hit. Try again later. |
| `paused` | The user ran `cww pause`. |
| `internal` | Unexpected failure. |

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
