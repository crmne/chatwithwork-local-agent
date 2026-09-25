//! Opening pages in the user's default browser.

/// An http(s) URL made only of characters no shell or URL handler treats
/// specially.
fn plain_url(url: &str) -> bool {
    (url.starts_with("https://") || url.starts_with("http://"))
        && url
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || ":/?=._~%-".contains(c))
}

/// Open `url` in the default browser. Only plain http(s) URLs with
/// unremarkable characters are handed to the system.
pub fn open(url: &str) -> bool {
    if !plain_url(url) {
        return false;
    }
    let mut command = if cfg!(target_os = "macos") {
        std::process::Command::new("open")
    } else if cfg!(windows) {
        let mut c = std::process::Command::new("rundll32");
        c.arg("url.dll,FileProtocolHandler");
        c
    } else {
        std::process::Command::new("xdg-open")
    };
    command
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .is_ok()
}

#[cfg(test)]
mod tests {
    #[test]
    fn only_plain_urls_go_to_the_browser() {
        assert!(super::plain_url(
            "https://chatwithwork.com/device?user_code=WDJB-MJHT"
        ));
        assert!(super::plain_url("http://localhost:3417/device"));
        for bad in [
            "file:///etc/passwd",
            "javascript:alert(1)",
            "https://x.example/a&calc.exe",
            "https://x.example/a b",
            "https://x.example/\"quoted\"",
            "https://x.example/$(id)",
        ] {
            assert!(!super::plain_url(bad), "{bad}");
        }
    }
}
