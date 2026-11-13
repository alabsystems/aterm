// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm-gui`'s [`SessionHost`] adapter over a resolved session's handles.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use aterm_control::{
    ChangeWait, HostCapabilities, Selector, SessionEntry, SessionHost, SessionState,
};
use aterm_core::terminal::Terminal;
#[cfg(test)]
use aterm_session::sink::SinkWriter;
use aterm_window::event_loop::EventLoopProxy;

use crate::session_store::Store;
use crate::session_watch::{Subscription, Watchers};
use crate::{SessionCtx, Wake, term_lock, term_lock_whole};

/// Borrowed [`SessionHost`] adapter for a resolved GUI session.
pub(crate) struct GuiHost<'a> {
    /// The one session this host serves. Every per-session method refuses any
    /// other sid, so a sid the fleet-wide `resolve` hands out fails closed.
    sid: u64,
    term: &'a Arc<Mutex<Terminal>>,
    /// `None` when there is no event loop to nudge. Production passes `Some`;
    /// tests run off the main thread, where no `EventLoopProxy` can be built.
    proxy: Option<&'a EventLoopProxy<Wake>>,
    /// The served session's watchers, which [`SessionHost::subscribe`] parks on.
    watchers: &'a Arc<Watchers>,
    /// The process registry, for the fleet answers ([`SessionHost::sessions`],
    /// [`SessionHost::resolve`]).
    store: Option<&'a Store>,
    /// The resolved target's context, whose fabric endpoint says whether the
    /// session has ended.
    ctx: Option<&'a SessionCtx>,
}

impl<'a> GuiHost<'a> {
    /// A session-scoped host: serves `sid` only and declares that it keeps no roster, so its empty `sessions()` is not read as "no sessions exist".
    #[cfg(test)]
    pub(crate) fn new(
        sid: u64,
        term: &'a Arc<Mutex<Terminal>>,
        proxy: Option<&'a EventLoopProxy<Wake>>,
        watchers: &'a Arc<Watchers>,
    ) -> Self {
        Self {
            sid,
            term,
            proxy,
            watchers,
            store: None,
            ctx: None,
        }
    }

    /// Construct a host backed by the process session store and target context.
    pub(crate) const fn with_fleet(
        sid: u64,
        term: &'a Arc<Mutex<Terminal>>,
        proxy: Option<&'a EventLoopProxy<Wake>>,
        store: &'a Store,
        ctx: &'a SessionCtx,
    ) -> Self {
        Self {
            sid,
            term,
            proxy,
            watchers: &ctx.watchers,
            store: Some(store),
            ctx: Some(ctx),
        }
    }

    const fn serves(&self, sid: u64) -> bool {
        sid == self.sid
    }
}

struct SubscriberWait(Subscription);

impl ChangeWait for SubscriberWait {
    fn wait(&self, timeout: Duration) -> bool {
        let now = std::time::Instant::now();
        let until = now
            .checked_add(timeout)
            .unwrap_or_else(|| now + Duration::from_secs(86_400 * 365));
        let woke = crate::park::park(
            |bound| self.0.wait(bound),
            until,
            crate::park::Lane::SERVED,
            &|| false,
        );
        woke == crate::park::Woke::Rang
    }

    fn caller_gone(&self) -> bool {
        crate::control::caller_hung_up()
    }
}

impl SessionHost for GuiHost<'_> {
    fn capabilities(&self) -> HostCapabilities {
        HostCapabilities {
            frame_source: true,
            event_loop: self.proxy.is_some(),
            clipboard: true,
            roster: self.store.is_some(),
        }
    }

    fn sessions(&self) -> Vec<SessionEntry> {
        let Some(store) = self.store else {
            return Vec::new();
        };
        let snapshot = {
            let g = store.read().unwrap_or_else(|p| p.into_inner());
            g.snapshot()
        };
        snapshot
            .into_iter()
            .map(|h| SessionEntry {
                sid: h.local_id,
                id: h.sid.as_str().to_string(),
                parent: h.parent.as_ref().map(|p| p.as_str().to_string()),
                state: match h.state {
                    crate::session_store::SessionState::Spawning => SessionState::Spawning,
                    crate::session_store::SessionState::Alive => SessionState::Alive,
                    crate::session_store::SessionState::Exited => SessionState::Exited,
                },
                title: h.title,
                has_meta: h
                    .ctx
                    .meta
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .any_set(),
            })
            .collect()
    }

    fn resolve(&self, selector: Selector<'_>) -> Option<u64> {
        let g = self.store?.read().unwrap_or_else(|p| p.into_inner());
        g.by_selector(&selector.into()).map(|h| h.local_id)
    }

    fn with_terminal<R>(&self, sid: u64, f: impl FnOnce(&Terminal) -> R) -> Option<R> {
        if !self.serves(sid) {
            return None;
        }
        let t = term_lock_whole(self.term);
        still_wanted(&t).then(|| f(&t))
    }

    fn with_terminal_mut<R>(&self, sid: u64, f: impl FnOnce(&mut Terminal) -> R) -> Option<R> {
        if !self.serves(sid) {
            return None;
        }
        let mut t = term_lock_whole(self.term);
        still_wanted(&t).then(|| f(&mut t))
    }

    fn with_terminal_as_it_stands<R>(&self, sid: u64, f: impl FnOnce(&Terminal) -> R) -> Option<R> {
        self.serves(sid).then(|| f(&term_lock(self.term)))
    }

    fn session_ended(&self, sid: u64) -> bool {
        let Some(ctx) = self.ctx.filter(|_| self.serves(sid)) else {
            return false;
        };
        ctx.fabric.endpoint_closed()
            || self.store.is_some_and(|store| {
                store
                    .read()
                    .unwrap_or_else(|p| p.into_inner())
                    .by_local(sid)
                    .is_some_and(|h| h.state == crate::session_store::SessionState::Exited)
            })
    }

    fn request_redraw(&self, sid: u64) {
        if !self.serves(sid) {
            return;
        }
        if let Some(proxy) = self.proxy {
            let _ = proxy.send_event(Wake::redraw(sid));
        }
    }

    fn subscribe(&self, sid: u64) -> Box<dyn ChangeWait + '_> {
        Box::new(SubscriberWait(if self.serves(sid) {
            Subscription::single(self.watchers)
        } else {
            Subscription::new()
        }))
    }

    fn clipboard_set(&self, text: &str) -> bool {
        crate::control::pbcopy(text)
    }

    fn native_cwd<'p>(&self, reported: &'p str) -> std::borrow::Cow<'p, str> {
        crate::cwd_native::native_path(reported)
    }
}

fn still_wanted(t: &Terminal) -> bool {
    !(crate::control::caller_hung_up() && crate::frame_unfinished_now(t))
}

/// Runs `aterm-control`'s verb matrix against this host, not only `MemoryHost`.
#[cfg(test)]
#[path = "control_host_tests.rs"]
mod tests;
