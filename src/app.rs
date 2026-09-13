//! Application state and the event handling that drives it.
//!
//! Time is passed in rather than read, so every transition — hover delays, the
//! slide animation, notification expiry — is testable without sleeping.

use std::time::{Duration, Instant};

use crate::bar::ModuleValue;
use crate::config::Config;
use crate::ipc::Command;
use crate::notify::stack::{ClosedReason, Notification, NotificationStack, Push, Request};

/// How long the status bar takes to slide fully in or out.
const SLIDE: Duration = Duration::from_millis(150);

/// Which end of the slide the bar is heading for, and who asked for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BarTarget {
    /// Collapsed, showing only the border.
    Hidden,
    /// Expanded because the pointer is on the top edge; leaving collapses it.
    Hover,
    /// Expanded by a command; the pointer must not collapse it.
    Command,
}

impl BarTarget {
    fn progress(self) -> f32 {
        match self {
            Self::Hidden => 0.0,
            Self::Hover | Self::Command => 1.0,
        }
    }
}

/// A slide in progress.
#[derive(Debug, Clone, Copy)]
struct Animation {
    from: f32,
    to: f32,
    started: Instant,
}

/// A transition that has been asked for but not applied yet.
#[derive(Debug, Clone, Copy)]
struct Pending {
    at: Instant,
    target: BarTarget,
}

#[derive(Debug)]
struct Bar {
    target: BarTarget,
    /// How far out the bar is, updated by [`App::advance`].
    progress: f32,
    animation: Option<Animation>,
    pending: Option<Pending>,
}

impl Bar {
    fn new() -> Self {
        Self {
            target: BarTarget::Hidden,
            progress: 0.0,
            animation: None,
            pending: None,
        }
    }

    /// Head for `target` now, dropping anything that was queued.
    fn go_to(&mut self, target: BarTarget, now: Instant) {
        self.pending = None;
        self.target = target;
        self.animation = Some(Animation {
            from: self.progress,
            to: target.progress(),
            started: now,
        });
    }

    /// Head for `target` later, replacing anything queued earlier.
    fn delay(&mut self, target: BarTarget, at: Instant) {
        self.pending = Some(Pending { at, target });
    }

    fn is_queued(&self, target: BarTarget) -> bool {
        self.pending.is_some_and(|pending| pending.target == target)
    }

    /// Move on to `now`. Returns true when the slide's extent changed.
    fn step(&mut self, now: Instant) -> bool {
        let before = self.progress;

        if self.pending.is_some_and(|pending| pending.at <= now) {
            let Pending { at, target } = self.pending.take().unwrap();
            // The slide started when the delay ran out, not when we noticed.
            self.animation = Some(Animation {
                from: self.progress,
                to: target.progress(),
                started: at,
            });
            self.target = target;
        }

        if let Some(animation) = self.animation {
            let elapsed = now.saturating_duration_since(animation.started);
            let fraction = (elapsed.as_secs_f32() / SLIDE.as_secs_f32()).clamp(0.0, 1.0);
            self.progress = animation.from + (animation.to - animation.from) * fraction;
            if fraction >= 1.0 {
                self.progress = animation.to;
                self.animation = None;
            }
        }

        self.progress != before
    }
}

/// Something that happened, to be handled by [`App::handle`].
#[derive(Debug, Clone)]
pub enum Event {
    /// The pointer entered the top edge's input region.
    PointerEnter,
    /// The pointer left the top edge's input region.
    PointerLeave,
    /// A module produced a new value.
    Module { index: usize, value: ModuleValue },
    /// A client closed a notification by id.
    CloseNotification(u32),
    /// The user clicked a card.
    DismissNotification(u32),
    /// A command arrived over the control channel.
    Command(Command),
}

/// Anything the shell has to do beyond repainting.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Update {
    /// Notifications that left the queue, with the reason to report for each.
    pub closed: Vec<(u32, ClosedReason)>,
    pub reload: bool,
    pub quit: bool,
}

pub struct App {
    config: Config,
    modules: Vec<ModuleValue>,
    notifications: NotificationStack,
    bar: Bar,
    /// Set when anything affecting the pixels changed; cleared by [`App::take_dirty`].
    dirty: bool,
}

impl App {
    pub fn new(config: Config) -> Self {
        let modules = vec![ModuleValue::default(); config.bar.module.len()];
        Self {
            config,
            modules,
            notifications: NotificationStack::new(),
            bar: Bar::new(),
            // A fresh shell has not painted yet.
            dirty: true,
        }
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    pub fn modules(&self) -> &[ModuleValue] {
        &self.modules
    }

    pub fn notifications(&self) -> &[Notification] {
        self.notifications.notifications()
    }

    /// How far the status bar is out: 0.0 collapsed, 1.0 fully shown.
    pub fn bar_progress(&self) -> f32 {
        self.bar.progress
    }

    pub fn take_dirty(&mut self) -> bool {
        std::mem::take(&mut self.dirty)
    }

    /// When the shell should next call [`App::advance`], if ever.
    pub fn next_deadline(&self) -> Option<Instant> {
        let bar = [
            self.bar.pending.map(|pending| pending.at),
            self.bar
                .animation
                .map(|animation| animation.started + SLIDE),
        ];
        bar.into_iter()
            .flatten()
            .chain(self.notifications.next_expiry())
            .min()
    }

    /// Move time-driven state forward: queued transitions, the slide, and
    /// notification expiry.
    pub fn advance(&mut self, now: Instant) -> Update {
        if self.bar.step(now) {
            self.dirty = true;
        }
        Update {
            closed: self.notifications.expire(now),
            ..Update::default()
        }
    }

    /// Show a notification and report the id the client should be told.
    pub fn notify(&mut self, request: Request, now: Instant) -> Push {
        let push = self.notifications.push(request, &self.config.notifications, now);
        if push.displayed {
            self.dirty = true;
        }
        push
    }

    pub fn handle(&mut self, event: Event, now: Instant) -> Update {
        match event {
            Event::PointerEnter => {
                // Coming back cancels a queued collapse.
                if self.bar.is_queued(BarTarget::Hidden) {
                    self.bar.pending = None;
                }
                if self.bar.target == BarTarget::Hidden && self.bar.pending.is_none() {
                    let at = now + Duration::from_millis(self.config.bar.hover_delay_ms);
                    self.bar.delay(BarTarget::Hover, at);
                }
            }
            Event::PointerLeave => {
                // Leaving cancels a queued expansion.
                if self.bar.pending.is_some() && !self.bar.is_queued(BarTarget::Hidden) {
                    self.bar.pending = None;
                }
                if self.bar.target == BarTarget::Hover {
                    let at = now + Duration::from_millis(self.config.bar.hide_delay_ms);
                    self.bar.delay(BarTarget::Hidden, at);
                }
            }
            Event::Module { index, value } => {
                if let Some(slot) = self.modules.get_mut(index)
                    && *slot != value
                {
                    *slot = value;
                    self.dirty = true;
                }
            }
            Event::CloseNotification(id) => {
                if let Some(closed) = self.notifications.close(id) {
                    return Update {
                        closed: vec![closed],
                        ..Update::default()
                    };
                }
            }
            Event::DismissNotification(id) => {
                if self.notifications.dismiss(id).is_some() {
                    self.dirty = true;
                }
            }
            Event::Command(command) => return self.run(command, now),
        }
        Update::default()
    }

    fn run(&mut self, command: Command, now: Instant) -> Update {
        match command {
            Command::ShowBar => self.bar.go_to(BarTarget::Command, now),
            Command::HideBar => self.bar.go_to(BarTarget::Hidden, now),
            Command::ToggleBar => match self.bar.target {
                BarTarget::Hidden => self.bar.go_to(BarTarget::Command, now),
                BarTarget::Hover | BarTarget::Command => self.bar.go_to(BarTarget::Hidden, now),
            },
            Command::Dnd { enabled } => {
                self.notifications.set_dnd(enabled);
                self.dirty = true;
            }
            Command::Reload => {
                return Update {
                    reload: true,
                    ..Update::default()
                };
            }
            Command::Quit => {
                return Update {
                    quit: true,
                    ..Update::default()
                };
            }
        }
        Update::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notify::stack::Urgency;

    fn config() -> Config {
        Config::default()
    }

    /// A time far enough in the future to finish any slide.
    fn settled(now: Instant) -> Instant {
        now + Duration::from_secs(1)
    }

    fn request(summary: &str, timeout_ms: i32) -> Request {
        Request {
            summary: summary.to_string(),
            expire_timeout: timeout_ms,
            ..Request::default()
        }
    }

    #[test]
    fn a_quick_pass_over_the_top_edge_does_not_expand() {
        let now = Instant::now();
        let mut app = App::new(config());

        app.handle(Event::PointerEnter, now);
        let before_delay = now + Duration::from_millis(100);
        app.advance(before_delay);
        assert_eq!(app.bar_progress(), 0.0, "must not expand before the delay");

        app.handle(Event::PointerLeave, before_delay);
        app.advance(settled(now));
        assert_eq!(app.bar_progress(), 0.0, "leaving cancels the expansion");
        assert!(app.next_deadline().is_none(), "nothing should be queued");
    }

    #[test]
    fn resting_on_the_top_edge_expands_after_the_delay() {
        let now = Instant::now();
        let mut app = App::new(config());

        app.handle(Event::PointerEnter, now);
        assert_eq!(
            app.next_deadline(),
            Some(now + Duration::from_millis(config().bar.hover_delay_ms))
        );

        app.advance(settled(now));
        assert_eq!(app.bar_progress(), 1.0);
    }

    #[test]
    fn the_slide_takes_its_configured_time() {
        let now = Instant::now();
        let mut app = App::new(config());
        app.handle(Event::PointerEnter, now);
        let shown_at = now + Duration::from_millis(config().bar.hover_delay_ms);

        app.advance(shown_at + SLIDE / 2);
        let half = app.bar_progress();
        assert!((0.3..0.7).contains(&half), "halfway should be near 0.5, got {half}");

        app.advance(shown_at + SLIDE);
        assert_eq!(app.bar_progress(), 1.0);
    }

    #[test]
    fn leaving_a_hover_expansion_collapses_after_the_delay() {
        let now = Instant::now();
        let mut app = App::new(config());
        app.handle(Event::PointerEnter, now);
        app.advance(settled(now));
        assert_eq!(app.bar_progress(), 1.0);

        let left_at = settled(now);
        app.handle(Event::PointerLeave, left_at);
        app.advance(left_at + Duration::from_millis(config().bar.hide_delay_ms) / 2);
        assert_eq!(app.bar_progress(), 1.0, "still inside the grace period");

        app.advance(left_at + Duration::from_millis(config().bar.hide_delay_ms));
        let collapsed_by = left_at + Duration::from_millis(config().bar.hide_delay_ms) + SLIDE;
        app.advance(collapsed_by);
        assert_eq!(app.bar_progress(), 0.0);
    }

    #[test]
    fn returning_cancels_the_pending_collapse() {
        let now = Instant::now();
        let mut app = App::new(config());
        app.handle(Event::PointerEnter, now);
        app.advance(settled(now));

        let left_at = settled(now);
        app.handle(Event::PointerLeave, left_at);
        app.handle(Event::PointerEnter, left_at + Duration::from_millis(50));

        app.advance(left_at + Duration::from_secs(5));
        assert_eq!(app.bar_progress(), 1.0, "the collapse should have been cancelled");
    }

    #[test]
    fn a_command_shows_the_bar_without_the_pointer() {
        let now = Instant::now();
        let mut app = App::new(config());

        app.handle(Event::Command(Command::ShowBar), now);
        app.advance(settled(now));
        assert_eq!(app.bar_progress(), 1.0);
    }

    #[test]
    fn the_pointer_cannot_collapse_a_command_expansion() {
        let now = Instant::now();
        let mut app = App::new(config());
        app.handle(Event::Command(Command::ShowBar), now);

        let left_at = settled(now);
        app.handle(Event::PointerLeave, left_at);
        app.advance(left_at + Duration::from_secs(30));

        assert_eq!(app.bar_progress(), 1.0, "a command expansion ignores the pointer");
    }

    #[test]
    fn hide_bar_collapses_a_hover_expansion() {
        let now = Instant::now();
        let mut app = App::new(config());
        app.handle(Event::PointerEnter, now);
        app.advance(settled(now));

        let hidden_at = settled(now);
        app.handle(Event::Command(Command::HideBar), hidden_at);
        app.advance(hidden_at + SLIDE);

        assert_eq!(app.bar_progress(), 0.0);
        // The pointer is still resting on the edge, so it must not pop back out.
        app.advance(hidden_at + Duration::from_secs(10));
        assert_eq!(app.bar_progress(), 0.0);
    }

    #[test]
    fn toggle_switches_between_shown_and_hidden() {
        let now = Instant::now();
        let mut app = App::new(config());

        app.handle(Event::Command(Command::ToggleBar), now);
        app.advance(settled(now));
        assert_eq!(app.bar_progress(), 1.0, "first toggle shows it");

        let second = settled(now);
        app.handle(Event::Command(Command::ToggleBar), second);
        app.advance(second + SLIDE);
        assert_eq!(app.bar_progress(), 0.0, "second toggle hides it");

        let third = second + SLIDE;
        app.handle(Event::Command(Command::ToggleBar), third);
        app.advance(third + SLIDE);
        assert_eq!(app.bar_progress(), 1.0, "third toggle shows it again");
    }

    #[test]
    fn a_toggle_during_the_slide_reverses_it() {
        let now = Instant::now();
        let mut app = App::new(config());
        app.handle(Event::Command(Command::ShowBar), now);
        app.advance(now + SLIDE / 2);
        let halfway = app.bar_progress();
        assert!(halfway > 0.0 && halfway < 1.0, "should be mid-slide, got {halfway}");

        app.handle(Event::Command(Command::ToggleBar), now + SLIDE / 2);
        app.advance(now + SLIDE * 2);
        assert_eq!(app.bar_progress(), 0.0);
    }

    #[test]
    fn module_updates_merge_into_a_single_repaint() {
        let now = Instant::now();
        let mut config = config();
        config.bar.module = vec![crate::config::Module {
            name: "clock".to_string(),
            exec: "date".to_string(),
            interval: Some(1),
            ..crate::config::Module::default()
        }];
        let mut app = App::new(config);
        assert!(app.take_dirty(), "the first frame always paints");

        let value = ModuleValue {
            text: "14:03".to_string(),
            color: None,
        };
        app.handle(
            Event::Module {
                index: 0,
                value: value.clone(),
            },
            now,
        );
        app.handle(
            Event::Module {
                index: 0,
                value: ModuleValue {
                    text: "14:04".to_string(),
                    ..value.clone()
                },
            },
            now,
        );

        assert!(app.take_dirty(), "the updates should ask for a repaint");
        assert!(!app.take_dirty(), "and only once between frames");
    }

    #[test]
    fn an_unchanged_module_value_does_not_ask_for_a_repaint() {
        let now = Instant::now();
        let mut config = config();
        config.bar.module = vec![crate::config::Module {
            name: "clock".to_string(),
            exec: "date".to_string(),
            interval: Some(1),
            ..crate::config::Module::default()
        }];
        let mut app = App::new(config);
        let value = ModuleValue {
            text: "14:03".to_string(),
            color: None,
        };
        app.handle(
            Event::Module {
                index: 0,
                value: value.clone(),
            },
            now,
        );
        app.take_dirty();

        app.handle(Event::Module { index: 0, value }, now);

        assert!(!app.take_dirty(), "the same value is not a repaint");
    }

    #[test]
    fn a_module_update_for_an_unknown_index_is_ignored() {
        let now = Instant::now();
        let mut app = App::new(config());
        app.take_dirty();

        app.handle(
            Event::Module {
                index: 7,
                value: ModuleValue {
                    text: "x".to_string(),
                    color: None,
                },
            },
            now,
        );

        assert!(!app.take_dirty());
    }

    #[test]
    fn reload_and_quit_are_reported_to_the_caller() {
        let now = Instant::now();
        let mut app = App::new(config());

        assert!(app.handle(Event::Command(Command::Reload), now).reload);
        assert!(app.handle(Event::Command(Command::Quit), now).quit);
    }

    #[test]
    fn do_not_disturb_suppresses_notifications() {
        let now = Instant::now();
        let mut app = App::new(config());
        app.handle(Event::Command(Command::Dnd { enabled: true }), now);

        let push = app.notify(request("quiet", 0), now);

        assert!(!push.displayed);
        assert!(app.notifications().is_empty());
    }

    #[test]
    fn expired_notifications_are_reported_once() {
        let now = Instant::now();
        let mut app = App::new(config());
        let push = app.notify(request("brief", 100), now);
        app.take_dirty();

        let expired = app.advance(now + Duration::from_millis(200));
        assert_eq!(expired.closed, [(push.id, ClosedReason::Expired)]);

        let again = app.advance(now + Duration::from_millis(400));
        assert!(again.closed.is_empty(), "an expired notification is reported once");
    }

    #[test]
    fn the_next_deadline_covers_notification_expiry() {
        let now = Instant::now();
        let mut app = App::new(config());
        app.notify(request("brief", 100), now);

        assert_eq!(app.next_deadline(), Some(now + Duration::from_millis(100)));
    }

    #[test]
    fn dismissing_a_card_removes_it_and_repaints() {
        let now = Instant::now();
        let mut app = App::new(config());
        let push = app.notify(request("click me", 0), now);
        app.take_dirty();

        app.handle(Event::DismissNotification(push.id), now);

        assert!(app.notifications().is_empty());
        assert!(app.take_dirty(), "removing a card changes the pixels");
    }

    #[test]
    fn closing_a_notification_reports_the_reason_to_the_caller() {
        let now = Instant::now();
        let mut app = App::new(config());
        let push = app.notify(request("close me", 0), now);

        let update = app.handle(Event::CloseNotification(push.id), now);

        assert_eq!(update.closed, [(push.id, ClosedReason::ClosedByCall)]);
        assert!(app.notifications().is_empty());
    }

    #[test]
    fn closing_an_unknown_notification_reports_nothing() {
        let now = Instant::now();
        let mut app = App::new(config());

        let update = app.handle(Event::CloseNotification(999), now);

        assert!(update.closed.is_empty());
    }

    #[test]
    fn nothing_is_pending_while_the_bar_is_at_rest() {
        let now = Instant::now();
        let mut app = App::new(config());
        app.handle(Event::Command(Command::ShowBar), now);

        app.advance(now + SLIDE * 2);

        assert_eq!(app.next_deadline(), None);
    }

    #[test]
    fn urgency_is_kept_for_painting() {
        let now = Instant::now();
        let mut app = App::new(config());
        app.notify(
            Request {
                summary: "urgent".to_string(),
                urgency: Urgency::Urgent,
                ..Request::default()
            },
            now,
        );

        assert!(app.notifications()[0].is_urgent());
    }
}
