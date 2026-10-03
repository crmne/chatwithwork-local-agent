//! The default-deny list for secrets.
//!
//! Patterns are matched against the path components of the absolute path,
//! even inside shared roots, both sides in a folded form ([`fold`]): full
//! Unicode case folding, NFD, and without the code points filesystems
//! ignore. A case-insensitive filesystem (NTFS, APFS, HFS+, ext4 and tmpfs
//! with `casefold`, FAT) treats `.zſhrc`, `.\u{212A}ube` (a Kelvin sign) or a
//! decomposed `é` as the name it folds to, so the deny list must too:
//!
//!
//! - A pattern without `/` (such as `.ssh` or `*.pem`) matches any single
//!   component.
//! - A pattern with `/` (such as `.config/gcloud`) matches that run of
//!   consecutive components anywhere in the path.
//! - A pattern starting with `/` (such as `/etc/shadow`) is anchored at the
//!   filesystem root.
//!
//! Matching a directory denies everything below it.

use std::borrow::Cow;
use std::path::{Component, Path};

use anyhow::{Context, Result};
use globset::{GlobBuilder, GlobMatcher};

use crate::config::DenyConfig;
use crate::paths::Paths;

/// Built-in deny patterns. Users can drop one only by listing it under
/// `[deny] remove` in the local config file.
pub const DEFAULT_DENY: &[&str] = &[
    // SSH, GPG, cloud and container credentials
    ".ssh",
    ".gnupg",
    ".aws",
    ".azure",
    ".config/gcloud",
    ".kube",
    ".docker/config.json",
    ".netrc",
    ".npmrc",
    ".pypirc",
    ".git-credentials",
    ".config/gh/hosts.yml",
    // Environment files, keys and certificates
    ".env*",
    "*.pem",
    "*.key",
    "*.p12",
    "*.pfx",
    "id_*",
    // Password managers and keychains
    "*.kdbx",
    "*.1pux",
    ".password-store",
    ".local/share/keyrings",
    "Library/Keychains",
    "*.keychain",
    "*.keychain-db",
    "AppData/Roaming/Microsoft/Credentials",
    "AppData/Local/Microsoft/Credentials",
    "AppData/Roaming/Microsoft/Protect",
    "AppData/Roaming/Microsoft/Crypto",
    "AppData/Roaming/Microsoft/SystemCertificates",
    "AppData/Local/Microsoft/Vault",
    "NTUSER.DAT*",
    // Mail and messages
    "Library/Mail",
    "Library/Messages",
    // Browser profiles (cookies, saved passwords, history)
    ".mozilla",
    ".config/google-chrome",
    ".config/chromium",
    ".config/BraveSoftware",
    ".config/microsoft-edge",
    "Library/Application Support/Google/Chrome",
    "Library/Application Support/Chromium",
    "Library/Application Support/Firefox",
    "Library/Application Support/BraveSoftware",
    "Library/Application Support/Microsoft Edge",
    "Library/Safari",
    "AppData/Local/Google/Chrome/User Data",
    "AppData/Local/Chromium/User Data",
    "AppData/Local/BraveSoftware",
    "AppData/Local/Microsoft/Edge/User Data",
    "AppData/Roaming/Mozilla/Firefox",
    "AppData/Roaming/Opera Software",
    "Library/Cookies",
    // System secrets
    "/etc/shadow",
    "/etc/gshadow",
    "/etc/sudoers",
    "/etc/ssl/private",
    // Trashes: what was deleted stays deleted
    ".local/share/Trash",
    ".Trash",
    ".Trash-*",
    ".Trashes",
    "$Recycle.Bin",
];

/// Paths that can be read (unless [`DEFAULT_DENY`] hides them) but never
/// changed: whatever runs on its own when it changes, or when a tool is
/// merely started in a folder. Version control internals (git runs
/// `core.fsmonitor` and hooks), shell startup files, autostart and launch
/// agents, editor, debugger and interpreter settings that run code, Python
/// startup files, and direnv. On top of these, [`HOME_DOT_ENTRIES`]: every
/// entry of the home folder whose name starts with a dot. Users can drop
/// one under `[deny] remove`, like the deny list.
///
/// Not here, on purpose: build files people ask to have edited (`Makefile`,
/// `package.json`, `Cargo.toml`, scripts). They run only when the person
/// builds or runs something, which is the residual risk the README states.
pub const DEFAULT_WRITE_DENY: &[&str] = &[
    // Version control
    ".git",
    ".hg",
    ".svn",
    ".bzr",
    ".jj",
    // Shell startup
    ".bashrc",
    ".bash_profile",
    ".bash_login",
    ".bash_logout",
    ".profile",
    ".zshrc",
    ".zshenv",
    ".zprofile",
    ".zlogin",
    ".zlogout",
    ".config/fish",
    ".config/nushell",
    ".config/powershell",
    "Microsoft.PowerShell_profile.ps1",
    // Git and terminal settings that run commands (core.fsmonitor, hooks)
    ".gitconfig",
    ".config/git",
    ".tmux.conf",
    ".config/tmux",
    // Desktop sessions and window managers that run commands at login
    ".xinitrc",
    ".xprofile",
    ".xsession",
    ".xsessionrc",
    ".config/hypr",
    ".config/i3",
    ".config/sway",
    // Autostart and services
    ".config/autostart",
    ".config/systemd",
    ".config/environment.d",
    ".local/share/applications",
    ".local/share/systemd",
    "Library/LaunchAgents",
    "Library/LaunchDaemons",
    "Library/StartupItems",
    "AppData/Roaming/Microsoft/Windows/Start Menu",
    ".config/uwsm",
    // Editors and tools that run things from a folder's settings
    ".vscode",
    "*.code-workspace",
    ".idea",
    ".envrc",
    ".direnv",
    ".cargo",
    ".vim",
    ".vimrc",
    ".gvimrc",
    ".exrc",
    ".nvimrc",
    ".nvim.lua",
    ".config/nvim",
    ".emacs",
    ".emacs.d",
    ".config/emacs",
    ".dir-locals.el",
    ".dir-locals-2.el",
    // Git hook managers and tool version managers that run commands
    ".husky",
    ".pre-commit-config.yaml",
    "lefthook.yml",
    "lefthook.yaml",
    ".lefthook.yml",
    ".lefthook.yaml",
    "lefthook-local.yml",
    ".mise.toml",
    "mise.toml",
    ".mise.local.toml",
    "mise.local.toml",
    ".mise",
    ".yarnrc",
    ".yarnrc.yml",
    // Interpreters and debuggers that run a file in the folder they start in
    "*.pth",
    "sitecustomize.py",
    "usercustomize.py",
    "site-packages",
    "dist-packages",
    ".pdbrc",
    ".pdbrc.py",
    ".gdbinit",
    ".lldbinit",
    ".irbrc",
    ".pryrc",
    ".Rprofile",
    // Windows folder settings
    "desktop.ini",
    "autorun.inf",
];

/// The never-changed entry for every name starting with a dot directly in
/// the home folder (`.config`, `.local`, `.ssh`, `.bashrc`, and whatever
/// else programs read their settings from). Dropped with `[deny] remove =
/// ["~/.*"]`.
pub const HOME_DOT_ENTRIES: &str = "~/.*";

/// The never-changed patterns in effect, as `cww status` lists them.
pub fn never_changed(removed: &[String]) -> Vec<&'static str> {
    DEFAULT_WRITE_DENY
        .iter()
        .copied()
        .chain([HOME_DOT_ENTRIES])
        .filter(|p| !is_removed(removed, p))
        .collect()
}

fn is_removed(removed: &[String], pattern: &str) -> bool {
    removed
        .iter()
        .any(|r| r.eq_ignore_ascii_case(pattern.trim_end_matches('/')))
}

#[derive(Debug, Clone)]
struct Pattern {
    source: String,
    anchored: bool,
    components: Vec<GlobMatcher>,
}

#[derive(Debug, Clone)]
pub struct DenyList {
    patterns: Vec<Pattern>,
}

impl DenyList {
    /// The built-in list plus the config's changes, plus cww's own
    /// directories (config, index, logs, socket), which are always denied.
    pub fn from_config(config: &DenyConfig, paths: &Paths) -> Result<Self> {
        // Anchored patterns built from components, so they work with any
        // separator, and escaped, so a `[` in a home folder name is literal.
        let own_dirs: Vec<String> = paths
            .all_dirs()
            .iter()
            .copied()
            .chain(paths.home_trash.as_deref())
            .map(|p| {
                let parts: Vec<String> = p
                    .components()
                    .filter_map(|c| match c {
                        Component::Normal(s) => Some(globset::escape(&s.to_string_lossy())),
                        _ => None,
                    })
                    .collect();
                format!("/{}", parts.join("/"))
            })
            .collect();
        let kept = |p: &&&str| !is_removed(&config.remove, p);
        let patterns = DEFAULT_DENY
            .iter()
            .filter(kept)
            .map(|p| p.to_string())
            .chain(config.extra.iter().cloned())
            .chain(own_dirs);
        Self::new(patterns)
    }

    /// The paths changes may never touch, on top of the deny list: the
    /// built-in [`DEFAULT_WRITE_DENY`] and, with a `home` folder, every
    /// entry in it whose name starts with a dot ([`HOME_DOT_ENTRIES`]),
    /// less what `[deny] remove` drops.
    pub fn write_from_config(config: &DenyConfig, home: Option<&Path>) -> Result<Self> {
        let mut list = Self::new(
            DEFAULT_WRITE_DENY
                .iter()
                .filter(|p| !is_removed(&config.remove, p))
                .map(|p| p.to_string()),
        )?;
        if let Some(home) = home
            && !is_removed(&config.remove, HOME_DOT_ENTRIES)
        {
            let parts: Vec<String> = home
                .components()
                .filter_map(|c| match c {
                    Component::Normal(s) => Some(globset::escape(&s.to_string_lossy())),
                    _ => None,
                })
                .collect();
            if !parts.is_empty() {
                let mut pattern = compile(&format!("/{}/.*", parts.join("/")))?;
                pattern.source = HOME_DOT_ENTRIES.to_string();
                list.patterns.push(pattern);
            }
        }
        Ok(list)
    }

    pub fn new(patterns: impl IntoIterator<Item = String>) -> Result<Self> {
        let patterns = patterns
            .into_iter()
            .filter(|p| !p.trim().is_empty())
            .map(|p| compile(&p))
            .collect::<Result<_>>()?;
        Ok(Self { patterns })
    }

    /// Returns the pattern that denies `path`, if any. `path` should be
    /// absolute so anchored patterns and multi-component patterns work.
    pub fn denied_by(&self, path: &Path) -> Option<&str> {
        let components: Vec<String> = path
            .components()
            .filter_map(|c| match c {
                Component::Normal(s) => Some(fold(&s.to_string_lossy()).into_owned()),
                _ => None,
            })
            .collect();
        self.patterns
            .iter()
            .find(|p| p.matches(&components))
            .map(|p| p.source.as_str())
    }

    pub fn is_denied(&self, path: &Path) -> bool {
        self.denied_by(path).is_some()
    }
}

impl Pattern {
    fn matches(&self, components: &[String]) -> bool {
        let n = self.components.len();
        if n == 0 || components.len() < n {
            return false;
        }
        let window_matches = |start: usize| {
            self.components
                .iter()
                .zip(&components[start..start + n])
                .all(|(glob, comp)| glob.is_match(comp))
        };
        if self.anchored {
            window_matches(0)
        } else {
            (0..=components.len() - n).any(window_matches)
        }
    }
}

fn compile(source: &str) -> Result<Pattern> {
    let trimmed = source.trim().trim_end_matches('/');
    let anchored = trimmed.starts_with('/');
    let components = trimmed
        .split('/')
        .filter(|c| !c.is_empty())
        .map(|c| {
            GlobBuilder::new(&fold(c))
                .case_insensitive(true)
                .literal_separator(true)
                .backslash_escape(true)
                .build()
                .map(|g| g.compile_matcher())
                .with_context(|| format!("invalid deny pattern {source:?}"))
        })
        .collect::<Result<_>>()?;
    Ok(Pattern {
        source: source.to_string(),
        anchored,
        components,
    })
}

/// A name as case-insensitive filesystems compare it, as far as any of
/// them goes: decomposed (NFD, as APFS and HFS+ store names), without the
/// code points HFS+ ignores (zero-width and direction marks, variation
/// selectors and other default-ignorable characters), and case-folded by
/// mapping each character to upper case and back to lower case, twice.
/// That catches what Unicode case folding catches (`ſ` and `s`, the Kelvin
/// sign and `k`, `ß` and `ss`) and what NTFS's upper-case table adds (`ı`
/// and `i`). Matching folded names on both sides can only deny more.
pub fn fold(name: &str) -> Cow<'_, str> {
    use unicode_normalization::UnicodeNormalization;
    if name.is_ascii() {
        return if name.bytes().any(|b| b.is_ascii_uppercase()) {
            Cow::Owned(name.to_ascii_lowercase())
        } else {
            Cow::Borrowed(name)
        };
    }
    let round = |s: &str| -> String {
        s.nfd()
            .filter(|c| !is_ignorable(*c))
            .flat_map(char::to_uppercase)
            .flat_map(char::to_lowercase)
            .collect()
    };
    let once = round(name);
    Cow::Owned(round(&once).nfd().collect())
}

/// Default-ignorable code points (Unicode's `Default_Ignorable_Code_Point`),
/// which render as nothing and which HFS+ leaves out when it compares names.
fn is_ignorable(c: char) -> bool {
    matches!(
        c,
        '\u{00AD}'
            | '\u{034F}'
            | '\u{061C}'
            | '\u{115F}'..='\u{1160}'
            | '\u{17B4}'..='\u{17B5}'
            | '\u{180B}'..='\u{180F}'
            | '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{206F}'
            | '\u{3164}'
            | '\u{FE00}'..='\u{FE0F}'
            | '\u{FEFF}'
            | '\u{FFA0}'
            | '\u{FFF0}'..='\u{FFF8}'
            | '\u{1BCA0}'..='\u{1BCA3}'
            | '\u{1D173}'..='\u{1D17A}'
            | '\u{E0000}'..='\u{E0FFF}'
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_list() -> DenyList {
        DenyList::new(DEFAULT_DENY.iter().map(|s| s.to_string())).unwrap()
    }

    #[test]
    fn denies_secrets_anywhere() {
        let deny = default_list();
        for path in [
            "/home/u/.ssh/id_ed25519",
            "/home/u/work/.ssh/config",
            "/home/u/work/.env",
            "/home/u/work/.env.production",
            "/home/u/work/certs/server.PEM",
            "/home/u/work/keys/id_rsa",
            "/home/u/.config/gcloud/credentials.db",
            "/home/u/proj/.docker/config.json",
            "/Users/u/Library/Keychains/login.keychain-db",
            "/Users/u/Library/Application Support/Google/Chrome/Default/Cookies",
            "/etc/shadow",
            "/home/u/Vault.KDBX",
            "/home/u/.AWS/credentials",
        ] {
            assert!(deny.is_denied(Path::new(path)), "{path} should be denied");
        }
    }

    #[test]
    fn allows_ordinary_files() {
        let deny = default_list();
        for path in [
            "/home/u/work/notes.md",
            "/home/u/work/environment.md",
            "/home/u/work/ssh-notes.txt",
            "/home/u/.config/other/app.toml",
            "/home/u/docker/config.json",
            "/srv/etc/shadow",
            "/home/u/work/report.pdf",
        ] {
            assert!(!deny.is_denied(Path::new(path)), "{path} should be allowed");
        }
    }

    #[test]
    fn config_can_extend_and_remove() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::under(tmp.path());
        let config = DenyConfig {
            extra: vec!["*.secret".into()],
            remove: vec!["*.key".into()],
            allow_hardlinks: false,
        };
        let deny = DenyList::from_config(&config, &paths).unwrap();
        assert!(deny.is_denied(Path::new("/w/a.secret")));
        assert!(!deny.is_denied(Path::new("/w/slides.key")));
        assert!(deny.is_denied(Path::new("/w/.ssh")));
        assert!(deny.is_denied(&paths.index_dir().join("meta.json")));
        if let Some(trash) = &paths.home_trash {
            assert!(deny.is_denied(&trash.join("files/old.md")));
        }
    }

    /// Names a case-insensitive filesystem treats as a denied name, though
    /// they differ from it in more than ASCII case.
    #[test]
    fn denies_names_that_fold_to_a_denied_one() {
        let deny = default_list();
        let write = DenyList::write_from_config(&DenyConfig::default(), None).unwrap();
        for path in [
            "/home/u/.\u{212A}ube/config", // Kelvin sign for K
            "/home/u/.\u{212A}UBE/config",
            "/home/u/.s\u{017F}h/id_ed25519", // long s
        ] {
            assert!(deny.is_denied(Path::new(path)), "{path} should be denied");
        }
        // A look-alike that no filesystem folds (Cyrillic dze) stays apart.
        assert!(!deny.is_denied(Path::new("/home/u/.\u{0455}sh/x")));
        for path in [
            "/home/u/.z\u{017F}hrc",
            "/home/u/proj/.g\u{0131}t/config", // dotless i, which NTFS upper-cases to I
            "/home/u/proj/.G\u{200D}IT/hooks/pre-commit", // a zero-width joiner HFS+ ignores
        ] {
            assert!(
                write.is_denied(Path::new(path)),
                "{path} should be never changed"
            );
        }
        // Decomposed and precomposed forms match each other both ways.
        let accents =
            DenyList::new(["caf\u{00E9}".to_string(), "re\u{0301}sume\u{0301}".into()]).unwrap();
        assert!(accents.is_denied(Path::new("/w/cafe\u{0301}")));
        assert!(accents.is_denied(Path::new("/w/CAF\u{00C9}")));
        assert!(accents.is_denied(Path::new("/w/r\u{00E9}sum\u{00E9}")));
        // Ordinary names in other scripts stay allowed.
        for path in [
            "/home/u/Dokumente/\u{00DC}bersicht.md",
            "/home/u/\u{65E5}\u{672C}/a.txt",
        ] {
            assert!(!deny.is_denied(Path::new(path)), "{path}");
            assert!(!write.is_denied(Path::new(path)), "{path}");
        }
    }

    #[test]
    fn folds_like_case_insensitive_filesystems() {
        assert_eq!(fold("ReadMe.MD"), "readme.md");
        assert!(matches!(fold("plain.md"), Cow::Borrowed(_)));
        assert_eq!(fold("\u{212A}"), "k");
        assert_eq!(fold("\u{017F}"), "s");
        assert_eq!(fold("Stra\u{00DF}e"), "strasse");
        assert_eq!(fold("STRA\u{1E9E}E"), "strasse");
        assert_eq!(fold("\u{0131}"), "i");
        assert_eq!(fold("a\u{200B}b\u{FEFF}"), "ab");
        assert_eq!(fold("\u{00C9}"), "e\u{0301}");
    }

    #[test]
    fn trashes_are_private() {
        let deny = default_list();
        for path in [
            "/home/u/.local/share/Trash/files/old.md",
            "/media/usb/.Trash-1000/files/old.md",
            "/media/usb/.Trash/1000/files/old.md",
            "/Users/u/.Trash/old.md",
            "/Volumes/Backup/.Trashes/501/old.md",
            "/C:/$Recycle.Bin/S-1-5-21-1/$RABC123.md",
        ] {
            assert!(deny.is_denied(Path::new(path)), "{path}");
        }
    }

    #[test]
    fn nothing_starting_with_a_dot_in_the_home_folder_is_changed() {
        let home = Path::new("/home/u");
        let write = DenyList::write_from_config(&DenyConfig::default(), Some(home)).unwrap();
        for path in [
            "/home/u/.local/bin/ls",
            "/home/u/.config/anything/settings.toml",
            "/home/u/.pythonrc",
            "/home/u/.Inputrc",
        ] {
            assert_eq!(
                write.denied_by(Path::new(path)),
                Some(HOME_DOT_ENTRIES),
                "{path}"
            );
        }
        for path in [
            "/home/u/notes.md",
            "/home/u/Documents/.hidden-notes.md",
            "/home/other/.bashrc2",
        ] {
            assert!(!write.is_denied(Path::new(path)), "{path}");
        }
        let relaxed = DenyList::write_from_config(
            &DenyConfig {
                remove: vec![HOME_DOT_ENTRIES.into()],
                ..DenyConfig::default()
            },
            Some(home),
        )
        .unwrap();
        assert!(!relaxed.is_denied(Path::new("/home/u/.pythonrc")));
        assert!(never_changed(&[]).contains(&HOME_DOT_ENTRIES));
        assert!(!never_changed(&[HOME_DOT_ENTRIES.into()]).contains(&HOME_DOT_ENTRIES));
    }

    #[test]
    fn some_paths_are_never_changed() {
        let write = DenyList::write_from_config(&DenyConfig::default(), None).unwrap();
        for path in [
            "/home/u/proj/.git/config",
            "/home/u/proj/.git/hooks/pre-commit",
            "/home/u/.bashrc",
            "/home/u/proj/.vscode/tasks.json",
            "/home/u/.config/autostart/x.desktop",
            "/home/u/.gitconfig",
            "/home/u/.config/hypr/hyprland.conf",
            "/Users/u/Library/LaunchAgents/x.plist",
            "/C:/Users/u/AppData/Roaming/Microsoft/Windows/Start Menu/Programs/Startup/x.txt",
            "/home/u/proj/.envrc",
            "/home/u/photos/Desktop.ini",
            "/home/u/proj/.venv/lib/python3.13/site-packages/evil.pth",
            "/home/u/proj/sitecustomize.py",
            "/home/u/proj/.nvim.lua",
            "/home/u/proj/.dir-locals.el",
            "/home/u/proj/.husky/pre-commit",
            "/home/u/proj/.pre-commit-config.yaml",
            "/home/u/proj/mise.toml",
            "/home/u/proj/app.code-workspace",
            "/home/u/proj/.gdbinit",
        ] {
            assert!(write.is_denied(Path::new(path)), "{path}");
        }
        // Build files people want edited stay changeable: they run only when
        // the person builds.
        for path in ["/home/u/proj/Makefile", "/home/u/proj/package.json"] {
            assert!(!write.is_denied(Path::new(path)), "{path}");
        }
        for path in ["/home/u/proj/src/main.rs", "/home/u/notes/git.md"] {
            assert!(!write.is_denied(Path::new(path)), "{path}");
        }
        let relaxed = DenyList::write_from_config(
            &DenyConfig {
                remove: vec![".vscode".into()],
                ..DenyConfig::default()
            },
            None,
        )
        .unwrap();
        assert!(!relaxed.is_denied(Path::new("/home/u/proj/.vscode/settings.json")));
        assert!(relaxed.is_denied(Path::new("/home/u/proj/.git/config")));
    }
}
