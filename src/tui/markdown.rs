//! Answers are Markdown. This draws the parts that read well in a terminal:
//! headings, bold and italic, inline code, lists, quotes, rules, tables as
//! plain rows, fenced code, and links, which become numbered footnotes next
//! to the answer's sources, as the web's citation chips do.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::theme::Theme;

/// An answer's footnotes: its sources first, then any other link it makes.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Footnotes {
    pub notes: Vec<(String, Option<String>)>,
}

impl Footnotes {
    pub fn new(sources: impl IntoIterator<Item = (String, Option<String>)>) -> Self {
        let mut notes = Self::default();
        for (title, url) in sources {
            notes.number(&title, url.as_deref());
        }
        notes
    }

    /// The footnote for a link, numbered from 1; the same URL keeps its number.
    fn number(&mut self, title: &str, url: Option<&str>) -> usize {
        let existing = url.and_then(|url| {
            self.notes
                .iter()
                .position(|(_, u)| u.as_deref() == Some(url))
        });
        existing.unwrap_or_else(|| {
            self.notes
                .push((title.to_string(), url.map(str::to_string)));
            self.notes.len() - 1
        }) + 1
    }
}

/// `text` as lines at most `width` wide.
pub fn render(
    text: &str,
    width: usize,
    theme: &Theme,
    notes: &mut Footnotes,
) -> Vec<Line<'static>> {
    let width = width.max(12);
    let mut out: Vec<Line<'static>> = Vec::new();
    let mut paragraph = String::new();
    let mut code: Option<Vec<String>> = None;

    let flush = |paragraph: &mut String, out: &mut Vec<Line<'static>>, notes: &mut Footnotes| {
        if !paragraph.is_empty() {
            let spans = inline(paragraph, theme, notes, theme.ink());
            out.extend(wrap_spans(spans, width, Vec::new(), Vec::new()));
            paragraph.clear();
        }
    };

    for raw in text.lines() {
        let line = raw.trim_end();
        let trimmed = line.trim_start();
        if let Some(block) = code.as_mut() {
            if trimmed.starts_with("```") {
                out.extend(code_block(block, width, theme));
                code = None;
            } else {
                block.push(line.to_string());
            }
            continue;
        }
        if trimmed.starts_with("```") {
            flush(&mut paragraph, &mut out, notes);
            code = Some(Vec::new());
            continue;
        }
        if trimmed.is_empty() {
            flush(&mut paragraph, &mut out, notes);
            if out.last().is_some_and(|l| l.width() > 0) {
                out.push(Line::raw(""));
            }
            continue;
        }
        if let Some((level, heading)) = heading(trimmed) {
            flush(&mut paragraph, &mut out, notes);
            if out.last().is_some_and(|l| l.width() > 0) {
                out.push(Line::raw(""));
            }
            let style = if level <= 2 {
                theme.strong().add_modifier(Modifier::UNDERLINED)
            } else {
                theme.strong()
            };
            let spans = inline(heading, theme, notes, style);
            out.extend(wrap_spans(spans, width, Vec::new(), Vec::new()));
            continue;
        }
        if is_rule(trimmed) {
            flush(&mut paragraph, &mut out, notes);
            out.push(Line::styled("─".repeat(width.min(40)), theme.line()));
            continue;
        }
        if let Some(quote) = trimmed.strip_prefix('>') {
            flush(&mut paragraph, &mut out, notes);
            let spans = inline(quote.trim_start(), theme, notes, theme.muted());
            let bar = vec![Span::styled("▎ ", theme.faint())];
            out.extend(wrap_spans(spans, width, bar.clone(), bar));
            continue;
        }
        if let Some((indent, marker, item)) = list_item(line) {
            flush(&mut paragraph, &mut out, notes);
            let pad = "  ".repeat(indent);
            let first = vec![
                Span::raw(pad.clone()),
                Span::styled(format!("{marker} "), theme.muted()),
            ];
            let rest = vec![Span::raw(format!(
                "{pad}{}",
                " ".repeat(marker.chars().count() + 1)
            ))];
            let spans = inline(item, theme, notes, theme.ink());
            out.extend(wrap_spans(spans, width, first, rest));
            continue;
        }
        if trimmed.starts_with('|') {
            flush(&mut paragraph, &mut out, notes);
            if let Some(row) = table_row(trimmed) {
                let text = row.join("  │  ");
                out.push(Line::styled(ellipsize(&text, width), theme.muted()));
            }
            continue;
        }
        if !paragraph.is_empty() {
            paragraph.push(' ');
        }
        paragraph.push_str(trimmed);
    }
    if let Some(block) = code {
        out.extend(code_block(&block, width, theme));
    }
    flush(&mut paragraph, &mut out, notes);
    while out.last().is_some_and(|l| l.width() == 0) {
        out.pop();
    }
    out
}

/// `[1] Title  https://…` lines for an answer's footnotes.
pub fn footnote_lines(notes: &Footnotes, width: usize, theme: &Theme) -> Vec<Line<'static>> {
    notes
        .notes
        .iter()
        .enumerate()
        .map(|(i, (title, url))| {
            let number = format!("[{}] ", i + 1);
            let room = width.saturating_sub(number.chars().count());
            let title = ellipsize(title, room.min(48));
            let mut spans = vec![
                Span::styled(number, theme.faint()),
                Span::styled(title.clone(), theme.ink()),
            ];
            if let Some(url) = url {
                let left = room.saturating_sub(title.chars().count() + 2);
                if left > 8 {
                    spans.push(Span::raw("  "));
                    spans.push(Span::styled(ellipsize(url, left), theme.faint()));
                }
            }
            Line::from(spans)
        })
        .collect()
}

fn heading(line: &str) -> Option<(usize, &str)> {
    let level = line.chars().take_while(|c| *c == '#').count();
    let rest = &line[level..];
    ((1..=6).contains(&level) && rest.starts_with(' ')).then(|| (level, rest.trim()))
}

fn is_rule(line: &str) -> bool {
    let compact: String = line.chars().filter(|c| !c.is_whitespace()).collect();
    compact.len() >= 3
        && ["-", "*", "_"]
            .iter()
            .any(|c| compact.chars().all(|x| x.to_string() == *c))
}

/// Indent level, the marker to show, and the item's text.
fn list_item(line: &str) -> Option<(usize, String, &str)> {
    let spaces = line.len() - line.trim_start().len();
    let rest = line.trim_start();
    let indent = spaces / 2;
    for bullet in ["- ", "* ", "+ "] {
        if let Some(item) = rest.strip_prefix(bullet) {
            let item = item
                .strip_prefix("[ ] ")
                .map(|i| ("☐", i))
                .or_else(|| item.strip_prefix("[x] ").map(|i| ("☑", i)));
            return Some(match item {
                Some((box_, text)) => (indent, box_.to_string(), text),
                None => (indent, "•".to_string(), &rest[2..]),
            });
        }
    }
    let digits = rest.chars().take_while(char::is_ascii_digit).count();
    if digits > 0 && digits < 4 {
        let after = &rest[digits..];
        for sep in [". ", ") "] {
            if let Some(item) = after.strip_prefix(sep) {
                return Some((indent, format!("{}.", &rest[..digits]), item));
            }
        }
    }
    None
}

/// A table row's cells, or `None` for the `|---|---|` line under a header.
fn table_row(line: &str) -> Option<Vec<String>> {
    let cells: Vec<String> = line
        .trim_matches('|')
        .split('|')
        .map(|c| c.trim().to_string())
        .collect();
    let separator = cells
        .iter()
        .all(|c| !c.is_empty() && c.chars().all(|x| matches!(x, '-' | ':')));
    (!separator).then_some(cells)
}

fn code_block(lines: &[String], width: usize, theme: &Theme) -> Vec<Line<'static>> {
    let inner = width.saturating_sub(2);
    lines
        .iter()
        .map(|line| {
            let text = ellipsize(&line.replace('\t', "    "), inner);
            let pad = inner.saturating_sub(text.chars().count());
            Line::from(vec![
                Span::styled("│ ", theme.faint()),
                Span::styled(format!("{text}{}", " ".repeat(pad)), theme.code()),
            ])
        })
        .collect()
}

/// Inline Markdown as styled spans on top of `base`.
fn inline(text: &str, theme: &Theme, notes: &mut Footnotes, base: Style) -> Vec<Span<'static>> {
    let chars: Vec<char> = text.chars().collect();
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut plain = String::new();
    let mut bold = false;
    let mut italic = false;
    let style = |bold: bool, italic: bool| {
        let mut style = base;
        if bold {
            style = style.add_modifier(Modifier::BOLD);
        }
        if italic {
            style = style.add_modifier(Modifier::ITALIC);
        }
        style
    };
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        let prev = i.checked_sub(1).map(|p| chars[p]);
        // **bold** and __bold__
        if (c == '*' || c == '_') && next == Some(c) {
            let closing = find(&chars, i + 2, &[c, c]);
            if bold || closing.is_some() {
                push(&mut spans, &mut plain, style(bold, italic));
                bold = !bold;
                i += 2;
                continue;
            }
        }
        // *italic* and _italic_, but not snake_case or a lone asterisk
        if (c == '*' || c == '_')
            && (italic
                || (next.is_some_and(|n| !n.is_whitespace())
                    && !prev.is_some_and(char::is_alphanumeric)
                    && find(&chars, i + 1, &[c]).is_some()))
        {
            push(&mut spans, &mut plain, style(bold, italic));
            italic = !italic;
            i += 1;
            continue;
        }
        // `code`
        if c == '`'
            && let Some(end) = find(&chars, i + 1, &['`'])
        {
            push(&mut spans, &mut plain, style(bold, italic));
            let code: String = chars[i + 1..end].iter().collect();
            spans.push(Span::styled(code, theme.code()));
            i = end + 1;
            continue;
        }
        // [text](url)
        if c == '['
            && let Some(close) = find(&chars, i + 1, &[']'])
            && chars.get(close + 1) == Some(&'(')
            && let Some(end) = find(&chars, close + 2, &[')'])
        {
            let label: String = chars[i + 1..close].iter().collect();
            let url: String = chars[close + 2..end].iter().collect();
            push(&mut spans, &mut plain, style(bold, italic));
            let n = notes.number(&label, Some(&url));
            spans.push(Span::styled(
                label,
                style(bold, italic).add_modifier(Modifier::UNDERLINED),
            ));
            spans.push(Span::styled(format!("[{n}]"), theme.faint()));
            i = end + 1;
            continue;
        }
        plain.push(c);
        i += 1;
    }
    push(&mut spans, &mut plain, style(bold, italic));
    spans
}

fn find(chars: &[char], from: usize, needle: &[char]) -> Option<usize> {
    (from..chars.len().saturating_sub(needle.len() - 1))
        .find(|&i| chars[i..i + needle.len()] == *needle)
}

fn push(spans: &mut Vec<Span<'static>>, plain: &mut String, style: Style) {
    if !plain.is_empty() {
        spans.push(Span::styled(std::mem::take(plain), style));
    }
}

/// Greedy word wrap over styled spans. `first` starts the first line and
/// `rest` every other one (a list bullet and its hanging indent).
pub fn wrap_spans(
    spans: Vec<Span<'static>>,
    width: usize,
    first: Vec<Span<'static>>,
    rest: Vec<Span<'static>>,
) -> Vec<Line<'static>> {
    // Words keep their style; a word may span styles ("**Q3**'s").
    let mut words: Vec<Vec<Span<'static>>> = vec![Vec::new()];
    let mut spaces: Vec<bool> = vec![false];
    for span in spans {
        let style = span.style;
        let mut piece = String::new();
        for c in span.content.chars() {
            if c == ' ' {
                if !piece.is_empty() {
                    words
                        .last_mut()
                        .unwrap()
                        .push(Span::styled(std::mem::take(&mut piece), style));
                }
                if !words.last().unwrap().is_empty() {
                    words.push(Vec::new());
                    spaces.push(true);
                }
            } else {
                piece.push(c);
            }
        }
        if !piece.is_empty() {
            words.last_mut().unwrap().push(Span::styled(piece, style));
        }
    }
    let mut lines = Vec::new();
    let mut line: Vec<Span<'static>> = first.clone();
    let lead = |l: &Vec<Span<'static>>| l.iter().map(Span::width).sum::<usize>();
    let mut used = lead(&line);
    let mut empty = true;
    for word in words.into_iter().filter(|w| !w.is_empty()) {
        let len: usize = word.iter().map(Span::width).sum();
        let gap = usize::from(!empty);
        if !empty && used + gap + len > width {
            lines.push(Line::from(std::mem::take(&mut line)));
            line = rest.clone();
            used = lead(&line);
            empty = true;
        }
        if !empty {
            line.push(Span::raw(" "));
            used += 1;
        }
        // A word longer than a line is cut.
        let room = width.saturating_sub(used);
        if len > room && room > 0 {
            let mut left = room;
            for span in word {
                if left == 0 {
                    break;
                }
                let text: String = span.content.chars().take(left).collect();
                left -= text.chars().count();
                line.push(Span::styled(text, span.style));
            }
            used = width;
        } else {
            line.extend(word);
            used += len;
        }
        empty = false;
    }
    if !empty || lines.is_empty() {
        lines.push(Line::from(line));
    }
    lines
}

fn ellipsize(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::theme::Depth;

    fn text(lines: &[Line]) -> Vec<String> {
        lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    #[test]
    fn draws_the_common_markdown() {
        let theme = Theme::new(Depth::TrueColor);
        let mut notes =
            Footnotes::new([("Q3 plan.pdf".to_string(), Some("https://d/q3".to_string()))]);
        let lines = render(
            "## Budget\n\nThe **Q3** budget is `€40k`, see [the plan](https://d/q3) and [Slack](https://s/1).\n\n- one\n- two which wraps around here\n1. first\n\n```\nlet x = 1;\n```\n> quoted",
            24,
            &theme,
            &mut notes,
        );
        let t = text(&lines);
        assert_eq!(t[0], "Budget");
        assert!(
            lines[0].spans[0]
                .style
                .add_modifier
                .contains(Modifier::BOLD)
        );
        assert!(t.iter().any(|l| l.contains("the plan[1]")), "{t:?}");
        assert!(t.iter().any(|l| l.contains("Slack[2]")), "{t:?}");
        assert!(t.contains(&"• one".to_string()), "{t:?}");
        assert!(t.contains(&"• two which wraps around".to_string()), "{t:?}");
        assert!(t.contains(&"  here".to_string()), "{t:?}");
        assert!(t.contains(&"1. first".to_string()), "{t:?}");
        assert!(t.iter().any(|l| l.starts_with("│ let x = 1;")), "{t:?}");
        assert!(t.contains(&"▎ quoted".to_string()), "{t:?}");
        assert_eq!(notes.notes.len(), 2, "the plan is the source's footnote");
        let bold = lines
            .iter()
            .flat_map(|l| &l.spans)
            .find(|s| s.content == "Q3")
            .unwrap();
        assert!(bold.style.add_modifier.contains(Modifier::BOLD));
        let footnotes = text(&footnote_lines(&notes, 60, &theme));
        assert_eq!(footnotes[0], "[1] Q3 plan.pdf  https://d/q3");
        assert_eq!(footnotes[1], "[2] Slack  https://s/1");
    }

    #[test]
    fn leaves_snake_case_and_lone_asterisks_alone() {
        let theme = Theme::new(Depth::Mono);
        let mut notes = Footnotes::default();
        let t = text(&render(
            "use local_read * 2 and_more",
            80,
            &theme,
            &mut notes,
        ));
        assert_eq!(t, vec!["use local_read * 2 and_more"]);
    }
}
