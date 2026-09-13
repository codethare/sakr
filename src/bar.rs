//! Status-bar layout and painting.
//!
//! Modules are grouped into three segments by their configured [`Align`]. The
//! layout is computed before painting so it can be asserted on directly: every
//! module that is not drawn was dropped because its segment ran out of room, and
//! no module can reach past its segment.

use tiny_skia::Pixmap;

use crate::config::{Align, Config, Rgba};
use crate::render::{self, Renderer, CORNER_RADIUS};

/// Gap between the content edge and the outermost module.
pub const PADDING: f32 = 12.0;

/// Gap between two modules inside the same segment.
const MODULE_GAP: f32 = 12.0;

/// Minimum gap between two segments.
const SEGMENT_GAP: f32 = 16.0;

/// One module's current content.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModuleValue {
    pub text: String,
    /// Overrides `bar.foreground` for this module when set.
    pub color: Option<Rgba>,
}

/// A module that will be drawn, and where.
#[derive(Debug, Clone, PartialEq)]
pub struct Placed {
    /// Left edge in surface coordinates.
    pub x: f32,
    /// Text to draw, already shortened to fit its segment.
    pub text: String,
    pub color: Rgba,
}

/// Work out where each module goes inside a status bar of `width` pixels.
///
/// The centre segment is placed first and keeps its natural width; the left and
/// right segments get whatever room is left on their side. Each segment is then
/// truncated to its own band, so no segment can reach into a neighbour and a
/// long module only ever costs its own segment. Modules with no text take no
/// room.
pub fn layout(
    width: f32,
    config: &Config,
    values: &[ModuleValue],
    renderer: &mut Renderer,
) -> Vec<Placed> {
    let size = config.bar.font_size;
    let inner_width = (width - 2.0 * PADDING).max(0.0);

    // Which modules belong to which segment, and how wide each segment wants to be.
    let mut modules: [Vec<usize>; 3] = [Vec::new(), Vec::new(), Vec::new()];
    let mut natural = [0.0_f32; 3];
    for (index, module) in config.bar.module.iter().enumerate() {
        let slot = slot_of(module.align);
        let text = segment_text(values, index);
        if text.is_empty() {
            continue;
        }
        if !modules[slot].is_empty() {
            natural[slot] += MODULE_GAP;
        }
        natural[slot] += renderer.measure_text(text, size).0;
        modules[slot].push(index);
    }

    // Bands, in the order left / centre / right: the origin the segment is laid
    // out from and the width it may occupy.
    let centre_width = natural[1].min(inner_width);
    let centre_left = (width - centre_width) / 2.0;
    let centre_right = centre_left + centre_width;
    let bands = [
        PADDING,
        centre_left,
        centre_right + SEGMENT_GAP,
    ];
    let budgets = [
        (centre_left - SEGMENT_GAP - PADDING).max(0.0),
        centre_width,
        (width - PADDING - centre_right - SEGMENT_GAP).max(0.0),
    ];

    // Truncate every segment to its band first: the right segment is anchored on
    // its right edge, so its final width has to be known before placing it.
    // The resolved colour is carried along so a dropped module cannot shift the
    // colours of the ones after it.
    let mut fitted: [Vec<(String, f32, Rgba)>; 3] = [Vec::new(), Vec::new(), Vec::new()];
    for slot in 0..3 {
        let mut remaining = budgets[slot];
        let mut first = true;
        for &index in &modules[slot] {
            let gap = if first { 0.0 } else { MODULE_GAP };
            if remaining <= gap {
                break;
            }
            let text = renderer.shorten(segment_text(values, index), remaining - gap, size);
            let width = renderer.measure_text(&text, size).0;
            if width <= 0.0 {
                continue;
            }
            remaining -= gap + width;
            let color = values
                .get(index)
                .and_then(|value| value.color)
                .unwrap_or(config.bar.foreground);
            fitted[slot].push((text, width, color));
            first = false;
        }
    }

    // Place each segment as one block so the outer two anchor exactly on the
    // padding and the centre block is exactly centred.
    let mut placed = Vec::new();
    for (slot, fitted) in fitted.iter().enumerate() {
        let block: f32 = fitted.iter().map(|(_, width, _)| width).sum::<f32>()
            + MODULE_GAP * fitted.len().saturating_sub(1) as f32;
        if block <= 0.0 {
            continue;
        }
        let mut cursor = match slot {
            0 => bands[0],
            1 => (width - block) / 2.0,
            _ => width - PADDING - block,
        };
        for (index, (text, module_width, color)) in fitted.iter().enumerate() {
            if index > 0 {
                cursor += MODULE_GAP;
            }
            placed.push(Placed {
                x: cursor,
                text: text.clone(),
                color: *color,
            });
            cursor += module_width;
        }
    }

    placed
}

fn slot_of(align: Align) -> usize {
    match align {
        Align::Left => 0,
        Align::Center => 1,
        Align::Right => 2,
    }
}

fn segment_text(values: &[ModuleValue], index: usize) -> &str {
    values.get(index).map_or("", |value| value.text.as_str())
}

/// Paint the top edge: border strip, background, then the modules.
///
/// The border and the content are drawn into one buffer, so the border strip
/// squares off the top of the rounded content block and the two can never drift
/// apart.
pub fn paint(pixmap: &mut Pixmap, config: &Config, values: &[ModuleValue], renderer: &mut Renderer) {
    let width = pixmap.width() as f32;
    let height = pixmap.height() as f32;
    let border = config.border.width as f32;

    // One rounded block, squared off above the bottom radius and then again by the
    // border strip. Without the middle step the rounded top corners would leave a
    // transparent notch just below the border, where the arc is outside the block.
    render::fill_round_rect(
        pixmap,
        0.0,
        0.0,
        width,
        height,
        CORNER_RADIUS,
        config.bar.background,
    );
    render::fill_rect(
        pixmap,
        0.0,
        0.0,
        width,
        (height - CORNER_RADIUS).max(0.0),
        config.bar.background,
    );
    render::fill_rect(pixmap, 0.0, 0.0, width, border, config.border.color);

    let size = config.bar.font_size;
    let line = renderer.line_height(size);
    let top = border + ((config.bar.height as f32 - line) / 2.0).max(0.0);
    for module in layout(width, config, values, renderer) {
        renderer.draw_text(pixmap, &module.text, module.x, top, size, module.color);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Bar, Module};

    const WIDTH: f32 = 600.0;

    fn module(name: &str, align: Align) -> Module {
        Module {
            name: name.to_string(),
            exec: "true".to_string(),
            interval: Some(1),
            align,
            ..Module::default()
        }
    }

    fn config_with(modules: Vec<Module>) -> Config {
        Config {
            bar: Bar {
                module: modules,
                ..Bar::default()
            },
            ..Config::default()
        }
    }

    fn values(texts: &[&str]) -> Vec<ModuleValue> {
        texts
            .iter()
            .map(|text| ModuleValue {
                text: text.to_string(),
                color: None,
            })
            .collect()
    }

    fn width_of(text: &str, renderer: &mut Renderer) -> f32 {
        renderer.measure_text(text, Config::default().bar.font_size).0
    }

    #[test]
    fn the_three_segments_anchor_left_centre_and_right() {
        let config = config_with(vec![
            module("a", Align::Left),
            module("b", Align::Center),
            module("c", Align::Right),
        ]);
        let values = values(&["left", "middle", "right"]);
        let mut renderer = Renderer::new(None);

        let placed = layout(WIDTH, &config, &values, &mut renderer);
        assert_eq!(placed.len(), 3);

        assert_eq!(placed[0].x, PADDING, "left segment starts at the padding");

        let middle_width = width_of("middle", &mut renderer);
        assert!(
            (placed[1].x - (WIDTH - middle_width) / 2.0).abs() <= 1.0,
            "centre segment should be centred, got {}",
            placed[1].x
        );

        let right_width = width_of("right", &mut renderer);
        assert!(
            (placed[2].x + right_width - (WIDTH - PADDING)).abs() <= 1.0,
            "right segment should end at the padding, got {}",
            placed[2].x + right_width
        );
    }

    #[test]
    fn modules_in_a_segment_follow_the_configured_order() {
        let config = config_with(vec![
            module("first", Align::Left),
            module("second", Align::Left),
            module("third", Align::Left),
        ]);
        let values = values(&["one", "two", "three"]);
        let mut renderer = Renderer::new(None);

        let placed = layout(WIDTH, &config, &values, &mut renderer);

        assert_eq!(
            placed.iter().map(|m| m.text.as_str()).collect::<Vec<_>>(),
            ["one", "two", "three"]
        );
        for pair in placed.windows(2) {
            let gap = pair[1].x - (pair[0].x + width_of(&pair[0].text, &mut renderer));
            assert!(gap >= MODULE_GAP - 1.0, "modules overlap, gap was {gap}");
        }
    }

    #[test]
    fn a_module_without_a_value_is_not_drawn() {
        let config = config_with(vec![module("a", Align::Left), module("b", Align::Right)]);
        let values = values(&["", "shown"]);
        let mut renderer = Renderer::new(None);

        let placed = layout(WIDTH, &config, &values, &mut renderer);

        assert_eq!(placed.len(), 1);
        assert_eq!(placed[0].text, "shown");
    }

    #[test]
    fn a_module_colour_overrides_the_bar_foreground() {
        let config = config_with(vec![module("a", Align::Left)]);
        let mut values = values(&["battery"]);
        values[0].color = Some(Rgba::new(1, 2, 3, 4));
        let mut renderer = Renderer::new(None);

        let placed = layout(WIDTH, &config, &values, &mut renderer);

        assert_eq!(placed[0].color, Rgba::new(1, 2, 3, 4));
    }

    #[test]
    fn an_overlong_segment_is_cut_short_of_its_neighbour() {
        let config = config_with(vec![module("wide", Align::Left), module("b", Align::Right)]);
        let long = "x".repeat(400);
        let values = values(&[&long, "right"]);
        let mut renderer = Renderer::new(None);

        let placed = layout(WIDTH, &config, &values, &mut renderer);

        let left = placed.iter().find(|m| m.text.starts_with('x')).unwrap();
        let right = placed.iter().find(|m| m.text == "right").unwrap();
        let left_width = width_of(&left.text, &mut renderer);

        assert!(left.text.ends_with('…'), "truncation should be visible: {}", left.text);
        assert!(
            left.x + left_width <= right.x,
            "left segment reached into the right one: {} > {}",
            left.x + left_width,
            right.x
        );
    }

    #[test]
    fn nothing_is_drawn_outside_the_content_area() {
        let config = config_with(vec![
            module("wide", Align::Left),
            module("also wide", Align::Center),
            module("wide too", Align::Right),
        ]);
        let long = "y".repeat(300);
        let values = values(&[&long, &long, &long]);
        let mut renderer = Renderer::new(None);

        let placed = layout(WIDTH, &config, &values, &mut renderer);

        for module in &placed {
            let width = width_of(&module.text, &mut renderer);
            assert!(
                module.x >= PADDING - 0.5 && module.x + width <= WIDTH - PADDING + 0.5,
                "`{}` spans {}..{}",
                module.text,
                module.x,
                module.x + width
            );
        }
    }

    fn pixel(pixmap: &Pixmap, x: u32, y: u32) -> (u8, u8, u8, u8) {
        let px = pixmap.pixels()[(y * pixmap.width() + x) as usize];
        (px.red(), px.green(), px.blue(), px.alpha())
    }

    #[test]
    fn the_border_row_does_not_bleed_into_the_content_row() {
        let config = Config::default();
        let mut canvas = Pixmap::new(400, config.border.width + config.bar.height).unwrap();
        let mut renderer = Renderer::new(None);
        paint(&mut canvas, &config, &[], &mut renderer);

        let border = config.border.color;
        let expected = (border.r, border.g, border.b, border.a);
        for y in 0..config.border.width {
            for x in 0..canvas.width() {
                assert_eq!(pixel(&canvas, x, y), expected, "border pixel at ({x}, {y})");
            }
        }

        let content_row = config.border.width;
        let bleed = (0..canvas.width())
            .filter(|&x| pixel(&canvas, x, content_row) == expected)
            .count();
        assert_eq!(bleed, 0, "the row below the border must not repeat the border colour");
    }

    #[test]
    fn the_content_area_has_no_notch_below_the_border() {
        let config = Config::default();
        let mut canvas = Pixmap::new(400, config.border.width + config.bar.height).unwrap();
        let mut renderer = Renderer::new(None);
        paint(&mut canvas, &config, &[], &mut renderer);

        // The rounded top corners must be squared off: every pixel of the content
        // area is opaque, including the ones diagonally below the border ends.
        for y in config.border.width..canvas.height() {
            for x in 0..canvas.width() {
                let alpha = pixel(&canvas, x, y).3;
                // The bottom radius legitimately cuts the last few rows at the ends.
                let in_bottom_radius = y >= canvas.height() - CORNER_RADIUS as u32;
                let near_edge = x < CORNER_RADIUS as u32 || x >= canvas.width() - CORNER_RADIUS as u32;
                if in_bottom_radius && near_edge {
                    continue;
                }
                assert!(alpha > 0, "transparent notch at ({x}, {y})");
            }
        }
    }

    #[test]
    fn module_text_lands_below_the_border() {
        let config = config_with(vec![module("clock", Align::Center)]);
        let values = values(&["14:03"]);
        let mut canvas = Pixmap::new(400, config.border.width + config.bar.height).unwrap();
        let mut renderer = Renderer::new(None);
        paint(&mut canvas, &config, &values, &mut renderer);

        // Sample the background well clear of the text, then count what differs.
        let background = pixel(&canvas, canvas.width() / 2, canvas.height() - 2);
        let painted = (config.border.width..canvas.height())
            .flat_map(|y| (0..canvas.width()).map(move |x| (x, y)))
            .filter(|&(x, y)| pixel(&canvas, x, y) != background)
            .count();
        assert!(painted > 20, "expected module glyphs, found {painted} non-background pixels");
    }
}
