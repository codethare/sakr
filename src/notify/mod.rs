//! Notifications: the queue of what is showing, and how it is drawn.
//!
//! The bottom edge hosts the stack. Cards sit flush against the border and grow
//! upward, newest nearest the border, all in the same buffer as the border.

pub mod dbus;
pub mod stack;

use tiny_skia::Pixmap;

use crate::config::{Config, Rgba};
use crate::render::{self, Renderer, CORNER_RADIUS};
use stack::Notification;

/// Card width. Narrower surfaces shorten it further.
const CARD_WIDTH: f32 = 420.0;
/// Horizontal inset of the stack from the surface edges.
const STACK_MARGIN: f32 = 16.0;
/// Vertical space between two cards.
const CARD_GAP: f32 = 8.0;
/// Padding between a card's edge and its text.
const CARD_PADDING: f32 = 16.0;
/// Vertical padding inside a card.
const CARD_PADDING_V: f32 = 12.0;
/// Width of the coloured bar marking a card's urgency.
const ACCENT_WIDTH: f32 = 3.0;
/// Vertical inset of that bar from the card's edges.
const ACCENT_INSET: f32 = 12.0;
/// The body line is drawn this much smaller than the summary.
const BODY_SIZE_STEP: f32 = 1.5;
/// Alpha of the body text, so it reads as secondary.
const BODY_ALPHA: u8 = 0xaa;

/// Total height the cards need, excluding the border.
pub fn stack_height(config: &Config, notifications: &[Notification], renderer: &Renderer) -> f32 {
    match notifications.len() {
        0 => 0.0,
        count => {
            let cards: f32 = notifications
                .iter()
                .map(|item| card_height(config, item, renderer))
                .sum();
            cards + CARD_GAP * (count - 1) as f32
        }
    }
}

/// Height of a single card: one line for the summary, plus one for the body.
fn card_height(config: &Config, notification: &Notification, renderer: &Renderer) -> f32 {
    let size = config.bar.font_size;
    let mut height = 2.0 * CARD_PADDING_V + renderer.line_height(size);
    if !notification.body.is_empty() {
        height += renderer.line_height(size - BODY_SIZE_STEP);
    }
    height
}

/// Where one card sits on the surface.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Card {
    /// The notification's id, for click handling.
    pub id: u32,
    /// Index into the notification list this was laid out from.
    pub index: usize,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// Lay the cards out on a surface of the given size.
///
/// The newest sits nearest the border and the stack grows upward from there. A
/// card that would not fit is left out entirely rather than drawn half off the
/// top. Painting and click handling both go through this, so they cannot
/// disagree about where a card is.
pub fn layout(
    config: &Config,
    notifications: &[Notification],
    renderer: &Renderer,
    surface: (u32, u32),
) -> Vec<Card> {
    let width = surface.0 as f32;
    let height = surface.1 as f32;
    let border = config.border.width as f32;
    let card_width = CARD_WIDTH.min((width - 2.0 * STACK_MARGIN).max(0.0));
    let x = width - STACK_MARGIN - card_width;

    let mut cards: Vec<Card> = Vec::new();
    let mut bottom = height - border;
    for (index, notification) in notifications.iter().enumerate().rev() {
        let height = card_height(config, notification, renderer);
        if bottom - height < 0.0 {
            break;
        }
        cards.push(Card {
            id: notification.id,
            index,
            x,
            y: bottom - height,
            width: card_width,
            height,
        });
        bottom -= height + CARD_GAP;
    }
    // Oldest first, matching the order they were pushed in.
    cards.reverse();
    cards
}

/// The notification under a point, if any.
///
pub fn hit(cards: &[Card], x: f32, y: f32) -> Option<u32> {
    cards
        .iter()
        .rev()
        .find(|card| {
            x >= card.x && x < card.x + card.width && y >= card.y && y < card.y + card.height
        })
        .map(|card| card.id)
}

/// Paint the bottom edge: the notification stack, then the border strip.
///
/// Cards are placed from the border upward, so the newest is nearest the border
/// and the stack grows as notifications arrive. A card that would not fit in the
/// surface is left out entirely rather than drawn half off the top.
pub fn paint(
    pixmap: &mut Pixmap,
    config: &Config,
    notifications: &[Notification],
    renderer: &mut Renderer,
) {
    let width = pixmap.width() as f32;
    let height = pixmap.height() as f32;
    let border = config.border.width as f32;

    for card in layout(config, notifications, renderer, (pixmap.width(), pixmap.height())) {
        draw_card(pixmap, config, &notifications[card.index], &card, renderer);
    }

    // Drawn last so it is never separated from the content above it.
    render::fill_rect(
        pixmap,
        0.0,
        height - border,
        width,
        border,
        config.border.color,
    );
}

fn draw_card(
    pixmap: &mut Pixmap,
    config: &Config,
    notification: &Notification,
    card: &Card,
    renderer: &mut Renderer,
) {
    let Card { x, y, width, height, .. } = *card;
    render::fill_round_rect(
        pixmap,
        x,
        y,
        width,
        height,
        CORNER_RADIUS,
        config.notifications.background,
    );

    let accent = if notification.is_urgent() {
        config.notifications.urgent_color
    } else {
        config.notifications.text_color
    };
    render::fill_round_rect(
        pixmap,
        x,
        y + ACCENT_INSET,
        ACCENT_WIDTH,
        (height - 2.0 * ACCENT_INSET).max(0.0),
        ACCENT_WIDTH / 2.0,
        accent,
    );

    let size = config.bar.font_size;
    let text_width = (width - 2.0 * CARD_PADDING).max(0.0);

    let summary = renderer.shorten(&notification.summary, text_width, size);
    renderer.draw_text(
        pixmap,
        &summary,
        x + CARD_PADDING,
        y + CARD_PADDING_V,
        size,
        config.notifications.text_color,
    );

    if notification.body.is_empty() {
        return;
    }
    let body_size = size - BODY_SIZE_STEP;
    let body = renderer.shorten(&notification.body, text_width, body_size);
    renderer.draw_text(
        pixmap,
        &body,
        x + CARD_PADDING,
        y + CARD_PADDING_V + renderer.line_height(size),
        body_size,
        Rgba {
            a: BODY_ALPHA,
            ..config.notifications.text_color
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use stack::Urgency;

    pub(super) const WIDTH: u32 = 600;

    pub(super) fn item(summary: &str, body: &str, urgency: Urgency) -> Notification {
        Notification {
            id: 1,
            summary: summary.to_string(),
            body: body.to_string(),
            urgency,
            expires_at: None,
        }
    }

    pub(super) fn canvas(height: u32) -> Pixmap {
        Pixmap::new(WIDTH, height).unwrap()
    }

    fn pixel(pixmap: &Pixmap, x: u32, y: u32) -> (u8, u8, u8, u8) {
        let px = pixmap.pixels()[(y * pixmap.width() + x) as usize];
        (px.red(), px.green(), px.blue(), px.alpha())
    }

    fn row(pixmap: &Pixmap, y: u32) -> Vec<u8> {
        let stride = pixmap.width() as usize * 4;
        pixmap.data()[y as usize * stride..(y as usize + 1) * stride].to_vec()
    }

    fn card_width() -> f32 {
        CARD_WIDTH.min(WIDTH as f32 - 2.0 * STACK_MARGIN)
    }

    fn card_left() -> f32 {
        WIDTH as f32 - STACK_MARGIN - card_width()
    }

    /// Top row of a single card sitting flush on the border.
    fn card_top(config: &Config, height: u32, card: f32) -> f32 {
        height as f32 - config.border.width as f32 - card
    }

    /// Rows that contain at least one non-transparent pixel.
    fn opaque_rows(pixmap: &Pixmap) -> Vec<bool> {
        (0..pixmap.height())
            .map(|y| (0..pixmap.width()).any(|x| pixel(pixmap, x, y).3 > 0))
            .collect()
    }

    /// Runs of consecutive `true`s, as inclusive row ranges.
    fn runs(flags: &[bool]) -> Vec<(usize, usize)> {
        let mut out: Vec<(usize, usize)> = Vec::new();
        for (index, flag) in flags.iter().enumerate() {
            match (flag, out.last_mut()) {
                (true, Some(last)) if last.1 + 1 == index => last.1 = index,
                (true, _) => out.push((index, index)),
                (false, _) => {}
            }
        }
        out
    }

    /// The card's own background, sampled where no text can reach.
    fn card_background(
        pixmap: &Pixmap,
        config: &Config,
        height: u32,
        card: f32,
    ) -> (u8, u8, u8, u8) {
        let y = card_top(config, height, card) + card / 2.0;
        pixel(pixmap, (card_left() + card_width()) as u32 - 4, y as u32)
    }

    /// Left and right edges of the band text is drawn in.
    fn text_band() -> (u32, u32) {
        (
            (card_left() + CARD_PADDING) as u32,
            (card_left() + card_width() - CARD_PADDING) as u32,
        )
    }

    /// Pixels inside a band that differ from the card's background.
    fn glyphs_in(
        pixmap: &Pixmap,
        background: (u8, u8, u8, u8),
        x: (u32, u32),
        y: (u32, u32),
    ) -> usize {
        (y.0..y.1)
            .flat_map(|row| (x.0..x.1).map(move |column| (column, row)))
            .filter(|&(column, row)| pixel(pixmap, column, row) != background)
            .count()
    }

    #[test]
    fn the_newest_card_sits_flush_on_the_border() {
        let config = Config::default();
        let mut renderer = Renderer::new(None);
        let card = item("Battery low", "12% remaining", Urgency::Normal);
        let card_size = card_height(&config, &card, &renderer);
        let height = config.border.width + card_size as u32 + 40;
        let mut canvas = canvas(height);

        paint(&mut canvas, &config, &[card], &mut renderer);

        let border = config.border.color;
        for y in (height - config.border.width)..height {
            assert_eq!(
                pixel(&canvas, WIDTH / 2, y),
                (border.r, border.g, border.b, border.a),
                "row {y} should be border"
            );
        }

        let above_border = height - config.border.width - 1;
        let middle = (card_left() + card_width() / 2.0) as u32;
        assert!(
            pixel(&canvas, middle, above_border).3 > 0,
            "the row above the border must be card, not a transparent gap"
        );
    }

    #[test]
    fn a_gap_separates_stacked_cards() {
        let config = Config::default();
        let mut renderer = Renderer::new(None);
        let older = item("Build finished", "42s", Urgency::Normal);
        let newer = item("Battery low", "12%", Urgency::Normal);
        let card = card_height(&config, &newer, &renderer);
        let height = config.border.width + (2.0 * card + CARD_GAP + 40.0) as u32;
        let mut canvas = canvas(height);

        paint(&mut canvas, &config, &[older, newer], &mut renderer);

        let bands = runs(&opaque_rows(&canvas));
        assert_eq!(
            bands.len(),
            2,
            "expected the older card, then the newest merged with the border: {bands:?}"
        );

        // The newest card is flush, so its band also covers the border.
        let expected = card + config.border.width as f32;
        let measured = (bands[1].1 - bands[1].0 + 1) as f32;
        assert!(
            (measured - expected).abs() <= 1.0,
            "newest card plus border should be {expected} rows, measured {measured}"
        );
        assert_eq!(
            bands[1].1 + 1,
            canvas.height() as usize,
            "the band should reach the bottom"
        );
    }

    #[test]
    fn the_newest_notification_does_not_move_when_an_older_one_arrives() {
        let config = Config::default();
        let mut renderer = Renderer::new(None);
        let older = item("Build finished", "42s", Urgency::Normal);
        let newer = item("Battery low", "12%", Urgency::Normal);
        let height = config.border.width + 160;
        let first = card_top(
            &config,
            height,
            card_height(&config, &newer, &renderer),
        ) as u32;

        let mut alone = canvas(height);
        paint(&mut alone, &config, std::slice::from_ref(&newer), &mut renderer);
        let mut together = canvas(height);
        paint(&mut together, &config, &[older, newer], &mut renderer);

        for y in first..height {
            assert_eq!(row(&alone, y), row(&together, y), "row {y} moved");
        }
    }

    #[test]
    fn a_card_paints_its_summary_and_body() {
        let config = Config::default();
        let mut renderer = Renderer::new(None);
        let with_body = item("Battery low", "12% remaining. Plug in soon.", Urgency::Normal);
        let without_body = item("Battery low", "", Urgency::Normal);
        let height = config.border.width + 160;

        let full_card = card_height(&config, &with_body, &renderer);
        let brief_card = card_height(&config, &without_body, &renderer);
        assert!(full_card > brief_card, "a body line needs room");

        let mut canvas = canvas(height);
        paint(&mut canvas, &config, &[with_body], &mut renderer);

        let size = config.bar.font_size;
        let line = renderer.line_height(size);
        let top = card_top(&config, height, full_card);
        let background = card_background(&canvas, &config, height, full_card);
        let band = text_band();

        let summary_top = top + CARD_PADDING_V;
        let body_top = summary_top + line;
        let summary = glyphs_in(
            &canvas,
            background,
            band,
            (summary_top as u32, body_top as u32),
        );
        let body = glyphs_in(
            &canvas,
            background,
            band,
            (body_top as u32, (top + full_card) as u32),
        );

        assert!(summary > 20, "the summary should be drawn, found {summary}");
        assert!(body > 20, "the body should be drawn, found {body}");
    }

    #[test]
    fn an_urgent_card_uses_the_urgent_colour_for_its_accent() {
        let config = Config::default();
        let mut renderer = Renderer::new(None);
        let urgent = item("Disk full", "", Urgency::Urgent);
        let card = card_height(&config, &urgent, &renderer);
        let height = config.border.width + card as u32 + 40;
        let mut canvas = canvas(height);
        paint(&mut canvas, &config, &[urgent], &mut renderer);

        let top = card_top(&config, height, card);
        let sample = pixel(
            &canvas,
            (card_left() + ACCENT_WIDTH / 2.0) as u32,
            (top + card / 2.0) as u32,
        );
        let wanted = config.notifications.urgent_color;
        for (got, want, channel) in [
            (sample.0, wanted.r, "red"),
            (sample.1, wanted.g, "green"),
            (sample.2, wanted.b, "blue"),
            (sample.3, wanted.a, "alpha"),
        ] {
            assert!(
                (i32::from(got) - i32::from(want)).abs() <= 6,
                "{channel} was {got}, wanted about {want}"
            );
        }
    }

    #[test]
    fn a_long_body_stays_inside_the_card() {
        let config = Config::default();
        let mut renderer = Renderer::new(None);
        let long = item("Subject", &"w".repeat(400), Urgency::Normal);
        let card = card_height(&config, &long, &renderer);
        let height = config.border.width + card as u32 + 40;
        let mut canvas = canvas(height);
        paint(&mut canvas, &config, &[long], &mut renderer);

        let right = (card_left() + card_width()).ceil() as u32;
        let top = card_top(&config, height, card) as u32;
        // Only the rows the card occupies: the border below it spans the full width.
        for y in top..(height - config.border.width) {
            for x in right..WIDTH {
                assert_eq!(
                    pixel(&canvas, x, y).3,
                    0,
                    "something was painted at ({x}, {y}), past the card's right edge"
                );
            }
        }
    }

    #[test]
    fn an_empty_stack_paints_only_the_border() {
        let config = Config::default();
        let mut renderer = Renderer::new(None);
        let height = config.border.width + 100;
        let mut canvas = canvas(height);
        paint(&mut canvas, &config, &[], &mut renderer);

        for y in 0..(height - config.border.width) {
            for x in 0..WIDTH {
                assert_eq!(pixel(&canvas, x, y).3, 0, "row {y} should be untouched");
            }
        }
    }

    #[test]
    fn stack_height_adds_up_the_cards_and_their_gaps() {
        let config = Config::default();
        let renderer = Renderer::new(None);
        let card = item("Subject", "Body", Urgency::Normal);
        let one = card_height(&config, &card, &renderer);

        assert_eq!(stack_height(&config, &[], &renderer), 0.0);
        assert!((stack_height(&config, std::slice::from_ref(&card), &renderer) - one).abs() < 0.01);
        assert!(
            (stack_height(&config, &[card.clone(), card], &renderer) - (2.0 * one + CARD_GAP)).abs()
                < 0.01
        );
    }
}

#[cfg(test)]
mod hit_tests {
    use super::tests::{canvas, item};
    use super::*;
    use crate::notify::stack::Urgency;

    fn two(config: &Config, renderer: &Renderer) -> Vec<Card> {
        let mut older = item("older", "body", Urgency::Normal);
        older.id = 7;
        let mut newer = item("newer", "", Urgency::Normal);
        newer.id = 8;
        let notifications = vec![older, newer];
        layout(config, &notifications, renderer, (600, 200))
    }

    #[test]
    fn cards_do_not_overlap_and_the_newest_is_lowest() {
        let config = Config::default();
        let renderer = Renderer::new(None);
        let cards = two(&config, &renderer);

        assert_eq!(cards.len(), 2);
        assert_eq!(cards[1].id, 8, "the newest card is last, nearest the border");
        assert!(
            cards[1].y > cards[0].y,
            "the newest card should be lower on the surface: {:?} vs {:?}",
            cards[0],
            cards[1]
        );
        assert!(
            cards[0].y + cards[0].height <= cards[1].y,
            "cards must not overlap: {:?} vs {:?}",
            cards[0],
            cards[1]
        );
    }

    #[test]
    fn a_click_inside_a_card_finds_it() {
        let config = Config::default();
        let renderer = Renderer::new(None);
        let cards = two(&config, &renderer);
        let card = cards[0];

        let centre = (
            card.x + card.width / 2.0,
            card.y + card.height / 2.0,
        );
        assert_eq!(hit(&cards, centre.0, centre.1), Some(card.id));
    }

    #[test]
    fn a_click_outside_every_card_finds_nothing() {
        let config = Config::default();
        let renderer = Renderer::new(None);
        let cards = two(&config, &renderer);

        assert_eq!(hit(&cards, 4.0, 4.0), None, "left of the stack");
        assert_eq!(hit(&cards, 599.0, 4.0), None, "above the stack");
    }

    #[test]
    fn the_layout_matches_what_gets_painted() {
        // The same layout drives painting, so a card's rectangle must sit on
        // pixels that are actually part of that card.
        let config = Config::default();
        let mut renderer = Renderer::new(None);
        let notifications = vec![item("only", "one", Urgency::Normal)];
        let height = config.border.width + 200;
        let mut canvas = canvas(height);
        paint(&mut canvas, &config, &notifications, &mut renderer);

        let card = layout(&config, &notifications, &renderer, (600, height))[0];
        let inside = canvas.pixels()[((card.y as u32 + 4) * 600 + card.x as u32 + 4) as usize];
        assert!(inside.alpha() > 0, "the card's own rectangle should be painted");
    }
}
