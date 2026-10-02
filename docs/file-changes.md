# Changing files (proposal)

Status: proposal, 2026-10-03. Nothing here is built. Today the daemon has four read-only tools and no code that writes, deletes or runs anything, and the README and the threat model promise that. Building this changes both, so the open questions at the end need answers first.

Goal: let the assistant create, edit, move, rename and delete files in the folders you shared, the way the Google Drive connector changes Drive files, with every change approved by you.

## The trust problem

The rule behind the daemon is that **every control that must hold against a compromised Chat with Work server lives in the daemon**. An approval on the web is a decision the server reports; the daemon can't tell a real approval from a forged one. So the server's approval card protects you from the model, not from a compromised server. The daemon has to make writes safe on its own:

1. **Off unless you turn it on, per folder.** A shared folder stays read-only until you allow changes in it (`writable = true` in `config.toml`, a switch in the app and the TUI's Shared folders page). Nothing the server says can turn it on.
2. **Nothing is lost.** Deleting moves to a trash the daemon keeps; overwriting or editing keeps the previous version there first. Every change can be undone from the app or `cww undo` for 30 days (configurable).
3. **Bounded.** Separate limits for changes (for example 30 changes a minute, 500 a day, 50 MB written an hour), size caps per file, and no change to a file bigger than the read cap without a local confirmation.
4. **Seen.** Every change is in the audit log with its before and after paths, and the app can notify you as it happens.
5. **Optionally confirmed here.** `[changes] confirm = "local"` makes the daemon ask in the app or the TUI before every change, in addition to the web's approval. That is the only mode that holds against a compromised server; the default (`"web"`) relies on 1 to 4 to keep the damage recoverable.

## Tools

Each is an MCP tool with annotations, so the server can tell writes from reads without a list of names:

| Tool | Does | `readOnlyHint` | `destructiveHint` | `idempotentHint` |
|---|---|---|---|---|
| `create` | New file with text content (UTF-8, at most 1 MB); fails if the path exists | false | false | false |
| `write` | Replace a file's text, or `append`; the old version goes to the trash first | false | **true** (overwrites) | false |
| `edit` | Replace one exact span (`old_text` → `new_text`, must match once); the old version is kept | false | false | false |
| `mkdir` | New folder (parents optional) | false | false | true |
| `move` | Move or rename a file or folder, within or between writable folders; fails if the target exists unless `replace` (then destructive, old target to the trash) | false | false (true with `replace`) | false |
| `delete` | Move a file or folder to the daemon's trash | false | **true** | false |

All paths use the existing `root-id:relative/path` form. Binary files (PDF, Office) are never written; only text formats the reader already extracts as plain text (`.txt`, `.md`, `.csv`, `.json`, source files and the like), so the model can't corrupt a document it can't see whole.

## Path rules (daemon)

The write path reuses `reader::safe_fs`, which already resolves on handles, never strings:

- Only inside a **writable** shared folder. The deny list applies to writes too, so `.env`, keys, `.ssh` and the rest can be neither created, changed nor moved into or out of.
- Resolve the parent directory with the same `openat2(RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS | RESOLVE_NO_MAGICLINKS)` on Linux, the `O_NOFOLLOW` walk plus `F_GETPATH` check on macOS, and the reparse-point walk plus `GetFinalPathNameByHandleW` on Windows. Then act relative to that directory handle: `openat(O_CREAT | O_EXCL | O_NOFOLLOW)`, `renameat2(RENAME_NOREPLACE)` (`renamex_np(RENAME_EXCL)` on macOS, `MoveFileEx` without `REPLACE_EXISTING` on Windows), `unlinkat` only inside the trash.
- Never follow a symlink, never write through a hard link (more than one link is refused, as reads are), never touch FIFOs, sockets or devices.
- Writes are atomic: write a temporary file in the same directory, `fsync`, then rename over, so a crash leaves the old or the new file, never half of one.
- Moving between folders on different filesystems copies then trashes the source, never deletes it.

## The trash

`~/.local/share/cww/trash/<timestamp>-<id>/` (the daemon's own data directory, already writable in the sandbox) with a small JSON record of the original root and path. Undo puts it back unless something is there now. Cross-filesystem trashing copies. The OS trash (Finder, Recycle Bin, freedesktop) was considered and left out: the sandboxed daemon can't reach it portably, and a trash of its own can say which chat made each change.

## Sandbox

Landlock and Seatbelt give writable folders write rights (`WriteFile`, `MakeReg`, `MakeDir`, `RemoveFile`, `RemoveDir`, `Refer`, `Truncate` on Landlock), read-only folders keep read only. Turning writes on for a folder restarts the daemon under the new rules, as sharing a folder already does. The planned split into a network process and a reader process becomes a writer process too: parsers never run with write rights.

## What the server changes (contract for the Rails side)

1. **Tools from annotations, not a name list.** `LocalAgent::Device::Files#tools` keeps only tools with `read_only?`; it should instead load every tool the daemon offers whose name is in an extended `LocalAgent::ACTIONS` (`roots search list read create write edit mkdir move delete`), and record each call's effect from its annotations as MCP tools already do (`WorkTools::Tool.effect_of`): `readOnlyHint` → `read`; otherwise `destructiveHint` → `destructive`, else `write`. A daemon that doesn't offer the new tools (every released one) works as today.
2. **Every change asks, every time.** Local changes go through `requires_approval` like any change. "Allow for the rest of this chat" is never offered for `local_*` tools, read-only or not (today it stops once the chat read local files; for local writes it should not exist at all), and destructive calls always ask, as now.
3. **Approval preview.** Approval partials for `local_*` tools, on the web and in the Local Agent API's `approvals` (`summary`, `details`):
   - `create`: "Create `notes/plan.md` in Work docs", with the content (first 40 lines, monospace, with a line count).
   - `write`: "Replace `notes/plan.md` in Work docs", with a unified diff against the current text when the chat has read it (the server's cache of the read), otherwise the new content; "The current version goes to the trash on this computer."
   - `edit`: "Edit `notes/plan.md`", with the span as a diff.
   - `mkdir`: "Create the folder `reports/2026` in Work docs".
   - `move`: "Move `a.md` to `archive/a.md`" (and "replacing the file there" when `replace`).
   - `delete`: "Move `old.md` to the trash on this computer".
   Folder labels, never absolute paths (the server never sees them). The computer's name in the card's header.
4. **Errors stay sentences.** The daemon refuses with MCP tool errors (`not writable`, `exists`, `denied path`, `limit reached`, `waiting for confirmation on this computer`, `the person declined on this computer`); the server shows them as it shows other tool errors.
5. **Caching.** A write invalidates the server's cached text of that file (`remote_resources`, provider `local`) so a later read doesn't answer from a stale copy.
6. **Audit and activity.** Record changes as their own events (`local_agent.file_changed` with tool, root id and effect, never contents), and show them in the activity line ("Edited plan.md on Carmine's MacBook").

## The clients

The TUI and the app get: the Shared folders switch "Allow changes" per folder (with a sentence on what that means), a Trash page under Settings with Undo, the local confirmation prompt when `confirm = "local"`, and notifications for changes in `"web"` mode.

## Open questions for Carmine

1. Is per-folder opt-in (off by default) right, or should changes be on for every shared folder?
2. Default confirmation: `"web"` (one approval, damage recoverable) or `"local"` (approve on the web and again on this computer; safe against a compromised server, but two clicks)?
3. Text files only at first, or also writing Office documents (needs a writer for each format, and much more risk)?
4. Trash retention: 30 days and a size cap (say 2 GB), oldest first?
5. Does "allow for the rest of this chat" stay off for local changes for good, or come back for `create` and `mkdir` in a chat that read no local files?
