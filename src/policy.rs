//! The default-deny list for secrets.
//!
//! Patterns are matched case-insensitively against the path components of the
//! absolute path, even inside shared roots:
//!
//! - A pattern without `/` (such as `.ssh` or `*.pem`) matches any single
//!   component.
//! - A pattern with `/` (such as `.config/gcloud`) matches that run of
//!   consecutive components anywhere in the path.
//! - A pattern starting with `/` (such as `/etc/shadow`) is anchored at the
//!   filesystem root.
//!
//! Matching a directory denies everything below it.

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
];

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
        let kept = |p: &&&str| {
            !config
                .remove
                .iter()
                .any(|r| r.eq_ignore_ascii_case(p.trim_end_matches('/')))
        };
        let patterns = DEFAULT_DENY
            .iter()
            .filter(kept)
            .map(|p| p.to_string())
            .chain(config.extra.iter().cloned())
            .chain(own_dirs);
        Self::new(patterns)
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
                Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
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
            GlobBuilder::new(c)
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
    }
}
