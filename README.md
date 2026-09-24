# Chat with Work Local Agent

The Chat with Work Local Agent (`cww` on the command line) is the open-source companion for [Chat with Work](https://chatwithwork.com). It lets the assistant search and read the folders you choose on your Mac or Linux machine, and nothing else.

- **Four read-only tools:** `roots`, `search`, `list` and `read`. There is no code that writes, deletes, or runs anything.
- **Only the folders you share,** and never the secrets inside them. SSH keys, `.env` files, keychains and browser profiles stay private even inside a shared folder.
- **Outbound only.** The daemon opens one WebSocket to Chat with Work. No port is opened on your machine, and no third-party relay sees your data.
- **Local index.** Full-text search (BM25) runs on your machine. Only the results of a specific tool call leave it: a few snippets, a directory listing, or a chunk of one file.
- **Auditable.** Every request is written to a local log you can follow live with `cww log -f`. The wire protocol is published in [PROTOCOL.md](PROTOCOL.md).

## Install

**Shell installer (macOS and Linux)**

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/crmne/chatwithwork-local-agent/releases/latest/download/cww-installer.sh | sh
```

**Homebrew**

```sh
brew install crmne/tap/cww
```

**From source** (Rust 1.90 or newer)

```sh
cargo install --locked --git https://github.com/crmne/chatwithwork-local-agent
```

Release archives come with SHA-256 checksums and GitHub artifact attestations. To check a download:

```sh
gh attestation verify cww-aarch64-apple-darwin.tar.xz --repo crmne/chatwithwork-local-agent
```

## Pairing

```sh
cww login
```

`cww login` creates a key for this computer and prints a short code:

```
To connect "carmine-mbp" to Chat with Work:

  1. Open https://chatwithwork.com/device
  2. Enter the code WDJB-MJHT

Key fingerprint: 3sQ2…
Waiting for approval...
```

Open the link, sign in, check that the computer name matches, and approve. Chat with Work emails you whenever a computer is connected. For a self-hosted server, use `cww login --server https://chat.example.com`.

The device key never leaves your computer. It is kept in the macOS Keychain or the Secret Service (GNOME Keyring, KWallet), or in a `0600` file on machines without either. To disconnect a computer, revoke it under **Settings ▸ Computers**; the daemon notices right away. Run `cww logout` to forget the pairing locally.

## Share folders

```sh
cww roots add ~/Documents/Work --label "Work docs"
cww roots list
cww roots remove work-docs
```

Share narrow folders: anything inside a shared folder can be read by the assistant, except files on the deny list. `cww` refuses to share `/`, your whole home folder, or system directories unless you pass `--i-know`.

Symlinks are not followed. `--follow-symlinks` allows links that stay inside the folder; links that point outside are refused either way.

## Run the daemon

```sh
cww daemon install     # systemd --user unit on Linux, LaunchAgent on macOS
cww status             # connection, roots and index state
cww pause              # refuse every request until...
cww resume
cww reload             # re-read config.toml after editing it
cww log -f             # follow the audit log
cww daemon uninstall   # add --purge to also delete keys, config, index and logs
```

`cww daemon run` runs in the foreground instead. Set `CWW_LOG=debug` for more output.

## Terminal UI

```sh
cww        # or cww tui
```

Run in a terminal, `cww` opens a terminal UI; piped, it prints the help. On the left: your shared folders with their index state, and the daemon's connection. On the right: the latest request from Chat with Work as it happens, and the full audit log, colour-coded by decision.

| Key | Does |
|---|---|
| `a` | Share a folder (prefilled with your Documents folder if it isn't shared) |
| `d`, `x`, `Delete` | Stop sharing the selected folder, after asking |
| `↑` `↓`, `j` `k` | Select a folder, or scroll the audit log |
| `p` | Pause or resume answering Chat with Work |
| `l` | Switch between the chat view and the audit log (`PgUp`, `PgDn`, `End` to follow) |
| `r` | Look for the daemon again |
| `?` | All the keys |
| `q`, `Ctrl-C` | Quit |

If nothing is shared yet, it offers your Documents folder and shares it only when you press `y`. If the daemon isn't running, it says so, shows `cww daemon install`, and works from `config.toml` and the audit file until the daemon starts; changes are saved there, like the `cww roots` commands. If the computer isn't paired, it shows `cww login`.

Chat doesn't work in the terminal yet: Chat with Work has no chat API for it, so the chat pane says so and links to the browser. [CHAT_API.md](CHAT_API.md) proposes the API it needs.

The UI uses no CPU while nothing happens, follows `NO_COLOR`, and falls back to 256 colours on terminals without true colour.

## What the assistant can do

| Tool | What it does |
|---|---|
| `roots` | Lists your shared folders by ID and label. The server never sees absolute paths. |
| `search` | Full-text search, 20 hits at most, with a snippet of up to 300 characters each. A live grep covers files that aren't indexed yet. |
| `list` | Lists one folder, 200 entries per page. |
| `read` | Returns the text of one file, 32 000 characters at a time. PDF, Word, PowerPoint and Excel files are converted to text. |

Paths always look like `work-docs:plans/q3.pdf`.

## Configuration

Everything lives in `~/.config/cww/config.toml`. The server can't change any of it.

```toml
paused = false

[[roots]]
id = "work-docs"
label = "Work docs"
path = "/Users/carmine/Documents/Work"
follow_symlinks = false

[deny]
extra = ["*.secret", "Clients/Confidential"]  # more patterns to block
remove = ["*.key"]                             # built-in patterns to drop (Keynote files use .key)
allow_hardlinks = false

[limits]
calls_per_minute = 120
read_chars_per_call = 32000
read_chars_per_chat_hour = 256000
read_chars_per_hour = 2000000

[index]
enabled = true
watch = true
rescan_secs = 1800
```

Run `cww reload` after editing the file. The `cww roots` commands reload the daemon for you.

| Path | Contents |
|---|---|
| `~/.config/cww/` | `config.toml`, and `secrets.json` if no keychain is available |
| `~/.local/share/cww/index/` | The search index (`0700`) |
| `~/.local/state/cww/audit.jsonl` | The audit log (`0600`, rotated at 10 MB, never uploaded) |
| `$XDG_RUNTIME_DIR/cww/cww.sock` | The control socket for the CLI (`0600`) |

The same layout is used on macOS. `CWW_HOME=/some/dir` puts all of it under one directory.

## Threat model

The rule behind the design: **every control that must hold against a compromised Chat with Work server lives in the daemon.** Server-side checks are only defense in depth.

### What it protects against

| Threat | How cww handles it |
|---|---|
| A compromised server, admin or database sends arbitrary tool calls | Only four read-only tools exist. Reads are confined to your shared folders and the deny list. Rate and volume limits, the audit log, and `cww pause` all run locally. |
| Prompt injection ("read ~/.ssh/id_ed25519") | Paths outside shared folders don't resolve at all. The deny list applies inside shared folders, and it can only be changed in the local config file. |
| Path tricks (`..`, absolute paths, symlinks, `/a/root2` vs `/a/root`, hard links) | Paths are parsed and rejected before any filesystem access. Resolution uses `openat2(RESOLVE_BENEATH \| RESOLVE_NO_MAGICLINKS \| RESOLVE_NO_SYMLINKS)` on Linux, and a component-by-component `O_NOFOLLOW` walk on macOS followed by a check of the opened handle's real path (`F_GETPATH`). Checks run on the opened handle, not on strings, which rules out the CVE-2025-53109/53110 class of bugs. Files with more than one hard link, FIFOs, sockets and devices are refused. |
| Other users on the same machine | The control socket is `0600` inside a `0700` directory, and connections from another UID are refused. Keys live in the OS keychain. |
| Network attackers | TLS with rustls and the Mozilla root store, HTTPS/WSS only, no redirects followed. Tokens live 10 minutes, travel in headers only, and are bound to the device key with DPoP proofs. |
| A stolen access token | It is useless without the device key: every connection needs a fresh proof signed by that key. |
| Revocation | Revoking a device in Settings closes its socket and makes every later token refresh fail. The daemon then stops reconnecting. |

**Default deny list:** matched on path components, case-insensitively, even inside shared folders.

`.ssh`, `.gnupg`, `.aws`, `.azure`, `.config/gcloud`, `.kube`, `.docker/config.json`, `.netrc`, `.npmrc`, `.pypirc`, `.git-credentials`, `.config/gh/hosts.yml`, `.env*`, `*.pem`, `*.key`, `*.p12`, `*.pfx`, `id_*`, `*.kdbx`, `*.1pux`, `.password-store`, `.local/share/keyrings`, `Library/Keychains`, `*.keychain`, `*.keychain-db`, `AppData/Roaming/Microsoft/Credentials`, `Library/Mail`, `Library/Messages`, browser profiles (Chrome, Chromium, Brave, Edge, Firefox, Safari, cookies), `/etc/shadow`, `/etc/gshadow`, `/etc/sudoers`, `/etc/ssl/private`, and cww's own config, index, and log directories.

### What it does not protect against

- **Anything inside a shared folder that isn't on the deny list can be read.** Share narrow folders. A secret stored in `notes.txt` is readable.
- **Results do reach Chat with Work and the model provider.** "Private" means your files and the index stay on your computer, and only the snippets and chunks needed for an answer leave it. Once they leave, the server's retention and sharing rules apply.
- **A malicious file could exploit a parser** (PDF, Office). Extraction runs with size caps and panic isolation. Running it in a separate, sandboxed process with no network access (Landlock/seccomp, Seatbelt) is planned for the next phase; the `reader` module is structured for that split.
- **Local malware running as your user** can read your files directly and doesn't need cww.
- **Content-based secret redaction** (API keys inside ordinary text files) and "ask before every read" approvals are planned, not built.

### Planned hardening

- Split into a network process and a sandboxed reader process (Landlock + seccomp on Linux, Seatbelt on macOS).
- A signed and notarized macOS app bundle registered through `SMAppService`, so folder permissions survive updates.
- Local approval prompts, secret redaction, Windows support, reproducible builds, and an external audit before general availability.

## Development

```sh
cargo test                                  # unit tests and the end-to-end test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
cargo deny check                            # licenses, advisories, bans, sources
```

`tests/e2e.rs` runs a real daemon against a fake Chat with Work server: DPoP-checked token endpoint, device flow, and an Action Cable WebSocket. It covers reads outside the roots, denied secrets, symlink and hard-link escapes, pause, and revocation.

| Module | Role |
|---|---|
| `reader` | Everything that touches your files: safe path resolution, extraction, the index, grep, watching. This is the future sandboxed process. |
| `tools` | The MCP server (via [`rmcp`](https://github.com/modelcontextprotocol/rust-sdk)): four tools, limits, audit. |
| `tunnel` | The outbound WebSocket, framing, reconnects. |
| `auth` | Device key, DPoP proofs, device flow, secret storage. |
| `daemon`, `control`, `service` | The daemon, its control socket, and systemd/launchd registration. |
| `tui` | The terminal UI, a client of the control socket. Screens are pinned as text snapshots in `src/tui/snapshots`; `UPDATE_SNAPSHOTS=1 cargo test` rewrites them. |

Releases are built by [dist](https://github.com/axodotdev/cargo-dist) (`dist-workspace.toml`, `.github/workflows/release.yml`) when a `v*` tag is pushed.

## Security

See [SECURITY.md](SECURITY.md) for how to report a vulnerability.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option. Unless you explicitly state otherwise, any contribution you intentionally submit for inclusion in this project, as defined in the Apache-2.0 license, is dual licensed as above, without any additional terms or conditions.

"Chat with Work" is a trademark of Plenty UG. The license covers the code, not the name.
