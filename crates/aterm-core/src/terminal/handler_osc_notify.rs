// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! OSC desktop-notification handlers: OSC 9 (simple), OSC 99 (kitty: title,
//! body, urgency, multi-part) and OSC 777 (rxvt-unicode `notify`).
//!
//! All three are gated by `modes.allow_notifications`, off by default and set
//! from `TerminalConfig::allow_notifications` by
//! [`Terminal::apply_config`](super::Terminal::apply_config): a wired callback
//! alone is never reached.

use super::handler::TerminalHandler;
use aterm_types::osc::{Notification, NotificationUrgency, TaskbarProgress};

/// Maximum in-flight multi-part OSC 99 notifications; while full, a non-final
/// chunk of a new id is dropped.
const MAX_PENDING_NOTIFICATIONS: usize = 64;

/// Pending-map key shared by every OSC 99 chunk without an `i=` id.
const ANON_NOTIFICATION_KEY: &str = "\u{0}anon";

/// Maximum bytes of a notification title or body; longer text is cut at a UTF-8
/// boundary.
const MAX_NOTIFICATION_FIELD_BYTES: usize = 4096;

impl TerminalHandler<'_> {
    /// Handle OSC 9 (Terminal / ConEmu): `ESC ] 9 ; message ST` goes to the
    /// simple notification callback. ConEmu's `9;4;st;pr` taskbar progress
    /// carries no text: it sets
    /// [`Terminal::taskbar_progress`](super::Terminal::taskbar_progress) ungated
    /// and never reaches the callback.
    pub(super) fn handle_osc_9(&mut self, params: &[&[u8]]) {
        // The message is params[1..] rejoined, so literal semicolons round-trip.
        let Some(message) = join_params(params, 1) else {
            return;
        };
        // ConEmu taskbar progress: ungated, never a notification.
        if message == "4" || message.starts_with("4;") {
            if let Some(progress) = parse_conemu_taskbar_progress(&message) {
                *self.taskbar_progress = Some(progress);
            }
            return;
        }
        // Asked for, whatever the gate says (ruling S19).
        *self.notify_total = self.notify_total.saturating_add(1);
        if !self.modes.allow_notifications {
            return;
        }
        let message = sanitize_notification(&message);
        if message.is_empty() {
            return;
        }
        if let Some(ref mut callback) = self.notifications.callback {
            callback(&message);
        }
    }

    /// Handle OSC 99, the kitty notification protocol:
    /// `ESC ] 99 ; <metadata> ; <payload> ST`, metadata a `:`-separated list of
    /// - `i=<id>`: groups multi-part chunks (id-less chunks share one default id)
    /// - `p=title|body`: payload kind (default `title`)
    /// - `u=0|1|2`: urgency low, normal (default), critical
    /// - `d=0`: more chunks follow (`d=1` or absent: final)
    ///
    /// Same-kind text accumulates, capped at [`MAX_NOTIFICATION_FIELD_BYTES`],
    /// until the final chunk, then goes to the advanced notification callback.
    pub(super) fn handle_osc_99(&mut self, params: &[&[u8]]) {
        // params[1] = metadata (key=value pairs), params[2..] = payload.
        let metadata = params
            .get(1)
            .and_then(|p| std::str::from_utf8(p).ok())
            .unwrap_or("");
        // A final chunk is one notification asked for, counted before the
        // gate (ruling S19): `d=` absent or not `0`, the last `d=` winning as
        // in the parse below. Only the metadata is read.
        if metadata
            .split(':')
            .rev()
            .find_map(|pair| pair.strip_prefix("d="))
            .is_none_or(|d| d != "0")
        {
            *self.notify_total = self.notify_total.saturating_add(1);
        }
        if !self.modes.allow_notifications {
            return;
        }
        let payload = join_params(params, 2).unwrap_or_default();

        let mut id: Option<String> = None;
        let mut payload_kind = Osc99Payload::Title;
        let mut urgency = NotificationUrgency::Normal;
        let mut done = true; // absent `d` means final.

        for pair in metadata.split(':') {
            let Some((key, value)) = pair.split_once('=') else {
                continue;
            };
            match key {
                "i" => {
                    // Arm-local `if`, not a match guard (a guard would fall
                    // through to the unknown-key default arm when false).
                    #[allow(clippy::collapsible_match)]
                    if !value.is_empty() && value.len() <= 256 {
                        id = Some(value.to_string());
                    }
                }
                "p" => {
                    payload_kind = match value {
                        "body" => Osc99Payload::Body,
                        _ => Osc99Payload::Title,
                    };
                }
                "u" => urgency = NotificationUrgency::from_param(value),
                "d" => done = value != "0",
                _ => {} // Unknown / unsupported key — ignore.
            }
        }

        let text = sanitize_notification(&payload);

        // Every id-less chunk shares one key (kitty treats a missing `i` as one
        // default id), so an id-less `d=0` chunk can be completed and cannot fill
        // the map.
        let key = id
            .clone()
            .unwrap_or_else(|| ANON_NOTIFICATION_KEY.to_string());

        let mut entry = self
            .notifications
            .pending
            .remove(&key)
            .unwrap_or_else(|| Notification {
                id: id.clone(),
                ..Notification::default()
            });
        entry.urgency = urgency;
        match payload_kind {
            Osc99Payload::Title => append_notification_field(&mut entry.title, &text),
            Osc99Payload::Body => append_notification_field(&mut entry.body, &text),
        }

        if done {
            if entry.has_content() {
                if let Some(ref mut callback) = self.notifications.advanced_callback {
                    callback(entry);
                }
            }
        } else if self.notifications.pending.len() < MAX_PENDING_NOTIFICATIONS {
            // More chunks follow. The cap is checked after this key's own entry
            // was removed, so a continuing id always has room.
            self.notifications.pending.insert(key, entry);
        }
        // Otherwise the map is full of other in-flight ids: drop this chunk.
    }

    /// Handle OSC 777 (rxvt-unicode): `ESC ] 777 ; notify ; <title> ; <body> ST`
    /// goes to the advanced notification callback; other sub-commands are
    /// ignored.
    pub(super) fn handle_osc_777(&mut self, params: &[&[u8]]) {
        // params[1] = sub-command. Only "notify" is a notification.
        let subcmd = params.get(1).and_then(|p| std::str::from_utf8(p).ok());
        if subcmd != Some("notify") {
            return;
        }
        // Asked for, whatever the gate says (ruling S19).
        *self.notify_total = self.notify_total.saturating_add(1);
        if !self.modes.allow_notifications {
            return;
        }
        let title = params
            .get(2)
            .and_then(|p| std::str::from_utf8(p).ok())
            .map(sanitize_notification)
            .filter(|s| !s.is_empty());
        // The body may contain literal semicolons; rejoin params[3..].
        let body = join_params(params, 3)
            .map(|s| sanitize_notification(&s))
            .filter(|s| !s.is_empty());

        if title.is_none() && body.is_none() {
            return;
        }
        let notification = Notification {
            id: None,
            title,
            body,
            urgency: NotificationUrgency::Normal,
        };
        if let Some(ref mut callback) = self.notifications.advanced_callback {
            callback(notification);
        }
    }
}

/// OSC 99 payload kind (the `p=` metadata key).
enum Osc99Payload {
    Title,
    Body,
}

/// Parse a ConEmu OSC 9;4 body (`"4"`, `"4;st"` or `"4;st;pr"`, `pr` a 0-100
/// progress, clamped). State `0` is Hidden, `1` Normal, `2` Error, `3`
/// Indeterminate, `4` Paused; `None` for an unknown state, so the caller keeps
/// the current progress.
fn parse_conemu_taskbar_progress(message: &str) -> Option<TaskbarProgress> {
    let mut fields = message.split(';');
    let marker = fields.next(); // "4" sub-command marker
    debug_assert_eq!(marker, Some("4"));
    let state = fields.next().unwrap_or("0");
    let value = fields
        .next()
        .and_then(|s| s.parse::<u32>().ok())
        .map(|v| v.min(100) as u8);
    Some(match state {
        "0" => TaskbarProgress::Hidden,
        "1" => TaskbarProgress::Normal(value.unwrap_or(0)),
        "2" => TaskbarProgress::Error(value.unwrap_or(0)),
        "3" => TaskbarProgress::Indeterminate,
        "4" => TaskbarProgress::Paused(value.unwrap_or(0)),
        _ => return None,
    })
}

/// Append one OSC 99 chunk's sanitized text to a title/body field, keeping the
/// field within [`MAX_NOTIFICATION_FIELD_BYTES`] (cut at a UTF-8 boundary). Empty
/// text leaves the field untouched.
fn append_notification_field(field: &mut Option<String>, text: &str) {
    if text.is_empty() {
        return;
    }
    let value = field.get_or_insert_with(String::new);
    let room = MAX_NOTIFICATION_FIELD_BYTES.saturating_sub(value.len());
    value.push_str(&text[..text.floor_char_boundary(room)]);
}

/// Rejoin OSC params from `start..` with `;` (the parser splits a payload's
/// literal semicolons); `None` if there is no param at `start`.
fn join_params(params: &[&[u8]], start: usize) -> Option<String> {
    if params.len() <= start {
        return None;
    }
    Some(String::from_utf8_lossy(&params[start..].join(&b';')).into_owned())
}

/// Strip control characters and BiDi overrides, as
/// [`super::handler_osc::sanitize_title`] does (notification surfaces, like title
/// bars, render outside the grid's BiDi filter), then truncate to
/// [`MAX_NOTIFICATION_FIELD_BYTES`] at a UTF-8 boundary.
fn sanitize_notification(text: &str) -> String {
    let filtered = super::handler_osc::sanitize_title(text);
    if filtered.len() <= MAX_NOTIFICATION_FIELD_BYTES {
        filtered
    } else {
        let end = filtered.floor_char_boundary(MAX_NOTIFICATION_FIELD_BYTES);
        filtered[..end].to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::Terminal;
    use std::sync::{Arc, Mutex};

    #[test]
    fn osc_9_fires_notification_callback_when_authorized() {
        let mut term = Terminal::new(24, 80);
        let captured = Arc::new(Mutex::new(None::<String>));
        let captured_clone = Arc::clone(&captured);
        term.set_notification_callback(move |msg| {
            *captured_clone.lock().expect("poisoned") = Some(msg.to_string());
        });
        term.authorize_notifications();

        term.process(b"\x1b]9;Build finished\x07");

        assert_eq!(
            captured.lock().expect("poisoned").as_deref(),
            Some("Build finished"),
            "OSC 9 must fire the notification callback after authorization"
        );
    }

    #[test]
    fn osc_9_is_noop_when_not_authorized() {
        let mut term = Terminal::new(24, 80);
        let fired = Arc::new(Mutex::new(false));
        let fired_clone = Arc::clone(&fired);
        term.set_notification_callback(move |_| {
            *fired_clone.lock().expect("poisoned") = true;
        });
        // No authorize_notifications() call: default posture is denied.

        term.process(b"\x1b]9;Should not fire\x07");

        assert!(
            !*fired.lock().expect("poisoned"),
            "OSC 9 must not fire the callback without host authorization"
        );
    }

    #[test]
    fn osc_9_strips_bidi_override() {
        let mut term = Terminal::new(24, 80);
        let captured = Arc::new(Mutex::new(None::<String>));
        let captured_clone = Arc::clone(&captured);
        term.set_notification_callback(move |msg| {
            *captured_clone.lock().expect("poisoned") = Some(msg.to_string());
        });
        term.authorize_notifications();

        term.process("\x1b]9;safe\u{202E}evil\x07".as_bytes());

        assert_eq!(
            captured.lock().expect("poisoned").as_deref(),
            Some("safeevil"),
            "U+202E must be stripped from OSC 9 notification text"
        );
    }

    #[test]
    fn osc_9_ignores_conemu_taskbar_progress() {
        let mut term = Terminal::new(24, 80);
        let fired = Arc::new(Mutex::new(false));
        let fired_clone = Arc::clone(&fired);
        term.set_notification_callback(move |_| {
            *fired_clone.lock().expect("poisoned") = true;
        });
        term.authorize_notifications();

        // ConEmu taskbar progress, not a notification.
        term.process(b"\x1b]9;4;1;50\x07");

        assert!(
            !*fired.lock().expect("poisoned"),
            "OSC 9;4 taskbar progress must not be treated as a notification"
        );
    }

    #[test]
    fn osc_9_4_state_mapping_and_clamp() {
        let mut term = Terminal::new(24, 80);
        term.process(b"\x1b]9;4;2;75\x07");
        assert_eq!(term.taskbar_progress(), Some(TaskbarProgress::Error(75)));
        term.process(b"\x1b]9;4;3\x07");
        assert_eq!(
            term.taskbar_progress(),
            Some(TaskbarProgress::Indeterminate)
        );
        term.process(b"\x1b]9;4;4;10\x07");
        assert_eq!(term.taskbar_progress(), Some(TaskbarProgress::Paused(10)));
        // Value over 100 is clamped.
        term.process(b"\x1b]9;4;1;250\x07");
        assert_eq!(term.taskbar_progress(), Some(TaskbarProgress::Normal(100)));
        // State 0 hides progress.
        term.process(b"\x1b]9;4;0\x07");
        assert_eq!(term.taskbar_progress(), Some(TaskbarProgress::Hidden));
    }

    #[test]
    fn osc_9_4_unknown_state_leaves_progress_unchanged() {
        let mut term = Terminal::new(24, 80);
        term.process(b"\x1b]9;4;1;42\x07");
        assert_eq!(term.taskbar_progress(), Some(TaskbarProgress::Normal(42)));
        // Unknown state 9 must not clobber the existing progress.
        term.process(b"\x1b]9;4;9;1\x07");
        assert_eq!(term.taskbar_progress(), Some(TaskbarProgress::Normal(42)));
    }

    #[test]
    fn osc_99_single_message_fires_advanced_callback() {
        let mut term = Terminal::new(24, 80);
        let captured = Arc::new(Mutex::new(None::<Notification>));
        let captured_clone = Arc::clone(&captured);
        term.set_advanced_notification_callback(move |n| {
            *captured_clone.lock().expect("poisoned") = Some(n);
        });
        term.authorize_notifications();

        term.process(b"\x1b]99;u=2:p=title;Hello\x07");

        let got = captured.lock().expect("poisoned").clone();
        let got = got.expect("OSC 99 must fire the advanced callback");
        assert_eq!(got.title.as_deref(), Some("Hello"));
        assert_eq!(got.urgency, NotificationUrgency::Critical);
    }

    #[test]
    fn osc_99_multipart_accumulates_title_and_body() {
        let mut term = Terminal::new(24, 80);
        let captured = Arc::new(Mutex::new(None::<Notification>));
        let captured_clone = Arc::clone(&captured);
        term.set_advanced_notification_callback(move |n| {
            *captured_clone.lock().expect("poisoned") = Some(n);
        });
        term.authorize_notifications();

        // First chunk (title, more to come), then final chunk (body).
        term.process(b"\x1b]99;i=42:p=title:d=0;My Title\x07");
        assert!(
            captured.lock().expect("poisoned").is_none(),
            "non-final OSC 99 chunk must not dispatch yet"
        );
        term.process(b"\x1b]99;i=42:p=body:d=1;My Body\x07");

        let got = captured
            .lock()
            .expect("poisoned")
            .clone()
            .expect("dispatched");
        assert_eq!(got.id.as_deref(), Some("42"));
        assert_eq!(got.title.as_deref(), Some("My Title"));
        assert_eq!(got.body.as_deref(), Some("My Body"));
    }

    fn capture_all(term: &mut Terminal) -> Arc<Mutex<Vec<Notification>>> {
        let captured = Arc::new(Mutex::new(Vec::<Notification>::new()));
        let captured_clone = Arc::clone(&captured);
        term.set_advanced_notification_callback(move |n| {
            captured_clone.lock().expect("poisoned").push(n);
        });
        term.authorize_notifications();
        captured
    }

    /// Id-less `d=0` chunks share one default id: a flood stays one pending
    /// entry, the next id-less final chunk completes it, and a fresh id is never
    /// refused for capacity.
    #[test]
    fn osc_99_anonymous_nonfinal_chunks_do_not_starve_final_notifications() {
        let mut term = Terminal::new(24, 80);
        let captured = capture_all(&mut term);
        let last_title = |c: &Arc<Mutex<Vec<Notification>>>| {
            c.lock()
                .expect("poisoned")
                .last()
                .and_then(|n| n.title.clone())
        };

        for _ in 0..200 {
            term.process(b"\x1b]99;d=0;x\x07");
        }
        assert!(term.notifications.pending.len() <= 1);
        term.process(b"\x1b]99;i=build;Build finished\x07");
        assert_eq!(
            last_title(&captured).as_deref(),
            Some("Build finished"),
            "id-less non-final chunks must not fill the pending map"
        );
        term.process(b"\x1b]99;;tail\x07");
        assert!(
            last_title(&captured).is_some_and(|t| t.ends_with("tail")),
            "an id-less final chunk completes the id-less notification"
        );
        assert!(term.notifications.pending.is_empty());

        // A full map of distinct in-flight ids must not reject a final chunk.
        for i in 0..MAX_PENDING_NOTIFICATIONS {
            term.process(format!("\x1b]99;i={i}:d=0;t\x07").as_bytes());
        }
        term.process(b"\x1b]99;i=other;Done\x07");
        assert_eq!(
            last_title(&captured).as_deref(),
            Some("Done"),
            "a final chunk must never be rejected by the pending-map cap"
        );
    }

    /// Same-kind chunks are concatenated, not replaced.
    #[test]
    fn osc_99_same_kind_chunks_concatenate() {
        let mut term = Terminal::new(24, 80);
        let captured = capture_all(&mut term);

        term.process(b"\x1b]99;i=1:d=0;Hello \x07");
        term.process(b"\x1b]99;i=1;world\x07");
        term.process(b"\x1b]99;i=2:p=body:d=0;a\x07");
        term.process(b"\x1b]99;i=2:p=body;b\x07");

        let got = captured.lock().expect("poisoned").clone();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].title.as_deref(), Some("Hello world"));
        assert_eq!(got[1].body.as_deref(), Some("ab"));
    }

    /// Concatenation stays bounded by the per-field cap, cut at a char boundary.
    #[test]
    fn osc_99_concatenated_field_is_capped() {
        let mut term = Terminal::new(24, 80);
        let captured = capture_all(&mut term);

        let chunk = "\u{00e9}".repeat(1500); // 3000 bytes
        for _ in 0..3 {
            term.process(format!("\x1b]99;i=big:d=0;{chunk}\x07").as_bytes());
        }
        term.process(b"\x1b]99;i=big;end\x07");

        let got = captured.lock().expect("poisoned").clone();
        let title = got[0].title.as_deref().expect("title");
        assert!(title.len() <= MAX_NOTIFICATION_FIELD_BYTES);
        assert_eq!(title.len(), MAX_NOTIFICATION_FIELD_BYTES);
    }

    #[test]
    fn osc_777_notify_fires_advanced_callback() {
        let mut term = Terminal::new(24, 80);
        let captured = Arc::new(Mutex::new(None::<Notification>));
        let captured_clone = Arc::clone(&captured);
        term.set_advanced_notification_callback(move |n| {
            *captured_clone.lock().expect("poisoned") = Some(n);
        });
        term.authorize_notifications();

        term.process(b"\x1b]777;notify;Title Here;Body Here\x07");

        let got = captured
            .lock()
            .expect("poisoned")
            .clone()
            .expect("dispatched");
        assert_eq!(got.title.as_deref(), Some("Title Here"));
        assert_eq!(got.body.as_deref(), Some("Body Here"));
    }

    #[test]
    fn osc_777_non_notify_subcmd_ignored() {
        let mut term = Terminal::new(24, 80);
        let fired = Arc::new(Mutex::new(false));
        let fired_clone = Arc::clone(&fired);
        term.set_advanced_notification_callback(move |_| {
            *fired_clone.lock().expect("poisoned") = true;
        });
        term.authorize_notifications();

        term.process(b"\x1b]777;something;Title;Body\x07");

        assert!(
            !*fired.lock().expect("poisoned"),
            "non-notify OSC 777 sub-command must be ignored"
        );
    }

    /// THE NOTIFY COUNT (session log Phase 3B, ruling S19): every desktop
    /// notification a program asks for is counted, with notifications off
    /// and on alike — OSC 9 past its `9;4` branch, OSC 99 at its final
    /// chunk, OSC 777 `notify`. NEGATIVE CONTROLS: a `9;4` progress, a
    /// non-final 99 chunk and a 777 sub-command other than `notify` count
    /// nothing; the gate still decides what reaches a callback.
    #[test]
    fn every_notification_asked_for_is_counted_whatever_the_gate() {
        for allowed in [false, true] {
            let mut term = Terminal::new(24, 80);
            let fired = Arc::new(Mutex::new(0_u32));
            let (a, b) = (Arc::clone(&fired), Arc::clone(&fired));
            term.set_notification_callback(move |_| *a.lock().expect("poisoned") += 1);
            term.set_advanced_notification_callback(move |_| *b.lock().expect("poisoned") += 1);
            if allowed {
                term.authorize_notifications();
            }
            let count = |term: &mut Terminal, bytes: &[u8]| {
                let before = term.notify_total();
                term.process(bytes);
                term.notify_total() - before
            };
            assert_eq!(count(&mut term, b"\x1b]9;Build finished\x07"), 1);
            assert_eq!(count(&mut term, b"\x1b]9;4;1;50\x07"), 0, "progress");
            assert_eq!(count(&mut term, b"\x1b]99;i=1:d=0;Part one\x07"), 0);
            assert_eq!(count(&mut term, b"\x1b]99;i=1:d=0:p=body;Part two\x07"), 0);
            assert_eq!(count(&mut term, b"\x1b]99;i=1:p=body;Last\x07"), 1, "final");
            assert_eq!(count(&mut term, b"\x1b]99;i=2:d=1;Whole\x07"), 1);
            assert_eq!(count(&mut term, b"\x1b]777;notify;Title;Body\x07"), 1);
            assert_eq!(count(&mut term, b"\x1b]777;preexec;x\x07"), 0, "not notify");
            assert_eq!(term.notify_total(), 4);
            assert_eq!(
                *fired.lock().expect("poisoned"),
                if allowed { 4 } else { 0 },
                "the gate decides delivery, not the count"
            );
        }
    }
}
