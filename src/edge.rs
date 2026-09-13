//! One screen edge: its geometry, its painting, and the surface that carries it.
//!
//! The geometry is pure, so every anchor, size and input rectangle is testable
//! without a compositor.

use smithay_client_toolkit::{
    compositor::{CompositorState, Region},
    shell::{
        wlr_layer::{Anchor, LayerSurface},
        WaylandSurface,
    },
    shm::slot::SlotPool,
};
use tiny_skia::Pixmap;
use wayland_client::protocol::{wl_output, wl_shm};

use crate::bar::{self, ModuleValue};
use crate::config::Config;
use crate::notify;
use crate::notify::stack::Notification;
use crate::render::{self, Renderer};

/// Which side of the screen a surface hugs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    Top,
    Bottom,
    Left,
    Right,
}

impl Edge {
    pub const ALL: [Self; 4] = [Self::Top, Self::Bottom, Self::Left, Self::Right];

    /// Where the surface is pinned. Each edge is anchored on both ends of its
    /// own axis, so the compositor stretches it along that axis.
    pub fn anchor(self) -> Anchor {
        match self {
            Self::Top => Anchor::TOP | Anchor::LEFT | Anchor::RIGHT,
            Self::Bottom => Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT,
            Self::Left => Anchor::LEFT | Anchor::TOP | Anchor::BOTTOM,
            Self::Right => Anchor::RIGHT | Anchor::TOP | Anchor::BOTTOM,
        }
    }

    /// The size to ask the compositor for. A zero axis is the stretched one.
    ///
    /// These never change after startup: collapsing the bar changes what is
    /// painted and what accepts input, never the surface's geometry.
    pub fn requested_size(self, config: &Config) -> (u32, u32) {
        let border = config.border.width;
        match self {
            Self::Top => (0, border + config.bar.height),
            Self::Bottom => (0, border + config.bar.content_max_height),
            Self::Left | Self::Right => (border, 0),
        }
    }

    /// True for the two edges that are nothing but a border strip.
    fn is_vertical(self) -> bool {
        matches!(self, Self::Left | Self::Right)
    }
}

/// Everything an edge needs in order to draw.
#[derive(Debug, Default)]
pub struct Content<'a> {
    pub modules: &'a [ModuleValue],
    pub notifications: &'a [Notification],
    /// How far the top edge's status bar is out, 0.0 to 1.0.
    pub bar_progress: f32,
}

impl Content<'_> {
    /// Height of the content that slides out of this edge.
    pub fn height(&self, edge: Edge, config: &Config, renderer: &Renderer) -> f32 {
        match edge {
            Edge::Top => config.bar.height as f32,
            Edge::Bottom => notify::stack_height(config, self.notifications, renderer),
            Edge::Left | Edge::Right => 0.0,
        }
    }
}

/// The rectangle that accepts pointer input, in surface coordinates.
///
/// Collapsed, only the border strip does; the rest of the surface is transparent
/// to the pointer, so clicks land on whatever is underneath. Expanded, the
/// content is included as well.
///
/// Content counts as out as soon as it starts sliding (`progress > 0`), so a
/// pointer that follows the bar does not fall through it mid-slide and start the
/// collapse timer.
pub fn input_rect(
    edge: Edge,
    surface: (u32, u32),
    border: u32,
    content: f32,
    progress: f32,
) -> (i32, i32, i32, i32) {
    let width = surface.0 as i32;
    let height = surface.1 as i32;
    let border = (border as i32).min(if edge.is_vertical() { width } else { height });

    let strip = match edge {
        Edge::Top => (0, 0, width, border),
        Edge::Bottom => (0, height - border, width, border),
        Edge::Left => (0, 0, border, height),
        Edge::Right => (width - border, 0, border, height),
    };
    if progress <= 0.0 || edge.is_vertical() {
        return strip;
    }

    let thickness = (border + content.round() as i32).min(height);
    match edge {
        Edge::Top => (0, 0, width, thickness),
        Edge::Bottom => (0, height - thickness, width, thickness),
        Edge::Left | Edge::Right => strip,
    }
}

/// Paint one edge's surface.
pub fn paint(
    edge: Edge,
    pixmap: &mut Pixmap,
    config: &Config,
    content: &Content,
    renderer: &mut Renderer,
) {
    match edge {
        Edge::Top => bar::paint(
            pixmap,
            config,
            content.modules,
            renderer,
            content.bar_progress,
        ),
        Edge::Bottom => notify::paint(pixmap, config, content.notifications, renderer),
        // A side surface is only ever border, and it has no content to slide.
        Edge::Left | Edge::Right => render::fill_rect(
            pixmap,
            0.0,
            0.0,
            pixmap.width() as f32,
            pixmap.height() as f32,
            config.border.color,
        ),
    }
}

/// One edge's layer surface on one output.
pub struct Surface {
    pub edge: Edge,
    pub output: wl_output::WlOutput,
    pub layer: LayerSurface,
    pool: SlotPool,
    /// What the compositor configured. Zero until it has, and fixed afterwards.
    size: (u32, u32),
    configured: bool,
}

impl Surface {
    pub fn new(
        edge: Edge,
        output: wl_output::WlOutput,
        layer: LayerSurface,
        pool: SlotPool,
    ) -> Self {
        Self {
            edge,
            output,
            layer,
            pool,
            size: (0, 0),
            configured: false,
        }
    }

    /// Remember the size the compositor chose.
    ///
    /// It stays fixed for the life of the surface: collapsing the bar changes
    /// what is painted and what accepts input, never the geometry.
    pub fn configure(&mut self, width: u32, height: u32) {
        self.size = (width, height);
        self.configured = true;
    }

    pub fn size(&self) -> (u32, u32) {
        self.size
    }

    /// Paint this edge, submit it, and update what accepts pointer input.
    pub fn draw(
        &mut self,
        compositor: &CompositorState,
        config: &Config,
        content: &Content,
        renderer: &mut Renderer,
    ) -> Result<(), String> {
        if !self.configured {
            return Ok(());
        }
        let (width, height) = self.size;
        if width == 0 || height == 0 {
            return Ok(());
        }

        let stride = (width * 4) as i32;
        let (buffer, canvas) = self
            .pool
            .create_buffer(
                width as i32,
                height as i32,
                stride,
                wl_shm::Format::Argb8888,
            )
            .map_err(|error| failure(self.edge, format_args!("cannot create a buffer: {error}")))?;

        let mut pixmap = Pixmap::new(width, height)
            .ok_or_else(|| failure(self.edge, format_args!("cannot allocate a {width}x{height} canvas")))?;
        paint(self.edge, &mut pixmap, config, content, renderer);
        canvas.copy_from_slice(pixmap.data());

        self.set_input_region(compositor, config, content, renderer)?;

        let surface = self.layer.wl_surface();
        surface.damage_buffer(0, 0, width as i32, height as i32);
        buffer
            .attach_to(surface)
            .map_err(|error| failure(self.edge, format_args!("cannot attach a buffer: {error}")))?;
        self.layer.commit();
        Ok(())
    }

    /// Tell the compositor which part of the surface accepts the pointer.
    ///
    /// The region is snapshotted when the surface is committed, so building a
    /// fresh one each time is both correct and the simplest way to change its
    /// shape as the bar slides.
    fn set_input_region(
        &self,
        compositor: &CompositorState,
        config: &Config,
        content: &Content,
        renderer: &Renderer,
    ) -> Result<(), String> {
        let (x, y, width, height) = input_rect(
            self.edge,
            self.size,
            config.border.width,
            content.height(self.edge, config, renderer),
            content.bar_progress,
        );
        let region = Region::new(compositor)
            .map_err(|error| failure(self.edge, format_args!("cannot create an input region: {error}")))?;
        region.add(x, y, width, height);
        self.layer.set_input_region(Some(region.wl_region()));
        Ok(())
    }
}

fn failure(edge: Edge, message: std::fmt::Arguments<'_>) -> String {
    format!("edge {edge:?}: {message}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bar::ModuleValue;
    use crate::config::Rgba;

    const WIDTH: u32 = 800;

    fn pixel(pixmap: &Pixmap, x: u32, y: u32) -> (u8, u8, u8, u8) {
        let px = pixmap.pixels()[(y * pixmap.width() + x) as usize];
        (px.red(), px.green(), px.blue(), px.alpha())
    }

    fn border_of(config: &Config) -> (u8, u8, u8, u8) {
        let colour = config.border.color;
        (colour.r, colour.g, colour.b, colour.a)
    }

    #[test]
    fn every_edge_is_anchored_on_both_ends_of_its_axis() {
        let top = Anchor::TOP | Anchor::LEFT | Anchor::RIGHT;
        let bottom = Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT;
        let left = Anchor::LEFT | Anchor::TOP | Anchor::BOTTOM;
        let right = Anchor::RIGHT | Anchor::TOP | Anchor::BOTTOM;

        assert_eq!(Edge::Top.anchor(), top);
        assert_eq!(Edge::Bottom.anchor(), bottom);
        assert_eq!(Edge::Left.anchor(), left);
        assert_eq!(Edge::Right.anchor(), right);
    }

    #[test]
    fn requested_sizes_follow_the_config() {
        let config = Config::default();
        let border = config.border.width;

        assert_eq!(Edge::Top.requested_size(&config), (0, border + config.bar.height));
        assert_eq!(
            Edge::Bottom.requested_size(&config),
            (0, border + config.bar.content_max_height)
        );
        assert_eq!(Edge::Left.requested_size(&config), (border, 0));
        assert_eq!(Edge::Right.requested_size(&config), (border, 0));
    }

    #[test]
    fn a_collapsed_edge_only_accepts_input_on_its_border() {
        let config = Config::default();
        let border = config.border.width;
        let surface = (WIDTH, 34);

        assert_eq!(
            input_rect(Edge::Top, surface, border, 28.0, 0.0),
            (0, 0, WIDTH as i32, border as i32)
        );
        assert_eq!(
            input_rect(Edge::Bottom, surface, border, 200.0, 0.0),
            (0, 34 - border as i32, WIDTH as i32, border as i32)
        );
    }

    #[test]
    fn an_expanded_edge_accepts_input_over_its_content() {
        let config = Config::default();
        let border = config.border.width;
        let surface = (WIDTH, 34);

        assert_eq!(
            input_rect(Edge::Top, surface, border, 28.0, 1.0),
            (0, 0, WIDTH as i32, (border + 28) as i32)
        );

        // A surface tall enough for a card: the whole stack accepts input.
        let tall = (WIDTH, 200);
        assert_eq!(
            input_rect(Edge::Bottom, tall, border, 130.0, 1.0),
            (
                0,
                200 - (border as i32 + 130),
                WIDTH as i32,
                border as i32 + 130
            )
        );
    }

    #[test]
    fn content_counts_as_out_the_moment_it_starts_to_slide() {
        let config = Config::default();
        let border = config.border.width;

        let collapsed = input_rect(Edge::Top, (WIDTH, 34), border, 28.0, 0.0);
        let sliding = input_rect(Edge::Top, (WIDTH, 34), border, 28.0, 0.01);

        assert!(sliding.3 > collapsed.3, "the area should grow as it slides out");
    }

    #[test]
    fn input_is_clamped_to_the_surface() {
        let config = Config::default();
        let border = config.border.width;

        // Absurd content height, tiny surface: the rectangle stays inside it.
        let rect = input_rect(Edge::Bottom, (WIDTH, 20), border, 900.0, 1.0);
        assert_eq!(rect, (0, 0, WIDTH as i32, 20));
    }

    #[test]
    fn side_edges_accept_input_over_the_whole_strip() {
        let config = Config::default();
        let border = config.border.width;
        let surface = (border, 1000);

        assert_eq!(
            input_rect(Edge::Left, surface, border, 0.0, 1.0),
            (0, 0, border as i32, 1000)
        );
        assert_eq!(
            input_rect(Edge::Right, surface, border, 0.0, 1.0),
            (0, 0, border as i32, 1000)
        );
    }

    #[test]
    fn side_edges_are_painted_as_solid_border() {
        let config = Config::default();
        let mut renderer = Renderer::new(None);

        for edge in [Edge::Left, Edge::Right] {
            let mut canvas = Pixmap::new(config.border.width, 60).unwrap();
            paint(edge, &mut canvas, &config, &Content::default(), &mut renderer);

            for y in 0..canvas.height() {
                for x in 0..canvas.width() {
                    assert_eq!(pixel(&canvas, x, y), border_of(&config), "{edge:?} at ({x}, {y})");
                }
            }
        }
    }

    #[test]
    fn the_top_edge_paints_the_bar_at_the_given_progress() {
        let config = Config::default();
        let mut renderer = Renderer::new(None);
        let height = config.border.width + config.bar.height;

        let content = Content {
            modules: &[ModuleValue {
                text: "14:03".to_string(),
                color: Some(Rgba::new(0xff, 0xff, 0xff, 0xff)),
            }],
            notifications: &[],
            bar_progress: 0.0,
        };
        let mut collapsed = Pixmap::new(WIDTH, height).unwrap();
        paint(Edge::Top, &mut collapsed, &config, &content, &mut renderer);
        assert_eq!(
            pixel(&collapsed, WIDTH / 2, height - 1).3,
            0,
            "a collapsed bar shows nothing but the border"
        );

        let content = Content {
            bar_progress: 1.0,
            ..content
        };
        let mut shown = Pixmap::new(WIDTH, height).unwrap();
        paint(Edge::Top, &mut shown, &config, &content, &mut renderer);
        assert_eq!(
            pixel(&shown, WIDTH / 2, height - 1).3,
            config.bar.background.a,
            "an expanded bar covers the content area"
        );
    }

    #[test]
    fn the_bottom_edge_paints_cards_above_the_border() {
        let config = Config::default();
        let mut renderer = Renderer::new(None);
        let notification = Notification {
            id: 1,
            summary: "hello".to_string(),
            body: "world".to_string(),
            urgency: crate::notify::stack::Urgency::Normal,
            expires_at: None,
        };
        let height = config.border.width + 200;
        let mut canvas = Pixmap::new(WIDTH, height).unwrap();
        let content = Content {
            notifications: std::slice::from_ref(&notification),
            ..Content::default()
        };

        paint(Edge::Bottom, &mut canvas, &config, &content, &mut renderer);

        for y in (height - config.border.width)..height {
            assert_eq!(pixel(&canvas, WIDTH / 2, y), border_of(&config), "border row {y}");
        }
        assert!(
            pixel(&canvas, WIDTH - 60, height - config.border.width - 1).3 > 0,
            "a card should sit on the border"
        );
    }

    #[test]
    fn painting_does_not_depend_on_the_surface_being_the_requested_size() {
        // The compositor has the last word on size, so painting must work for
        // whatever it configured, including a wider or narrower surface.
        let config = Config::default();
        let mut renderer = Renderer::new(None);
        let content = Content::default();

        for width in [320, 1920] {
            let mut canvas = Pixmap::new(width, config.border.width + config.bar.height).unwrap();
            paint(Edge::Top, &mut canvas, &config, &content, &mut renderer);
            assert_eq!(pixel(&canvas, width - 1, 0), border_of(&config));
        }
    }
}
