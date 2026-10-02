//! Answers, drawn as the web's typography plugin and `.message__body.prose`
//! set them: paragraphs and headings, lists with faint markers, quotes,
//! tables, code blocks with a header and highlighting, inline code and
//! citation chips, and the rainbow caret at the end of a streaming answer.

use std::ops::Range;
use std::sync::Arc;

use egui::text::LayoutJob;
use egui::{Color32, Galley, Id, Painter, Pos2, Rect, Sense, Stroke, Ui, pos2, vec2};
use pulldown_cmark::Alignment;

use cww::tui::chat::Source;

use super::icons::{Icon, Images};
use super::markdown::{Block, Inline};
use super::paint;
use super::tokens::{self, Palette, Type, scale};

/// What a click in an answer asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Clicked {
    Link(String),
    Source(usize),
    Copy(String),
}

pub struct Prose<'a> {
    pub palette: &'a Palette,
    pub images: &'a Images,
    pub sources: &'a [Source],
    /// Draw the blinking caret after the last text, as the answer streams.
    pub caret: bool,
    pub time: f64,
    pub id: Id,
}

/// Margins above and below a block, in points, which collapse with their
/// neighbours' as CSS margins do.
fn margins(block: &Block) -> (f32, f32) {
    match block {
        Block::Paragraph(_) => (20.0, 20.0),
        Block::Heading(1, _) => (28.0, 12.0),
        Block::Heading(2, _) => (28.0, 10.0),
        Block::Heading(..) => (24.0, 8.0),
        Block::List { .. } => (20.0, 20.0),
        Block::Quote(_) => (25.6, 25.6),
        Block::Code { .. } => (0.0, 16.0),
        Block::Table { .. } => (28.0, 28.0),
        Block::Rule => (32.0, 32.0),
    }
}

fn heading_type(level: u8) -> Type {
    let (size, line) = match level {
        1 => (24.0, 26.7),
        2 => (20.0, 26.7),
        3 => (17.0, 27.2),
        _ => (16.0, 24.0),
    };
    Type::sans(size, line).weight(620.0).tracking(-0.025)
}

impl Prose<'_> {
    /// Draw `blocks` one under another, filling the width of `ui`.
    pub fn show(&self, ui: &mut Ui, blocks: &[Block]) -> Option<Clicked> {
        self.blocks(ui, blocks, self.caret, Container::Root)
    }

    fn blocks(
        &self,
        ui: &mut Ui,
        blocks: &[Block],
        caret: bool,
        within: Container,
    ) -> Option<Clicked> {
        let mut clicked = None;
        let mut previous: Option<(f32, &Block)> = None;
        for (i, block) in blocks.iter().enumerate() {
            let (top, bottom) = margins(block);
            let (top, bottom) = match within {
                // A list item's blocks sit 12 apart; tight items have one.
                Container::Item => (12.0_f32.min(top), 12.0_f32.min(bottom)),
                _ => (top, bottom),
            };
            if let Some((previous_bottom, previous_block)) = previous {
                // The element after a heading starts right under it.
                let top = if matches!(previous_block, Block::Heading(..)) {
                    0.0
                } else {
                    top
                };
                ui.add_space(previous_bottom.max(top));
            }
            let last = i + 1 == blocks.len();
            let found = self.block(ui, block, caret && last);
            clicked = clicked.or(found);
            previous = Some((bottom, block));
        }
        clicked
    }

    fn block(&self, ui: &mut Ui, block: &Block, caret: bool) -> Option<Clicked> {
        let p = self.palette;
        match block {
            Block::Paragraph(inlines) => self.text(ui, inlines, scale::PROSE, p.ink, caret),
            Block::Heading(level, inlines) => {
                self.text(ui, inlines, heading_type(*level), p.ink, caret)
            }
            Block::List { start, items } => self.list(ui, *start, items, caret),
            Block::Quote(blocks) => self.quote(ui, blocks, caret),
            Block::Code { language, text } => self.code(ui, language, text),
            Block::Table { align, head, rows } => self.table(ui, align, head, rows),
            Block::Rule => {
                let (rect, _) =
                    ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
                ui.painter()
                    .hline(rect.x_range(), rect.center().y, Stroke::new(1.0, p.line));
                None
            }
        }
    }

    fn list(
        &self,
        ui: &mut Ui,
        start: Option<u64>,
        items: &[Vec<Block>],
        caret: bool,
    ) -> Option<Clicked> {
        let p = self.palette;
        let mut clicked = None;
        let width = ui.available_width();
        for (i, item) in items.iter().enumerate() {
            if i > 0 {
                ui.add_space(8.0);
            }
            let left = ui.cursor().left();
            let top = ui.cursor().top();
            // ul/ol pad 26 and li 6 more: the text starts 32 in.
            let inner = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(Rect::from_min_size(
                        pos2(left + 32.0, top),
                        vec2(width - 32.0, f32::INFINITY),
                    ))
                    .layout(egui::Layout::top_down(egui::Align::Min)),
            );
            let mut inner = inner;
            let last = i + 1 == items.len();
            let found = self.blocks(&mut inner, item, caret && last, Container::Item);
            clicked = clicked.or(found);
            let rect = inner.min_rect();
            // The marker, centered on the first line.
            let line_center = top + scale::PROSE.line_height / 2.0;
            let painter = ui.painter();
            match start {
                None => {
                    painter.circle_filled(pos2(left + 12.0, line_center + 0.5), 2.3, p.ink_faint);
                }
                Some(first) => {
                    let label = format!("{}.", first + i as u64);
                    let galley = paint::layout(
                        painter,
                        paint::job(&label, scale::PROSE, p.ink_faint, f32::INFINITY),
                    );
                    painter.galley(
                        pos2(
                            left + 22.0 - galley.size().x,
                            top + (scale::PROSE.line_height - galley.size().y) / 2.0,
                        ),
                        galley,
                        p.ink_faint,
                    );
                }
            }
            ui.allocate_exact_size(
                vec2(width, rect.height().max(scale::PROSE.line_height)),
                Sense::hover(),
            );
        }
        clicked
    }

    fn quote(&self, ui: &mut Ui, blocks: &[Block], caret: bool) -> Option<Clicked> {
        let p = self.palette;
        let left = ui.cursor().left();
        let top = ui.cursor().top();
        let width = ui.available_width();
        let mut inner = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(Rect::from_min_size(
                    pos2(left + 18.0, top),
                    vec2(width - 18.0, f32::INFINITY),
                ))
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        let quoted = Prose {
            palette: &Palette {
                ink: p.ink_muted,
                ..*p
            },
            ..*self
        };
        let clicked = quoted.blocks(&mut inner, blocks, caret, Container::Quote);
        let height = inner.min_rect().height();
        ui.painter().rect_filled(
            Rect::from_min_size(pos2(left, top), vec2(2.0, height)),
            0.0,
            p.line_strong,
        );
        ui.allocate_exact_size(vec2(width, height), Sense::hover());
        clicked
    }

    /// A paragraph or heading: one galley, with chips, inline code and
    /// links drawn around and under its runs.
    fn text(
        &self,
        ui: &mut Ui,
        inlines: &[Inline],
        ty: Type,
        color: Color32,
        caret: bool,
    ) -> Option<Clicked> {
        let width = ui.available_width();
        let (job, runs) = self.inline_job(ui.painter(), inlines, ty, color, width);
        let galley = ui.painter().layout_job(job);
        // Selectable with the mouse, as text on a web page is: drag to
        // select (across paragraphs too), and copy with the shortcut.
        let (rect, response) =
            ui.allocate_exact_size(vec2(width, galley.size().y), selectable_sense());
        response.widget_info(|| {
            egui::WidgetInfo::labeled(
                egui::WidgetType::Label,
                true,
                super::markdown::plain(inlines),
            )
        });
        if response.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Text);
        }
        let clicked = self.decorate(ui, rect.min, &galley, &runs);
        egui::text_selection::LabelSelectionState::label_text_selection(
            ui,
            &response,
            rect.min,
            Arc::clone(&galley),
            color,
            Stroke::NONE,
        );
        if caret {
            self.caret(ui.painter(), rect.min, &galley, ty);
        }
        clicked
    }

    fn caret(&self, painter: &Painter, origin: Pos2, galley: &Galley, ty: Type) {
        // `caret-blink`: on for the first half of each second.
        if self.time.fract() >= 0.5 {
            return;
        }
        let (x, baseline) = match galley.rows.last() {
            Some(row) => {
                let right = row.pos.x + row.row.size.x;
                let baseline = row
                    .row
                    .glyphs
                    .last()
                    .map_or(row.pos.y + row.row.size.y * 0.75, |g| row.pos.y + g.pos.y);
                (right, baseline)
            }
            None => (0.0, ty.line_height * 0.75),
        };
        let em = ty.size;
        let rect = Rect::from_min_size(
            origin + vec2(x + 0.15 * em, baseline + 0.2 * em - 1.1 * em),
            vec2(0.5 * em, 1.1 * em),
        );
        rainbow_rect(painter, rect, 2.0);
    }

    /// Lay out runs of text, with room left around chips and inline code
    /// for their padding and icons.
    fn inline_job(
        &self,
        painter: &Painter,
        inlines: &[Inline],
        ty: Type,
        color: Color32,
        width: f32,
    ) -> (LayoutJob, Vec<Run>) {
        let p = self.palette;
        let mut job = LayoutJob::default();
        job.wrap.max_width = width;
        let mut runs = Vec::new();
        let code_ty = Type::mono(ty.size * 0.85, ty.line_height)
            .weight(450.0)
            .tracking(0.0);
        for inline in inlines {
            match inline {
                Inline::Text { text, style, link } => {
                    let mut t = ty;
                    if style.bold {
                        t = t.weight(600.0);
                    }
                    if link.is_some() {
                        t = t.weight(t.weight.max(500.0));
                    }
                    let mut format = t.format(color);
                    format.italics = style.italic;
                    if style.strike {
                        format.strikethrough = Stroke::new(1.0, color);
                    }
                    let start = job.text.len();
                    job.append(text, 0.0, format);
                    if let Some(url) = link {
                        runs.push(Run {
                            range: start..job.text.len(),
                            kind: RunKind::Link(url.clone()),
                        });
                    }
                }
                Inline::Code(code) => {
                    let start = job.text.len();
                    spacer(painter, &mut job, code_ty, 6.8);
                    job.append(code, 0.0, code_ty.format(color));
                    spacer(painter, &mut job, code_ty, 6.8);
                    runs.push(Run {
                        range: start..job.text.len(),
                        kind: RunKind::Code,
                    });
                }
                Inline::Chip { label, source } => {
                    // A thin margin, then the chip: padding, icon, gap, label, padding.
                    spacer(painter, &mut job, scale::CHIP, 1.65);
                    let start = job.text.len();
                    spacer(painter, &mut job, scale::CHIP, 3.85 + 12.1 + 3.85);
                    let label = label.replace(' ', "\u{a0}");
                    job.append(&label, 0.0, scale::CHIP.format(p.ink_muted));
                    spacer(painter, &mut job, scale::CHIP, 6.05);
                    runs.push(Run {
                        range: start..job.text.len(),
                        kind: RunKind::Chip(*source),
                    });
                    spacer(painter, &mut job, scale::CHIP, 1.65);
                }
                Inline::Break => job.append("\n", 0.0, ty.format(color)),
            }
        }
        // Every run sits on the paragraph's line height.
        for section in &mut job.sections {
            section.format.line_height = Some(ty.line_height);
        }
        (job, runs)
    }

    /// Backgrounds for chips and inline code, underlines for links, and
    /// their clicks.
    fn decorate(
        &self,
        ui: &mut Ui,
        origin: Pos2,
        galley: &Galley,
        runs: &[Run],
    ) -> Option<Clicked> {
        let p = self.palette;
        let mut clicked = None;
        for (n, run) in runs.iter().enumerate() {
            for (rect, baseline, glyph_height, ascent) in run_rects(galley, &run.range) {
                let rect = rect.translate(origin.to_vec2());
                let baseline = baseline + origin.y;
                match &run.kind {
                    RunKind::Code => {
                        let top = baseline - ascent - 2.72 - 1.0;
                        let r = Rect::from_min_max(
                            pos2(rect.left(), top),
                            pos2(rect.right(), top + glyph_height + 2.0 * 2.72 + 2.0),
                        );
                        ui.painter().rect_filled(r, 8.0, p.canvas_sunken);
                        ui.painter().rect_stroke(
                            r.shrink(0.5),
                            8.0,
                            Stroke::new(1.0, p.line),
                            egui::StrokeKind::Middle,
                        );
                    }
                    RunKind::Chip(source) => {
                        let em = scale::CHIP.size;
                        let center = baseline - ascent + glyph_height / 2.0 - 0.08 * em;
                        let r = Rect::from_min_max(
                            pos2(rect.left(), center - 9.5),
                            pos2(rect.right(), center + 9.5),
                        );
                        let id = self
                            .id
                            .with(("chip", n, rect.left() as i32, rect.top() as i32));
                        let response = ui.interact(r, id, Sense::click());
                        let title = self.sources.get(*source).map_or("", |s| s.title.as_str());
                        response.widget_info(|| {
                            egui::WidgetInfo::labeled(
                                egui::WidgetType::Link,
                                true,
                                format!("Source {}: {title}", source + 1),
                            )
                        });
                        let hover =
                            ui.ctx()
                                .animate_bool_with_time(id, response.hovered(), tokens::FAST);
                        let fill = tokens::lerp_rgb(p.canvas_sunken, p.surface, hover);
                        let ring = tokens::lerp_rgb(p.line, p.line_strong, hover);
                        ui.painter().rect_filled(r, tokens::RADIUS_TAG, fill);
                        ui.painter().rect_stroke(
                            r.shrink(0.5),
                            tokens::RADIUS_TAG,
                            Stroke::new(1.0, ring),
                            egui::StrokeKind::Middle,
                        );
                        let icon = self
                            .sources
                            .get(*source)
                            .map_or(Icon::FileText, source_icon);
                        let tint = tokens::lerp_rgb(p.ink_faint, p.ink_muted, hover);
                        self.images.icon_at(
                            ui.painter(),
                            pos2(r.left() + 3.85 + 6.05, r.center().y),
                            12.1,
                            icon,
                            tint,
                        );
                        if response.hovered() {
                            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                        }
                        if response.clicked() {
                            clicked = Some(Clicked::Source(*source));
                        }
                    }
                    RunKind::Link(url) => {
                        let id = self
                            .id
                            .with(("link", n, rect.left() as i32, rect.top() as i32));
                        let response = ui.interact(rect, id, Sense::click());
                        response.widget_info(|| {
                            egui::WidgetInfo::labeled(egui::WidgetType::Link, true, url)
                        });
                        let color = if response.hovered() {
                            p.ink
                        } else {
                            p.line_strong
                        };
                        let y = baseline + 3.0;
                        ui.painter()
                            .hline(rect.x_range(), y, Stroke::new(1.0, color));
                        if response.hovered() {
                            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                        }
                        if response.clicked() {
                            clicked = Some(Clicked::Link(url.clone()));
                        }
                    }
                }
            }
        }
        clicked
    }

    fn code(&self, ui: &mut Ui, language: &str, text: &str) -> Option<Clicked> {
        let p = self.palette;
        let width = ui.available_width();
        let mut job = super::highlight::job(text, language, p);
        job.wrap.max_width = f32::INFINITY;
        let galley = ui.painter().layout_job(job);
        let header = 33.0;
        let height = 1.0 + header + 16.0 + galley.size().y + 16.0 + 1.0;
        let (rect, response) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
        response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, text));
        let painter = ui.painter();
        let r = tokens::RADIUS_CARD;
        painter.rect_filled(rect, r, p.surface);
        let header_rect = Rect::from_min_size(rect.min, vec2(width, header + 1.0));
        painter.rect_filled(
            header_rect,
            egui::CornerRadius {
                nw: r as u8,
                ne: r as u8,
                sw: 0,
                se: 0,
            },
            p.canvas_sunken,
        );
        painter.hline(
            rect.x_range(),
            header_rect.bottom() - 0.5,
            Stroke::new(1.0, p.line),
        );
        painter.rect_stroke(
            rect.shrink(0.5),
            r,
            Stroke::new(1.0, p.line),
            egui::StrokeKind::Middle,
        );
        let language = if language.is_empty() {
            "plaintext"
        } else {
            language
        };
        let label = paint::layout(
            painter,
            paint::job(language, scale::MICRO, p.ink_faint, f32::INFINITY),
        );
        painter.galley(
            pos2(
                rect.left() + 17.0,
                header_rect.center().y - label.size().y / 2.0,
            ),
            label,
            p.ink_faint,
        );
        // Copy, a ghost extra-small button.
        let copy_label = paint::layout(
            painter,
            paint::job(
                "Copy",
                Type::sans(12.0, 16.0).weight(560.0),
                p.ink_muted,
                f32::INFINITY,
            ),
        );
        let copy_w = 8.0 + 14.0 + 6.0 + copy_label.size().x + 8.0;
        let copy_rect = Rect::from_center_size(
            pos2(rect.right() - 7.0 - copy_w / 2.0, header_rect.center().y),
            vec2(copy_w, 24.0),
        );
        let id = self.id.with(("copy-code", rect.top() as i32));
        let copy = ui.interact(copy_rect, id, Sense::click());
        copy.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Copy code"));
        let hover = ui
            .ctx()
            .animate_bool_with_time(id, copy.hovered(), tokens::FAST);
        let painter = ui.painter();
        if hover > 0.0 {
            painter.rect_filled(
                copy_rect,
                tokens::RADIUS_CONTROL - 2.0,
                p.ink_wash(7.0 * hover),
            );
        }
        let copied = ui
            .ctx()
            .data(|d| d.get_temp::<f64>(id))
            .is_some_and(|at| self.time - at < 2.0);
        let (icon, word) = if copied {
            (Icon::Check, "Copied")
        } else {
            (Icon::Copy, "Copy")
        };
        let word_galley = if copied {
            paint::layout(
                painter,
                paint::job(
                    word,
                    Type::sans(12.0, 16.0).weight(560.0),
                    p.ink_muted,
                    f32::INFINITY,
                ),
            )
        } else {
            copy_label
        };
        self.images.icon_at(
            painter,
            pos2(copy_rect.left() + 8.0 + 7.0, copy_rect.center().y),
            14.0,
            icon,
            p.ink_muted,
        );
        painter.galley(
            pos2(
                copy_rect.left() + 8.0 + 14.0 + 6.0,
                copy_rect.center().y - word_galley.size().y / 2.0,
            ),
            word_galley,
            p.ink_muted,
        );
        let mut clicked = None;
        if copy.clicked() {
            ui.ctx().data_mut(|d| d.insert_temp(id, self.time));
            clicked = Some(Clicked::Copy(text.to_string()));
        }
        // The code, scrolling sideways when it's wider than the block.
        let code_rect = Rect::from_min_max(
            pos2(rect.left() + 1.0, header_rect.bottom()),
            pos2(rect.right() - 1.0, rect.bottom() - 1.0),
        );
        let mut code_ui = ui.new_child(egui::UiBuilder::new().max_rect(code_rect));
        egui::ScrollArea::horizontal()
            .id_salt(self.id.with(("code", rect.top() as i32)))
            .auto_shrink([false, true])
            .show(&mut code_ui, |ui| {
                let (r, _) =
                    ui.allocate_exact_size(galley.size() + vec2(32.0, 32.0), Sense::hover());
                ui.painter().galley(r.min + vec2(16.0, 16.0), galley, p.ink);
            });
        clicked
    }

    fn table(
        &self,
        ui: &mut Ui,
        align: &[Alignment],
        head: &[Vec<Inline>],
        rows: &[Vec<Vec<Inline>>],
    ) -> Option<Clicked> {
        let p = self.palette;
        let painter = ui.painter().clone();
        let body = Type::sans(14.0, 24.0);
        let header = scale::MICRO.weight(400.0);
        let header = Type {
            line_height: 20.57,
            ..header
        };
        let columns = head.len().max(rows.iter().map(Vec::len).max().unwrap_or(0));
        if columns == 0 {
            return None;
        }
        // Natural widths, then shrink to fit with wrapping.
        let natural = |inlines: &[Inline], ty: Type, color: Color32| {
            let (job, _) = self.inline_job(&painter, inlines, ty, color, f32::INFINITY);
            painter.layout_job(job).size().x
        };
        let pad = |c: usize| -> (f32, f32) {
            let left = if c == 0 { 0.0 } else { 8.0 };
            let right = if c + 1 == columns { 0.0 } else { 8.0 };
            (left, right)
        };
        let mut widths = vec![0.0f32; columns];
        for (c, cell) in head.iter().enumerate() {
            widths[c] = widths[c].max(natural(cell, header, p.ink_faint));
        }
        for row in rows {
            for (c, cell) in row.iter().enumerate() {
                widths[c] = widths[c].max(natural(cell, body, p.ink));
            }
        }
        let available = ui.available_width();
        let padding: f32 = (0..columns).map(|c| pad(c).0 + pad(c).1).sum();
        let total: f32 = widths.iter().sum::<f32>() + padding;
        if total > available {
            let scale = ((available - padding) / widths.iter().sum::<f32>()).max(0.2);
            for w in &mut widths {
                *w *= scale;
            }
        }
        let table_width: f32 = widths.iter().sum::<f32>() + padding;
        let mut clicked = None;
        let mut draw_row = |ui: &mut Ui,
                            cells: &[Vec<Inline>],
                            ty: Type,
                            color: Color32,
                            top_pad: f32,
                            bottom_pad: f32,
                            line: Option<Color32>| {
            let laid: Vec<(Arc<Galley>, Vec<Run>)> = (0..columns)
                .map(|c| {
                    let inlines = cells.get(c).map_or(&[][..], Vec::as_slice);
                    let (job, runs) =
                        self.inline_job(&painter, inlines, ty, color, widths[c] + 0.5);
                    (painter.layout_job(job), runs)
                })
                .collect();
            let content = laid
                .iter()
                .map(|(g, _)| g.size().y)
                .fold(ty.line_height, f32::max);
            let height = top_pad + content + bottom_pad + if line.is_some() { 1.0 } else { 0.0 };
            let (rect, response) =
                ui.allocate_exact_size(vec2(table_width, height), Sense::hover());
            let text: Vec<String> = cells.iter().map(|c| super::markdown::plain(c)).collect();
            response.widget_info(|| {
                egui::WidgetInfo::labeled(egui::WidgetType::Label, true, text.join(" · "))
            });
            let mut x = rect.left();
            for (c, (galley, runs)) in laid.into_iter().enumerate() {
                let (l, r) = pad(c);
                let w = widths[c];
                let gx = match align.get(c) {
                    Some(Alignment::Right) => x + l + w - galley.size().x,
                    Some(Alignment::Center) => x + l + (w - galley.size().x) / 2.0,
                    _ => x + l,
                };
                let origin = pos2(gx, rect.top() + top_pad);
                if let Some(found) = self.decorate(ui, origin, &galley, &runs) {
                    clicked = Some(found);
                }
                ui.painter().galley(origin, galley, color);
                x += l + w + r;
            }
            if let Some(line) = line {
                ui.painter()
                    .hline(rect.x_range(), rect.bottom() - 0.5, Stroke::new(1.0, line));
            }
        };
        let spacing = std::mem::replace(&mut ui.spacing_mut().item_spacing.y, 0.0);
        if !head.is_empty() {
            draw_row(
                ui,
                head,
                header,
                p.ink_faint,
                0.0,
                6.86,
                Some(p.line_strong),
            );
        }
        for (i, row) in rows.iter().enumerate() {
            let line = (i + 1 < rows.len()).then_some(p.line);
            draw_row(ui, row, body, p.ink, 8.0, 8.0, line);
        }
        ui.spacing_mut().item_spacing.y = spacing;
        clicked
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Container {
    Root,
    Item,
    Quote,
}

struct Run {
    range: Range<usize>,
    kind: RunKind,
}

enum RunKind {
    Code,
    Chip(usize),
    Link(String),
}

/// Blank room `width` wide that never breaks: two non-breaking spaces,
/// pulled together or pushed apart by the letter spacing egui puts between
/// them (it puts none before a section's first glyph).
/// How selectable text senses the pointer: clicks and drags, but no
/// keyboard focus, as egui's selectable labels do.
pub fn selectable_sense() -> Sense {
    Sense::click_and_drag() - Sense::FOCUSABLE
}

fn spacer(painter: &Painter, job: &mut LayoutJob, ty: Type, width: f32) {
    let font = ty.font_id();
    let space = painter
        .fonts_mut(|f| f.glyph_width(&font, '\u{a0}'))
        .max(0.1);
    // Narrow room needs smaller spaces: egui won't pull glyphs together.
    let scale = (width / (2.0 * space)).min(1.0);
    let small = Type {
        size: ty.size * scale.max(0.05),
        ..ty
    };
    let mut format = small.format(Color32::TRANSPARENT);
    format.line_height = Some(ty.line_height);
    format.extra_letter_spacing = (width - 2.0 * space * scale.max(0.05)).max(0.0);
    job.append("\u{a0}\u{a0}", 0.0, format);
}

/// The rectangles a byte range of the job's text covers, one per row:
/// (rect, baseline, the font's height, its ascent).
fn run_rects(galley: &Galley, range: &Range<usize>) -> Vec<(Rect, f32, f32, f32)> {
    let mut out = Vec::new();
    for row in &galley.rows {
        let mut span: Option<(f32, f32, f32, f32, f32)> = None;
        for glyph in &row.row.glyphs {
            if range.contains(&(glyph.cluster as usize)) {
                let left = row.pos.x + glyph.pos.x;
                let right = left + glyph.advance_width;
                let baseline = row.pos.y + glyph.pos.y;
                span = Some(match span {
                    None => (left, right, baseline, glyph.font_height, glyph.font_ascent),
                    Some((l, r, b, h, a)) => (
                        l.min(left),
                        r.max(right),
                        b.max(baseline),
                        h.max(glyph.font_height),
                        a.max(glyph.font_ascent),
                    ),
                });
            }
        }
        if let Some((l, r, baseline, height, ascent)) = span {
            let rect = Rect::from_min_max(pos2(l, row.pos.y), pos2(r, row.pos.y + row.row.size.y));
            out.push((rect, baseline, height, ascent));
        }
    }
    out
}

/// A rounded rectangle filled with the rainbow from left to right.
pub fn rainbow_rect(painter: &Painter, rect: Rect, radius: f32) {
    let outline = paint::rounded_outline(rect, [radius; 4], 4);
    let mut mesh = egui::epaint::Mesh::default();
    let center = rect.center();
    let color = |p: Pos2| tokens::rainbow((p.x - rect.left()) / rect.width().max(1.0));
    mesh.colored_vertex(center, color(center));
    for p in &outline {
        mesh.colored_vertex(*p, color(*p));
    }
    let n = outline.len() as u32;
    for k in 0..n {
        mesh.add_triangle(0, 1 + k, 1 + (k + 1) % n);
    }
    painter.add(egui::Shape::mesh(mesh));
}

/// The icon a source gets in its chip and in the sources list.
pub fn source_icon(source: &Source) -> Icon {
    let title = source.title.to_lowercase();
    let url = source.url.as_deref().unwrap_or_default();
    if title.ends_with(".pdf") {
        Icon::FilePdf
    } else if [".xlsx", ".xls", ".csv", ".numbers"]
        .iter()
        .any(|e| title.ends_with(e))
        || url.contains("/spreadsheets/")
    {
        Icon::FileXls
    } else if [".docx", ".doc", ".pages", ".odt"]
        .iter()
        .any(|e| title.ends_with(e))
        || url.contains("/document/")
    {
        Icon::FileDoc
    } else if url.starts_with("http") && !url.contains("drive.google.com") {
        Icon::GlobeSimple
    } else {
        Icon::FileText
    }
}
