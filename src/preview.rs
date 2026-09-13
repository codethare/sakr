//! Off-screen renders of the top and bottom edges.
//!
//! `quickbar --preview` writes these to disk so the look can be inspected
//! without a compositor. The top edge goes through the real painter; the bottom
//! edge still draws its own sample until the notification painter exists.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use tiny_skia::Pixmap;

use crate::bar::{self, ModuleValue, CORNER_RADIUS};
use crate::config::{Align, Config, Module, Rgba};
use crate::render::{self, Renderer};

/// Width of the sample canvas. Real surfaces span the whole output, but a
/// wide-enough sample is enough to judge margins and alignment.
const WIDTH: u32 = 900;

const CARD_WIDTH: f32 = 420.0;
const CARD_HEIGHT: f32 = 74.0;
const CARD_GAP: f32 = 8.0;
const CARD_MARGIN: f32 = 16.0;

pub const TOP_FILE: &str = "preview-top.png";
pub const BOTTOM_FILE: &str = "preview-bottom.png";

/// Render both samples into `dir`, returning the paths written.
pub fn write_all(config: &Config, dir: &Path) -> io::Result<Vec<PathBuf>> {
    let mut written = Vec::new();
    for (name, canvas) in [
        (TOP_FILE, render_top(config)),
        (BOTTOM_FILE, render_bottom(config)),
    ] {
        let path = dir.join(name);
        let png = canvas.encode_png().map_err(io::Error::other)?;
        fs::write(&path, png)?;
        written.push(path);
    }
    Ok(written)
}

/// A copy of `config` carrying sample modules, so the real painter can be
/// exercised without a running script layer.
fn sample_bar(config: &Config) -> (Config, Vec<ModuleValue>) {
    let mut config = config.clone();
    config.bar.module = vec![
        sample_module("workspaces", Align::Left),
        sample_module("cpu", Align::Left),
        sample_module("clock", Align::Center),
        sample_module("battery", Align::Right),
    ];

    let value = |text: &str, color| ModuleValue {
        text: text.to_string(),
        color,
    };
    let values = vec![
        value("1  2  3  4", None),
        value("cpu 12%", None),
        value("14:03  Tue 11 Aug", None),
        value("78%  WIFI", Some(Rgba::new(0xa3, 0xbe, 0x8c, 0xff))),
    ];
    (config, values)
}

fn sample_module(name: &str, align: Align) -> Module {
    Module {
        name: name.to_string(),
        exec: "true".to_string(),
        interval: Some(1),
        align,
        ..Module::default()
    }
}

/// The top edge in its expanded state: border, then status bar content.
pub fn render_top(config: &Config) -> Pixmap {
    let (config, values) = sample_bar(config);
    let mut canvas =
        Pixmap::new(WIDTH, config.border.width + config.bar.height).unwrap();
    let mut renderer = Renderer::new(config.bar.font.clone());
    bar::paint(&mut canvas, &config, &values, &mut renderer);
    canvas
}

/// The bottom edge: a notification stack growing up out of the border.
pub fn render_bottom(config: &Config) -> Pixmap {
    let border = config.border.width as f32;
    let stack_height = 2.0 * CARD_HEIGHT + CARD_GAP + CARD_MARGIN;
    let height = config.border.width + stack_height as u32;
    let mut canvas = Pixmap::new(WIDTH, height).unwrap();
    let mut renderer = Renderer::new(config.bar.font.clone());
    let size = config.bar.font_size;

    let samples = [
        (
            "Battery low",
            "12% remaining. Plug in soon.",
            config.notifications.urgent_color,
        ),
        (
            "Build finished",
            "cargo build --release in 42s",
            config.bar.foreground,
        ),
    ];

    for (index, (summary, body, accent)) in samples.iter().enumerate() {
        let y = height as f32
            - border
            - CARD_MARGIN
            - (index as f32 + 1.0) * CARD_HEIGHT
            - index as f32 * CARD_GAP;
        let x = WIDTH as f32 - CARD_WIDTH - CARD_MARGIN;

        render::fill_round_rect(
            &mut canvas,
            x,
            y,
            CARD_WIDTH,
            CARD_HEIGHT,
            CORNER_RADIUS,
            config.notifications.background,
        );
        render::fill_round_rect(
            &mut canvas,
            x,
            y + 12.0,
            3.0,
            CARD_HEIGHT - 24.0,
            1.5,
            *accent,
        );
        renderer.draw_text(
            &mut canvas,
            summary,
            x + 16.0,
            y + 10.0,
            size,
            config.notifications.text_color,
        );
        renderer.draw_text(
            &mut canvas,
            body,
            x + 16.0,
            y + 34.0,
            size - 1.5,
            Rgba {
                a: 0xaa,
                ..config.notifications.text_color
            },
        );
    }

    // Drawn last so it is never separated from the content above it.
    render::fill_rect(
        &mut canvas,
        0.0,
        height as f32 - border,
        WIDTH as f32,
        border,
        config.border.color,
    );

    canvas
}

#[cfg(test)]
mod tests {
    use super::*;

    fn px(pixmap: &Pixmap, x: u32, y: u32) -> tiny_skia::PremultipliedColorU8 {
        pixmap.pixels()[(y * pixmap.width() + x) as usize]
    }

    #[test]
    fn top_edge_starts_with_the_configured_border() {
        let config = Config::default();
        let canvas = render_top(&config);
        let colour = config.border.color;

        assert_eq!(canvas.width(), WIDTH);
        assert_eq!(canvas.height(), config.border.width + config.bar.height);

        for y in 0..config.border.width {
            let pixel = px(&canvas, 5, y);
            assert_eq!(u32::from(pixel.red()), u32::from(colour.r), "row {y}");
            assert_eq!(u32::from(pixel.green()), u32::from(colour.g));
            assert_eq!(u32::from(pixel.blue()), u32::from(colour.b));
            assert_eq!(u32::from(pixel.alpha()), u32::from(colour.a));
        }

        let below = px(&canvas, 5, config.border.width);
        assert_ne!(
            u32::from(below.red()),
            u32::from(colour.r),
            "the content area must not repeat the border colour, or the seam check is vacuous"
        );
    }

    #[test]
    fn bottom_edge_ends_with_the_configured_border() {
        let config = Config::default();
        let canvas = render_bottom(&config);
        let colour = config.border.color;
        let bottom = canvas.height() - 1;

        let pixel = px(&canvas, 5, bottom);
        assert_eq!(u32::from(pixel.red()), u32::from(colour.r));
        assert_eq!(u32::from(pixel.alpha()), u32::from(colour.a));
    }

    #[test]
    fn samples_are_written_as_png() {
        let dir = std::env::temp_dir().join(format!("quickbar-preview-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();

        let written = write_all(&Config::default(), &dir).unwrap();

        assert_eq!(written.len(), 2);
        for path in &written {
            let bytes = fs::read(path).unwrap();
            assert_eq!(
                &bytes[..8],
                b"\x89PNG\r\n\x1a\n",
                "{} is not a PNG",
                path.display()
            );
        }
        fs::remove_dir_all(&dir).unwrap();
    }
}
