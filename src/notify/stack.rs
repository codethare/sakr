//! The notification queue: replacement, timeouts, limits, and do-not-disturb.
//!
//! Everything here is pure logic over logical time, so the whole queue
//! behaviour is testable without a bus or a compositor. Painting decides how a
//! notification looks; this module only decides which ones exist.

use std::time::{Duration, Instant};

use crate::config::Notifications;

/// The `urgency` hint: 0 low, 1 normal, 2 urgent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Urgency {
    Low,
    #[default]
    Normal,
    Urgent,
}

impl Urgency {
    /// Anything the spec does not define is treated as normal.
    pub fn from_hint(value: u8) -> Self {
        match value {
            0 => Self::Low,
            2 => Self::Urgent,
            _ => Self::Normal,
        }
    }
}

/// Why a notification left the queue. The values are the `NotificationClosed`
/// reason codes from the desktop notification spec.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClosedReason {
    /// Expired, or pushed out by the visible-notification limit.
    Expired = 1,
    /// The user clicked it.
    Dismissed = 2,
    /// A client called `CloseNotification`.
    ClosedByCall = 3,
}

/// A `Notify` call, after the D-Bus layer has unpacked it.
///
/// `app_name`, `app_icon` and `actions` are accepted on the bus but not kept:
/// the minimal daemon neither shows an app name nor offers action buttons.
#[derive(Debug, Clone, Default)]
pub struct Request {
    pub summary: String,
    pub body: String,
    pub urgency: Urgency,
    /// Milliseconds from the client: positive is a delay, `0` means "use the
    /// configured default", negative means "never expire".
    pub expire_timeout: i32,
    /// Replace this notification instead of adding one; `0` means "no".
    pub replaces_id: u32,
}

/// A notification currently in the queue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notification {
    pub id: u32,
    pub summary: String,
    pub body: String,
    pub urgency: Urgency,
    /// `None` when it never expires on its own.
    pub expires_at: Option<Instant>,
}

impl Notification {
    pub fn is_urgent(&self) -> bool {
        self.urgency == Urgency::Urgent
    }
}

/// What a [`NotificationStack::push`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Push {
    /// Returned to the client. Always non-zero.
    pub id: u32,
    /// Notifications that left the queue, with the reason to report for each.
    pub closed: Vec<(u32, ClosedReason)>,
    /// False when do-not-disturb swallowed the notification.
    pub displayed: bool,
}

/// The queue of notifications currently on screen.
#[derive(Debug)]
pub struct NotificationStack {
    notifications: Vec<Notification>,
    next_id: u32,
    dnd: bool,
}

impl Default for NotificationStack {
    fn default() -> Self {
        Self::new()
    }
}

impl NotificationStack {
    pub fn new() -> Self {
        Self {
            notifications: Vec::new(),
            // Ids start at 1: zero means "no notification" in the protocol.
            next_id: 1,
            dnd: false,
        }
    }

    pub fn notifications(&self) -> &[Notification] {
        &self.notifications
    }

    pub fn is_empty(&self) -> bool {
        self.notifications.is_empty()
    }

    pub fn set_dnd(&mut self, enabled: bool) {
        self.dnd = enabled;
    }

    /// Add or replace a notification.
    ///
    /// Under do-not-disturb the notification is dropped: the client still gets a
    /// usable id, but nothing is shown and nothing is reported as closed.
    pub fn push(&mut self, request: Request, config: &Notifications, now: Instant) -> Push {
        let id = self.allocate_id(request.replaces_id);

        if self.dnd {
            return Push {
                id,
                closed: Vec::new(),
                displayed: false,
            };
        }

        let notification = Notification {
            id,
            summary: request.summary,
            body: request.body,
            urgency: request.urgency,
            expires_at: expires_at(request.urgency, request.expire_timeout, config, now),
        };

        if let Some(existing) = self.notifications.iter_mut().find(|item| item.id == id) {
            *existing = notification;
            return Push {
                id,
                closed: Vec::new(),
                displayed: true,
            };
        }

        self.notifications.push(notification);
        let closed = self.evict_overflow(config.max_visible);
        Push {
            id,
            closed,
            displayed: true,
        }
    }

    /// Remove a notification the user clicked on.
    pub fn dismiss(&mut self, id: u32) -> Option<(u32, ClosedReason)> {
        self.remove(id).map(|id| (id, ClosedReason::Dismissed))
    }

    /// Remove a notification a client closed by name.
    ///
    /// Unknown ids are ignored, as the spec requires.
    pub fn close(&mut self, id: u32) -> Option<(u32, ClosedReason)> {
        self.remove(id).map(|id| (id, ClosedReason::ClosedByCall))
    }

    /// Drop everything that has expired by `now`.
    pub fn expire(&mut self, now: Instant) -> Vec<(u32, ClosedReason)> {
        let mut closed = Vec::new();
        let mut kept = Vec::with_capacity(self.notifications.len());
        for notification in self.notifications.drain(..) {
            match notification.expires_at {
                Some(at) if at <= now => closed.push((notification.id, ClosedReason::Expired)),
                _ => kept.push(notification),
            }
        }
        self.notifications = kept;
        closed
    }

    /// When the next notification expires, so the caller can schedule a timer.
    pub fn next_expiry(&self) -> Option<Instant> {
        self.notifications
            .iter()
            .filter_map(|notification| notification.expires_at)
            .min()
    }

    fn allocate_id(&mut self, replaces_id: u32) -> u32 {
        if replaces_id != 0 && self.notifications.iter().any(|item| item.id == replaces_id) {
            return replaces_id;
        }
        let id = self.next_id;
        // Skip zero on wrap-around: it is a valid id only as "none".
        self.next_id = self.next_id.wrapping_add(1);
        if self.next_id == 0 {
            self.next_id = 1;
        }
        id
    }

    fn remove(&mut self, id: u32) -> Option<u32> {
        let index = self.notifications.iter().position(|item| item.id == id)?;
        self.notifications.remove(index);
        Some(id)
    }

    /// Keep at most `max_visible`, dropping the oldest first.
    fn evict_overflow(&mut self, max_visible: usize) -> Vec<(u32, ClosedReason)> {
        let mut closed = Vec::new();
        while self.notifications.len() > max_visible {
            let evicted = self.notifications.remove(0);
            closed.push((evicted.id, ClosedReason::Expired));
        }
        closed
    }
}

/// Work out when a notification should disappear.
///
/// Urgent notifications never expire, whatever the client asked for.
fn expires_at(
    urgency: Urgency,
    expire_timeout: i32,
    config: &Notifications,
    now: Instant,
) -> Option<Instant> {
    if urgency == Urgency::Urgent {
        return None;
    }
    match expire_timeout {
        timeout if timeout < 0 => None,
        0 => Some(now + Duration::from_millis(config.timeout_ms)),
        timeout => Some(now + Duration::from_millis(timeout as u64)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(max_visible: usize, timeout_ms: u64) -> Notifications {
        Notifications {
            max_visible,
            timeout_ms,
            ..Notifications::default()
        }
    }

    fn request(summary: &str) -> Request {
        Request {
            summary: summary.to_string(),
            body: String::new(),
            urgency: Urgency::Normal,
            expire_timeout: 0,
            replaces_id: 0,
        }
    }

    fn summaries(stack: &NotificationStack) -> Vec<&str> {
        stack
            .notifications()
            .iter()
            .map(|item| item.summary.as_str())
            .collect()
    }

    #[test]
    fn ids_are_non_zero_and_never_reused() {
        let now = Instant::now();
        let config = config(9, 1000);
        let mut stack = NotificationStack::new();

        let first = stack.push(request("a"), &config, now);
        let second = stack.push(request("b"), &config, now);

        assert_ne!(first.id, 0);
        assert_ne!(second.id, 0);
        assert_ne!(first.id, second.id);
    }

    #[test]
    fn replacing_keeps_the_id_and_resets_the_timer() {
        let now = Instant::now();
        let config = config(9, 1000);
        let mut stack = NotificationStack::new();

        let original = stack.push(request("first"), &config, now);
        let later = now + Duration::from_millis(800);
        let mut replacement = request("second");
        replacement.replaces_id = original.id;
        let replaced = stack.push(replacement, &config, later);

        assert_eq!(replaced.id, original.id);
        assert_eq!(summaries(&stack), ["second"]);
        assert!(replaced.closed.is_empty(), "a replacement is not a closure");

        // The reset timer runs from the replacement, not the original.
        let at = now + Duration::from_millis(1500);
        assert!(stack.expire(at).is_empty(), "should have been re-armed");
        assert_eq!(
            stack.expire(later + Duration::from_millis(1000)),
            [(original.id, ClosedReason::Expired)]
        );
    }

    #[test]
    fn replacing_an_unknown_id_adds_a_new_notification() {
        let now = Instant::now();
        let config = config(9, 1000);
        let mut stack = NotificationStack::new();

        let mut stale = request("stale");
        stale.replaces_id = 4242;
        let pushed = stack.push(stale, &config, now);

        assert_ne!(pushed.id, 4242);
        assert_eq!(summaries(&stack), ["stale"]);
    }

    #[test]
    fn a_positive_timeout_wins() {
        let now = Instant::now();
        let config = config(9, 10_000);
        let mut stack = NotificationStack::new();

        let mut short = request("short");
        short.expire_timeout = 1000;
        stack.push(short, &config, now);

        assert!(stack.expire(now + Duration::from_millis(900)).is_empty());
        assert_eq!(stack.expire(now + Duration::from_millis(1000)).len(), 1);
    }

    #[test]
    fn a_zero_timeout_uses_the_configured_default() {
        let now = Instant::now();
        let config = config(9, 5000);
        let mut stack = NotificationStack::new();

        stack.push(request("default"), &config, now);

        assert!(stack.expire(now + Duration::from_millis(4999)).is_empty());
        assert_eq!(stack.expire(now + Duration::from_millis(5000)).len(), 1);
    }

    #[test]
    fn a_negative_timeout_never_expires() {
        let now = Instant::now();
        let config = config(9, 1000);
        let mut stack = NotificationStack::new();

        let mut persistent = request("persistent");
        persistent.expire_timeout = -1;
        stack.push(persistent, &config, now);

        assert!(stack.expire(now + Duration::from_secs(86_400)).is_empty());
        assert_eq!(summaries(&stack), ["persistent"]);
    }

    #[test]
    fn urgent_notifications_never_expire() {
        let now = Instant::now();
        let config = config(9, 1000);
        let mut stack = NotificationStack::new();

        let mut urgent = request("urgent");
        urgent.urgency = Urgency::Urgent;
        urgent.expire_timeout = 100;
        stack.push(urgent, &config, now);

        assert!(stack.expire(now + Duration::from_secs(3600)).is_empty());
        assert!(stack.notifications()[0].is_urgent());
    }

    #[test]
    fn urgency_hints_map_to_levels() {
        assert_eq!(Urgency::from_hint(0), Urgency::Low);
        assert_eq!(Urgency::from_hint(1), Urgency::Normal);
        assert_eq!(Urgency::from_hint(2), Urgency::Urgent);
        assert_eq!(Urgency::from_hint(7), Urgency::Normal, "undefined means normal");
    }

    #[test]
    fn exceeding_the_limit_evicts_the_oldest() {
        let now = Instant::now();
        let config = config(3, 10_000);
        let mut stack = NotificationStack::new();

        let first = stack.push(request("1"), &config, now);
        stack.push(request("2"), &config, now);
        stack.push(request("3"), &config, now);
        let fourth = stack.push(request("4"), &config, now);

        assert_eq!(fourth.closed, [(first.id, ClosedReason::Expired)]);
        assert_eq!(summaries(&stack), ["2", "3", "4"]);
    }

    #[test]
    fn a_replacement_does_not_evict_anything() {
        let now = Instant::now();
        let config = config(1, 10_000);
        let mut stack = NotificationStack::new();

        let first = stack.push(request("1"), &config, now);
        let mut replacement = request("1 again");
        replacement.replaces_id = first.id;
        let replaced = stack.push(replacement, &config, now);

        assert!(replaced.closed.is_empty());
        assert_eq!(summaries(&stack), ["1 again"]);
    }

    #[test]
    fn dismissing_reports_the_dismissed_reason() {
        let now = Instant::now();
        let config = config(9, 10_000);
        let mut stack = NotificationStack::new();
        let pushed = stack.push(request("clicked"), &config, now);

        assert_eq!(
            stack.dismiss(pushed.id),
            Some((pushed.id, ClosedReason::Dismissed))
        );
        assert!(stack.is_empty());
    }

    #[test]
    fn closing_by_call_reports_the_closed_reason() {
        let now = Instant::now();
        let config = config(9, 10_000);
        let mut stack = NotificationStack::new();
        let pushed = stack.push(request("closed"), &config, now);

        assert_eq!(
            stack.close(pushed.id),
            Some((pushed.id, ClosedReason::ClosedByCall))
        );
    }

    #[test]
    fn closing_an_unknown_id_is_silently_ignored() {
        let now = Instant::now();
        let config = config(9, 10_000);
        let mut stack = NotificationStack::new();
        stack.push(request("kept"), &config, now);

        assert_eq!(stack.close(9999), None);
        assert_eq!(stack.dismiss(9999), None);
        assert_eq!(summaries(&stack), ["kept"]);
    }

    #[test]
    fn do_not_disturb_drops_new_notifications_without_reporting_them() {
        let now = Instant::now();
        let config = config(9, 1000);
        let mut stack = NotificationStack::new();
        stack.set_dnd(true);

        let pushed = stack.push(request("swallowed"), &config, now);

        assert_ne!(pushed.id, 0, "the client still gets a usable id");
        assert!(!pushed.displayed);
        assert!(pushed.closed.is_empty(), "no NotificationClosed for a dropped one");
        assert!(stack.is_empty());
    }

    #[test]
    fn turning_do_not_disturb_off_restores_delivery() {
        let now = Instant::now();
        let config = config(9, 1000);
        let mut stack = NotificationStack::new();

        stack.set_dnd(true);
        stack.push(request("lost"), &config, now);
        stack.set_dnd(false);
        let pushed = stack.push(request("shown"), &config, now);

        assert!(pushed.displayed);
        assert_eq!(summaries(&stack), ["shown"]);
    }

    #[test]
    fn do_not_disturb_leaves_earlier_notifications_alone() {
        let now = Instant::now();
        let config = config(9, 10_000);
        let mut stack = NotificationStack::new();

        stack.push(request("before"), &config, now);
        stack.set_dnd(true);
        stack.push(request("during"), &config, now);

        assert_eq!(summaries(&stack), ["before"]);
    }

    #[test]
    fn the_next_expiry_is_the_earliest_one() {
        let now = Instant::now();
        let config = config(9, 10_000);
        let mut stack = NotificationStack::new();

        let mut slow = request("slow");
        slow.expire_timeout = 5000;
        let mut quick = request("quick");
        quick.expire_timeout = 1000;
        let mut never = request("never");
        never.expire_timeout = -1;

        stack.push(slow, &config, now);
        assert_eq!(stack.next_expiry(), Some(now + Duration::from_millis(5000)));
        stack.push(quick, &config, now);
        assert_eq!(stack.next_expiry(), Some(now + Duration::from_millis(1000)));
        stack.push(never, &config, now);
        assert_eq!(stack.next_expiry(), Some(now + Duration::from_millis(1000)));
    }

    #[test]
    fn expiring_reports_every_notification_that_aged_out() {
        let now = Instant::now();
        let config = config(9, 10_000);
        let mut stack = NotificationStack::new();

        let mut quick = request("quick");
        quick.expire_timeout = 100;
        let mut slow = request("slow");
        slow.expire_timeout = 1000;
        let quick = stack.push(quick, &config, now).id;
        let slow = stack.push(slow, &config, now).id;

        assert_eq!(
            stack.expire(now + Duration::from_millis(500)),
            [(quick, ClosedReason::Expired)]
        );
        assert_eq!(
            stack.expire(now + Duration::from_millis(1000)),
            [(slow, ClosedReason::Expired)]
        );
        assert!(stack.is_empty());
    }

    #[test]
    fn reason_codes_match_the_protocol() {
        assert_eq!(ClosedReason::Expired as u32, 1);
        assert_eq!(ClosedReason::Dismissed as u32, 2);
        assert_eq!(ClosedReason::ClosedByCall as u32, 3);
    }
}
