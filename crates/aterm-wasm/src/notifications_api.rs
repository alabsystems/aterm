// SPDX-License-Identifier: MIT
// Copyright 2026 Andrew Yates

//! OSC 9 / 99 / 777 desktop notifications on [`WebTerminal`](crate::WebTerminal):
//! an authorization passthrough plus a poll drain (JSON out, `None` when empty).
//!
//! The engine delivers notifications only through its host callbacks, never the OSC
//! event queue, so [`wire_notification_queue`] routes both callbacks into a bounded
//! queue that
//! [`WebTerminal::take_notifications`](crate::WebTerminal::take_notifications)
//! drains. The engine's gate stays fail-closed: the handlers return before any
//! callback runs until the host passes `true` to
//! [`WebTerminal::authorize_notifications`](crate::WebTerminal::authorize_notifications).
//!
//! Shared with `aterm-gpu-web`, which mounts this file by `#[path]`;
//! [`WebTerminal`](crate::WebTerminal) is each crate's own terminal type.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use aterm_core::terminal::Terminal;
use aterm_types::osc::{Notification, NotificationUrgency};

use crate::json_string;

/// Drain handle shared with the engine callbacks, which must be `Send + 'static`
/// (hence `Arc<Mutex<..>>` on single-threaded wasm).
pub(crate) type NotificationQueue = Arc<Mutex<VecDeque<Notification>>>;

/// Cap on undrained notifications, matching the engine's `MAX_PENDING_NOTIFICATIONS`.
/// Overflow refuses the new notification rather than evicting old ones.
pub(crate) const MAX_QUEUED_NOTIFICATIONS: usize = 64;

/// Wire the OSC 9 and OSC 99 / 777 notification callbacks into a fresh bounded
/// queue and return its drain handle. Called once at construction.
pub(crate) fn wire_notification_queue(term: &mut Terminal) -> NotificationQueue {
    let queue: NotificationQueue = Arc::new(Mutex::new(VecDeque::new()));
    // OSC 9 carries a bare message: a body with no title, as the native host maps it.
    let q = Arc::clone(&queue);
    term.set_notification_callback(move |message| {
        enqueue(
            &q,
            Notification {
                id: None,
                title: None,
                body: Some(message.to_string()),
                urgency: NotificationUrgency::Normal,
            },
        );
    });
    let q = Arc::clone(&queue);
    term.set_advanced_notification_callback(move |n| enqueue(&q, n));
    queue
}

/// Append one notification, refusing the new one at the cap.
fn enqueue(queue: &NotificationQueue, notification: Notification) {
    let mut q = queue.lock().expect("notification queue poisoned");
    if q.len() < MAX_QUEUED_NOTIFICATIONS {
        q.push_back(notification);
    }
}

/// Drain the queue into the `take_notifications` JSON array; `None` when
/// nothing is pending.
pub(crate) fn drain_notifications_json(queue: &NotificationQueue) -> Option<String> {
    let mut q = queue.lock().expect("notification queue poisoned");
    if q.is_empty() {
        return None;
    }
    let objects: Vec<String> = q.drain(..).map(|n| notification_json(&n)).collect();
    Some(format!("[{}]", objects.join(",")))
}

/// One notification as `{"id","title","body","urgency"}` (string or `null`; urgency
/// `"low"|"normal"|"critical"`).
fn notification_json(n: &Notification) -> String {
    let field = |v: &Option<String>| v.as_deref().map_or_else(|| "null".to_string(), json_string);
    format!(
        r#"{{"id":{},"title":{},"body":{},"urgency":"{}"}}"#,
        field(&n.id),
        field(&n.title),
        field(&n.body),
        urgency_label(n.urgency),
    )
}

fn urgency_label(urgency: NotificationUrgency) -> &'static str {
    match urgency {
        NotificationUrgency::Low => "low",
        NotificationUrgency::Critical => "critical",
        // Normal, plus any future non_exhaustive variant, reads as normal.
        _ => "normal",
    }
}

/// The JS-visible surface. Each crate expands it onto its own terminal
/// type, under its own export attribute, with `web_terminal_impl!`: the
/// attribute names the JS class after the impl's type, which this shared
/// file cannot spell.
macro_rules! bind {
    ($terminal:ident, $export:ident) => {
        #[cfg_attr(target_arch = "wasm32", $export)]
        impl $terminal {
            /// Authorize (`true`) or revoke (`false`) OSC 9 / 99 / 777 notifications; the
            /// engine is fail-closed until authorized. Already-queued notifications stay
            /// drainable after a revoke.
            pub fn authorize_notifications(&mut self, allowed: bool) {
                self.term.set_allow_notifications(allowed);
            }

            /// Drain pending notifications as a JSON array of
            /// `{"id","title","body","urgency"}` objects; `None` when nothing is pending.
            /// OSC 9's bare message arrives as `body` with no title. The queue is bounded,
            /// so poll after `process`.
            pub fn take_notifications(&mut self) -> Option<String> {
                drain_notifications_json(&self.notifications)
            }
        }
    };
}
web_terminal_impl!(bind);

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::MAX_QUEUED_NOTIFICATIONS;
    use crate::WebTerminal;

    #[test]
    fn authorized_osc9_round_trips_through_the_drain() {
        let Some(mut t) = WebTerminal::new_from_system(24, 80, 16.0) else {
            return; // no system font in this environment
        };
        t.authorize_notifications(true);
        t.process(b"\x1b]9;Build finished\x07");
        assert_eq!(
            t.take_notifications().as_deref(),
            Some(r#"[{"id":null,"title":null,"body":"Build finished","urgency":"normal"}]"#),
            "OSC 9 message must surface as a body-only notification"
        );
        assert!(t.take_notifications().is_none(), "drain empties the queue");
    }

    #[test]
    fn unauthorized_notifications_never_enqueue() {
        let Some(mut t) = WebTerminal::new_from_system(24, 80, 16.0) else {
            return;
        };
        // No authorize call: the engine's fail-closed default must hold.
        t.process(b"\x1b]9;nope\x07");
        t.process(b"\x1b]99;u=2;nope\x07");
        t.process(b"\x1b]777;notify;Nope;Nope\x07");
        assert!(
            t.take_notifications().is_none(),
            "unauthorized notifications must never reach the queue"
        );
        // Revoking after a grant restores the engine's fail-closed gate.
        t.authorize_notifications(true);
        t.authorize_notifications(false);
        t.process(b"\x1b]9;still nope\x07");
        assert!(
            t.take_notifications().is_none(),
            "revoke re-closes the gate"
        );
    }

    #[test]
    fn advanced_forms_carry_id_title_body_urgency() {
        let Some(mut t) = WebTerminal::new_from_system(24, 80, 16.0) else {
            return;
        };
        t.authorize_notifications(true);
        t.process(b"\x1b]99;i=7:u=2:p=title;Alert\x07");
        t.process(b"\x1b]777;notify;Title Here;Body Here\x07");
        assert_eq!(
            t.take_notifications().as_deref(),
            Some(concat!(
                r#"[{"id":"7","title":"Alert","body":null,"urgency":"critical"},"#,
                r#"{"id":null,"title":"Title Here","body":"Body Here","urgency":"normal"}]"#
            )),
            "OSC 99 id/urgency/title and OSC 777 title/body must round-trip"
        );
    }

    #[test]
    fn queue_is_bounded_and_drops_new_when_full() {
        let Some(mut t) = WebTerminal::new_from_system(24, 80, 16.0) else {
            return;
        };
        t.authorize_notifications(true);
        for i in 0..(MAX_QUEUED_NOTIFICATIONS + 8) {
            t.process(format!("\x1b]9;n {i}\x07").as_bytes());
        }
        let json = t.take_notifications().expect("queued notifications");
        assert_eq!(
            json.matches("\"body\":\"n ").count(),
            MAX_QUEUED_NOTIFICATIONS,
            "the cap must hold under a flood"
        );
        assert!(
            json.contains("\"n 0\""),
            "oldest survives (drop-new posture)"
        );
        assert!(
            !json.contains(&format!("\"n {MAX_QUEUED_NOTIFICATIONS}\"")),
            "overflow notifications are refused, not evicting old ones"
        );
        assert!(t.take_notifications().is_none(), "drain empties the queue");
        // The queue accepts again after a drain.
        t.process(b"\x1b]9;after\x07");
        assert!(
            t.take_notifications()
                .expect("post-drain enqueue works")
                .contains("after"),
            "capacity is restored by draining"
        );
    }

    #[test]
    fn cursor_color_follows_osc12_and_resets_on_112() {
        let Some(mut t) = WebTerminal::new_from_system(24, 80, 16.0) else {
            return;
        };
        let theme_cursor = t.theme_cursor;
        assert_eq!(
            t.cursor_color(),
            Some(theme_cursor),
            "the host-configured cursor baseline is live at start"
        );
        t.process(b"\x1b]12;#ff8800\x07");
        assert_eq!(
            t.cursor_color(),
            Some(0x00FF_8800),
            "OSC 12 must surface as a packed 0x00RRGGBB"
        );
        t.process(b"\x1b]112\x07");
        assert_eq!(
            t.cursor_color(),
            Some(theme_cursor),
            "OSC 112 restores the host-configured cursor baseline"
        );
    }
}
