# Chats in the desktop app

The desktop app shows your Chat with Work chats the way the web app does: the sidebar with New chat, search and the chats by day, the conversation with your questions on the right and the answers in Markdown, the tool work folded into one live line, sources as citation chips, and the composer with its rainbow edge. It talks to the daemon exactly as `cww tui` does ([CHAT_API.md](../CHAT_API.md), [CONTROL.md, "Chats"](../CONTROL.md#chats)): `chats`, `chat`, `chat_send`, `chat_cancel`, `chat_access`, and a `subscribe` to the `chat` topic while a chat is open. It never holds a token.

This page is the design spec: how the web's design system (Live Wire, in the web app's `app/assets/tailwind`) maps onto egui, and what egui can't do the same way.

## Where it lives

Chat is a page of the existing window, not a second window. The settings sidebar starts with **Chat**, and `cww-app --page chat` opens on it. On the Chat page the whole window becomes the web's layout, with its own sidebar; the row at the foot of that sidebar (on the web, your name and "Settings and usage") goes back to the settings pages.

Why one window:

- It keeps the idle discipline in [settings-app.md](settings-app.md): the window, its GL context and its fonts exist only while it's open, and a closed window costs nothing. A second window would need the shell to own two eframe applications, route window events between them, and keep two GL contexts alive.
- The web does the same: settings are a page of the app's shell, beside the chats.
- The chat follows the web's look and the settings follow the platform's, so they don't mix on one screen: each page owns the whole window.

## Tokens

All colors are the web's `tokens.css` values, light and dark, in `app/src/ui/chat/tokens.rs`. The page follows the system's light or dark mode like the rest of the window.

| Token | Light | Dark | Used for |
|---|---|---|---|
| `canvas` | `#F3F5F9` | `#06070B` | the conversation's background |
| `canvas-sunken` | `#EAEEF4` | `#0A0C12` | the sidebar, inline code, citation chips, code block headers |
| `surface` | `#FFFFFF` | `#0F121A` | the composer, the selected chat, New chat, the search field |
| `surface-raised` | `#FFFFFF` | `#141824` | your questions, menus |
| `surface-hover` | `#F1F4F8` | `#161B27` | New chat on hover |
| `ink` / `ink-muted` / `ink-faint` | `#0A0D14` / `#5A6374` / `#7C8599` | `#E9ECF4` / `#8D96AA` / `#636B80` | text, secondary text, labels and icons |
| `line` / `line-strong` | `#D6DCE7` / `#C3CBDA` | `#1C2130` / `#2A3144` | hairlines |
| `live` | `#1E8FE6` | `#44B0FF` | a chat being answered, focus rings |
| `attention` / `negative` | `#D99A06` / `#F0552F` | `#FFC247` / `#FF6644` | a chat needing attention, failures |
| signals | green `#13C977`, blue, violet `#7A3CF0`, orange, lime | brighter on dark | code highlighting, the activity wire |
| rainbow | `#44ff9a` 0%, `#44b0ff` 23%, `#8b44ff` 48%, `#ff6644` 73%, `#ebff70` 99% | same | the composer's edge and glow, the streaming caret, turning rings |

`color-mix(in oklab, …)` is computed in Oklab too (`tokens::mix`), so hovers like "ink at 6%" and the primary button's hover ("ink 84% into canvas") come out the same as in the browser.

Radii: tag 6, control 10, card 14, panel 20, pill fully round. Elevation is the web's: a hairline ring plus one soft drop shadow (`0 20px 50px -24px`), drawn with egui's blurred rectangles.

## Type

The web sets everything in Geist and labels in Geist Mono. The app embeds both (SIL Open Font License, `app/assets/chat/fonts`), as variable TrueType files decoded from the web's own WOFF2 files, and picks weights per run with the `wght` axis: 400 for text, 500 and 550 for titles and buttons, 560 for New chat, 620 for headings in answers, 650 for the greeting. Sizes, line heights and tracking are the web's:

| Text | Face | Size / line height |
|---|---|---|
| Answers (`.prose`) | Geist 400 | 16 / 27.2 |
| Your questions | Geist 400 | 16 / 24 |
| Headings in answers | Geist 620, tracking -0.025em | 24, 20, 17 |
| Sidebar rows, activity line | Geist 400 (title 550) | 14 / 21 |
| Section labels (Today, Yesterday), table headers, `kbd` | Geist Mono 400, tracking 0.02em | 12, 11 |
| Citation chips | Geist Mono 400 | 11 |
| Code | Geist Mono 400 | 14 / 20 |

The settings pages keep the system font.

## Components

Each is drawn to the web component's measurements (from the compiled CSS, checked in a headless browser):

- **Sidebar** (`sidebar.css`): 288 wide on `canvas-sunken` with a hairline on its right. The logotype (22 high), New chat (a `surface` row with a hairline ring, icon `note-pencil`, `Alt N` hint on hover), the search field (32 high, `Ctrl K` hint), then the chats under mono labels Today, Yesterday and Earlier, as the terminal UI and the web's chat history page group them (the web's sidebar has one "Recent" label). A chat being answered has a pulsing `live` dot and a shimmering title; one needing attention an `attention` dot. The open chat sits on `surface` with a hairline ring. Your name is at the foot, and goes to Settings.
- **Responsive**: as on the web, the sidebar is docked from 1024 points wide. Narrower, it's a drawer: a toggle in the top left opens it over the conversation, sliding in over a dimmed backdrop, and choosing a chat or pressing Escape closes it. Wide, the sidebar's own toggle collapses it.
- **Conversation** (`conversation.css`): one column, at most 736 wide, centered, with 16 on each side; 24 at the top and 176 at the bottom so the last answer clears the composer. It sticks to the bottom while an answer streams, unless you scroll up, and then a round "scroll to the latest" button appears over the composer, with a `live` dot while more streams in.
- **Your questions**: a `surface-raised` bubble at most 85% (and 576) wide, right-aligned, corners 14 except 6 at the bottom right, padding 10 by 16, with a hairline ring. Copy appears under it on hover.
- **Answers**: Markdown (paragraphs, headings, bold, italic, strikethrough, inline code, links, ordered and unordered lists with nesting, quotes, rules, GitHub tables, fenced code), set as the web's typography plugin sets it: paragraphs 20 apart, list items 8 apart with faint markers, tables with mono headers and hairline rows, quotes with a 2-wide `line-strong` rule in `ink-muted`. Code blocks are a hairline card with a `canvas-sunken` header (the language, and Copy), highlighted with the web's signal colors (keywords violet, strings green, numbers and attributes orange, titles blue, comments faint italic). Links that point at one of the answer's sources become citation chips, as `MessagesHelper#chipify_source_links` does on the web. The latest answer shows Copy, and every answer shows its Sources pill (a stack of icons, "Sources", the count) that opens the numbered list of sources, each opening in the browser.
- **Activity** (`activity.css`): one pill-shaped line, "Searched Drive and Slack · 3 searches, read 2 files", with the services' avatars and a caret; it opens into the log of steps on a hairline rail, each with how many files it found ("4 found", the names on hover). While it runs, the title shimmers, the details show the running step's progress, a green spark and a flowing wire lead into the avatars (they retract when it settles), the active avatar pulses, and the rail turns into a green-to-blue gradient.
- **Thinking** (`.thinking`): the mark in a turning rainbow ring with "Thinking", then "Writing" once the answer streams, or "Stopping". A live activity line takes its place, as on the web.
- **Notices** (`notices.css`): failures and running out of credits as a card with a colored edge on the left.
- **Composer** (`composer.css`): a `surface` box with radius 20, the float shadow and a 2-wide rainbow edge (70% opaque, full while it has focus) inside a soft rainbow glow (14%, 30% with focus). The field grows with what you type up to 9 lines; Enter sends and Shift+Enter starts a new line, as on the web, and `Ctrl /` (`⌘ /`) focuses it. The paperclip opens the chat in the browser, where attachments are added. Send is a 32 square in `ink`, concentric with the box's corner; while an answer is written it becomes Stop, with a rainbow ring turning around it, and the edge flows. Locked (out of credits, nothing connected, a chat you can only read) the box dims, the edge loses its color, and the reason shows above the bar. While sending or stopping, the button says so in its tooltip and accessible name.
- **New chat** (`new_chat.css`): the greeting ("What are we working on, Alex?") over the composer, centered on a faded dot grid.
- **MCP Apps**: a step whose tool has a UI gets a slot in the activity log, a hairline card the width of the column, as `mcp_app.css` draws it. For now the slot draws a placeholder card with the service's name and a button to open the chat in the browser; the Servo-based view, which renders the app's HTML in a helper process and sends RGBA frames, will draw into the same rectangle (`ui::chat::mcp_app::McpAppSlot` takes the frame's texture and reports the rectangle to forward input from). A step says it has a view with an optional `app` (`service`, `uri`) in the chat API's steps; the server doesn't send it yet, so only the stand-in daemon's sample shows the slot.
- **Not ready to chat**: when the agent isn't running, the computer isn't paired, chats aren't allowed yet, or the server can't serve chats, the conversation area shows the web's empty state (dashed card on the dot grid) with the one thing to do: start the agent, pair, ask for access (then "waiting for you to allow it in Chat with Work"), or the reason.

Icons are the web's: Phosphor, bold weight (MIT), rasterized into one alpha atlas (`app/assets/chat/icons.png`) by `app/assets/chat/render.sh` and tinted when drawn.

## Motion

The web's durations and curves, in `tokens.rs`:

| Token | Value | Used for |
|---|---|---|
| instant | 80 ms linear | sidebar row hover |
| fast | 150 ms, `ease-snap` cubic-bezier(0.2, 0.7, 0.2, 1) | button and action hovers, the activity caret turning |
| base | 220 ms `ease-snap` | messages rising in (6 up, from transparent), the scroll button, glow and edge opacity |
| slow | 400 ms `ease-snap` | the activity wire wiring in and out, the greeting rising in |
| wire | 1.2 s linear | the activity wire's flow |
| drawer | 300 ms ease-out | the narrow sidebar sliding in |

Continuous animations, each only while its state holds: the composer edge flows (5 s per loop) while an answer is written; the stop ring and the thinking ring turn (2.4 s); titles of running work shimmer (2 s sweep); the caret at the end of a streaming answer blinks (1 s, stepped); the spark, the active avatar and the sidebar's live dots pulse (1.6 s and 2 s, `ease-glide` cubic-bezier(0.6, 0, 0.3, 1)).

## Zero CPU when idle

The page asks egui for another frame only while something on it moves: a transition in progress, an answer streaming, a running chat's shimmer and rings. A settled conversation, a chat list, or an empty composer request nothing: no timer, no polling. The text field's caret doesn't blink, since a blinking caret would redraw twice a second. While a chat in the list is being answered and isn't the open one (so nothing follows it), the page reads the list again every 4 seconds until it's done, so its dot stops; the terminal UI only reads it again on demand. New text arrives from the follower thread, which blocks on the daemon's socket and wakes the window only when an update comes; the daemon's heartbeat on that socket doesn't redraw. A tool waiting for approval in the browser, or an answer stuck in "processing" on the server, keeps its ring turning, as the web does. The UI tests check that a settled chat stops requesting frames.

## What egui does differently

- **Blur.** CSS blurs the composer's rainbow glow (`filter: blur(20px)`). egui has no filters, so the glow is a feathered ring of gradient-colored vertices around the box, with the same reach and opacity; it reads the same, but it is a smooth falloff rather than a true Gaussian of the gradient.
- **Gradients.** The rainbow edge, wire and caret are meshes with per-vertex colors, interpolated in sRGB, where the browser interpolates gradients in sRGB too; the edge is tessellated finely enough that its colors match.
- **Text.** Glyphs are rasterized by egui with the desktop's hinting (fastframe-text), not by Skia, so stems can differ by a fraction of a pixel and wrapping can break a word earlier or later than the browser. Synthetic italics: Geist has no italic face, which the browser also synthesizes.
- **Provider icons.** The web shows each service's logo (Drive, Slack) in the avatars and citation chips, and file-type icons for documents. The app draws the service's initial in the avatar and a Phosphor file icon in the chip instead of bundling third-party logos.
- **Model picker and attachments.** New chats use your default model and attachments stay in the browser (CHAT_API.md, "Left out for now"), so the composer's bar has a disabled paperclip and no model picker.
- **Reduced motion.** The browser honors "reduce motion"; egui doesn't expose it, so the animations always run.
- **Selecting text.** Answers are painted, not egui labels, so their text can't be selected with the mouse; Copy (on the latest answer, and on hover elsewhere) and Copy on code blocks copy it.
- **Actions.** The web's answers also have Retry, Branch and Share; the chat API has none of them (CHAT_API.md, "Left out for now"), so only Copy shows.
- **The caret.** The composer's caret is steady rather than blinking (see above).

## Screens

Rendered offscreen with wgpu by the UI tests against the stand-in daemon's synthetic sample (`cargo test -p cww-app renders_like_the_web`; `UPDATE_SNAPSHOTS=1` rewrites them). On Linux the test compares the page with these, within a small tolerance; elsewhere it only renders. They were checked side by side against the web app's own markup and compiled CSS, rendered by headless Chromium at the same sizes with the same sample.

| | Light | Dark |
|---|---|---|
| A chat | ![](../app/tests/snapshots/chat-light.png) | ![](../app/tests/snapshots/chat-dark.png) |
| An answer streaming | ![](../app/tests/snapshots/chat-streaming-light.png) | ![](../app/tests/snapshots/chat-streaming-dark.png) |
| A new chat | ![](../app/tests/snapshots/chat-new-light.png) | ![](../app/tests/snapshots/chat-new-dark.png) |
| Narrow | ![](../app/tests/snapshots/chat-narrow-light.png) | ![](../app/tests/snapshots/chat-narrow-dark.png) |
| A new chat, narrow | ![](../app/tests/snapshots/chat-new-narrow-light.png) | ![](../app/tests/snapshots/chat-new-narrow-dark.png) |
