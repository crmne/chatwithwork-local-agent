//! Code blocks in answers, colored as the web's highlight.js theme colors
//! them (`code.css`): keywords violet, strings green, numbers, literals and
//! attributes orange, titles and types blue and lime, comments faint and
//! italic. A small tokenizer rather than highlight.js's grammars: it knows
//! comments, strings, numbers and the keywords of the usual languages, and
//! JSON's keys.

use egui::Color32;
use egui::text::LayoutJob;

use super::tokens::{Palette, scale};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Plain,
    Comment,
    Keyword,
    Literal,
    String,
    Number,
    Attr,
    Title,
}

const KEYWORDS: &[&str] = &[
    "fn",
    "let",
    "mut",
    "pub",
    "use",
    "mod",
    "impl",
    "struct",
    "enum",
    "trait",
    "match",
    "if",
    "else",
    "for",
    "while",
    "loop",
    "return",
    "break",
    "continue",
    "in",
    "as",
    "where",
    "async",
    "await",
    "move",
    "const",
    "static",
    "type",
    "def",
    "class",
    "end",
    "do",
    "module",
    "require",
    "import",
    "from",
    "export",
    "function",
    "var",
    "new",
    "this",
    "self",
    "Self",
    "elif",
    "lambda",
    "yield",
    "try",
    "catch",
    "except",
    "finally",
    "raise",
    "throw",
    "with",
    "not",
    "and",
    "or",
    "interface",
    "package",
    "func",
    "go",
    "defer",
    "select",
    "case",
    "switch",
    "default",
    "begin",
    "rescue",
    "ensure",
    "unless",
    "then",
    "SELECT",
    "FROM",
    "WHERE",
    "AND",
    "OR",
    "ORDER",
    "BY",
    "GROUP",
    "JOIN",
    "ON",
    "INSERT",
    "INTO",
    "VALUES",
    "UPDATE",
    "SET",
    "DELETE",
    "LIMIT",
];

const LITERALS: &[&str] = &[
    "true",
    "false",
    "null",
    "nil",
    "None",
    "True",
    "False",
    "undefined",
];

fn color(kind: Kind, p: &Palette) -> Color32 {
    match kind {
        Kind::Plain => p.ink,
        Kind::Comment => p.ink_faint,
        Kind::Keyword => p.violet_ink,
        // highlight.js marks `true` as a keyword inside a literal.
        Kind::Literal => p.violet_ink,
        Kind::Number | Kind::Attr => p.orange_ink,
        Kind::String => p.green_ink,
        Kind::Title => p.blue_ink,
    }
}

fn tokens<'a>(code: &'a str, language: &str) -> Vec<(Kind, &'a str)> {
    let hash_comments = matches!(
        language,
        "python"
            | "py"
            | "ruby"
            | "rb"
            | "bash"
            | "sh"
            | "shell"
            | "zsh"
            | "yaml"
            | "yml"
            | "toml"
            | "r"
    );
    let json = matches!(language, "json" | "jsonc");
    let bytes = code.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    let mut plain_start = 0;
    let mut previous_word: &'a str = "";
    let flush = |out: &mut Vec<(Kind, &'a str)>, from: usize, to: usize| {
        if to > from {
            out.push((Kind::Plain, &code[from..to]));
        }
    };
    while i < bytes.len() {
        let c = bytes[i];
        let rest = &code[i..];
        let (kind, len) = if rest.starts_with("//") && !hash_comments && !json
            || (c == b'#' && hash_comments)
            || rest.starts_with("--") && language == "sql"
        {
            (Kind::Comment, rest.find('\n').unwrap_or(rest.len()))
        } else if rest.starts_with("/*") {
            (Kind::Comment, rest.find("*/").map_or(rest.len(), |e| e + 2))
        } else if c == b'"' || c == b'\'' || c == b'`' {
            let mut j = 1;
            while j < rest.len() {
                let b = rest.as_bytes()[j];
                if b == b'\\' {
                    j += 2;
                    continue;
                }
                if b == c || b == b'\n' {
                    j += 1;
                    break;
                }
                j += 1;
            }
            let len = j.min(rest.len());
            // A JSON key is a string followed by a colon.
            let after = rest[len..].trim_start();
            let kind = if json && after.starts_with(':') {
                Kind::Attr
            } else {
                Kind::String
            };
            (kind, len)
        } else if c.is_ascii_digit() && (i == 0 || !is_word(bytes[i - 1])) {
            let len = rest
                .find(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '.' || ch == '_'))
                .unwrap_or(rest.len());
            (Kind::Number, len)
        } else if is_word(c) && (i == 0 || !is_word(bytes[i - 1])) {
            let len = rest
                .find(|ch: char| !is_word_char(ch))
                .unwrap_or(rest.len());
            let word = &rest[..len];
            let kind = if LITERALS.contains(&word) {
                Kind::Literal
            } else if KEYWORDS.contains(&word) {
                Kind::Keyword
            } else if matches!(
                previous_word,
                "fn" | "def"
                    | "function"
                    | "class"
                    | "struct"
                    | "enum"
                    | "func"
                    | "trait"
                    | "module"
            ) {
                Kind::Title
            } else {
                Kind::Plain
            };
            previous_word = word;
            if kind == Kind::Plain {
                i += len;
                continue;
            }
            (kind, len)
        } else {
            i += rest.chars().next().map_or(1, char::len_utf8);
            continue;
        };
        flush(&mut out, plain_start, i);
        out.push((kind, &code[i..i + len]));
        i += len;
        plain_start = i;
    }
    flush(&mut out, plain_start, code.len());
    out
}

fn is_word(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn is_word_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// `code` in Geist Mono, colored for `language`.
pub fn job(code: &str, language: &str, palette: &Palette) -> LayoutJob {
    let mut job = LayoutJob::default();
    let language = language.to_ascii_lowercase();
    for (kind, text) in tokens(code, &language) {
        let mut format = scale::CODE.format(color(kind, palette));
        format.italics = kind == Kind::Comment;
        job.append(text, 0.0, format);
    }
    job
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors_json_as_highlight_js_does() {
        let found = tokens(
            "{\n  \"vendor\": \"Acme\",\n  \"fee\": 42000,\n  \"auto\": true\n}",
            "json",
        );
        let kinds: Vec<(Kind, &str)> = found
            .into_iter()
            .filter(|(k, _)| *k != Kind::Plain)
            .collect();
        assert_eq!(
            kinds,
            vec![
                (Kind::Attr, "\"vendor\""),
                (Kind::String, "\"Acme\""),
                (Kind::Attr, "\"fee\""),
                (Kind::Number, "42000"),
                (Kind::Attr, "\"auto\""),
                (Kind::Literal, "true"),
            ]
        );
    }

    #[test]
    fn knows_comments_keywords_and_titles() {
        let found = tokens("fn main() { // go\n    let x = \"hi\";\n}", "rust");
        assert!(found.contains(&(Kind::Keyword, "fn")));
        assert!(found.contains(&(Kind::Title, "main")));
        assert!(found.contains(&(Kind::Comment, "// go")));
        assert!(found.contains(&(Kind::String, "\"hi\"")));
        let python = tokens("# note\nx = 1", "python");
        assert_eq!(python[0], (Kind::Comment, "# note"));
        // Every byte is kept, in order.
        let code = "a = 'x' # y\nb";
        assert_eq!(
            tokens(code, "ruby")
                .iter()
                .map(|(_, t)| *t)
                .collect::<String>(),
            code
        );
    }
}
