//! Drawing what CSS draws and egui doesn't: gradient rings, blurred glows,
//! soft drop shadows, turning conic rings, shimmering text, pulsing halos,
//! the dot grid, and the web's tooltips.

use std::f32::consts::{PI, TAU};
use std::sync::Arc;

use egui::epaint::{Mesh, Shape};
use egui::text::LayoutJob;
use egui::{Color32, Galley, Painter, Pos2, Rect, Vec2, pos2, vec2};

use super::tokens::{self, Palette, Type};

/// Points along a rounded rectangle's outline, clockwise from the top-left
/// corner's start, with `steps` points per corner. Radii are per corner:
/// top-left, top-right, bottom-right, bottom-left.
pub fn rounded_outline(rect: Rect, radii: [f32; 4], steps: usize) -> Vec<Pos2> {
    rounded_outline_with(rect, radii, steps, side_pieces(rect))
}

/// How many pieces the top and right sides of `rect` are cut into.
fn side_pieces(rect: Rect) -> [usize; 2] {
    let pieces = |length: f32| (length / 8.0).ceil().clamp(1.0, 256.0) as usize;
    [pieces(rect.width()), pieces(rect.height())]
}

/// [`rounded_outline`] with the sides cut into `sides` pieces, so outlines
/// of nested rectangles have matching points.
pub fn rounded_outline_with(
    rect: Rect,
    radii: [f32; 4],
    steps: usize,
    sides: [usize; 2],
) -> Vec<Pos2> {
    let max = (rect.width().min(rect.height()) / 2.0).max(0.0);
    let [tl, tr, br, bl] = radii.map(|r| r.clamp(0.0, max));
    let corners = [
        (pos2(rect.left() + tl, rect.top() + tl), tl, PI),
        (pos2(rect.right() - tr, rect.top() + tr), tr, 1.5 * PI),
        (pos2(rect.right() - br, rect.bottom() - br), br, 0.0),
        (pos2(rect.left() + bl, rect.bottom() - bl), bl, 0.5 * PI),
    ];
    let mut corner_points = Vec::with_capacity(4 * (steps + 1));
    for (center, r, start) in corners {
        for i in 0..=steps {
            let a = start + 0.5 * PI * i as f32 / steps as f32;
            corner_points.push(center + r * vec2(a.cos(), a.sin()));
        }
    }
    // Long straight sides get points every 8 or so, so colors that vary
    // along them (gradients) have vertices to vary on. The count depends
    // only on the rectangle, so rings built from two outlines line up.
    let mut points = Vec::with_capacity(corner_points.len() * 2);
    let n = corner_points.len();
    for (k, p) in corner_points.iter().enumerate() {
        points.push(*p);
        // The side after each corner.
        if (k + 1) % (steps + 1) == 0 {
            let next = corner_points[(k + 1) % n];
            let side = (k / (steps + 1)) % 2;
            let pieces = sides[side];
            for j in 1..pieces {
                points.push(*p + (next - *p) * (j as f32 / pieces as f32));
            }
        }
    }
    points
}

fn corner_steps(radius: f32) -> usize {
    ((radius * 0.9) as usize).clamp(4, 24)
}

/// A ring `width` wide inside the edge of a rounded rectangle, colored per
/// vertex by `color`. With `width` negative the ring sits outside the edge.
pub fn ring(
    painter: &Painter,
    rect: Rect,
    radius: f32,
    width: f32,
    color: impl Fn(Pos2) -> Color32,
) {
    let steps = corner_steps(radius);
    let sides = side_pieces(rect);
    let outer = rounded_outline_with(rect, [radius; 4], steps, sides);
    let inner_rect = rect.shrink(width);
    let inner = rounded_outline_with(inner_rect, [(radius - width).max(0.0); 4], steps, sides);
    let mut mesh = Mesh::default();
    for (o, i) in outer.iter().zip(&inner) {
        mesh.colored_vertex(*o, color(*o));
        mesh.colored_vertex(*i, color(*i));
    }
    strip(&mut mesh, outer.len());
    painter.add(Shape::mesh(mesh));
}

/// Join `pairs` (outer, inner) vertex pairs into a closed quad strip.
fn strip(mesh: &mut Mesh, pairs: usize) {
    for k in 0..pairs {
        let next = (k + 1) % pairs;
        let (a, b, c, d) = (2 * k, 2 * k + 1, 2 * next, 2 * next + 1);
        mesh.add_triangle(a as u32, b as u32, c as u32);
        mesh.add_triangle(b as u32, d as u32, c as u32);
    }
}

/// A blurred rounded rectangle filled by `color` (per position), like a
/// CSS `filter: blur()` on a gradient: a stack of feathered rings around
/// the shape whose coverage follows a Gaussian's integral.
pub fn glow(
    painter: &Painter,
    rect: Rect,
    radius: f32,
    blur: f32,
    opacity: f32,
    color: impl Fn(Pos2) -> Color32,
) {
    if opacity <= 0.0 {
        return;
    }
    painter.add(glow_shape(rect, radius, blur, opacity, color));
}

/// [`glow`] as a shape, to place under something drawn first.
pub fn glow_shape(
    rect: Rect,
    radius: f32,
    blur: f32,
    opacity: f32,
    color: impl Fn(Pos2) -> Color32,
) -> Shape {
    // CSS blur(r) is a Gaussian with a standard deviation of r.
    let sigma = blur.max(1.0);
    let coverage = |d: f32| 0.5 * erfc(d / (sigma * std::f32::consts::SQRT_2));
    let offsets: Vec<f32> = (0..=14)
        .map(|i| -2.0 * sigma + 5.0 * sigma * i as f32 / 14.0)
        .collect();
    let steps = corner_steps(radius + 3.0 * sigma);
    let mut mesh = Mesh::default();
    let sides = side_pieces(rect.expand(3.0 * sigma));
    let rings: Vec<Vec<Pos2>> = offsets
        .iter()
        .map(|&d| {
            // Rings inside a small shape stop at its middle.
            let d = d.max(0.5 - rect.width().min(rect.height()) / 2.0);
            let r = (radius + d).max(0.0);
            rounded_outline_with(rect.expand(d), [r; 4], steps, sides)
        })
        .collect();
    let n = rings[0].len();
    for (ring, &d) in rings.iter().zip(&offsets) {
        let a = opacity * coverage(d);
        for p in ring {
            mesh.colored_vertex(*p, color(*p).gamma_multiply(a));
        }
    }
    for r in 0..rings.len() - 1 {
        for k in 0..n {
            let next = (k + 1) % n;
            let (a, b) = ((r * n + k) as u32, (r * n + next) as u32);
            let (c, d) = (((r + 1) * n + k) as u32, ((r + 1) * n + next) as u32);
            mesh.add_triangle(a, c, b);
            mesh.add_triangle(b, c, d);
        }
    }
    // The middle, at full coverage of the innermost ring.
    let center = rect.center();
    let base = mesh.vertices.len() as u32;
    mesh.colored_vertex(
        center,
        color(center).gamma_multiply(opacity * coverage(offsets[0])),
    );
    for k in 0..n {
        mesh.add_triangle(base, k as u32, ((k + 1) % n) as u32);
    }
    Shape::mesh(mesh)
}

/// A CSS `box-shadow` without inset, as a shape.
pub fn drop_shadow_shape(
    rect: Rect,
    radius: f32,
    offset: Vec2,
    blur: f32,
    spread: f32,
    color: Color32,
) -> Shape {
    let rect = rect.translate(offset).expand(spread);
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return Shape::Noop;
    }
    glow_shape(rect, (radius + spread).max(0.0), blur / 2.0, 1.0, |_| color)
}

/// The complementary error function, to within a few thousandths.
fn erfc(x: f32) -> f32 {
    // Abramowitz and Stegun 7.1.26.
    let sign = if x < 0.0 { -1.0 } else { 1.0 };
    let x = x.abs();
    let t = 1.0 / (1.0 + 0.327_591_1 * x);
    let y = 1.0
        - (((((1.061_405_4 * t - 1.453_152_1) * t) + 1.421_413_7) * t - 0.284_496_74) * t
            + 0.254_829_6)
            * t
            * (-x * x).exp();
    1.0 - sign * y
}

/// `--shadow-float`: `0 20px 50px -24px` in the shadow color.
pub fn float_shadow(painter: &Painter, rect: Rect, radius: f32, palette: &Palette) {
    drop_shadow(
        painter,
        rect,
        radius,
        vec2(0.0, 20.0),
        50.0,
        -24.0,
        palette.shadow,
    );
}

/// `--shadow-menu`: a hairline ring and `0 16px 40px -16px`.
pub fn menu_shadow(painter: &Painter, rect: Rect, radius: f32, palette: &Palette) {
    drop_shadow(
        painter,
        rect,
        radius,
        vec2(0.0, 16.0),
        40.0,
        -16.0,
        palette.shadow,
    );
}

/// A CSS `box-shadow` without inset.
pub fn drop_shadow(
    painter: &Painter,
    rect: Rect,
    radius: f32,
    offset: Vec2,
    blur: f32,
    spread: f32,
    color: Color32,
) {
    let rect = rect.translate(offset).expand(spread);
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return;
    }
    // A box-shadow's blur radius is twice the Gaussian's deviation.
    glow(
        painter,
        rect,
        (radius + spread).max(0.0),
        blur / 2.0,
        1.0,
        |_| color,
    );
}

/// A ring of a conic rainbow turning around `center` (`--rainbow-conic`
/// spinning): `turn` is how far it has turned, in turns.
pub fn conic_ring(
    painter: &Painter,
    rect: Rect,
    radius: f32,
    width: f32,
    turn: f32,
    gray: Option<f32>,
) {
    let center = rect.center();
    ring(painter, rect, radius, width, |p| {
        // Conic gradients start at the top and go clockwise.
        let d = p - center;
        let angle = d.x.atan2(-d.y).rem_euclid(TAU) / TAU;
        let color = tokens::rainbow_loop(angle - turn);
        match gray {
            // `filter: grayscale(1); opacity: 0.5`
            Some(opacity) => {
                let l = (0.2126 * f32::from(color.r())
                    + 0.7152 * f32::from(color.g())
                    + 0.0722 * f32::from(color.b())) as u8;
                Color32::from_gray(l).gamma_multiply(opacity)
            }
            None => color,
        }
    });
}

/// A circle ring of the turning conic rainbow.
pub fn conic_circle(
    painter: &Painter,
    center: Pos2,
    radius: f32,
    width: f32,
    turn: f32,
    gray: bool,
) {
    let rect = Rect::from_center_size(center, Vec2::splat(radius * 2.0));
    conic_ring(painter, rect, radius, width, turn, gray.then_some(0.5));
}

/// `pulse-halo`: a ring around a dot that breathes out and fades, over
/// `period` seconds with the glide curve.
pub fn pulse_halo(painter: &Painter, center: Pos2, radius: f32, color: Color32, phase: f32) {
    // 0% and 100%: 3px at 24%; 50%: 6px at 0%.
    let t = if phase < 0.5 {
        phase * 2.0
    } else {
        2.0 - phase * 2.0
    };
    let t = tokens::ease_glide(t);
    let spread = 3.0 + 3.0 * t;
    let alpha = 0.24 * (1.0 - t);
    painter.circle_filled(center, radius + spread, color.gamma_multiply(alpha));
}

/// Text in the page's type.
pub fn job(text: &str, ty: Type, color: Color32, wrap: f32) -> LayoutJob {
    let mut job = LayoutJob::single_section(text.to_string(), ty.format(color));
    job.wrap.max_width = wrap;
    job
}

/// One line, cut with an ellipsis to `width`.
pub fn line_job(text: &str, ty: Type, color: Color32, width: f32) -> LayoutJob {
    let mut job = job(text, ty, color, width);
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    job
}

pub fn layout(painter: &Painter, job: LayoutJob) -> Arc<Galley> {
    painter.layout_job(job)
}

/// Paint `galley` with a light sweeping across it, as `.shimmer` does: the
/// text's color, then `ink-faint` at the sweep, over a 2 s loop.
pub fn shimmer(painter: &Painter, pos: Pos2, galley: &Arc<Galley>, faint: Color32, time: f64) {
    let width = galley.size().x.max(1.0);
    // background-size 200%, moving from 200% to -200% in 2 s, repeating.
    let t = (time / 2.0).fract() as f32;
    let offset = -2.0 * width + 4.0 * width * t;
    let mut shimmered = (**galley).clone();
    for placed in &mut shimmered.rows {
        let row_x = placed.pos.x;
        let row = Arc::make_mut(&mut placed.row);
        let range = row.visuals.glyph_vertex_range.clone();
        for vertex in &mut row.visuals.mesh.vertices[range] {
            let x = row_x + vertex.pos.x;
            let u = ((x - offset) / (2.0 * width)).rem_euclid(1.0);
            let k = if u < 0.35 || u > 0.65 {
                0.0
            } else if u < 0.5 {
                (u - 0.35) / 0.15
            } else {
                (0.65 - u) / 0.15
            };
            vertex.color = tokens::lerp_rgb(vertex.color, faint, k);
        }
    }
    painter.galley(pos, Arc::new(shimmered), faint);
}

/// The dot grid (`.dot-grid`), faded out towards the edges of `rect` when
/// `fade` is set (`.dot-grid--fade`: an ellipse 70% by 60% at 50% 40%).
pub fn dot_grid(painter: &Painter, rect: Rect, spacing: f32, color: Color32, fade: bool) {
    let center = pos2(rect.center().x, rect.top() + rect.height() * 0.4);
    let (rx, ry) = (rect.width() * 0.7, rect.height() * 0.6);
    let x0 = rect.center().x - (rect.width() / 2.0 / spacing).ceil() * spacing;
    let mut y = rect.top() + spacing / 2.0;
    while y < rect.bottom() {
        let mut x = x0;
        while x < rect.right() {
            if x >= rect.left() {
                let mut a = 1.0;
                if fade {
                    let d = (((x - center.x) / rx).powi(2) + ((y - center.y) / ry).powi(2)).sqrt();
                    // Opaque to 30%, gone by 75%.
                    a = ((0.75 - d) / 0.45).clamp(0.0, 1.0);
                }
                if a > 0.01 {
                    painter.circle_filled(pos2(x, y), 1.0, color.gamma_multiply(a));
                }
            }
            x += spacing;
        }
        y += spacing;
    }
}

/// A daisyUI tooltip over `anchor`: `neutral` with rounded corners and a
/// small tail, fading and sliding in.
pub fn tooltip(
    ctx: &egui::Context,
    anchor: Rect,
    text: &str,
    palette: &Palette,
    shown: f32,
    above: bool,
) {
    if shown <= 0.0 {
        return;
    }
    let layer = egui::LayerId::new(egui::Order::Tooltip, egui::Id::new("chat-tooltip"));
    let painter = ctx.layer_painter(layer);
    let ty = tokens::scale::SMALL.weight(400.0);
    let galley = layout(
        &painter,
        job(
            text,
            Type {
                line_height: 17.5,
                ..ty
            },
            palette.neutral_content,
            320.0,
        ),
    );
    let size = galley.size() + vec2(16.0, 8.0);
    let slide = 4.0 * (1.0 - shown);
    let (top, tail_y) = if above {
        (
            anchor.top() - 8.0 - size.y + slide,
            anchor.top() - 8.0 + slide,
        )
    } else {
        (anchor.bottom() + 8.0 - slide, anchor.bottom() + 8.0 - slide)
    };
    let screen = ctx.content_rect();
    let left = (anchor.center().x - size.x / 2.0)
        .clamp(screen.left() + 4.0, screen.right() - size.x - 4.0);
    let rect = Rect::from_min_size(pos2(left, top), size);
    let bg = palette.neutral.gamma_multiply(shown);
    painter.rect_filled(rect, tokens::RADIUS_CONTROL, bg);
    let tip = anchor.center().x;
    let tail = if above {
        vec![
            pos2(tip - 5.0, tail_y),
            pos2(tip + 5.0, tail_y),
            pos2(tip, tail_y + 4.0),
        ]
    } else {
        vec![
            pos2(tip - 5.0, tail_y),
            pos2(tip + 5.0, tail_y),
            pos2(tip, tail_y - 4.0),
        ]
    };
    painter.add(Shape::convex_polygon(tail, bg, egui::Stroke::NONE));
    painter.galley_with_override_text_color(
        rect.min + vec2(8.0, 4.0),
        galley,
        palette.neutral_content.gamma_multiply(shown),
    );
}

/// A `.kbd-hint`: quiet keys beside a control, like `Ctrl K`.
pub fn kbd_hint(
    painter: &Painter,
    right_center: Pos2,
    keys: &[&str],
    palette: &Palette,
    opacity: f32,
) -> f32 {
    if opacity <= 0.0 {
        return 0.0;
    }
    let mut x = right_center.x;
    for key in keys.iter().rev() {
        let galley = layout(
            painter,
            job(
                key,
                tokens::scale::KBD,
                palette.ink_faint.gamma_multiply(opacity),
                f32::INFINITY,
            ),
        );
        // 4 padding and a 1 border each side, 24 high with a 2 bottom edge.
        let w = (galley.size().x + 10.0).max(18.0);
        let rect = Rect::from_min_max(
            pos2(x - w, right_center.y - 12.0),
            pos2(x, right_center.y + 12.0),
        );
        // A hairline key with a 2px bottom edge.
        let line = egui::Stroke::new(1.0, palette.line.gamma_multiply(opacity));
        painter.rect_stroke(
            rect.shrink(0.5),
            tokens::RADIUS_TAG,
            line,
            egui::StrokeKind::Middle,
        );
        painter.line_segment(
            [
                pos2(rect.left() + 3.0, rect.bottom() - 1.5),
                pos2(rect.right() - 3.0, rect.bottom() - 1.5),
            ],
            line,
        );
        let inner = Rect::from_min_max(rect.min + vec2(1.0, 1.0), rect.max - vec2(1.0, 2.0));
        painter.galley(
            pos2(
                rect.center().x - galley.size().x / 2.0,
                inner.center().y - galley.size().y / 2.0,
            ),
            galley,
            palette.ink_faint,
        );
        x -= w + 3.0;
    }
    right_center.x - x - 3.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rounded_outlines_stay_inside_their_rectangle() {
        let rect = Rect::from_min_size(pos2(10.0, 20.0), vec2(100.0, 40.0));
        for p in rounded_outline(rect, [20.0, 6.0, 0.0, 50.0], 8) {
            assert!(rect.expand(0.01).contains(p), "{p:?}");
        }
    }

    #[test]
    fn erfc_is_close() {
        assert!((erfc(0.0) - 1.0).abs() < 1e-3);
        assert!((erfc(1.0) - 0.1573).abs() < 1e-3);
        assert!((erfc(-1.0) - 1.8427).abs() < 1e-3);
    }
}
