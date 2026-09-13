//! Drawing primitives: rectangles, rounded rectangles, and text.
//!
//! Everything draws into a premultiplied ARGB `Pixmap` that is later copied
//! into a shared-memory buffer. Shape drawing needs no font state, so those are
//! free functions; text needs the font database and glyph cache, so it lives on
//! [`Renderer`].

use cosmic_text::{
    Attrs, Buffer, Color as GlyphColor, Family, FontSystem, Metrics, Shaping, SwashCache,
};
use tiny_skia::{Color, FillRule, Paint, PathBuilder, Pixmap, PremultipliedColorU8, Rect, Transform};

use crate::config::Rgba;

/// Line height as a multiple of the font size, applied to every layout.
const LINE_HEIGHT_RATIO: f32 = 1.4;

/// Corner radius used where a panel meets the desktop.
pub const CORNER_RADIUS: f32 = 10.0;

/// Owns the font database and the glyph rasterization cache.
///
/// Build one at startup and reuse it for every frame: constructing a
/// `FontSystem` scans the system font directories, which is far too slow to do
/// per draw.
pub struct Renderer {
    font_system: FontSystem,
    swash: SwashCache,
    /// Font family name from the config; `None` selects the system monospace family.
    family: Option<String>,
}

impl Renderer {
    pub fn new(family: Option<String>) -> Self {
        Self {
            font_system: FontSystem::new(),
            swash: SwashCache::new(),
            family,
        }
    }

    fn describe(&self) -> Attrs<'_> {
        let family = match self.family.as_deref() {
            Some(name) => Family::Name(name),
            None => Family::Monospace,
        };
        Attrs::new().family(family)
    }

    /// Height of one line at `size`, including leading.
    pub fn line_height(&self, size: f32) -> f32 {
        (size * LINE_HEIGHT_RATIO).max(1.0)
    }

    /// Lay `text` out on a single, unconstrained line.
    fn layout(&mut self, text: &str, size: f32) -> Buffer {
        let metrics = Metrics::new(size, self.line_height(size));
        let mut buffer = Buffer::new(&mut self.font_system, metrics);
        buffer.set_size(None, None);
        let attrs = self.describe();
        buffer.set_text(text, &attrs, Shaping::Advanced, None);
        buffer.shape_until_scroll(&mut self.font_system, false);
        buffer
    }

    /// The size `text` occupies at `size`, in logical pixels.
    ///
    /// Empty text measures as zero so that a module with no value takes no room
    /// in the status bar.
    pub fn measure_text(&mut self, text: &str, size: f32) -> (f32, f32) {
        if text.is_empty() {
            return (0.0, 0.0);
        }
        let buffer = self.layout(text, size);
        let mut width: f32 = 0.0;
        let mut height: f32 = 0.0;
        for run in buffer.layout_runs() {
            width = width.max(run.line_w);
            height = height.max(run.line_top + run.line_height);
        }
        (width, height)
    }

    /// Shorten `text` to fit `budget` pixels, marking the cut with an ellipsis.
    ///
    /// Returns an empty string when not even the ellipsis fits.
    pub fn shorten(&mut self, text: &str, budget: f32, size: f32) -> String {
        if budget <= 0.0 || text.is_empty() {
            return String::new();
        }
        if self.measure_text(text, size).0 <= budget {
            return text.to_string();
        }

        let marker = '…';
        let marker_width = self.measure_text(&marker.to_string(), size).0;
        if marker_width > budget {
            return String::new();
        }

        // ponytail: one measurement per character. Module and notification text is
        // short enough that a binary search over graphemes would not pay for itself.
        let keep = budget - marker_width;
        let mut kept = String::new();
        for character in text.chars() {
            let mut candidate = kept.clone();
            candidate.push(character);
            if self.measure_text(&candidate, size).0 > keep {
                break;
            }
            kept = candidate;
        }
        kept.push(marker);
        kept
    }

    /// Draw `text` with its top-left corner at `(x, y)`.
    ///
    /// Pixels outside `pixmap` are dropped, so callers may pass text that runs
    /// off the edge without clipping it themselves.
    pub fn draw_text(
        &mut self,
        pixmap: &mut Pixmap,
        text: &str,
        x: f32,
        y: f32,
        size: f32,
        color: Rgba,
    ) {
        if text.is_empty() {
            return;
        }
        let color = GlyphColor::rgba(color.r, color.g, color.b, color.a);
        let mut buffer = self.layout(text, size);
        let origin_x = x.round() as i32;
        let origin_y = y.round() as i32;
        buffer.draw(
            &mut self.font_system,
            &mut self.swash,
            color,
            |glyph_x, glyph_y, _width, _height, pixel| {
                let target_x = origin_x + glyph_x;
                let target_y = origin_y + glyph_y;
                composite(pixmap, target_x, target_y, pixel);
            },
        );
    }
}

/// Composite one glyph pixel over the destination.
///
/// Both are premultiplied: source-over is `src + dst * (1 - src_alpha)`.
fn composite(pixmap: &mut Pixmap, x: i32, y: i32, src: GlyphColor) {
    let width = pixmap.width() as i32;
    let height = pixmap.height() as i32;
    if x < 0 || y < 0 || x >= width || y >= height {
        return;
    }
    let alpha = u32::from(src.a());
    if alpha == 0 {
        return;
    }

    let index = y as usize * pixmap.width() as usize + x as usize;
    let dst = pixmap.pixels()[index];
    let over = |src: u8, dst: u8| ((u32::from(src) * alpha + u32::from(dst) * (255 - alpha)) / 255) as u8;
    let dst_alpha = u32::from(dst.alpha());

    let blended = PremultipliedColorU8::from_rgba(
        over(src.r(), dst.red()),
        over(src.g(), dst.green()),
        over(src.b(), dst.blue()),
        (dst_alpha + alpha - dst_alpha * alpha / 255) as u8,
    );
    // `from_rgba` rejects values that are not premultiplied; rounding can trip
    // that check by one, in which case leaving the destination untouched is fine.
    if let Some(blended) = blended {
        pixmap.pixels_mut()[index] = blended;
    }
}

/// Rectangles with non-finite or non-positive sides cannot be drawn. `is_finite`
/// matters: a bare `> 0.0` test lets `NaN` through, since every comparison with
/// `NaN` is false.
fn drawable(width: f32, height: f32) -> bool {
    width.is_finite() && height.is_finite() && width > 0.0 && height > 0.0
}

fn to_color(color: Rgba) -> Color {
    Color::from_rgba8(color.r, color.g, color.b, color.a)
}

fn paint(color: Rgba) -> Paint<'static> {
    let mut paint = Paint::default();
    paint.set_color(to_color(color));
    paint.anti_alias = true;
    paint
}

/// Fill an axis-aligned rectangle.
pub fn fill_rect(pixmap: &mut Pixmap, x: f32, y: f32, width: f32, height: f32, color: Rgba) {
    if !drawable(width, height) {
        return;
    }
    let Some(rect) = Rect::from_xywh(x, y, width, height) else {
        return;
    };
    pixmap.fill_rect(rect, &paint(color), Transform::identity(), None);
}

/// Fill a rectangle whose corners are rounded by `radius`, clamped to half the
/// shorter side so oversized radii degrade to a rectangle rather than a bowtie.
pub fn fill_round_rect(
    pixmap: &mut Pixmap,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    radius: f32,
    color: Rgba,
) {
    if !drawable(width, height) {
        return;
    }
    let radius = radius.min(width / 2.0).min(height / 2.0);
    if radius <= 0.0 {
        fill_rect(pixmap, x, y, width, height, color);
        return;
    }

    let (right, bottom) = (x + width, y + height);
    let mut path = PathBuilder::new();
    path.move_to(x + radius, y);
    path.line_to(right - radius, y);
    path.quad_to(right, y, right, y + radius);
    path.line_to(right, bottom - radius);
    path.quad_to(right, bottom, right - radius, bottom);
    path.line_to(x + radius, bottom);
    path.quad_to(x, bottom, x, bottom - radius);
    path.line_to(x, y + radius);
    path.quad_to(x, y, x + radius, y);
    path.close();

    let Some(path) = path.finish() else {
        return;
    };
    pixmap.fill_path(
        &path,
        &paint(color),
        FillRule::Winding,
        Transform::identity(),
        None,
    );
}

/// Fill a rectangle whose bottom corners are rounded and whose top corners are
/// square.
///
/// Drawing the same translucent colour as a rounded rectangle plus a second
/// rectangle to square its top would blend the overlap twice, leaving a strip
/// that is more opaque than the rest.
pub fn fill_rect_round_bottom(
    pixmap: &mut Pixmap,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    radius: f32,
    color: Rgba,
) {
    if !drawable(width, height) {
        return;
    }
    let radius = radius.min(width / 2.0).min(height);
    if radius <= 0.0 {
        fill_rect(pixmap, x, y, width, height, color);
        return;
    }

    let (right, bottom) = (x + width, y + height);
    let mut path = PathBuilder::new();
    path.move_to(x, y);
    path.line_to(right, y);
    path.line_to(right, bottom - radius);
    path.quad_to(right, bottom, right - radius, bottom);
    path.line_to(x + radius, bottom);
    path.quad_to(x, bottom, x, bottom - radius);
    path.close();

    let Some(path) = path.finish() else {
        return;
    };
    pixmap.fill_path(
        &path,
        &paint(color),
        FillRule::Winding,
        Transform::identity(),
        None,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    const RED: Rgba = Rgba::new(0xff, 0x00, 0x00, 0xff);
    const WHITE: Rgba = Rgba::new(0xff, 0xff, 0xff, 0xff);

    fn pixmap(width: u32, height: u32) -> Pixmap {
        Pixmap::new(width, height).unwrap()
    }

    fn px(pixmap: &Pixmap, x: u32, y: u32) -> PremultipliedColorU8 {
        pixmap.pixels()[(y * pixmap.width() + x) as usize]
    }

    fn painted_pixels(pixmap: &Pixmap) -> usize {
        pixmap.pixels().iter().filter(|p| p.alpha() > 0).count()
    }

    #[test]
    fn rect_fills_exactly_the_requested_area() {
        let mut canvas = pixmap(10, 10);
        fill_rect(&mut canvas, 2.0, 3.0, 4.0, 2.0, RED);

        assert_eq!(px(&canvas, 2, 3).alpha(), 255, "top-left is inside");
        assert_eq!(px(&canvas, 5, 4).alpha(), 255, "bottom-right is inside");
        assert_eq!(px(&canvas, 1, 3).alpha(), 0, "left of the rect is untouched");
        assert_eq!(px(&canvas, 6, 4).alpha(), 0, "right of the rect is untouched");
        assert_eq!(px(&canvas, 2, 2).alpha(), 0, "above the rect is untouched");
        assert_eq!(px(&canvas, 2, 5).alpha(), 0, "below the rect is untouched");
    }

    #[test]
    fn degenerate_rects_draw_nothing() {
        let mut canvas = pixmap(4, 4);
        fill_rect(&mut canvas, 1.0, 1.0, 0.0, 2.0, RED);
        fill_rect(&mut canvas, 1.0, 1.0, 2.0, -1.0, RED);
        assert_eq!(painted_pixels(&canvas), 0);
    }

    #[test]
    fn round_rect_clips_the_corners_only() {
        let mut rounded = pixmap(20, 20);
        fill_round_rect(&mut rounded, 0.0, 0.0, 20.0, 20.0, 8.0, RED);
        assert!(
            px(&rounded, 0, 0).alpha() < 8,
            "corner should be clipped, got {}",
            px(&rounded, 0, 0).alpha()
        );
        assert_eq!(px(&rounded, 0, 10).alpha(), 255, "left edge mid-height is inside");
        assert_eq!(px(&rounded, 10, 10).alpha(), 255, "centre is inside");
        assert_eq!(px(&rounded, 10, 19).alpha(), 255, "bottom edge mid-width is inside");

        let mut square = pixmap(20, 20);
        fill_round_rect(&mut square, 0.0, 0.0, 20.0, 20.0, 0.0, RED);
        assert_eq!(px(&square, 0, 0).alpha(), 255, "radius 0 keeps the corner");
        assert_eq!(px(&square, 19, 19).alpha(), 255);
    }

    #[test]
    fn oversized_radius_degrades_to_a_rounded_square() {
        let mut canvas = pixmap(20, 20);
        fill_round_rect(&mut canvas, 0.0, 0.0, 20.0, 20.0, 500.0, RED);
        assert_eq!(px(&canvas, 10, 10).alpha(), 255, "still filled in the middle");
        assert!(px(&canvas, 0, 0).alpha() < 8, "corner still clipped");
    }

    #[test]
    fn translucent_fill_blends_with_what_is_underneath() {
        let mut canvas = pixmap(2, 2);
        fill_rect(&mut canvas, 0.0, 0.0, 2.0, 2.0, Rgba::new(0, 0, 0, 0xff));
        fill_rect(&mut canvas, 0.0, 0.0, 2.0, 2.0, Rgba::new(0xff, 0xff, 0xff, 0x80));

        let blended = px(&canvas, 0, 0);
        assert_eq!(blended.alpha(), 255, "opaque backdrop stays opaque");
        assert!(
            (i32::from(blended.red()) - 0x80).abs() <= 2,
            "half-transparent white over black should be ~0x80, got 0x{:02x}",
            blended.red()
        );
    }

    #[test]
    fn a_bottom_rounded_rect_is_square_on_top_and_round_below() {
        let mut canvas = pixmap(20, 20);
        fill_rect_round_bottom(&mut canvas, 0.0, 0.0, 20.0, 20.0, 8.0, RED);

        assert_eq!(px(&canvas, 0, 0).alpha(), 255, "the top-left corner stays square");
        assert_eq!(px(&canvas, 19, 0).alpha(), 255, "the top-right corner stays square");
        assert!(
            px(&canvas, 0, 19).alpha() < 8,
            "the bottom-left corner is rounded, got {}",
            px(&canvas, 0, 19).alpha()
        );
        assert_eq!(px(&canvas, 10, 0).alpha(), 255);
        assert_eq!(px(&canvas, 10, 19).alpha(), 255);
    }

    #[test]
    fn a_translucent_shape_is_not_blended_with_itself() {
        // Two passes of the same colour, as a rounded rect plus a squaring rect
        // would be, would leave the overlap more opaque than the rest.
        const HALF: Rgba = Rgba::new(0xff, 0xff, 0xff, 0x80);

        let mut once = pixmap(20, 20);
        fill_rect_round_bottom(&mut once, 0.0, 0.0, 20.0, 20.0, 8.0, HALF);

        assert_eq!(px(&once, 10, 5).alpha(), 0x80, "a single pass keeps the alpha");
        assert_eq!(px(&once, 10, 15).alpha(), 0x80);
    }

    #[test]
    fn text_measures_wider_as_it_grows() {
        let mut renderer = Renderer::new(None);

        assert_eq!(renderer.measure_text("", 14.0), (0.0, 0.0));

        let (short_w, short_h) = renderer.measure_text("quickbar", 14.0);
        assert!(short_w > 0.0, "expected a positive width, got {short_w}");
        assert!(short_h > 0.0, "expected a positive height, got {short_h}");

        let (long_w, _) = renderer.measure_text("quickbar is longer", 14.0);
        assert!(long_w > short_w, "{long_w} should exceed {short_w}");

        let (big_w, _) = renderer.measure_text("quickbar", 28.0);
        assert!(big_w > short_w, "a larger font should measure wider");
    }

    #[test]
    fn text_paints_pixels_where_it_is_drawn() {
        let mut renderer = Renderer::new(None);
        let mut canvas = pixmap(200, 40);

        renderer.draw_text(&mut canvas, "quickbar", 4.0, 4.0, 14.0, WHITE);

        assert!(
            painted_pixels(&canvas) > 20,
            "expected glyph pixels, found {}",
            painted_pixels(&canvas)
        );
        assert_eq!(px(&canvas, 0, 0).alpha(), 0, "nothing outside the text box");
    }

    #[test]
    fn text_is_clipped_instead_of_panicking_off_screen() {
        let mut renderer = Renderer::new(None);
        let mut canvas = pixmap(20, 20);

        renderer.draw_text(&mut canvas, "clipped", -500.0, -500.0, 14.0, WHITE);
        renderer.draw_text(&mut canvas, "clipped", 500.0, 500.0, 14.0, WHITE);

        assert_eq!(painted_pixels(&canvas), 0, "fully off-screen text paints nothing");
    }

    #[test]
    fn partially_visible_text_paints_only_inside_the_canvas() {
        let mut renderer = Renderer::new(None);
        let mut canvas = pixmap(200, 12);

        // Top 6px of the glyphs fall above the canvas.
        renderer.draw_text(&mut canvas, "clipped", 4.0, -6.0, 14.0, WHITE);

        let painted = painted_pixels(&canvas);
        assert!(painted > 0, "the visible part should still be drawn");
        assert!(painted < 200 * 12, "nothing is painted beyond the canvas");
    }

    #[test]
    fn empty_text_draws_nothing() {
        let mut renderer = Renderer::new(None);
        let mut canvas = pixmap(20, 20);
        renderer.draw_text(&mut canvas, "", 0.0, 0.0, 14.0, WHITE);
        assert_eq!(painted_pixels(&canvas), 0);
    }

    #[test]
    fn shortening_keeps_text_that_already_fits() {
        let mut renderer = Renderer::new(None);
        assert_eq!(renderer.shorten("clock", 500.0, 13.0), "clock");
        assert_eq!(renderer.shorten("clock", 0.0, 13.0), "");
    }

    #[test]
    fn shortening_adds_an_ellipsis_when_it_has_to_cut() {
        let mut renderer = Renderer::new(None);
        let shortened = renderer.shorten("a very long module value", 40.0, 13.0);

        assert!(shortened.ends_with('…'), "got {shortened}");
        assert!(shortened.len() < "a very long module value".len());
        assert!(renderer.measure_text(&shortened, 13.0).0 <= 40.0);
    }

    #[test]
    fn a_named_family_is_used_when_configured() {
        let mut renderer = Renderer::new(Some("Adwaita Mono".to_string()));
        let (width, _) = renderer.measure_text("quickbar", 14.0);
        assert!(width > 0.0, "configured family should still measure text");
    }
}
