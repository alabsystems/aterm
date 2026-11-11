// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! OSC desktop-notification handlers (OSC 9, OSC 99, and OSC 777).

use super::handler::TerminalHandler;
use super::notify_auth::NotificationPace;
use super::policy_gates::PolicyState;
use super::response_rate_limiter::BatchClock;
use aterm_types::osc::{Notification, NotificationUrgency, TaskbarProgress};

const MAX_PENDING_NOTIFICATIONS: usize = 64;

const ANON_NOTIFICATION_KEY: &str = "\u{0}anon";

const MAX_NOTIFICATION_FIELD_BYTES: usize = 4096;

impl TerminalHandler<'_> {
    /// Handles OSC 9 simple notifications and ConEmu `9;4;st;pr` taskbar progress.
    pub(super) fn handle_osc_9(&mut self, params: &[&[u8]]) {
        let Some(message) = join_params(params, 1) else {
            return;
        };
        if message == "4" || message.starts_with("4;") {
            if let Some(progress) = parse_conemu_taskbar_progress(&message) {
                *self.taskbar_progress = Some(progress);
            }
            return;
        }
        *self.notify_total = self.notify_total.saturating_add(1);
        if !self.modes.allow_notifications {
            return;
        }
        let message = sanitize_notification(&message);
        if message.is_empty() {
            return;
        }
        let pace = notification_pace(self.policy, self.transient.process_now);
        let sinks = &mut self.notifications.sinks;
        if let Some(cap) = sinks.try_mint(self.modes.allow_notifications, pace) {
            sinks.notify(cap, &message);
        }
    }

    /// Handles OSC 99 kitty notifications, accumulating multi-part chunks by id.
    pub(super) fn handle_osc_99(&mut self, params: &[&[u8]]) {
        let metadata = params
            .get(1)
            .and_then(|p| std::str::from_utf8(p).ok())
            .unwrap_or("");
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
        let mut done = true;

        for pair in metadata.split(':') {
            let Some((key, value)) = pair.split_once('=') else {
                continue;
            };
            match key {
                "i" => {
                    id = (!value.is_empty() && value.len() <= 256).then(|| value.to_string());
                }
                "p" => {
                    payload_kind = match value {
                        "body" => Osc99Payload::Body,
                        _ => Osc99Payload::Title,
                    };
                }
                "u" => urgency = NotificationUrgency::from_param(value),
                "d" => done = value != "0",
                _ => {}
            }
        }

        let text = sanitize_notification(&payload);

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
                let pace = notification_pace(self.policy, self.transient.process_now);
                let sinks = &mut self.notifications.sinks;
                let allowed = self.modes.allow_notifications;
                if let Some(cap) = sinks.try_mint_advanced(allowed, pace) {
                    sinks.notify_advanced(cap, entry);
                }
            }
        } else if self.notifications.pending.len() < MAX_PENDING_NOTIFICATIONS {
            self.notifications.pending.insert(key, entry);
        }
    }

    /// Handles OSC 777 rxvt-unicode `notify` sequences.
    pub(super) fn handle_osc_777(&mut self, params: &[&[u8]]) {
        let subcmd = params.get(1).and_then(|p| std::str::from_utf8(p).ok());
        if subcmd != Some("notify") {
            return;
        }
        *self.notify_total = self.notify_total.saturating_add(1);
        if !self.modes.allow_notifications {
            return;
        }
        let title = params
            .get(2)
            .and_then(|p| std::str::from_utf8(p).ok())
            .map(sanitize_notification)
            .filter(|s| !s.is_empty());
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
        let pace = notification_pace(self.policy, self.transient.process_now);
        let sinks = &mut self.notifications.sinks;
        if let Some(cap) = sinks.try_mint_advanced(self.modes.allow_notifications, pace) {
            sinks.notify_advanced(cap, notification);
        }
    }
}

/// The pace a program notification written at the batch's clock `now` is debited against.
#[inline]
const fn notification_pace(
    policy: &mut PolicyState,
    now: aterm_time::Instant,
) -> NotificationPace<'_> {
    NotificationPace {
        policy,
        clock: BatchClock(now),
    }
}

enum Osc99Payload {
    Title,
    Body,
}

fn parse_conemu_taskbar_progress(message: &str) -> Option<TaskbarProgress> {
    let mut fields = message.split(';');
    let marker = fields.next();
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

#[inline]
fn append_notification_field(field: &mut Option<String>, text: &str) {
    if text.is_empty() {
        return;
    }
    let value = field.get_or_insert_with(String::new);
    let room = MAX_NOTIFICATION_FIELD_BYTES.saturating_sub(value.len());
    value.push_str(&text[..text.floor_char_boundary(room)]);
}

#[inline]
fn join_params(params: &[&[u8]], start: usize) -> Option<String> {
    if params.len() <= start {
        return None;
    }
    Some(String::from_utf8_lossy(&params[start..].join(&b';')).into_owned())
}

#[inline]
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
#[path = "handler_osc_notify_tests.rs"]
mod tests;
