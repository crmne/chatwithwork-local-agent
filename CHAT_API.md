# Chats in the terminal

Status: **built.** `cww tui` lists your chats, reads them, asks questions and follows answers as they're written. This page explains the design; the wire details are in [PROTOCOL.md, section 12](PROTOCOL.md#12-chats-for-the-terminal-ui) (daemon to server) and [CONTROL.md, "Chats"](CONTROL.md#chats) (terminal to daemon).

## The terminal never holds a token

The TUI asks the daemon over the control socket, which only your user can open, and the daemon calls Chat with Work with the same DPoP-bound access token its tunnel uses. A stolen token is useless without the device key, and the key never leaves the daemon's keychain entry. An earlier proposal gave the TUI its own grant and its own refresh token; relaying through the daemon keeps one credential, one network client and one place to audit.

## Chats are a separate permission

Pairing lets Chat with Work call four read-only tools. Reading your chats is worth far more, since they quote Drive, Slack and everything else you connected, so it's asked for on its own:

- `cww login` asks for `local_agent:chat` as well as `local_agent:serve`. The approval page shows "Also let it use your chats", checked, and you can clear it to share folders only. `cww login --no-chats` doesn't ask.
- A computer paired without it asks the first time you open the TUI (`o`), and you allow or decline it in Chat with Work under Settings, Computers. The TUI notices by itself once you do.
- You can turn chats off there at any time without unsharing folders or unpairing. The server refuses the chat API at once, even with a live token, and closes the socket, so followed chats stop.
- The server only ever shows the chats you could see in the browser, in the organization the computer is paired with, and records every use as `local_agent.chat_*` events without their contents.

## Answers stream over the tunnel

The daemon already holds a WebSocket to the server. To follow a chat, it subscribes to one more Action Cable channel on it, `LocalAgent::ChatChannel`, and relays what arrives to the TUI's `chat` subscription: answer text as it's written, the running tool's progress, and "changed" for everything else, after which the TUI reads the chat again. This beats Server-Sent Events here because the socket is already authenticated and reconnects on its own, it adds no connection per open chat, and on the server it doesn't hold a web worker per watcher. A reconnect subscribes again, and the TUI catches up by reading the chat.

## What the TUI shows

The sidebar lists chats by day (Today, Yesterday, Earlier) under "New chat", and `/` searches them. The conversation looks like the web's: your questions as bubbles on the right, the tool work as one activity line ("Searched Drive and Slack · 3 searches", live while it runs, `e` shows every step and the files it found), answers in Markdown with bold, lists and code, and their sources and links as numbered footnotes with titles and URLs. The composer has the rainbow edge, flowing while an answer is written, dimmed with the reason when you can't ask (out of credits, nothing connected), and plain bold or dim without colour. When the daemon isn't running, the computer isn't paired, or chats aren't allowed, the screen says what to do.

## Left out for now

Attachments, choosing a model (new chats use your default model), starting a chat in a project, retrying, branching, sharing, renaming and deleting chats. They're all in the browser.
