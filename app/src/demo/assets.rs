//! The stand-in daemon's images: synthetic logos and file-type icons drawn
//! for the demo (in `assets/`, dedicated to the public domain, CC0-1.0),
//! served at fingerprinted `/assets/` paths as the paired server serves
//! the web's own. None of them is a real service's mark, and only demo and
//! test builds include them.

use serde_json::{Value, json};

/// Path, file, content type, and whether it's a single-colour logo.
const ASSETS: [(&str, &[u8], &str, bool); 11] = [
    (
        "/assets/providers/drive-7a3c91e0.svg",
        include_bytes!("assets/drive.svg"),
        "image/svg+xml",
        false,
    ),
    (
        "/assets/providers/slack-2b8d4f61.svg",
        include_bytes!("assets/slack.svg"),
        "image/svg+xml",
        false,
    ),
    (
        "/assets/providers/notion-c41e7d02.svg",
        include_bytes!("assets/notion.svg"),
        "image/svg+xml",
        true,
    ),
    (
        "/assets/providers/linear-9e5a3b17.svg",
        include_bytes!("assets/linear.svg"),
        "image/svg+xml",
        false,
    ),
    (
        "/assets/providers/vertexai-4d2e8f90.svg",
        include_bytes!("assets/gemini.svg"),
        "image/svg+xml",
        false,
    ),
    (
        "/assets/providers/azure-6a1b2c3d.svg",
        include_bytes!("assets/sol.svg"),
        "image/svg+xml",
        true,
    ),
    (
        "/assets/providers/hetzner-8f7e6d5c.png",
        include_bytes!("assets/blocks.png"),
        "image/png",
        false,
    ),
    (
        "/assets/mimetypes/application-pdf-1e2d3c4b.svg",
        include_bytes!("assets/pdf.svg"),
        "image/svg+xml",
        false,
    ),
    (
        "/assets/mimetypes/x-office-spreadsheet-5a6b7c8d.svg",
        include_bytes!("assets/spreadsheet.svg"),
        "image/svg+xml",
        false,
    ),
    (
        "/assets/mimetypes/x-office-document-9a8b7c6d.svg",
        include_bytes!("assets/document.svg"),
        "image/svg+xml",
        false,
    ),
    (
        "/assets/mimetypes/text-plain-0f1e2d3c.svg",
        include_bytes!("assets/text.svg"),
        "image/svg+xml",
        false,
    ),
];

/// The image for a service, a model's maker or a file type, as chat JSON
/// gives it: `{"path", "monochrome"}`.
pub fn logo(name: &str) -> Value {
    let (path, _, _, monochrome) = ASSETS
        .iter()
        .find(|(path, ..)| {
            path.rsplit('/')
                .next()
                .is_some_and(|file| file.starts_with(&format!("{name}-")))
        })
        .unwrap_or_else(|| panic!("no demo image for {name}"));
    json!({ "path": path, "monochrome": monochrome })
}

/// A file type's icon, for a content type.
pub fn file_icon(content_type: &str) -> Value {
    logo(if content_type == "application/pdf" {
        "application-pdf"
    } else if content_type.contains("spreadsheet") || content_type == "text/csv" {
        "x-office-spreadsheet"
    } else if content_type.contains("word") || content_type.contains("document") {
        "x-office-document"
    } else {
        "text-plain"
    })
}

/// `asset`: the image at `path`, in base64, as the daemon answers.
pub fn serve(path: &str) -> Value {
    match ASSETS.iter().find(|(p, ..)| *p == path) {
        Some((path, bytes, content_type, _)) => json!({
            "ok": true,
            "path": path,
            "content_type": content_type,
            "data": base64(bytes),
        }),
        None => {
            json!({ "ok": false, "code": "not_found", "error": "Chat with Work has no such image." })
        }
    }
}

fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = u32::from(b[0]) << 16 | u32::from(b[1]) << 8 | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(TABLE[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn base64_reads_back() {
        assert_eq!(super::base64(b"Man"), "TWFu");
        assert_eq!(super::base64(b"Ma"), "TWE=");
        assert_eq!(super::base64(b"M"), "TQ==");
    }
}
