//! Which names and kinds of file a change may produce.
//!
//! cww never makes anything that runs: no executables, scripts the system
//! runs on a double-click, installers, shortcuts or launchers, and never an
//! execute bit. It writes text files as text, and Word and Excel documents
//! only through `create_document`, never as raw text.

use crate::error::{ErrorCode, ToolError};

/// Extensions of things that run, install, or open something else when
/// clicked, on any of the three systems. Lower case, without the dot.
const EXECUTABLE: &[&str] = &[
    // Windows programs, scripts, installers, shortcuts and handlers
    "exe",
    "com",
    "bat",
    "cmd",
    "msi",
    "msix",
    "msixbundle",
    "appx",
    "appxbundle",
    "msp",
    "mst",
    "scr",
    "pif",
    "cpl",
    "ps1",
    "psm1",
    "psd1",
    "ps1xml",
    "psc1",
    "psc2",
    "vbs",
    "vbe",
    "jse",
    "wsf",
    "wsh",
    "wsc",
    "hta",
    "lnk",
    "url",
    "website",
    "reg",
    "inf",
    "scf",
    "application",
    "appref-ms",
    "gadget",
    "dll",
    "sys",
    "ocx",
    "drv",
    "msc",
    "xbap",
    "chm",
    "hlp",
    "library-ms",
    "search-ms",
    "searchconnector-ms",
    "settingcontent-ms",
    "diagcab",
    "appinstaller",
    "vsix",
    "xll",
    // Office files with macros, and add-ins
    "docm",
    "dotm",
    "xlsm",
    "xltm",
    "xlam",
    "xla",
    "pptm",
    "potm",
    "ppsm",
    "ppam",
    "ppa",
    "sldm",
    "accde",
    "mde",
    // macOS
    "app",
    "command",
    "tool",
    "pkg",
    "mpkg",
    "dmg",
    "workflow",
    "terminal",
    "action",
    "webloc",
    "inetloc",
    "fileloc",
    "scpt",
    "scptd",
    "applescript",
    "kext",
    "prefpane",
    "plugin",
    "dylib",
    // Linux and Java
    "desktop",
    "so",
    "appimage",
    "run",
    "deb",
    "rpm",
    "flatpakref",
    "snap",
    "jar",
    "jnlp",
    "apk",
];

/// Extensions that are executable on Windows only, where a double-click
/// runs them with Windows Script Host.
const WINDOWS_SCRIPTS: &[&str] = &["js"];

/// Kinds of file the text tools must not write: documents and media that
/// text would corrupt. Lower case, without the dot.
const BINARY: &[&str] = &[
    "pdf", "doc", "docx", "dot", "dotx", "xls", "xlsx", "xlsb", "xlt", "xltx", "ppt", "pptx",
    "pps", "ppsx", "pot", "potx", "odt", "ods", "odp", "odg", "ott", "pages", "numbers", "key",
    "epub", "mobi", "zip", "gz", "tgz", "bz2", "xz", "zst", "7z", "rar", "tar", "war", "png",
    "jpg", "jpeg", "gif", "bmp", "tif", "tiff", "webp", "heic", "heif", "avif", "ico", "icns",
    "psd", "ai", "sketch", "fig", "mp3", "mp4", "m4a", "m4v", "mov", "avi", "mkv", "wav", "flac",
    "ogg", "opus", "webm", "aac", "wma", "wmv", "ttf", "otf", "woff", "woff2", "eot", "sqlite",
    "sqlite3", "db", "mdb", "accdb", "wasm", "class", "o", "a", "lib", "obj", "pyc", "iso", "img",
    "vhd", "vhdx", "vmdk", "qcow2", "bin",
];

/// Names Windows keeps for devices, with or without an extension.
const DEVICE_NAMES: &[&str] = &[
    "con", "prn", "aux", "nul", "conin$", "conout$", "com0", "com1", "com2", "com3", "com4",
    "com5", "com6", "com7", "com8", "com9", "lpt0", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6",
    "lpt7", "lpt8", "lpt9", "com¹", "com²", "com³", "lpt¹", "lpt²", "lpt³",
];

/// The last extension of `name`, lower case.
pub fn extension(name: &str) -> Option<String> {
    let (stem, ext) = name.rsplit_once('.')?;
    (!stem.is_empty() && !ext.is_empty()).then(|| ext.to_lowercase())
}

fn not_changeable(message: impl Into<String>) -> ToolError {
    ToolError::new(ErrorCode::NotChangeable, message)
}

/// Whether `name` is something that runs: by extension, on any system.
pub fn is_executable_name(name: &str) -> bool {
    match extension(name) {
        Some(ext) => {
            EXECUTABLE.contains(&ext.as_str())
                || (cfg!(windows) && WINDOWS_SCRIPTS.contains(&ext.as_str()))
        }
        None => false,
    }
}

/// Check a name a change is about to give something: a new file or folder,
/// or the target of a move.
pub fn check_new_name(name: &str) -> Result<(), ToolError> {
    if name.is_empty() || name == "." || name == ".." {
        return Err(ToolError::invalid_path("This name is empty."));
    }
    if name.chars().any(|c| {
        c.is_control()
            || matches!(
                c,
                '/' | '\\'
                    | ':'
                    | '*'
                    | '?'
                    | '"'
                    | '<'
                    | '>'
                    | '|'
                    | '\u{200E}'
                    | '\u{200F}'
                    | '\u{202A}'..='\u{202E}'
                    | '\u{2066}'..='\u{2069}'
                    | '\u{FEFF}'
            )
    }) {
        return Err(ToolError::invalid_path(
            "Names can't contain control characters, direction marks, or any of \\ / : * ? \" \
             < > |.",
        ));
    }
    if name.ends_with('.') || name.ends_with(' ') || name.starts_with(' ') {
        // Windows drops trailing dots and spaces, so `x.exe.` would become
        // `x.exe`.
        return Err(ToolError::invalid_path(
            "Names can't start with a space or end with a dot or a space.",
        ));
    }
    let stem = name.split('.').next().unwrap_or(name).to_lowercase();
    if DEVICE_NAMES.contains(&stem.trim_end()) {
        return Err(ToolError::invalid_path(format!(
            "{name} is a name Windows keeps for a device."
        )));
    }
    if is_executable_name(name) {
        return Err(not_changeable(format!(
            "cww never creates programs, scripts that run when opened, installers or \
             shortcuts, so it can't make {name}."
        )));
    }
    Ok(())
}

/// Check that the text tools may write a file named `name`.
pub fn check_text_name(name: &str) -> Result<(), ToolError> {
    if is_executable_name(name) {
        return Err(not_changeable(format!(
            "cww never changes programs, scripts that run when opened, installers or \
             shortcuts, so it can't change {name}."
        )));
    }
    match extension(name).as_deref() {
        Some("docx" | "xlsx") => Err(not_changeable(format!(
            "{name} is an Office document, which text would corrupt. Use create_document to \
             make it."
        ))),
        Some(ext) if BINARY.contains(&ext) => Err(not_changeable(format!(
            "{name} isn't a text file, and writing text to it would corrupt it."
        ))),
        _ => Ok(()),
    }
}

/// Check text a tool is about to write.
pub fn check_text(text: &str, max: u64) -> Result<(), ToolError> {
    if text.contains('\0') {
        return Err(not_changeable(
            "The text contains NUL characters; only text files can be written.",
        ));
    }
    if text.len() as u64 > max {
        return Err(ToolError::new(
            ErrorCode::TooLarge,
            format!(
                "The new content is {} bytes; files up to {max} bytes can be changed.",
                text.len()
            ),
        ));
    }
    Ok(())
}

/// The text of an existing file, if it is text (UTF-8, no NUL bytes).
pub fn as_text(bytes: Vec<u8>, name: &str) -> Result<String, ToolError> {
    let text = String::from_utf8(bytes).map_err(|_| {
        not_changeable(format!(
            "{name} isn't UTF-8 text, so it can't be changed as text."
        ))
    })?;
    if text.contains('\0') {
        return Err(not_changeable(format!(
            "{name} isn't a text file, so it can't be changed as text."
        )));
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_programs_and_launchers() {
        for name in [
            "setup.exe",
            "RUN.BAT",
            "x.ps1",
            "invoice.pdf.scr",
            "evil.lnk",
            "a.desktop",
            "Thing.app",
            "go.command",
            "doc.docm",
            "budget.xlsm",
            "lib.so",
            "tool.jar",
            "x.hta",
            "x.url",
        ] {
            let err = check_new_name(name).unwrap_err();
            assert_eq!(err.code, ErrorCode::NotChangeable, "{name}");
            assert!(check_text_name(name).is_err(), "{name}");
        }
        assert_eq!(check_new_name("app.js").is_err(), cfg!(windows));
    }

    #[test]
    fn refuses_names_windows_would_change() {
        for name in [
            "x.exe.",
            "x.exe ",
            " lead",
            "CON",
            "nul.txt",
            "Com1.md",
            "a:b",
            "a\\b",
            "a*b",
            "tab\there",
            "invoice\u{202E}fdp.exe",
            "",
            "..",
        ] {
            assert!(check_new_name(name).is_err(), "{name:?}");
        }
    }

    #[test]
    fn allows_ordinary_names() {
        for name in [
            "notes.md",
            "Q3 plan (draft).txt",
            ".gitignore",
            "data.csv",
            "main.rs",
            "script.py",
            "run.sh",
            "Makefile",
            "README",
            "über.md",
            "configure.ac",
            "console.md",
        ] {
            check_new_name(name).unwrap();
            check_text_name(name).unwrap();
        }
    }

    #[test]
    fn text_tools_leave_documents_and_media_alone() {
        for name in [
            "report.pdf",
            "Budget.XLSX",
            "photo.jpg",
            "archive.zip",
            "deck.pptx",
        ] {
            let err = check_text_name(name).unwrap_err();
            assert_eq!(err.code, ErrorCode::NotChangeable, "{name}");
        }
        assert!(
            check_text_name("plan.docx")
                .unwrap_err()
                .message
                .contains("create_document")
        );
    }

    #[test]
    fn text_is_utf8_without_nul() {
        assert!(check_text("hello\n", 10).is_ok());
        assert_eq!(
            check_text("a\0b", 10).unwrap_err().code,
            ErrorCode::NotChangeable
        );
        assert_eq!(
            check_text("hello", 3).unwrap_err().code,
            ErrorCode::TooLarge
        );
        assert!(as_text(b"ok".to_vec(), "a").is_ok());
        assert!(as_text(vec![0xff, 0xfe], "a").is_err());
        assert!(as_text(b"MZ\0\0".to_vec(), "a").is_err());
    }
}
