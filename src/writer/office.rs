//! Word and Excel documents for `create_document`.
//!
//! A Word document is built from Markdown: headings, paragraphs, bold,
//! italic, strikethrough and inline code, bulleted and numbered lists,
//! quotes, code blocks and tables. Links become their text with the address
//! after it, never a live link, and images their description. An Excel
//! workbook is built from sheets of rows of values: text, numbers, true and
//! false, or empty. Cells are never formulas. Neither format can carry
//! macros (the writers can't make them, and `.docm` and `.xlsm` names are
//! refused), and existing documents are never edited in place: replacing
//! one writes a new file and moves the old one to the trash.

use docx_rs::{
    AbstractNumbering, BreakType, Docx, IndentLevel, Level, LevelJc, LevelText, LineSpacing,
    NumberFormat, Numbering, NumberingId, Paragraph, Run, RunFonts, SpecialIndentType, Start,
    Style, StyleType, Table, TableCell, TableRow,
};
use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use rust_xlsxwriter::{Format, Workbook};
use serde::Deserialize;
use serde_json::Value;

use crate::error::ToolError;

/// Excel's limit on the text in one cell.
const MAX_CELL_CHARS: usize = 32_767;
/// Excel's limits on rows and columns.
const MAX_ROWS: usize = 1_048_576;
const MAX_COLUMNS: usize = 16_384;
const MAX_SHEETS: usize = 255;

/// Numbering IDs: docx-rs brings its own 1, so bullets are 10 and ordered
/// lists 100 up.
const BULLETS: usize = 10;
const CODE_FONT: &str = "Consolas";
/// The text width of the default A4 page and margins, in twentieths of a
/// point, which tables share out between their columns.
const TEXT_WIDTH: usize = 11_906 - 2 * 1_701;

/// One sheet of a workbook.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sheet {
    pub name: String,
    pub rows: Vec<Vec<Value>>,
    /// Bold the first row and keep it in view.
    #[serde(default)]
    pub header: bool,
}

/// The kinds of document `create_document` makes, by extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocumentKind {
    Docx,
    Xlsx,
}

/// The document kind for a file name, if it is one cww can make.
pub fn kind(name: &str) -> Option<DocumentKind> {
    match super::kinds::extension(name).as_deref() {
        Some("docx") => Some(DocumentKind::Docx),
        Some("xlsx") => Some(DocumentKind::Xlsx),
        _ => None,
    }
}

fn invalid(message: impl Into<String>) -> ToolError {
    ToolError::invalid_argument(message)
}

/// A Word document from Markdown.
pub fn docx(markdown: &str) -> Result<Vec<u8>, ToolError> {
    let mut builder = DocBuilder::default();
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    for event in Parser::new_ext(markdown, options) {
        builder.event(event);
    }
    builder.flush();
    let now = crate::audit::now();
    let mut doc = Docx::new()
        .created_at(&now)
        .updated_at(&now)
        .add_style(
            Style::new("Normal", StyleType::Paragraph)
                .name("Normal")
                .q_format(true)
                .line_spacing(LineSpacing::new().after(120)),
        )
        .add_style(
            Style::new("Quote", StyleType::Paragraph)
                .name("Quote")
                .based_on("Normal")
                .italic()
                .indent(Some(720), None, None, None),
        )
        .add_abstract_numbering(bullets())
        .add_numbering(Numbering::new(BULLETS, BULLETS));
    let sizes = [40, 32, 28, 26, 24, 22];
    for (i, size) in sizes.iter().enumerate() {
        let level = i + 1;
        doc = doc.add_style(
            Style::new(format!("Heading{level}"), StyleType::Paragraph)
                .name(format!("heading {level}"))
                .based_on("Normal")
                .next("Normal")
                .q_format(true)
                .size(*size)
                .bold()
                .line_spacing(LineSpacing::new().before(240).after(80))
                .outline_lvl(i),
        );
    }
    for (id, start) in &builder.ordered {
        doc = doc
            .add_abstract_numbering(decimals(*id, *start))
            .add_numbering(Numbering::new(*id, *id));
    }
    // A document ends with a paragraph, never a table.
    if !matches!(builder.blocks.last(), Some(Block::Paragraph(_))) {
        builder
            .blocks
            .push(Block::Paragraph(Box::new(Paragraph::new())));
    }
    for block in builder.blocks {
        doc = match block {
            Block::Paragraph(p) => doc.add_paragraph(*p),
            Block::Table(t) => doc.add_table(*t),
        };
    }
    let mut out = std::io::Cursor::new(Vec::new());
    doc.build()
        .pack(&mut out)
        .map_err(|e| ToolError::internal(format!("writing the document: {e}")))?;
    Ok(out.into_inner())
}

fn bullets() -> AbstractNumbering {
    let marks = ["•", "◦", "▪"];
    (0..9).fold(AbstractNumbering::new(BULLETS), |n, level| {
        n.add_level(
            Level::new(
                level,
                Start::new(1),
                NumberFormat::new("bullet"),
                LevelText::new(marks[level % marks.len()]),
                LevelJc::new("left"),
            )
            .indent(
                Some(720 * (level as i32 + 1)),
                Some(SpecialIndentType::Hanging(360)),
                None,
                None,
            ),
        )
    })
}

fn decimals(id: usize, start: usize) -> AbstractNumbering {
    (0..9).fold(AbstractNumbering::new(id), |n, level| {
        n.add_level(
            Level::new(
                level,
                Start::new(if level == 0 { start } else { 1 }),
                NumberFormat::new(if level % 2 == 0 {
                    "decimal"
                } else {
                    "lowerLetter"
                }),
                LevelText::new(format!("%{}.", level + 1)),
                LevelJc::new("left"),
            )
            .indent(
                Some(720 * (level as i32 + 1)),
                Some(SpecialIndentType::Hanging(360)),
                None,
                None,
            ),
        )
    })
}

enum Block {
    Paragraph(Box<Paragraph>),
    Table(Box<Table>),
}

/// Where the paragraph being built goes.
#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Body,
    Heading(usize),
    Code,
}

#[derive(Default)]
struct DocBuilder {
    blocks: Vec<Block>,
    /// Ordered lists: their numbering ID and first number.
    ordered: Vec<(usize, usize)>,
    /// The paragraph being built, if any.
    current: Option<Paragraph>,
    kind: Option<Kind>,
    bold: usize,
    italic: usize,
    strike: usize,
    /// Open lists: the numbering ID of each.
    lists: Vec<usize>,
    quote: usize,
    link: Option<String>,
    link_text: String,
    /// Tables: rows of cells of paragraphs.
    table: Option<Vec<Vec<TableCell>>>,
    row: Vec<TableCell>,
    cell: Option<TableCell>,
    /// The cell being built has a paragraph.
    cell_filled: bool,
    head: bool,
}

impl DocBuilder {
    fn paragraph(&mut self) -> &mut Paragraph {
        if self.current.is_none() {
            let mut p = Paragraph::new();
            match self.kind.unwrap_or(Kind::Body) {
                Kind::Heading(level) => p = p.style(&format!("Heading{level}")),
                Kind::Code | Kind::Body => {
                    if self.quote > 0 {
                        p = p.style("Quote");
                    }
                }
            }
            if let Some(id) = self.lists.last()
                && self.kind != Some(Kind::Code)
            {
                p = p.numbering(
                    NumberingId::new(*id),
                    IndentLevel::new(self.lists.len().saturating_sub(1).min(8)),
                );
            }
            self.current = Some(p);
        }
        self.current.as_mut().expect("just made")
    }

    fn run(&self, text: &str) -> Run {
        let mut run = Run::new().add_text(text);
        if self.bold > 0 || self.head {
            run = run.bold();
        }
        if self.italic > 0 {
            run = run.italic();
        }
        if self.strike > 0 {
            run = run.strike();
        }
        run
    }

    fn text(&mut self, text: &str) {
        if self.link.is_some() {
            self.link_text.push_str(text);
        }
        let run = self.run(text);
        let p = std::mem::take(self.paragraph());
        self.current = Some(p.add_run(run));
    }

    fn code(&mut self, text: &str) {
        let run = self
            .run(text)
            .fonts(RunFonts::new().ascii(CODE_FONT).hi_ansi(CODE_FONT));
        let p = std::mem::take(self.paragraph());
        self.current = Some(p.add_run(run));
    }

    /// Finish the paragraph being built.
    fn flush(&mut self) {
        let Some(p) = self.current.take() else {
            return;
        };
        match self.cell.take() {
            Some(cell) => {
                self.cell = Some(cell.add_paragraph(p));
                self.cell_filled = true;
            }
            None => self.blocks.push(Block::Paragraph(Box::new(p))),
        }
    }

    fn event(&mut self, event: Event<'_>) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(text) => {
                if self.kind == Some(Kind::Code) {
                    // One paragraph a line, in a fixed-width font.
                    let mut lines = text.split('\n').peekable();
                    while let Some(line) = lines.next() {
                        if line.is_empty() && lines.peek().is_none() {
                            break;
                        }
                        self.code(line);
                        if let Some(p) = self.current.take() {
                            self.current = Some(p.line_spacing(LineSpacing::new().after(0)));
                        }
                        self.flush();
                    }
                } else {
                    self.text(&text);
                }
            }
            Event::Code(text) => self.code(&text),
            Event::Html(text) | Event::InlineHtml(text) => self.text(&text),
            Event::SoftBreak => self.text(" "),
            Event::HardBreak => {
                let p = std::mem::take(self.paragraph());
                self.current = Some(p.add_run(Run::new().add_break(BreakType::TextWrapping)));
            }
            Event::Rule => {
                self.flush();
                self.blocks
                    .push(Block::Paragraph(Box::new(Paragraph::new())));
            }
            Event::FootnoteReference(_)
            | Event::TaskListMarker(_)
            | Event::InlineMath(_)
            | Event::DisplayMath(_) => {}
        }
    }

    fn start(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Paragraph => self.flush(),
            Tag::Heading { level, .. } => {
                self.flush();
                self.kind = Some(Kind::Heading(heading(level)));
            }
            Tag::BlockQuote(_) => {
                self.flush();
                self.quote += 1;
            }
            Tag::CodeBlock(_) => {
                self.flush();
                self.kind = Some(Kind::Code);
            }
            Tag::List(start) => {
                self.flush();
                let id = match start {
                    Some(first) => {
                        let id = 100 + self.ordered.len();
                        self.ordered.push((id, first as usize));
                        id
                    }
                    None => BULLETS,
                };
                self.lists.push(id);
            }
            Tag::Item => self.flush(),
            Tag::Emphasis => self.italic += 1,
            Tag::Strong => self.bold += 1,
            Tag::Strikethrough => self.strike += 1,
            Tag::Link { dest_url, .. } => {
                self.link = Some(dest_url.to_string());
                self.link_text.clear();
            }
            Tag::Image { .. } => {}
            Tag::Table(_) => {
                self.flush();
                self.table = Some(Vec::new());
            }
            Tag::TableHead => self.head = true,
            Tag::TableRow => self.row.clear(),
            Tag::TableCell => {
                self.cell = Some(TableCell::new());
                self.cell_filled = false;
            }
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph | TagEnd::Item => self.flush(),
            TagEnd::Heading(_) => {
                self.flush();
                self.kind = None;
            }
            TagEnd::BlockQuote(_) => {
                self.flush();
                self.quote = self.quote.saturating_sub(1);
            }
            TagEnd::CodeBlock => {
                self.flush();
                self.kind = None;
            }
            TagEnd::List(_) => {
                self.flush();
                self.lists.pop();
            }
            TagEnd::Emphasis => self.italic = self.italic.saturating_sub(1),
            TagEnd::Strong => self.bold = self.bold.saturating_sub(1),
            TagEnd::Strikethrough => self.strike = self.strike.saturating_sub(1),
            TagEnd::Link => {
                // The address in plain text after the words, never a live
                // link.
                if let Some(url) = self.link.take()
                    && !url.is_empty()
                    && url != self.link_text
                {
                    self.text(&format!(" ({url})"));
                }
            }
            TagEnd::TableCell => {
                self.flush();
                let mut cell = self.cell.take().unwrap_or_default();
                // Word needs a paragraph in every cell.
                if !self.cell_filled {
                    cell = cell.add_paragraph(Paragraph::new());
                }
                self.row.push(cell);
            }
            TagEnd::TableHead => {
                self.head = false;
                self.end_row();
            }
            TagEnd::TableRow => self.end_row(),
            TagEnd::Table => {
                if let Some(rows) = self.table.take()
                    && let Some(columns) = rows.iter().map(Vec::len).max()
                    && columns > 0
                {
                    // Every row as wide as the widest, and a grid that
                    // shares out the page.
                    let rows = rows
                        .into_iter()
                        .map(|mut cells| {
                            while cells.len() < columns {
                                cells.push(TableCell::new().add_paragraph(Paragraph::new()));
                            }
                            TableRow::new(cells)
                        })
                        .collect();
                    let table = Table::new(rows).set_grid(vec![TEXT_WIDTH / columns; columns]);
                    self.blocks.push(Block::Table(Box::new(table)));
                }
            }
            _ => {}
        }
    }

    fn end_row(&mut self) {
        if self.row.is_empty() {
            return;
        }
        let cells = std::mem::take(&mut self.row);
        if let Some(rows) = &mut self.table {
            rows.push(cells);
        }
    }
}

fn heading(level: HeadingLevel) -> usize {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

/// An Excel workbook from sheets of rows.
pub fn xlsx(sheets: &[Sheet]) -> Result<Vec<u8>, ToolError> {
    if sheets.is_empty() {
        return Err(invalid("Give at least one sheet."));
    }
    if sheets.len() > MAX_SHEETS {
        return Err(invalid(format!(
            "A workbook holds at most {MAX_SHEETS} sheets."
        )));
    }
    let mut names: Vec<String> = Vec::new();
    let mut workbook = Workbook::new();
    let bold = Format::new().set_bold();
    for sheet in sheets {
        let name = sheet.name.trim();
        check_sheet_name(name)?;
        if names
            .iter()
            .any(|n| n.to_lowercase() == name.to_lowercase())
        {
            return Err(invalid(format!("Two sheets are named {name:?}.")));
        }
        names.push(name.to_string());
        if sheet.rows.len() > MAX_ROWS {
            return Err(invalid(format!("A sheet holds at most {MAX_ROWS} rows.")));
        }
        let ws = workbook.add_worksheet();
        ws.set_name(name)
            .map_err(|e| invalid(format!("The sheet name {name:?} can't be used: {e}")))?;
        for (r, row) in sheet.rows.iter().enumerate() {
            if row.len() > MAX_COLUMNS {
                return Err(invalid(format!("A row holds at most {MAX_COLUMNS} cells.")));
            }
            let header = sheet.header && r == 0;
            for (c, cell) in row.iter().enumerate() {
                let (r, c) = (r as u32, c as u16);
                let written = match cell {
                    Value::Null => Ok(()),
                    Value::String(text) => {
                        if text.chars().count() > MAX_CELL_CHARS {
                            return Err(invalid(format!(
                                "A cell holds at most {MAX_CELL_CHARS} characters."
                            )));
                        }
                        // Always text, even when it starts with `=`: cells
                        // are never formulas.
                        if header {
                            ws.write_string_with_format(r, c, text, &bold).map(|_| ())
                        } else {
                            ws.write_string(r, c, text).map(|_| ())
                        }
                    }
                    Value::Number(n) => {
                        let n = n
                            .as_f64()
                            .filter(|n| n.is_finite())
                            .ok_or_else(|| invalid("A number in the sheet is out of range."))?;
                        if header {
                            ws.write_number_with_format(r, c, n, &bold).map(|_| ())
                        } else {
                            ws.write_number(r, c, n).map(|_| ())
                        }
                    }
                    Value::Bool(b) => {
                        if header {
                            ws.write_boolean_with_format(r, c, *b, &bold).map(|_| ())
                        } else {
                            ws.write_boolean(r, c, *b).map(|_| ())
                        }
                    }
                    Value::Array(_) | Value::Object(_) => {
                        return Err(invalid(
                            "Cells are text, numbers, true or false, or null for an empty cell.",
                        ));
                    }
                };
                written.map_err(|e| ToolError::internal(format!("writing a cell: {e}")))?;
            }
        }
        if sheet.header && !sheet.rows.is_empty() {
            ws.set_freeze_panes(1, 0)
                .map_err(|e| ToolError::internal(format!("freezing the header: {e}")))?;
        }
        ws.autofit();
    }
    workbook
        .save_to_buffer()
        .map_err(|e| ToolError::internal(format!("writing the workbook: {e}")))
}

/// Excel's rules for sheet names.
fn check_sheet_name(name: &str) -> Result<(), ToolError> {
    if name.is_empty() || name.chars().count() > 31 {
        return Err(invalid("Sheet names have 1 to 31 characters."));
    }
    if name.contains(['[', ']', ':', '*', '?', '/', '\\'])
        || name.starts_with('\'')
        || name.ends_with('\'')
    {
        return Err(invalid(format!(
            "The sheet name {name:?} can't have [ ] : * ? / \\ or start or end with an apostrophe."
        )));
    }
    if name.eq_ignore_ascii_case("history") {
        return Err(invalid("Excel keeps the sheet name History for itself."));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reader::extract::extract;

    #[test]
    fn word_documents_read_back() {
        let markdown = "# Plan\n\nThe **budget** is *approved*, see [the memo](https://example.com/m).\n\n\
                        ## Steps\n\n1. Hire\n2. Ship\n   - with care\n\n- one\n- two\n\n\
                        > Quoted\n\n```\nlet x = 1;\nlet y = 2;\n```\n\n\
                        | Name | Amount |\n|---|---|\n| Falcon | 10 |\n| Owl | 2 |\n";
        let bytes = docx(markdown).unwrap();
        assert_eq!(&bytes[..2], b"PK");
        let text = extract("plan.docx", &bytes, 1_000_000).unwrap();
        for expected in [
            "Plan",
            "budget",
            "approved",
            "the memo (https://example.com/m)",
            "Steps",
            "Hire",
            "with care",
            "Quoted",
            "let x = 1;",
            "Falcon",
            "Amount",
        ] {
            assert!(text.contains(expected), "{expected:?} in {text:?}");
        }
        // Nothing that runs: no macros, no live links.
        let raw = String::from_utf8_lossy(&bytes);
        assert!(!raw.contains("vbaProject"));
        assert!(!raw.contains("hyperlink"));
        // Bullets and numbers each have their own numbering.
        let body = zip_entry(&bytes, "word/document.xml");
        assert!(body.contains("<w:gridCol"), "a table grid");
        assert!(body.contains("</w:tbl><w:p"), "a paragraph after the table");
        let numbering = zip_entry(&bytes, "word/numbering.xml");
        assert_eq!(
            numbering.matches(r#"w:abstractNumId="10""#).count(),
            1,
            "{numbering}"
        );
        assert!(
            numbering.contains(r#"<w:numFmt w:val="bullet" />"#),
            "{numbering}"
        );
    }

    #[test]
    fn empty_word_documents_are_valid() {
        let bytes = docx("").unwrap();
        assert_eq!(extract("x.docx", &bytes, 1000).unwrap().trim(), "");
    }

    #[test]
    fn workbooks_read_back_with_values_never_formulas() {
        let sheets = vec![
            Sheet {
                name: "Budget".into(),
                rows: vec![
                    vec![
                        Value::from("Item"),
                        Value::from("Cost"),
                        Value::from("Paid"),
                    ],
                    vec![Value::from("Falcon"), Value::from(10.5), Value::from(true)],
                    vec![Value::from("=1+1"), Value::Null, Value::from(false)],
                ],
                header: true,
            },
            Sheet {
                name: "Notes".into(),
                rows: vec![vec![Value::from("hello")]],
                header: false,
            },
        ];
        let bytes = xlsx(&sheets).unwrap();
        let text = extract("b.xlsx", &bytes, 1_000_000).unwrap();
        for expected in ["Budget", "Falcon", "10.5", "=1+1", "Notes", "hello"] {
            assert!(text.contains(expected), "{expected:?} in {text:?}");
        }
        let raw = String::from_utf8_lossy(&bytes);
        assert!(!raw.contains("<f>"), "no formulas");
    }

    #[test]
    fn refuses_bad_sheets() {
        let sheet = |name: &str, rows: Vec<Vec<Value>>| Sheet {
            name: name.into(),
            rows,
            header: false,
        };
        for bad in [
            vec![],
            vec![sheet("a/b", vec![])],
            vec![sheet("", vec![])],
            vec![sheet(&"x".repeat(32), vec![])],
            vec![sheet("A", vec![]), sheet("a", vec![])],
            vec![sheet("A", vec![vec![json_array()]])],
        ] {
            assert!(xlsx(&bad).is_err());
        }
    }

    fn zip_entry(bytes: &[u8], name: &str) -> String {
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
        let mut text = String::new();
        std::io::Read::read_to_string(&mut zip.by_name(name).unwrap(), &mut text).unwrap();
        text
    }

    fn json_array() -> Value {
        Value::Array(vec![Value::from(1)])
    }

    #[test]
    fn kinds_come_from_the_extension() {
        assert_eq!(kind("a.DOCX"), Some(DocumentKind::Docx));
        assert_eq!(kind("a.xlsx"), Some(DocumentKind::Xlsx));
        assert_eq!(kind("a.pptx"), None);
        assert_eq!(kind("a.docm"), None);
    }
}
