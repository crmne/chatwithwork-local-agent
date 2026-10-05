# Chat with Work Local Agent

The Chat with Work Local Agent (`cww` on the command line) is the open-source companion for [Chat with Work](https://chatwithwork.com). It lets the assistant search and read the folders you choose on your Mac, Windows PC or Linux machine, and nothing else, and change files only in the folders where you allow it.

- **Read-only unless you say otherwise:** four read-only tools, `roots`, `search`, `list` and `read`. Turn on "Allow changes" for a shared folder, on your computer, and the assistant can also create, edit, move and delete files there, each change approved by you in Chat with Work. Old versions and deleted files go to your system trash; nothing is deleted for good, and no programs, launchers or macro documents are ever written.
- **Only the folders you share,** and never the secrets inside them. SSH keys, `.env` files, keychains and browser profiles stay private even inside a shared folder.
- **Outbound only.** The daemon opens one WebSocket to Chat with Work. No port is opened on your machine, and no third-party relay sees your data.
- **Local index.** Full-text search (BM25) runs on your machine. Only the results of a specific tool call leave it: a few snippets, a directory listing, or a chunk of one file.
- **Auditable.** Every request is written to a local log you can follow live with `cww log -f`. The wire protocol is published in [PROTOCOL.md](PROTOCOL.md).

## Install

All packages include **Chat with Work**, the desktop app, **`cww`**, the terminal interface, and the background agent. Nothing is shared until you pair the computer and choose a folder. Linux and macOS run the daemon as `cww daemon run`; Windows also includes `cww-agent.exe` so the daemon runs without a console window.

| Platform | Install | Open |
|---|---|---|
| macOS 11 or later | `brew install --cask crmne/tap/chat-with-work`, or download the `.pkg` or `.dmg` from the [latest release](https://github.com/crmne/chatwithwork-local-agent/releases/latest) | **Chat with Work** in Applications. With the `.dmg`, drag the app to Applications first; its `cww` command is inside `Chat with Work.app/Contents/MacOS/`. |
| Windows 10 and 11 | `winget install ChatWithWork.LocalAgent`, or download the `.msi` from the latest release | **Chat with Work** or **Chat with Work (Terminal)** in the Start menu. `cww` is also on your PATH. |
| Arch | `yay -S chatwithwork-local-agent-bin` (or `chatwithwork-local-agent` to build from source, `chatwithwork-local-agent-git` for development builds) | **Chat with Work** in the applications menu, or `cww` in a terminal. |
| Debian, Ubuntu | Download the `.deb`, then `sudo apt install ./chatwithwork-local-agent_*.deb` | **Chat with Work** in the applications menu, or `cww`. |
| Fedora, RHEL, openSUSE | Download the `.rpm`, then `sudo dnf install ./chatwithwork-local-agent-*.rpm` | **Chat with Work** in the applications menu, or `cww`. |
| NixOS, Nix (Linux, Apple silicon macOS) | `nix profile install github:crmne/chatwithwork-local-agent` | `cww-app` or `cww`; the package also carries desktop integration. |
| Linux, Homebrew | `brew install crmne/tap/cww` | `cww-app` or `cww`. Add the Homebrew prefix's `share` directory to your desktop session's `XDG_DATA_DIRS` for menu discovery. |

Linux desktop binaries need glibc 2.35 or newer (Ubuntu 22.04+, Debian 12+, Fedora 36+, Rocky/RHEL 10+); the packages install the graphics libraries they need. A tray that shows StatusNotifierItems, such as Waybar, KDE, or GNOME with the AppIndicator extension, keeps the app available when its window closes. Install your desktop's XDG portal backend for the native folder picker. The separate `cww-vX.Y.Z-<arch>-unknown-linux-musl.tar.gz` downloads remain available for terminal-only use on older distributions and servers.

**Starting the background agent**

| Installation | Startup |
|---|---|
| Windows MSI / WinGet / PowerShell, per-user Scoop | The installer registers and starts the agent for your account, now and at every login. |
| macOS `.pkg` | The installer tries to register and start the agent for the logged-in console user. If nobody is logged in, finish setup after login. |
| Linux packages, Nix, Homebrew, portable app | Open the app and choose **Start Local Agent**, or run `cww daemon install` as your own user. System-wide package installation cannot choose which user account should run the agent. |
| macOS / Linux one-line installer | Tries `cww daemon install` for the installing user and prints recovery instructions if no user service manager is available. |

`cww daemon install` starts the agent now, enables it at future logins, and restarts an existing service to use the installed version. If automatic setup failed, run that command without `sudo`. On Linux, check it with `systemctl --user status cww.service`; after a package upgrade, restart it with `systemctl --user restart cww.service`. On any platform, `cww status` reports the agent's state. Without a user service manager, keep `cww daemon run` running in a terminal. Closing the desktop app does not stop the background service.

**One line, macOS or Linux, no root**

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/crmne/chatwithwork-local-agent/releases/latest/download/cww-installer.sh | sh
```

It checks release checksums, installs `cww` and `cww-app` into `~/.local/bin`, and registers the desktop launcher and icon on Linux. On macOS the app goes in `~/Applications`. Set `CWW_INSTALL_DIR` to change the command directory. The installer uses `XDG_DATA_HOME` for Linux desktop files and `XDG_CACHE_HOME` for temporary downloads, defaulting to `~/.local/share` and `~/.cache`.

**Windows without WinGet**

```powershell
powershell -ExecutionPolicy Bypass -c "irm https://github.com/crmne/chatwithwork-local-agent/releases/latest/download/cww-installer.ps1 | iex"
```

Windows builds are not signed yet. If SmartScreen warns, choose **More info**, then **Run anyway**. Portable Windows zips also contain all three executables; extract them to a permanent folder before registering the agent. macOS app downloads are signed with Developer ID and notarized; the `.pkg` needs the separate Installer signing identity.

**Portable Linux desktop app**

The `cww-app-vX.Y.Z-<arch>-unknown-linux-gnu.tar.gz` archive contains both executables, the service, launcher, icon, documentation and licenses. Put `cww-app` and `cww` on your PATH, `cww-app.desktop` in `~/.local/share/applications/`, and `cww-app.svg` in `~/.local/share/icons/hicolor/scalable/apps/`. Then run `cww daemon install`.

**From source** (Rust 1.98 or newer for the desktop app)

```sh
cargo install --locked --path .
cargo install --locked --path app
```

Run these commands from a checkout. They install both commands; the packages or one-line installer also provide desktop integration. The terminal agent alone needs Rust 1.90 or newer.

Release archives come with SHA-256 checksums, signed for the desktop app's updater, and GitHub artifact attestations. To check a download:

```sh
gh attestation verify cww-app-v0.3.0-x86_64-unknown-linux-gnu.tar.gz --repo crmne/chatwithwork-local-agent
```

[PACKAGING.md](PACKAGING.md) explains packaging, signing, startup and removal. Open the desktop app or run `cww` to pair and choose folders, or follow the steps below.

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

Open the link, sign in, check that the computer name matches, and approve. When nothing is shared yet, `cww login` then offers to share your Documents folder, and only does so if you answer yes. Chat with Work emails you whenever a computer is connected. For a self-hosted server, use `cww login --server https://chat.example.com`. `cww` trusts the certificates your operating system trusts, so a server behind a company CA or a local development CA works once that CA is installed; `SSL_CERT_FILE` points it at a different bundle.

The device key never leaves your computer. It is kept in the macOS Keychain, the Windows Credential Manager, or the Secret Service (GNOME Keyring, KWallet), or in a `0600` file on machines without any of them. To disconnect a computer, revoke it under **Settings ▸ Computers**; the daemon notices right away. Run `cww logout` to forget the pairing locally.

## Share folders

```sh
cww roots add ~/Documents/Work --label "Work docs"
cww roots list
cww roots allow-changes work-docs   # let Chat with Work change files there too
cww roots deny-changes work-docs    # read-only again
cww roots remove work-docs
```

Share narrow folders: anything inside a shared folder can be read by the assistant, except files on the deny list. `cww` refuses to share `/`, your whole home folder, or system directories unless you pass `--i-know`.

A shared folder is read-only until you allow changes in it (and a folder you share read-only stays read-only even inside one that allows changes), with `cww roots allow-changes` (or `--allow-changes` on `add`), the `w` key on the terminal UI's Shared folders page, or the "Allow changes" switch in the desktop app. Then the assistant can create, edit, move and delete files there. Chat with Work asks you before each change, the previous version of anything replaced and everything deleted goes to the system trash (the Trash on macOS, the Recycle Bin on Windows, your desktop's trash on Linux), and the activity log records every change. Nothing on the server can turn changes on. See [docs/file-changes.md](docs/file-changes.md).

Symlinks are not followed. `--follow-symlinks` allows links that stay inside the folder; links that point outside are refused either way.

## Run the daemon

`cww login` starts the daemon after pairing (pass `--no-daemon` to skip that).

```sh
cww daemon install     # register it again: systemd --user unit on Linux, LaunchAgent on macOS, logon task on Windows
cww status             # connection, roots and index state
cww pause              # refuse every request until...
cww resume
cww reload             # re-read config.toml after editing it
cww log -f             # follow the audit log
cww daemon stop        # stop it until the next logon
cww daemon uninstall   # add --purge to also delete keys, config, index and logs
```

`cww daemon run` runs in the foreground instead. Set `CWW_LOG=debug` for more output. With Homebrew, `brew services start cww` works too. On Windows the logon task runs `cww-agent.exe`, the same daemon without a console window, and it logs to `%LOCALAPPDATA%\cww\state\daemon.log`. If Task Scheduler denies access, setup uses your account's Startup folder instead and starts the agent immediately, without administrator rights. This fallback starts at login but does not restart the agent after a crash; open the app and choose **Start Local Agent** if needed. `cww daemon uninstall` removes either registration.

The CLI, the terminal UI and the settings app talk to the daemon over a local socket (a named pipe on Windows) that only your user can open. [CONTROL.md](CONTROL.md) documents it for other clients.

## Terminal UI

```sh
cww        # or cww tui
```

Run in a terminal, `cww` opens a terminal UI laid out like the desktop app's chat page. On the left: New chat, the search, Recent (your chats, newest first), and Pinned under it as on the web: the projects you pinned (picking one shows its chats and starts a new chat in it; picking it again shows every chat), then the chats you pinned, with your name at the foot, and your credits under it when they run low. The list stays current as chats start, change or go, on the web too. On the right, with no title over it, as on the web: the open chat and the composer, with the model on its edge. Your name, `,` or `/settings` open the settings, with the same pages as the desktop app: Shared folders (with their index state), Always private (the deny list, which only `config.toml` changes), Activity (every request from Chat with Work, colour-coded by decision), Account (the pairing, the connection, whether chats are allowed here, your organization and credits, and the web's own settings pages, which open in the browser) and General (the daemon, pausing, and where its files are). Nothing else stays on screen: a single word at the top right says when something needs you (the daemon stopped, offline, connecting, paused, not paired, revoked, or the open chat not live), and clicking it opens the page that helps.

| Key | Does |
|---|---|
| `Tab` | Move between the chats and the composer |
| `↑` `↓`, `j` `k`, `Enter` | Select and open a chat |
| `n` | New chat |
| `/` | Search chats |
| `e` | Show every tool step under its activity line |
| `o` | Open the chat in the browser, or ask to use your chats here |
| `,` | Open the settings, or go back to the chats |
| `Tab`, `←` `→`, `1` to `5` | In the settings: the next, previous or numbered page |
| `l` | The activity log (`PgUp`, `PgDn`, `End` to follow) |
| `a` | Share a folder (prefilled with your Documents folder if it isn't shared) |
| `r` | In Shared folders, rename the selected folder; elsewhere, look for the daemon again |
| `d`, `x`, `Delete` | In Shared folders, stop sharing the selected folder, after asking; in Account, disconnect this computer |
| `p` | Pause or resume answering Chat with Work |
| `c` | Pair this computer: shows the code, opens the approval page in your browser, and waits |
| `s` | Start the daemon in the background (`cww daemon install`) when it isn't running |
| `Esc` | Back to the chats |
| `?` | All the keys and commands |
| `q` | Quit |
| `Ctrl-C` | Clear what's typed; with nothing to clear, twice in a row quits |
| `Ctrl-L` | Draw the screen again |

In the composer, keys work as in Claude Code:

| Key | Does |
|---|---|
| `Enter` | Send, or run a `/` command |
| `Shift-Enter`, `Alt-Enter`, `Ctrl-J`, or `\` then `Enter` | A new line (`Shift-Enter` where the terminal tells it apart from `Enter`) |
| `↑` `↓` | Move between lines; from the first or last line, earlier questions |
| `←` `→`, `Home` `End`, `Ctrl-A` `Ctrl-E` | Move along the line |
| `Ctrl-U`, `Ctrl-K`, `Ctrl-W` | Delete to the start of the line, to its end, or the word before |
| `Esc` | Close the command list; stop an answer being written; or go back to the chats |

Typing `/` lists the commands and narrows them as you type; `↑` `↓` select, `Tab` completes and `Enter` runs one. `//` at the start asks something that begins with a slash.

| Command | Does |
|---|---|
| `/new` (`/clear`) | Start a new chat |
| `/resume` (`/chats`) | Pick a recent chat from a list you can filter |
| `/search <text>` | Search your chats in the sidebar |
| `/model [name]` | Pick the model for this chat, or the next new one, from what your plan offers; models you can't use now say why |
| `/project [name]` | Start the next chat in a project (`/project none` for none) |
| `/attach <path>`, `/detach` | Attach a file to your next question (up to 25 MB), or remove the attached files |
| `/copy` | Copy the last answer to the clipboard |
| `/open` | Open the chat in the browser |
| `/retry`, `/branch`, `/rename [title]`, `/delete` | Answer the last question again, continue the chat in a new one, rename it, or delete it (after asking), where Chat with Work allows it |
| `/share-chat`, `/unshare-chat` | Make a public link to the chat and copy it, or stop sharing it |
| `/steps` | Show or hide every tool step |
| `/settings` | Open the settings |
| `/folders`, `/share [path]`, `/unshare [folder]` | Show your shared folders, share one, or stop sharing one (after asking) |
| `/pause`, `/resume-sharing` | Refuse Chat with Work's requests for now, or answer them again |
| `/log`, `/status` | Show the activity log, or say how this computer is connected |
| `/login`, `/logout` | Pair this computer, or disconnect it by forgetting its pairing (after asking) |
| `/help`, `/exit` (`/quit`) | All the keys and commands; quit |

When an answer stops at a change that needs your approval (posting to Slack, creating an issue), the chat shows what it will do and asks: `↑` `↓` and `Enter`, or `y` to approve, `a` to approve it for the rest of the chat where that's allowed, `n` to deny, or deny and say what to do instead. When a connected service asks you something while its tool runs, the chat shows its question as a small form: `↑` `↓` move between the fields, `←` `→` go through a field's choices, `Enter` types a value, then Send or Decline; a service that needs you to open a page offers to open it in the browser. In a project chat, it says whose answer the chat waits for.

The mouse works too: click a chat, the search, your name, a settings page, a folder, the composer, a command, a picker's row, an answer to an approval, or a link in an answer; the wheel scrolls the chat, the lists and the activity log. While the TUI has the mouse, hold `Shift` (or your terminal's own modifier, such as `Option` in iTerm2) and drag to select text, or set `[tui] mouse = false` in `config.toml` to leave the mouse to the terminal.

Questions you ask, and the commands you run, are kept in `~/.local/state/cww/history` (only readable by you, the newest 500) so `↑` recalls them in the next run. Files you attach are read by `cww` itself, not the sandboxed daemon, and uploaded through the daemon only when you attach them; the clipboard gets text through your terminal (OSC 52), so it also works over SSH where the terminal allows it.

A first run is three keys: `c` to pair, `s` to start the daemon if the installer didn't, and `y` to share your Documents folder, which it offers when nothing is shared and never shares on its own. Until the daemon runs, the UI works from `config.toml` and the audit file, and changes are saved there, like the `cww roots` commands. `CWW_SERVER=https://chat.example.com cww` pairs with a self-hosted server.

Chats in the terminal are a separate permission from sharing folders. `cww login` asks for both, and the approval page lets you decline chats (`cww login --no-chats` doesn't ask). A computer paired without them asks when you press `o`, and you allow it in Chat with Work under Settings, Computers; you can turn it off there at any time. The TUI never holds a token: the daemon asks Chat with Work for it, and answers stream over the daemon's connection. The logos and file-type icons chats show (in the desktop app) come from the paired server too: the daemon fetches only images under its `/assets/`, without the token, at most 1 MiB each and never through a redirect. [CHAT_API.md](CHAT_API.md) explains the design.

The UI uses no CPU while nothing happens, follows `NO_COLOR`, and falls back to 256 colours on terminals without true colour.

## Desktop app

`cww-app` is a menu bar (macOS), notification area (Windows) or tray (Linux) app for the same daemon. It shows whether Chat with Work can reach this computer, pauses and resumes sharing, and has a window for your Chat with Work chats, drawn as the web draws them (models, attached files, approvals, and retry, branch, rename, delete and share included), and for shared folders, the deny list, the activity log, pairing, and start at login. On first run it starts the agent, pairs, and offers to share your Documents folder, and it shares nothing until you say so. The welcome page is centered, and the window uses one continuous header with native macOS traffic lights or integrated minimize, maximize and close controls on Windows and Linux. Closing the window keeps the tray app and agent running. It uses no CPU while idle.

```sh
cargo run --release -p cww-app
```

Install it as described under [Install](#install). It is a client of the control channel, like the terminal UI. See [docs/settings-app.md](docs/settings-app.md).

## What the assistant can do

| Tool | What it does |
|---|---|
| `roots` | Lists your shared folders by ID and label. The server never sees absolute paths. |
| `search` | Full-text search, 20 hits at most, with a snippet of up to 300 characters each. A live grep covers files that aren't indexed yet. |
| `list` | Lists one folder, 200 entries per page. |
| `read` | Returns the text of one file, 32 000 characters at a time. PDF, Word, PowerPoint and Excel files are converted to text. |

In folders where you allow changes, and only while at least one folder does:

| Tool | What it does |
|---|---|
| `create` | Creates a text file. Never overwrites anything. |
| `write` | Replaces a text file's content, or adds to its end. The previous version goes to the trash. |
| `edit` | Replaces one exact passage of a text file. The previous version goes to the trash. |
| `mkdir` | Creates a folder. |
| `move` | Moves or renames a file or folder, within or between folders that allow changes. A file it replaces goes to the trash. |
| `delete` | Moves a file or folder to the system trash. |
| `create_document` | Creates a Word document from Markdown, or an Excel workbook from rows of values (never formulas). Replacing one puts the old file in the trash; existing documents are never edited in place. |

Chat with Work asks before each change; "Allow for the rest of this chat" can cover writes, never a replace, a move over a file, or a delete. Programs, scripts that run when opened, shortcuts and Office files with macros are never made, no file gets an execute bit, and version control internals, shell startup files, autostart folders and editor task settings are never changed.

Paths always look like `work-docs:plans/q3.pdf`.

## Performance

The daemon does nothing until something happens. There is no polling: the index follows file events (inotify, FSEvents, ReadDirectoryChangesW) and waits for a folder to be quiet for a moment before it updates, and only the directories that changed are rescanned. The only regular wake-ups are the server's WebSocket pings.

Measured on real Documents folders, connected to a Chat with Work server:

| | Linux | macOS | Windows |
|---|---|---|---|
| Machine | Ryzen 9 9900X, SATA SSD, Arch | Mac mini M4, macOS 27 | Zenbook S16, Ryzen AI 9 HX 370, Windows 11 |
| Folder | 15 GB, 10,504 files indexed | 37 GB, 172,974 files indexed | 427 MB, 8,417 files indexed |
| First index | 107 s from a cold disk, 15 s cached; 17 CPU s | 256 s cold, 115 s cached; 55 CPU s | 156 s cold (Defender scans each file once), 1.5 s cached; 9 CPU s |
| Index on disk | 160 MB | 242 MB | 2 MB (mostly binary files, indexed by name) |
| Search, 20 hits with snippets | 1.6 to 11 ms, median 5.6 ms | 3.6 to 52 ms, median 12 ms | 4 to 5 ms |
| Idle CPU over 5 minutes | under 0.01 s | 0.01 s | 0.17 s (small folder) |
| Memory while idle | 77 MB resident, 55 MB heap | 29 to 66 MB footprint | 31 MB working set |

A search takes about 100 ms end to end through Chat with Work, most of it on the server. When the index has no answer, a live grep runs as well; over a folder of thousands of files that aren't text, that can take a second or more (at most 5 seconds). Indexing peaks at a few hundred megabytes while large PDFs and spreadsheets are extracted, and the memory is handed back when the pass ends.

## Configuration

Everything lives in `~/.config/cww/config.toml`. The server can't change any of it.

```toml
paused = false
proxy = "http://proxy.example:3128"  # optional; see "Behind a proxy"

[[roots]]
id = "work-docs"
label = "Work docs"
path = "/Users/carmine/Documents/Work"
follow_symlinks = false
writable = false      # true lets Chat with Work change files here (cww roots allow-changes)

[deny]
extra = ["*.secret", "Clients/Confidential"]  # more patterns to block
remove = ["*.key"]                             # built-in patterns to drop (Keynote files use .key);
                                               # also drops paths from the never-changed list
allow_hardlinks = false                        # reads only: changes never touch hard links

[limits]
calls_per_minute = 120
read_chars_per_call = 32000
read_chars_per_chat_hour = 256000
read_chars_per_hour = 2000000
changes_per_minute = 30          # changes: create, write, edit, mkdir, move, delete
changes_per_day = 500
change_bytes_per_hour = 52428800 # 50 MiB written by changes
max_change_file_bytes = 10485760 # largest file a change writes or copies
max_change_entries = 1000        # most files a folder move or delete may carry

[index]
enabled = true
watch = true          # follow file events; the index sleeps between them
rescan_secs = 1800    # full rescan interval, only when watching is off or unavailable

[sandbox]
enabled = true        # Landlock on Linux, Seatbelt on macOS

[tui]
mouse = true          # clicks and the wheel in cww tui; false leaves the mouse to the terminal
```

Run `cww reload` after editing the file. The `cww roots` commands reload the daemon for you.

| Path | Contents |
|---|---|
| `~/.config/cww/` | `config.toml`, and `secrets.json` if no keychain is available |
| `~/.local/share/cww/index/` | The search index (`0700`) |
| `~/.local/share/cww/chat-assets/` | The logos and file-type icons the desktop app's chats show, as the paired server sent them, kept so each is fetched once (`0700`, files `0600`) |
| `~/.local/state/cww/audit.jsonl` | The audit log (`0600`, rotated at 10 MB, never uploaded) |
| `~/.local/state/cww/history` | The questions and commands typed in `cww tui`, for `↑` (`0600`, never uploaded) |
| `$XDG_RUNTIME_DIR/cww/cww.sock` | The control socket for the CLI (`0600`) |
| `~/.local/share/Trash` (`$XDG_DATA_HOME/Trash`) | Where changes put old versions and deleted files on Linux: the desktop's own trash, or `.Trash-$uid` at the top of another filesystem. `~/.Trash` (or a volume's `.Trashes`) on macOS, the Recycle Bin on Windows. |

The same layout is used on macOS. On Windows everything lives under `%LOCALAPPDATA%\cww` (`config`, `data`, `state`), and the control channel is a named pipe. `CWW_HOME=/some/dir` puts all of it under one directory.

## Behind a proxy

Some networks only let traffic out through an HTTP proxy. `cww` sends pairing, token refreshes, the chats, the images they show and the WebSocket through one with HTTP `CONNECT`, so TLS still runs end to end between `cww` and Chat with Work: the proxy sees the server's host name and nothing else.

The daemon runs as a background service (systemd, launchd, or a Windows logon task), and services don't inherit your shell's environment. So set the proxy in `config.toml`, at the top of the file, before any `[section]`:

```toml
proxy = "http://proxy.example:3128"
# or with credentials, sent as Basic auth:
proxy = "http://alice:s3cret@proxy.example:3128"
```

Then run `cww reload` (or `cww login` if you haven't paired yet). `proxy = "none"` connects directly whatever the environment says. `config.toml` is readable only by you (`0600`), but a proxy password in it is stored in plain text; percent-encode special characters in the user name or password (`@` as `%40`). An `https://` proxy URL also encrypts the hop to the proxy. SOCKS proxies and NTLM or Kerberos authentication aren't supported.

Without `proxy` in the config, `cww` reads the environment as curl does: `https_proxy` or `HTTPS_PROXY` (`http_proxy` or `HTTP_PROXY` for a plain `http://` development server), then `all_proxy` or `ALL_PROXY`, unless the server matches `no_proxy` or `NO_PROXY` (comma-separated host names, which also match their subdomains; `.example.com` and `*.example.com` work too, as do IP addresses, CIDR ranges like `10.0.0.0/8`, `host:port`, and `*` for everything). That covers `cww login` and `cww daemon run` in a shell. `cww login` and `cww daemon install` warn when a proxy variable is set in the shell but not in `config.toml`.

`cww status` (and `--json`, and the terminal UI) show the proxy in use and where it came from, never the password:

```
Server:     https://chatwithwork.com
Proxy:      http://alice:***@proxy.example:3128 (from config)
```

`cww` doesn't read the proxy settings of macOS or Windows (System Settings, PAC files, WPAD); copy the proxy's address into `config.toml`. A proxy that inspects TLS needs its CA in the operating system's trust store, as described under [Pairing](#pairing).

## Threat model

The rule behind the design: **every control that must hold against a compromised Chat with Work server lives in the daemon.** Server-side checks are only defense in depth.

### What it protects against

| Threat | How cww handles it |
|---|---|
| A bug in cww, or a malicious document that exploits a parser | The daemon runs in a kernel sandbox: Landlock on Linux, Seatbelt on macOS. It can read the shared folders, its own directories and system libraries, and write only its own directories plus, when you allow changes, exactly those folders and the trash their files go to (create, write, rename and remove, never execute), so the rest of your home folder (`~/.ssh`, other projects, the keychain files) is out of reach even for code the daemon didn't mean to run. `cww status --json` shows the sandbox's state. |
| A compromised server, admin or database sends arbitrary tool calls | Unless you allow changes in a folder, only four read-only tools exist. Reads are confined to your shared folders and the deny list. Rate and volume limits, the audit log, and `cww pause` all run locally. For folders where you allow changes, see below. |
| A change escaping its folder, or hitting what it shouldn't | Changes open their folder one folder at a time, never through a symlink or junction, and check every folder opened by its real path (as the kernel reports it, so case variants and 8.3 short names can't dodge the lists) and its identity (so a folder shared read-only stays read-only whatever name reaches it), then create, rename and remove relative to that open folder (`O_CREAT \| O_EXCL \| O_NOFOLLOW`, `renameat2(RENAME_NOREPLACE)`, `renamex_np(RENAME_EXCL)`, handle renames without `ReplaceIfExists` on Windows). Links, hard links, FIFOs, sockets, devices, executables, read-only files and other users' files are never changed. The deny list covers every source and destination, and everything inside a folder that is moved or deleted, where it is and where it would land; so does a second built-in list, which keeps the home folder's dot files, Git internals, shell startup files, autostart and launch agents, and editor, interpreter and hook settings that run code unchanged. No programs, launchers, shortcuts, macro documents or documents that make Office run or fetch things (`.rtf`, `.mht`, `.slk`, `.iqy`, Office XML) are written, and never an execute bit; every file cww makes carries the Mark-of-the-Web on Windows and the quarantine mark on macOS, as a download would. |
| Losing files to a change | Nothing is deleted for good: every replaced version and every deleted file goes to the system trash, and a change the trash can't take is refused. Writes go to a temporary file that is flushed, then swapped with the old version in one step where the filesystem allows (keeping its group, extended attributes and ACLs) or renamed into place, so a crash never leaves half a file. |
| Prompt injection ("read ~/.ssh/id_ed25519") | Paths outside shared folders don't resolve at all. The deny list applies inside shared folders, and it can only be changed in the local config file. |
| Path tricks (`..`, absolute paths, symlinks, `/a/root2` vs `/a/root`, hard links) | Paths are parsed and rejected before any filesystem access. Resolution uses `openat2(RESOLVE_BENEATH \| RESOLVE_NO_MAGICLINKS \| RESOLVE_NO_SYMLINKS)` on Linux, a component-by-component `O_NOFOLLOW` walk on macOS followed by a check of the opened handle's real path (`F_GETPATH`), and on Windows a walk that opens each component as a reparse point, refuses symlinks and junctions, and checks the final handle's real path (`GetFinalPathNameByHandleW`), which also defeats 8.3 short names and alternate data streams. Checks run on the opened handle, not on strings, which rules out the CVE-2025-53109/53110 class of bugs. Files with more than one hard link, FIFOs, sockets and devices are refused. |
| Other users on the same machine | The control socket is `0600` inside a `0700` directory, and connections from another UID are refused. On Windows the named pipe admits only the user's SID. Keys live in the OS keychain. |
| Network attackers | TLS with rustls, trusting the operating system's certificate store plus Mozilla's roots, HTTPS/WSS only, no redirects followed. Tokens live 10 minutes, travel in headers only, and are bound to the device key with DPoP proofs. |
| A stolen access token | It is useless without the device key: every connection needs a fresh proof signed by that key. |
| Revocation | Revoking a device in Settings closes its socket and makes every later token refresh fail. The daemon then stops reconnecting. |

**Allowing changes trusts the server's approvals.** Chat with Work asks you before each change, but that approval happens on the server, and the daemon can't tell a real approval from a forged one. So a compromised server could change files in folders where you allow changes, without asking you. What limits the damage, whatever the server does: changes are off unless you turn them on for a folder, on your computer; everything replaced or deleted is in the system trash, where you can restore it; changes have their own budgets (30 a minute, 500 a day, 50 MB written an hour, 10 MB per file); every change is in the local audit log with what it did and where the old version went; and the deny list and the never-changed list hold. Allow changes only in folders whose contents you could restore, and `cww pause` (or the switch) stops everything at once.

**Default deny list:** matched on path components, even inside shared folders, ignoring case the way case-insensitive filesystems do: names are compared in full Unicode case folding and NFD, without zero-width and direction marks, so `.Kube` (with a Kelvin sign), `.zſhrc` or a decomposed accent match the name they stand for. The never-changed list below is matched the same way.

`.ssh`, `.gnupg`, `.aws`, `.azure`, `.config/gcloud`, `.kube`, `.docker/config.json`, `.netrc`, `.npmrc`, `.pypirc`, `.git-credentials`, `.config/gh/hosts.yml`, `.env*`, `*.pem`, `*.key`, `*.p12`, `*.pfx`, `id_*`, `*.kdbx`, `*.1pux`, `.password-store`, `.local/share/keyrings`, `Library/Keychains`, `*.keychain`, `*.keychain-db`, Windows credential stores (`AppData/Roaming/Microsoft/Credentials`, `Protect`, `Crypto`, `Vault`, `NTUSER.DAT`), `Library/Mail`, `Library/Messages`, browser profiles (Chrome, Chromium, Brave, Edge, Firefox, Safari, cookies), `/etc/shadow`, `/etc/gshadow`, `/etc/sudoers`, `/etc/ssl/private`, trashes (`.local/share/Trash`, `.Trash`, `.Trash-*`, `.Trashes`, `$Recycle.Bin`), and cww's own config, index, and log directories.

**Never changed,** even in a folder that allows changes (readable unless the deny list says otherwise): anything in your home folder whose name starts with a dot (`~/.config`, `~/.local`, `~/.bashrc`, and every other program's settings there); version control internals (`.git`, `.hg`, `.svn`, `.bzr`, `.jj`, and any folder holding Git's `HEAD`, `objects` and `refs`, such as a bare repository); shell startup files anywhere (`.bashrc`, `.bash_profile`, `.bash_login`, `.bash_logout`, `.profile`, `.zshrc`, `.zshenv`, `.zprofile`, `.zlogin`, `.zlogout`, `.config/fish`, `.config/nushell`, `.config/powershell`, `Microsoft.PowerShell_profile.ps1`); Git and terminal settings (`.gitconfig`, `.config/git`, `.tmux.conf`, `.config/tmux`); desktop sessions and window managers (`.xinitrc`, `.xprofile`, `.xsession`, `.xsessionrc`, `.config/hypr`, `.config/i3`, `.config/sway`, `.config/uwsm`); autostart and services (`.config/autostart`, `.config/systemd`, `.config/environment.d`, `.local/share/applications`, `.local/share/systemd`, `Library/LaunchAgents`, `Library/LaunchDaemons`, `Library/StartupItems`, the Windows Start Menu); editor settings that run code (`.vscode`, `*.code-workspace`, `.idea`, `.vim`, `.vimrc`, `.gvimrc`, `.exrc`, `.nvimrc`, `.nvim.lua`, `.config/nvim`, `.emacs`, `.emacs.d`, `.config/emacs`, `.dir-locals.el`); hook and tool managers (`.husky`, `.pre-commit-config.yaml`, `lefthook.yml`, `mise.toml`, `.mise`, `.yarnrc`, `.yarnrc.yml`, `.envrc`, `.direnv`, `.cargo`); files interpreters and debuggers run on start (`*.pth`, `sitecustomize.py`, `usercustomize.py`, `site-packages`, `dist-packages`, `.pdbrc`, `.gdbinit`, `.lldbinit`, `.irbrc`, `.pryrc`, `.Rprofile`); and `desktop.ini` and `autorun.inf`. Allowing changes in the home folder itself, a folder that holds it, a whole drive or a system folder takes `--i-know`, even when the folder is already shared.

### What it does not protect against

- **Anything inside a shared folder that isn't on the deny list can be read.** Share narrow folders. A secret stored in `notes.txt` is readable.
- **In a folder that allows changes, a compromised server can change files without your approval** (see above): recoverable from the trash, within the budgets, and logged, but changed.
- **Text that runs later can be written.** cww writes no programs and sets no execute bit, and the never-changed list keeps out the files that run on their own (startup files, autostart, editor and hook settings). But build files and scripts are ordinary text people ask to have edited, and stay changeable: a changed `Makefile`, `package.json` script, `build.rs`, test, or shell script runs when you build, test or run the project yourself, and changed source code runs when you run the program. Settings of programs that live outside the home folder's dot files (`~/Library/Application Support` on macOS, `AppData` on Windows) are only out of reach because those folders are too broad to share without `--i-know`. Read the diff before you build or run something a change touched.
- **Results do reach Chat with Work and the model provider.** "Private" means your files and the index stay on your computer, and only the snippets and chunks needed for an answer leave it. Once they leave, the server's retention and sharing rules apply.
- **A malicious file could exploit a parser** (PDF, Office). Extraction runs with size caps and panic isolation, inside the kernel sandbox, which keeps it to the shared folders. The sandboxed daemon can still use the network, so an exploit could send what it can read to someone else; a separate reader process without network access is planned, and the `reader` module is structured for that split.
- **Windows has no kernel sandbox yet.** The path checks hold there as everywhere, but nothing stops a bug at the kernel level.
- **Local malware running as your user** can read your files directly and doesn't need cww.
- **Content-based secret redaction** (API keys inside ordinary text files) and "ask before every read" approvals are planned, not built.

### The sandbox

`cww daemon run` starts a small supervisor, which runs the daemon itself as a confined child. Landlock and Seatbelt can't be widened once they are applied, so when you share a folder the current sandbox doesn't cover, or allow or stop changes in one, the daemon exits and the supervisor starts it again with rules that fit; it reconnects within a second. Folders that allow changes get exactly the rights a change needs (with Landlock: write, make files and folders, remove, rename across folders, truncate, never execute; with Seatbelt: create, write, remove, set the mode, extended attributes and group, never flags, times or setuid bits), their trash gets only the rights to take items in, and every other shared folder stays read-only to the kernel. A folder shared read-only inside one that allows changes is read-only to Seatbelt too; Landlock can only add rights, so on Linux the daemon's own checks keep it read-only. Turn it off with `[sandbox] enabled = false` or `cww daemon run --no-sandbox` if it gets in the way, and please report why.

On Linux, Landlock needs kernel 5.13 or newer with Landlock enabled (the default on current Ubuntu, Debian, Fedora and Arch kernels); an older kernel runs the daemon unconfined and says so in `cww status --json`.

### Planned hardening

- Split into a network process and a reader process without network access (seccomp on Linux, a stricter Seatbelt profile on macOS).
- A signed and notarized macOS app bundle registered through `SMAppService`, so folder permissions survive updates.
- A restricted token or AppContainer for the reader on Windows.
- Local approval prompts, secret redaction, reproducible builds, and an external audit before general availability.

## Development

```sh
cargo test                                  # unit tests and the end-to-end test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
cargo deny check                            # licenses, advisories, bans, sources
```

`tests/e2e.rs` runs a real daemon against a fake Chat with Work server: DPoP-checked token endpoint, device flow, and an Action Cable WebSocket. It covers reads outside the roots, denied secrets, symlink, junction and hard-link escapes, pause, revocation, and every change tool with its escapes, the deny lists, read-only folders, limits and the trash. `tests/sandbox.rs` checks under the real kernel sandbox that only folders that allow changes, and their trash, can be written. CI runs everything on Linux, macOS and Windows, and builds the Nix package.

| Module | Role |
|---|---|
| `reader` | Everything that touches your files: safe path resolution, extraction, the index, grep, watching. This is the future sandboxed process. |
| `tools` | The MCP server (via [`rmcp`](https://github.com/modelcontextprotocol/rust-sdk)): the tools, limits, audit. |
| `writer`, `trash` | Changes in folders that allow them (path rules, kinds of file, Word and Excel documents), and the system trash on each platform. |
| `tunnel` | The outbound WebSocket, framing, reconnects. |
| `auth` | Device key, DPoP proofs, device flow, secret storage. |
| `daemon`, `control`, `service` | The daemon, its control channel ([CONTROL.md](CONTROL.md)), and systemd, launchd and Scheduled Task registration. |
| `tui` | The terminal UI, a client of the control socket. Screens are pinned as text snapshots in `src/tui/snapshots`; `UPDATE_SNAPSHOTS=1 cargo test` rewrites them. |
| `app/` | The desktop app (`cww-app`): tray item and settings window, another client of the control socket. See [docs/settings-app.md](docs/settings-app.md). |

Releases are built by `.github/workflows/release.yml` when a `v*` tag is pushed, and packaged with [native-packages](https://github.com/crmne/native-packages); see [PACKAGING.md](PACKAGING.md).

## Security

See [SECURITY.md](SECURITY.md) for how to report a vulnerability.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option. Unless you explicitly state otherwise, any contribution you intentionally submit for inclusion in this project, as defined in the Apache-2.0 license, is dual licensed as above, without any additional terms or conditions.

"Chat with Work" and its logo are trademarks of PlentyLabs UG (haftungsbeschränkt) & Co. KG. The license covers the code, not the name or the logo in `app/assets`.
