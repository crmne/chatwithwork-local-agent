//! Slash commands in the composer, as Claude Code has them: `/` lists them,
//! typing narrows the list, Tab or Enter completes.

/// One thing a slash command does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cmd {
    New,
    Resume,
    Search,
    Model,
    Project,
    Attach,
    Detach,
    Copy,
    Open,
    Retry,
    Branch,
    Rename,
    Delete,
    ShareChat,
    UnshareChat,
    Steps,
    Settings,
    Folders,
    Share,
    Unshare,
    Pause,
    ResumeSharing,
    Log,
    Status,
    Login,
    Logout,
    Help,
    Exit,
}

pub struct Command {
    pub cmd: Cmd,
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    /// What it takes after its name, as the list shows it.
    pub args: &'static str,
    /// The argument is needed: completing it leaves room to type one.
    pub needs_args: bool,
    pub about: &'static str,
}

const fn command(
    cmd: Cmd,
    name: &'static str,
    aliases: &'static [&'static str],
    args: &'static str,
    needs_args: bool,
    about: &'static str,
) -> Command {
    Command {
        cmd,
        name,
        aliases,
        args,
        needs_args,
        about,
    }
}

/// Every command, in the order the list shows them.
pub const COMMANDS: &[Command] = &[
    command(Cmd::New, "new", &["clear"], "", false, "Start a new chat"),
    command(
        Cmd::Resume,
        "resume",
        &["chats"],
        "[search]",
        false,
        "Pick a recent chat",
    ),
    command(
        Cmd::Search,
        "search",
        &[],
        "<text>",
        true,
        "Search your chats",
    ),
    command(
        Cmd::Model,
        "model",
        &[],
        "[name]",
        false,
        "Pick the model for this chat",
    ),
    command(
        Cmd::Project,
        "project",
        &[],
        "[name]",
        false,
        "Start the next chat in a project",
    ),
    command(
        Cmd::Attach,
        "attach",
        &[],
        "<path>",
        true,
        "Attach a file to your next question",
    ),
    command(
        Cmd::Detach,
        "detach",
        &[],
        "",
        false,
        "Remove the attached files",
    ),
    command(Cmd::Copy, "copy", &[], "", false, "Copy the last answer"),
    command(
        Cmd::Open,
        "open",
        &[],
        "",
        false,
        "Open the chat in the browser",
    ),
    command(
        Cmd::Retry,
        "retry",
        &[],
        "",
        false,
        "Answer the last question again",
    ),
    command(
        Cmd::Branch,
        "branch",
        &[],
        "",
        false,
        "Continue this chat in a new one",
    ),
    command(
        Cmd::Rename,
        "rename",
        &[],
        "[title]",
        false,
        "Rename this chat",
    ),
    command(Cmd::Delete, "delete", &[], "", false, "Delete this chat"),
    command(
        Cmd::ShareChat,
        "share-chat",
        &[],
        "",
        false,
        "Make a public link to this chat and copy it",
    ),
    command(
        Cmd::UnshareChat,
        "unshare-chat",
        &[],
        "",
        false,
        "Stop sharing this chat's link",
    ),
    command(
        Cmd::Steps,
        "steps",
        &[],
        "",
        false,
        "Show or hide every tool step",
    ),
    command(
        Cmd::Settings,
        "settings",
        &["config"],
        "",
        false,
        "Shared folders, activity, pairing and the daemon",
    ),
    command(
        Cmd::Folders,
        "folders",
        &[],
        "",
        false,
        "Show your shared folders",
    ),
    command(Cmd::Share, "share", &[], "[path]", false, "Share a folder"),
    command(
        Cmd::Unshare,
        "unshare",
        &[],
        "[folder]",
        false,
        "Stop sharing a folder",
    ),
    command(
        Cmd::Pause,
        "pause",
        &[],
        "",
        false,
        "Refuse Chat with Work's requests for now",
    ),
    command(
        Cmd::ResumeSharing,
        "resume-sharing",
        &["unpause"],
        "",
        false,
        "Answer Chat with Work's requests again",
    ),
    command(
        Cmd::Log,
        "log",
        &["audit", "activity"],
        "",
        false,
        "Show every request from Chat with Work",
    ),
    command(
        Cmd::Status,
        "status",
        &[],
        "",
        false,
        "Say how this computer is connected",
    ),
    command(
        Cmd::Login,
        "login",
        &["pair"],
        "",
        false,
        "Pair this computer",
    ),
    command(
        Cmd::Logout,
        "logout",
        &[],
        "",
        false,
        "Disconnect this computer, forgetting its pairing",
    ),
    command(Cmd::Help, "help", &["?"], "", false, "Commands and keys"),
    command(Cmd::Exit, "exit", &["quit"], "", false, "Quit"),
];

pub fn get(cmd: Cmd) -> &'static Command {
    COMMANDS
        .iter()
        .find(|c| c.cmd == cmd)
        .expect("every command is listed")
}

/// The command named `name`, or one of its aliases.
pub fn find(name: &str) -> Option<&'static Command> {
    let name = name.to_lowercase();
    COMMANDS
        .iter()
        .find(|c| c.name == name || c.aliases.contains(&name.as_str()))
}

/// The commands that start with what's typed after `/`, names before
/// aliases, among those `shown` allows.
pub fn matching(prefix: &str, shown: impl Fn(Cmd) -> bool) -> Vec<&'static Command> {
    let prefix = prefix.to_lowercase();
    let mut by_name: Vec<&Command> = COMMANDS
        .iter()
        .filter(|c| shown(c.cmd) && c.name.starts_with(&prefix))
        .collect();
    for c in COMMANDS {
        if shown(c.cmd)
            && !by_name.iter().any(|n| n.cmd == c.cmd)
            && c.aliases.iter().any(|a| a.starts_with(&prefix))
        {
            by_name.push(c);
        }
    }
    by_name
}

/// A line that's a command: its name and the rest. `//` escapes a question
/// that starts with a slash, so it isn't one.
pub fn parse(input: &str) -> Option<(&str, &str)> {
    let rest = input.strip_prefix('/')?;
    if rest.starts_with('/') {
        return None;
    }
    let (name, args) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
    Some((name, args.trim()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_aliases_complete() {
        let names = |prefix| -> Vec<&str> {
            matching(prefix, |_| true)
                .into_iter()
                .map(|c| c.name)
                .collect()
        };
        assert_eq!(names("re"), ["resume", "retry", "rename", "resume-sharing"]);
        assert_eq!(names("cl"), ["new"], "by its alias");
        assert_eq!(names("qu"), ["exit"]);
        assert!(names("zz").is_empty());
        assert_eq!(matching("", |c| c != Cmd::Logout).len(), COMMANDS.len() - 1);
        assert_eq!(find("QUIT").map(|c| c.cmd), Some(Cmd::Exit));
        assert_eq!(get(Cmd::Model).name, "model");
    }

    #[test]
    fn parses_a_command_and_its_argument() {
        assert_eq!(parse("/search  budget q3 "), Some(("search", "budget q3")));
        assert_eq!(parse("/new"), Some(("new", "")));
        assert_eq!(parse("//etc/hosts?"), None);
        assert_eq!(parse("hello /new"), None);
    }
}
