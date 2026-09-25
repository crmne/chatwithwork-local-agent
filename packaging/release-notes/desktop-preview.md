A preview of the Chat with Work Local Agent desktop app: a menu bar item on macOS and a notification-area icon on Windows, with a settings window to pair the computer, choose folders, pause, and follow activity. It ships with the `cww` daemon. This is a test build, not a release: Homebrew, WinGet and the AUR don't get it.

## macOS (11 or later, Apple silicon and Intel)

1. Download `cww-app-macos-universal.dmg`, open it, and drag Chat with Work Local Agent to Applications. The app is signed with Developer ID and notarized, so it opens without warnings. (`cww-app-macos-universal.zip` holds the same app.)
2. Open it from Applications. It has no Dock icon; look for the icon in the menu bar.
3. The `cww` command is inside the app, at `/Applications/Chat with Work Local Agent.app/Contents/MacOS/cww`. Starting the agent from the app runs `cww daemon install` with that copy, so it replaces a LaunchAgent that points at a Homebrew `cww`.

## Windows 10 and 11 (x64 or arm64)

1. Download `cww-app-windows-x64.zip`, or `cww-app-windows-arm64.zip` on an ARM PC.
2. Extract it to a folder you'll keep, for example `%LOCALAPPDATA%\Programs\cww-app`. The logon task for the daemon points at `cww-agent.exe` in that folder, so don't run it from inside the zip or from Downloads if you'll clean that up.
3. Run `cww-app.exe`. These builds aren't signed yet, so SmartScreen says "Windows protected your PC": click **More info**, then **Run anyway**.
4. The icon may land in the hidden icons (the `^` next to the clock); drag it onto the taskbar to keep it visible.

## What to check

- The icon appears in the menu bar or notification area, and follows light and dark mode.
- The menu shows the agent's status and a Pause or Resume item that works both ways.
- Settings opens the settings window, and opening it again brings the same window forward.
- First run: the welcome page walks through starting the Local Agent, connecting to Chat with Work, and choosing folders. Connecting opens the browser on a code to approve; afterwards the computer shows up in Chat with Work under Settings, Connectors, Computers.
- Folders: adding a folder opens the system folder picker, and the folder appears in the list and in Chat with Work.
- Asking Chat with Work about a file in a shared folder shows up in the activity list.
- Start at login can be turned on and off.
- Quit closes the app and removes its icon. The daemon keeps running on its own: open the app again and it shows the agent still connected.

To remove everything: turn off start at login, run `cww daemon uninstall` with the `cww` that came with the app, then delete the app (macOS) or the folder (Windows).
