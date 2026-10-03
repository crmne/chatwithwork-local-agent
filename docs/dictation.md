# Dictation (plan)

Status: plan, 2026-10-03. Nothing is built.

Every Chat with Work client dictates the same way: the recording is transcribed in the EU by Chat with Work's own transcription, never by Google, Apple or the operating system. It is charged in credits to the organization, like the web's dictation, and the server doesn't keep the audio.

## In the desktop app

- A microphone button in the composer, where the web puts its own (the app follows the web's control), plus a shortcut. Press to start, press again or Esc to stop; a recording stops by itself at 5 minutes, the server's limit.
- While it records, the app measures the microphone's level, and sends nothing when it heard only silence (a model hears silence as a sentence it makes up), as the web's composer does.
- On stop, the recording goes to Chat with Work and the text comes back into the composer, where the person edits and sends it. Nothing is sent to the chat by itself.
- Refusals read as the web's do: no credits left, a guest, too long, too fast, or the service unavailable, with the server's sentence.
- No CPU while not dictating: the microphone is opened when dictation starts and closed when it stops.

## How the audio travels

- **Recording:** `cpal` (the platform's own audio API), downmixed to mono and resampled to 16 kHz, kept in memory only, and written as WAV (16-bit), which the server accepts: 5 minutes is about 9.6 MB, under its 10 MB limit. Opus could replace it later to make uploads smaller.
- **Upload:** the app has no web session, so the recording goes through the daemon, as attachments do: the app hands the bytes to the daemon over the control socket (a `dictate` request with the audio and its type), and the daemon sends them with the device's token to Chat with Work. Only a computer whose chats are allowed can dictate. The daemon keeps nothing.
- **The server's side:** a Local Agent counterpart of the web's dictation endpoint, with the same checks, limits, refusals and charging, the same DPoP token and the `local_agent:chat` permission:
  - `GET /local_agent/dictations/new` answers `{ "cloud": bool, "reason": ... }`, so the button can say why before anyone records. `cloud: false` with no reason reads as unavailable.
  - `POST /local_agent/dictations` with a multipart `audio` part answers `{ "text": ... }`; an empty text means no words were heard.
  - Refusals carry `{ "error", "error_description" }`: 400 `invalid`, 403 `no_credits`, `forbidden`, `chat_access_required` or `insufficient_scope`, 413 `too_large`, 415 `unsupported`, 429 `rate_limited`, 502 `transcription_failed`, 503 `unavailable`.

## The TUI

Not planned for now: `cww` is also the static, sandboxed daemon, and recording audio doesn't belong in it. The daemon's `dictate` request would let a small helper add it later.

## Steps

1. The server's Local Agent dictation endpoints (built in Chat with Work, not deployed yet).
2. The daemon's `dictate` relay, with its limits, documented in CONTROL.md and PROTOCOL.md.
3. Audio capture, the silence check and WAV encoding behind one `Dictation` interface in `cww-app`, tested on recorded fixtures rather than a live microphone.
4. The composer's button and states, matched to the web's, with offscreen snapshots.
5. The macOS microphone permission (`NSMicrophoneUsageDescription` in the bundle) and the README.
