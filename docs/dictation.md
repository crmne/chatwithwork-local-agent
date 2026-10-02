# Dictation (plan)

Status: plan, 2026-10-03. Nothing is built.

Carmine's direction: the browser uses the Web Speech API, with a cloud transcription fallback where it's missing or not private; the native apps, `cww-app` included, transcribe on the device. The TUI can wait, or use a helper later.

## In the desktop app

**Audio stays on the computer.** Speech is turned into text on the device and only the text goes into the composer, so nothing new leaves the machine and nothing is billed.

| Platform | Engine | Notes |
|---|---|---|
| macOS | Apple's on-device speech recognition (`SFSpeechRecognizer` with `requiresOnDeviceRecognition`, through `objc2` bindings) | Free and fast on Apple silicon. Needs the microphone and speech recognition permissions, so the bundle gains `NSMicrophoneUsageDescription` and `NSSpeechRecognitionUsageDescription`. The newer `SpeechAnalyzer` (macOS 26) is better but Swift-only, so it would take a small Swift shim; later. |
| Windows | Local Whisper (below) | `Windows.Media.SpeechRecognition` exists but its offline quality is modest; Whisper is better and the same as Linux. Win+H voice typing already works in the composer with no code. |
| Linux | Local Whisper (below) | No speech-to-text in the OS. |

**Local Whisper** is whisper.cpp through `whisper-rs`, with OpenAI's Whisper weights (MIT). The model is downloaded on first use, not shipped: `base` (about 140 MB) by default, `small` (about 470 MB) for better accuracy, both multilingual. It runs faster than real time on a recent CPU; Vulkan or CUDA speed it up where present. The download comes from a URL we control (the Chat with Work CDN or a GitHub release), checked against a pinned SHA-256, and is the only new network access; README and the threat model say so. Licenses and size go through `cargo deny` before anything is merged; whisper.cpp adds a few MB to the binary.

**Audio** comes from `cpal` (the platform's own audio API), resampled to 16 kHz mono with `rubato`, held in memory only, and dropped when dictation stops.

## How it works for the person

- A microphone button in the composer, where the web puts its own (the app follows the web's control once the web has it), plus a shortcut. Press to start, press again or Esc to stop.
- Text appears in the composer as it's recognized (partial results every second or two, settled when a pause ends a phrase). Nothing is sent until the person sends it.
- The first use on Linux and Windows asks to download the model, says how large it is, and shows progress.
- Settings, General: the engine (on macOS: Apple or Whisper), the Whisper model size, and the language (automatic by default).
- No CPU while not dictating: the engine and the model are loaded when dictation starts and dropped a while after it ends.

## Steps

1. An optional `dictation` feature in `cww-app`, so builds without it stay as small as today.
2. Audio capture and the Whisper engine behind one `Dictation` interface (start, partial text, final text, stop), tested on recorded WAV fixtures, not a live microphone.
3. The composer button and states, matched to the web's once it ships, with offscreen snapshots.
4. The macOS engine and the bundle's permission strings.
5. Model download with progress and checksum, and the settings.

## The TUI

Skipped for now. `cww` is also the static, sandboxed daemon, so microphone capture doesn't belong in it. If the terminal gets dictation later, it would run a small `cww-dictate` helper (the app's engine as its own binary) that writes recognized text to the TUI over a pipe.
