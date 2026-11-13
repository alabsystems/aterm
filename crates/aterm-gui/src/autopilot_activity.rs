// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Supervisor-authored continuation status.

use crate::{App, SessionCtx};

/// Latest supervisor decision, scoped to its claim revision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Activity {
    pub(crate) sequence: u64,
    pub(crate) reason: String,
    pub(crate) until_ms: Option<u64>,
    pub(crate) observed_ms: u64,
}

pub(crate) fn report(ctx: &SessionCtx, reason: &str, wait_ms: Option<u64>) {
    let now = aterm_time::unix_millis().unwrap_or(0);
    let mut meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
    let revision = meta.supervisor_revision;
    let sequence = meta
        .continuation_activity
        .as_ref()
        .filter(|(rev, _)| *rev == revision)
        .map_or(1, |(_, old)| old.sequence.saturating_add(1));
    let activity = Activity {
        sequence,
        reason: reason.into(),
        until_ms: wait_ms.map(|ms| now.saturating_add(ms)),
        observed_ms: now,
    };
    let changed = meta
        .continuation_activity
        .as_ref()
        .is_none_or(|(rev, old)| {
            *rev != revision || old.reason != activity.reason || old.until_ms != activity.until_ms
        });
    meta.continuation_activity = Some((revision, activity));
    drop(meta);
    if changed {
        ctx.watchers.ring_watchers();
    }
}

impl App {
    pub(crate) fn continuation_words(&self, session: u64) -> std::borrow::Cow<'static, str> {
        let Some(pooled) = self.pool.get(session) else {
            return "status unavailable".into();
        };
        if let Some(hold) = pooled.ctx.fabric.hold() {
            return if matches!(hold.origin.as_str(), "person" | "local") {
                "paused by you"
            } else {
                "held"
            }
            .into();
        }
        let now_us = crate::metrics::now_us();
        let meta = pooled.ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
        let Some((revision, activity)) = &meta.continuation_activity else {
            return "status unavailable".into();
        };
        if *revision != meta.supervisor_revision || meta.live_supervisor(now_us).is_none() {
            return "status unavailable".into();
        }
        if let Some(until) = activity.until_ms {
            let left = until
                .saturating_sub(aterm_time::unix_millis().unwrap_or(0))
                .div_ceil(1000);
            if left > 0 {
                return format!("resumes in {}:{:02}", left / 60, left % 60).into();
            }
            return "checking continuation".into();
        }
        match activity.reason.as_str() {
            "draft" | "composing" => "composing",
            "prompt" | "dialog" => "dialog open",
            "halted" | "paused" => "paused by you",
            "not-idle" | "working" | "ready" => "working",
            "person" => "waiting after input",
            "exchange" => "waiting for your reply",
            "taskless" => "waiting for a task",
            "autopilot" => "off",
            "memory" => "waiting for memory",
            "reach" => "waiting for connection",
            "retry" => "retrying",
            _ => "waiting",
        }
        .into()
    }
}
