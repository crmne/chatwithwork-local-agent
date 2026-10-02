//! Answers are Markdown. This parses one (CommonMark, with GitHub's tables
//! and strikethrough, as the web's `marked` with `gfm: true` reads it) into
//! the blocks and runs that `prose.rs` draws.
//!
//! Links to one of the answer's sources become citation chips, as the web's
//! `MessagesHelper#chipify_source_links` does: Google Drive's several URL
//! shapes for one file match by the file's id.

use pulldown_cmark::{Alignment, CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};

use cww::tui::chat::Source;

#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    Paragraph(Vec<Inline>),
    Heading(u8, Vec<Inline>),
    List {
        /// The first number of an ordered list.
        start: Option<u64>,
        items: Vec<Vec<Block>>,
    },
    Quote(Vec<Block>),
    Code {
        language: String,
        text: String,
    },
    Table {
        align: Vec<Alignment>,
        head: Vec<Vec<Inline>>,
        rows: Vec<Vec<Vec<Inline>>>,
    },
    Rule,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Style {
    pub bold: bool,
    pub italic: bool,
    pub strike: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Inline {
    Text {
        text: String,
        style: Style,
        link: Option<String>,
    },
    Code(String),
    /// A citation chip for the answer's source at this index.
    Chip {
        label: String,
        source: usize,
    },
    /// A hard line break.
    Break,
}

/// The longest chip label, as the web truncates it (Rails' `truncate(28)`).
const CHIP_LABEL: usize = 28;

pub fn parse(markdown: &str, sources: &[Source]) -> Vec<Block> {
    let options = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH;
    let mut builder = Builder {
        sources,
        stack: vec![Frame::Root(Vec::new())],
        style: Style::default(),
        link: None,
    };
    for event in Parser::new_ext(markdown, options) {
        builder.event(event);
    }
    // A stream can stop anywhere; close whatever is still open.
    while builder.stack.len() > 1 {
        builder.close();
    }
    match builder.stack.pop() {
        Some(Frame::Root(blocks)) => blocks,
        _ => Vec::new(),
    }
}

/// Drive's several URL shapes for one file share its id.
fn source_key(url: &str) -> &str {
    for marker in ["/d/", "?id=", "&id="] {
        if let Some(start) = url.find(marker) {
            let rest = &url[start + marker.len()..];
            let end = rest
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
                .unwrap_or(rest.len());
            if end >= 15 {
                return &rest[..end];
            }
        }
    }
    url
}

fn truncate(label: &str) -> String {
    if label.chars().count() <= CHIP_LABEL {
        label.to_string()
    } else {
        let kept: String = label.chars().take(CHIP_LABEL - 3).collect();
        format!("{kept}...")
    }
}

enum Frame {
    Root(Vec<Block>),
    Paragraph(Vec<Inline>),
    Heading(u8, Vec<Inline>),
    List {
        start: Option<u64>,
        items: Vec<Vec<Block>>,
    },
    Item {
        blocks: Vec<Block>,
        /// Text of a tight list item, outside any paragraph.
        loose: Vec<Inline>,
    },
    Quote(Vec<Block>),
    Code {
        language: String,
        text: String,
    },
    Table {
        align: Vec<Alignment>,
        head: Vec<Vec<Inline>>,
        rows: Vec<Vec<Vec<Inline>>>,
        in_head: bool,
    },
    Row(Vec<Vec<Inline>>),
    Cell(Vec<Inline>),
    /// A link to a source: its text becomes the chip's label.
    Chip {
        source: usize,
        label: String,
    },
}

struct Builder<'a> {
    sources: &'a [Source],
    stack: Vec<Frame>,
    style: Style,
    link: Option<String>,
}

impl Builder<'_> {
    fn event(&mut self, event: Event<'_>) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(end) => self.end(end),
            Event::Text(text) => self.text(&text),
            Event::Code(code) => match self.stack.last_mut() {
                Some(Frame::Chip { label, .. }) => label.push_str(&code),
                _ => self.inline(Inline::Code(code.into_string())),
            },
            Event::SoftBreak => self.text(" "),
            Event::HardBreak => self.inline(Inline::Break),
            Event::Rule => self.block(Block::Rule),
            Event::Html(html) | Event::InlineHtml(html) => self.text(&html),
            Event::FootnoteReference(_)
            | Event::TaskListMarker(_)
            | Event::InlineMath(_)
            | Event::DisplayMath(_) => {}
        }
    }

    fn start(&mut self, tag: Tag<'_>) {
        let frame = match tag {
            Tag::Paragraph => Frame::Paragraph(Vec::new()),
            Tag::Heading { level, .. } => Frame::Heading(
                match level {
                    HeadingLevel::H1 => 1,
                    HeadingLevel::H2 => 2,
                    HeadingLevel::H3 => 3,
                    _ => 4,
                },
                Vec::new(),
            ),
            Tag::BlockQuote(_) => Frame::Quote(Vec::new()),
            Tag::CodeBlock(kind) => Frame::Code {
                language: match kind {
                    CodeBlockKind::Fenced(info) => info
                        .split_whitespace()
                        .next()
                        .unwrap_or_default()
                        .to_string(),
                    CodeBlockKind::Indented => String::new(),
                },
                text: String::new(),
            },
            Tag::List(start) => Frame::List {
                start,
                items: Vec::new(),
            },
            Tag::Item => Frame::Item {
                blocks: Vec::new(),
                loose: Vec::new(),
            },
            Tag::Table(align) => Frame::Table {
                align,
                head: Vec::new(),
                rows: Vec::new(),
                in_head: false,
            },
            Tag::TableHead => {
                if let Some(Frame::Table { in_head, .. }) = self.stack.last_mut() {
                    *in_head = true;
                }
                Frame::Row(Vec::new())
            }
            Tag::TableRow => Frame::Row(Vec::new()),
            Tag::TableCell => Frame::Cell(Vec::new()),
            Tag::Emphasis => {
                self.style.italic = true;
                return;
            }
            Tag::Strong => {
                self.style.bold = true;
                return;
            }
            Tag::Strikethrough => {
                self.style.strike = true;
                return;
            }
            Tag::Link { dest_url, .. } => {
                let key = source_key(&dest_url);
                let source = self.sources.iter().position(|s| {
                    s.url
                        .as_deref()
                        .is_some_and(|url| !url.is_empty() && source_key(url) == key)
                });
                match source {
                    Some(source) => Frame::Chip {
                        source,
                        label: String::new(),
                    },
                    None => {
                        self.link = Some(dest_url.into_string());
                        return;
                    }
                }
            }
            Tag::Image { dest_url, .. } => {
                // Images aren't shown; their link is.
                self.link = Some(dest_url.into_string());
                return;
            }
            _ => return,
        };
        self.stack.push(frame);
    }

    fn end(&mut self, end: TagEnd) {
        match end {
            TagEnd::Emphasis => self.style.italic = false,
            TagEnd::Strong => self.style.bold = false,
            TagEnd::Strikethrough => self.style.strike = false,
            TagEnd::Link => {
                if matches!(self.stack.last(), Some(Frame::Chip { .. })) {
                    self.close();
                } else {
                    self.link = None;
                }
            }
            TagEnd::Image => self.link = None,
            TagEnd::Paragraph
            | TagEnd::Heading(_)
            | TagEnd::BlockQuote(_)
            | TagEnd::CodeBlock
            | TagEnd::List(_)
            | TagEnd::Item
            | TagEnd::Table
            | TagEnd::TableHead
            | TagEnd::TableRow
            | TagEnd::TableCell => self.close(),
            _ => {}
        }
    }

    /// Pop the innermost frame into its parent.
    fn close(&mut self) {
        let Some(frame) = self.stack.pop() else {
            return;
        };
        match frame {
            Frame::Root(blocks) => self.stack.push(Frame::Root(blocks)),
            Frame::Paragraph(inlines) => {
                if !inlines.is_empty() {
                    self.block(Block::Paragraph(inlines));
                }
            }
            Frame::Heading(level, inlines) => self.block(Block::Heading(level, inlines)),
            Frame::Quote(blocks) => self.block(Block::Quote(blocks)),
            Frame::Code { language, text } => {
                let text = text.strip_suffix('\n').unwrap_or(&text).to_string();
                self.block(Block::Code { language, text });
            }
            Frame::List { start, items } => self.block(Block::List { start, items }),
            Frame::Item { mut blocks, loose } => {
                if !loose.is_empty() {
                    blocks.insert(0, Block::Paragraph(loose));
                }
                if let Some(Frame::List { items, .. }) = self.stack.last_mut() {
                    items.push(blocks);
                }
            }
            Frame::Table {
                align, head, rows, ..
            } => self.block(Block::Table { align, head, rows }),
            Frame::Row(cells) => {
                if let Some(Frame::Table {
                    head,
                    rows,
                    in_head,
                    ..
                }) = self.stack.last_mut()
                {
                    if std::mem::take(in_head) {
                        *head = cells;
                    } else {
                        rows.push(cells);
                    }
                }
            }
            Frame::Cell(inlines) => {
                if let Some(Frame::Row(cells)) = self.stack.last_mut() {
                    cells.push(inlines);
                }
            }
            Frame::Chip { source, label } => {
                let label = if label.trim().is_empty() {
                    self.sources[source].title.clone()
                } else {
                    label
                };
                self.inline(Inline::Chip {
                    label: truncate(label.trim()),
                    source,
                });
            }
        }
    }

    fn block(&mut self, block: Block) {
        match self.stack.last_mut() {
            Some(Frame::Root(blocks) | Frame::Quote(blocks)) => blocks.push(block),
            Some(Frame::Item { blocks, loose }) => {
                if !loose.is_empty() {
                    blocks.push(Block::Paragraph(std::mem::take(loose)));
                }
                blocks.push(block);
            }
            Some(Frame::List { items, .. }) => items.push(vec![block]),
            _ => {}
        }
    }

    fn text(&mut self, text: &str) {
        match self.stack.last_mut() {
            Some(Frame::Code { text: code, .. }) => code.push_str(text),
            Some(Frame::Chip { label, .. }) => label.push_str(text),
            _ => self.inline(Inline::Text {
                text: text.to_string(),
                style: self.style,
                link: self.link.clone(),
            }),
        }
    }

    fn inline(&mut self, inline: Inline) {
        let target = match self.stack.last_mut() {
            Some(Frame::Paragraph(inlines) | Frame::Heading(_, inlines) | Frame::Cell(inlines)) => {
                inlines
            }
            Some(Frame::Item { loose, .. }) => loose,
            Some(Frame::Root(_) | Frame::Quote(_)) => {
                // Inline text outside a paragraph (an unclosed stream).
                self.stack.push(Frame::Paragraph(Vec::new()));
                return self.inline(inline);
            }
            _ => return,
        };
        // Join neighbouring runs of the same style, so a paragraph is one
        // run per style change rather than one per parser event.
        if let (
            Inline::Text { text, style, link },
            Some(Inline::Text {
                text: last,
                style: last_style,
                link: last_link,
            }),
        ) = (&inline, target.last_mut())
            && style == last_style
            && link == last_link
        {
            last.push_str(text);
            return;
        }
        target.push(inline);
    }
}

/// The plain text of some runs, for accessibility and copying.
pub fn plain(inlines: &[Inline]) -> String {
    let mut out = String::new();
    for inline in inlines {
        match inline {
            Inline::Text { text, .. } | Inline::Code(text) => out.push_str(text),
            Inline::Chip { label, .. } => out.push_str(label),
            Inline::Break => out.push('\n'),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(t: &str) -> Inline {
        Inline::Text {
            text: t.into(),
            style: Style::default(),
            link: None,
        }
    }

    #[test]
    fn reads_the_blocks_an_answer_uses() {
        let md = "# Title\n\nSome **bold** and `code`.\n\n- one\n- two\n  1. nested\n\n> quoted\n\n```json\n{\"a\": 1}\n```\n\n| A | B |\n|---|--:|\n| 1 | 2 |\n\n---\n";
        let blocks = parse(md, &[]);
        assert!(matches!(&blocks[0], Block::Heading(1, _)));
        let Block::Paragraph(runs) = &blocks[1] else {
            panic!("{blocks:?}")
        };
        assert_eq!(runs[0], text("Some "));
        assert!(matches!(
            &runs[1],
            Inline::Text {
                style: Style { bold: true, .. },
                ..
            }
        ));
        assert_eq!(runs[3], Inline::Code("code".into()));
        let Block::List { start: None, items } = &blocks[2] else {
            panic!("{blocks:?}")
        };
        assert_eq!(items.len(), 2);
        assert!(matches!(&items[1][1], Block::List { start: Some(1), .. }));
        assert!(matches!(&blocks[3], Block::Quote(_)));
        assert_eq!(
            blocks[4],
            Block::Code {
                language: "json".into(),
                text: "{\"a\": 1}".into()
            }
        );
        let Block::Table { align, head, rows } = &blocks[5] else {
            panic!("{blocks:?}")
        };
        assert_eq!(align[1], Alignment::Right);
        assert_eq!(head.len(), 2);
        assert_eq!(rows[0][1], vec![text("2")]);
        assert_eq!(blocks[6], Block::Rule);
    }

    #[test]
    fn links_to_sources_become_chips() {
        let sources = vec![Source {
            title: "Acme renewal 2026 — final signed version.pdf".into(),
            url: Some("https://drive.google.com/file/d/1AcmeRenewal2026Draft/view".into()),
            icon: None,
        }];
        let md = "Renews in March [renewal](https://docs.google.com/document/d/1AcmeRenewal2026Draft/edit), \
                  see [the site](https://acme.example/terms).";
        let blocks = parse(md, &sources);
        let Block::Paragraph(runs) = &blocks[0] else {
            panic!()
        };
        assert_eq!(
            runs[1],
            Inline::Chip {
                label: "renewal".into(),
                source: 0
            }
        );
        assert!(
            matches!(&runs[3], Inline::Text { link: Some(l), .. } if l == "https://acme.example/terms")
        );
        assert_eq!(truncate(&sources[0].title), "Acme renewal 2026 — final...");
    }

    #[test]
    fn a_half_written_answer_still_parses() {
        let blocks = parse("Q3 came in **under", &[]);
        assert_eq!(
            plain(match &blocks[0] {
                Block::Paragraph(runs) => runs,
                _ => panic!(),
            }),
            "Q3 came in **under"
        );
        let blocks = parse("```rust\nfn main() {", &[]);
        assert!(matches!(&blocks[0], Block::Code { text, .. } if text == "fn main() {"));
    }
}
