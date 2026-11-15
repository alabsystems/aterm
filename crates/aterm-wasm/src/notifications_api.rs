// SPDX-License-Identifier: MIT
// Copyright 2026 Andrew Yates

//! OSC 9 / 99 / 777 desktop notifications on [`WebTerminal`](crate::WebTerminal):
//! an authorization passthrough plus a poll drain (JSON out, `None` when empty).

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use aterm_core::terminal::Terminal;
use aterm_types::osc::{Notification, NotificationUrgency};

use crate::write_json_string;

/// Drain handle shared with the engine callbacks, which must be `Send + 'static` (hence `Arc<Mutex<..>>` on single-threaded wasm).
pub(crate) type NotificationQueue = Arc<Mutex<VecDeque<Notification>>>;

/// Cap on undrained notifications, matching the engine's `MAX_PENDING_NOTIFICATIONS`.
pub(crate) const MAX_QUEUED_NOTIFICATIONS: usize = 64;

/// Wire the OSC 9 and OSC 99 / 777 notification callbacks into a fresh bounded queue and return its drain handle.
pub(crate) fn wire_notification_queue(term: &mut Terminal) -> NotificationQueue {
    let queue: NotificationQueue = Arc::new(Mutex::new(VecDeque::new()));
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

fn enqueue(queue: &NotificationQueue, notification: Notification) {
    let mut q = queue.lock().expect("notification queue poisoned");
    if q.len() < MAX_QUEUED_NOTIFICATIONS {
        q.push_back(notification);
    }
}

/// Drain the queue into the `take_notifications` JSON array; `None` when nothing is pending.
pub(crate) fn drain_notifications_json(queue: &NotificationQueue) -> Option<String> {
    let mut q = queue.lock().expect("notification queue poisoned");
    if q.is_empty() {
        return None;
    }
    let mut out = String::from("[");
    for (i, notification) in q.drain(..).enumerate() {
        if i != 0 {
            out.push(',');
        }
        write_notification_json(&mut out, &notification);
    }
    out.push(']');
    Some(out)
}

fn write_nullable_json(out: &mut String, field: Option<&str>) {
    match field {
        Some(text) => write_json_string(out, text),
        None => out.push_str("null"),
    }
}

fn write_notification_json(out: &mut String, n: &Notification) {
    out.push_str(r#"{"id":"#);
    write_nullable_json(out, n.id.as_deref());
    out.push_str(r#","title":"#);
    write_nullable_json(out, n.title.as_deref());
    out.push_str(r#","body":"#);
    write_nullable_json(out, n.body.as_deref());
    out.push_str(r#","urgency":""#);
    out.push_str(urgency_label(n.urgency));
    out.push_str(r#""}"#);
}

#[inline]
const fn urgency_label(urgency: NotificationUrgency) -> &'static str {
    match urgency {
        NotificationUrgency::Low => "low",
        NotificationUrgency::Critical => "critical",
        _ => "normal",
    }
}

macro_rules! bind {
    ($terminal:ident, $export:ident) => {
        #[cfg_attr(target_arch = "wasm32", $export)]
        impl $terminal {
            /// Authorize (`true`) or revoke (`false`) OSC 9 / 99 / 777 notifications; the engine is fail-closed until authorized.
            pub const fn authorize_notifications(&mut self, allowed: bool) {
                self.term.set_allow_notifications(allowed);
            }

            /// Drain pending notifications as a JSON array of `{"id","title","body","urgency"}` objects; `None` when nothing is pending.
            pub fn take_notifications(&mut self) -> Option<String> {
                drain_notifications_json(&self.notifications)
            }
        }
    };
}
web_terminal_impl!(bind);

#[cfg(all(test, not(target_arch = "wasm32")))]
#[path = "notifications_api_tests.rs"]
mod tests;
