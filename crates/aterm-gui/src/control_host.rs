// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! aterm-gui's [`SessionHost`]: a BORROWED adapter over the `(term, proxy,
//! subscribers, …)` handles [`handle`](super::handle) has already resolved, so
//! the verb bodies in `aterm-control` drive the real window with no extra
//! ownership, allocation or lifetime.
//!
//! The two terminal accessors go through [`term_lock`], which keeps the debug
//! lock-hold tripwire on THIS side of the seam (the standing guard against a
//! reintroduced on-lock stall). Its `#[track_caller]` location is now this file
//! rather than the verb body — the warning still fires, one frame shallower.
//!
//! The FLEET half (roster, selector resolution, input sink) needs two more
//! borrows — the registry and the target's `ctx.sink` — which only the dispatcher
//! holds; [`GuiHost::with_fleet`] takes them. Until `control.rs` passes them, a
//! `GuiHost` answers the fleet verbs as a host that has no roster and cannot
//! write, never with a fabricated one. aterm-gui's own `sessions`/`send`/`feed`
//! still run their pre-seam paths, so no wire byte moves either way.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use aterm_control::{
    ChangeWait, HostCapabilities, Selector, SessionEntry, SessionHost, SessionState,
};
use aterm_core::terminal::Terminal;
use aterm_session::SessionId;
use aterm_session::sink::SinkWriter;
use winit::event_loop::EventLoopProxy;

use crate::session_store::Store;
use crate::subscribe::{SubscriberSet, Subscribers, Subscription};
use crate::{Wake, term_lock};

pub(crate) struct GuiHost<'a> {
    term: &'a Arc<Mutex<Terminal>>,
    /// `None` when there is no event loop to nudge. Production always passes
    /// `Some`; an `EventLoopProxy` is not buildable off the main thread, so this
    /// is what lets the block/selection verbs be tested on a worker thread.
    proxy: Option<&'a EventLoopProxy<Wake>>,
    subscribers: &'a Subscribers,
    /// The process registry, for the FLEET answers ([`SessionHost::sessions`] /
    /// [`SessionHost::resolve`]) a per-target adapter cannot give from one
    /// session. `None` until the dispatcher passes it — see [`GuiHost::with_fleet`].
    store: Option<&'a Store>,
    /// The RESOLVED target's PTY sink — the same `ctx.sink` `send`/`feed` write
    /// through, so trait-served input keeps whole-frame atomicity with the
    /// keyboard path. `None` until the dispatcher passes it.
    sink: Option<&'a SinkWriter>,
}

impl<'a> GuiHost<'a> {
    pub(crate) fn new(
        term: &'a Arc<Mutex<Terminal>>,
        proxy: Option<&'a EventLoopProxy<Wake>>,
        subscribers: &'a Subscribers,
    ) -> Self {
        Self::with_fleet(term, proxy, subscribers, None, None)
    }

    /// [`GuiHost::new`] plus the two handles a MULTI-session answer needs: the
    /// registry the roster/selector verbs read, and the target's input sink.
    /// Separate constructor so the single-session call sites stay unchanged; a
    /// host built without them refuses those verbs rather than guessing (see the
    /// impls below).
    pub(crate) fn with_fleet(
        term: &'a Arc<Mutex<Terminal>>,
        proxy: Option<&'a EventLoopProxy<Wake>>,
        subscribers: &'a Subscribers,
        store: Option<&'a Store>,
        sink: Option<&'a SinkWriter>,
    ) -> Self {
        Self {
            term,
            proxy,
            subscribers,
            store,
            sink,
        }
    }
}

/// A [`Subscription`] as a [`ChangeWait`]. Owning it in the box preserves the
/// RAII deregistration: the registry entry lives exactly as long as the handle.
struct SubscriberWait(Subscription);

impl ChangeWait for SubscriberWait {
    fn wait(&self, timeout: Duration) -> bool {
        self.0.wait(timeout)
    }
}

impl SessionHost for GuiHost<'_> {
    fn capabilities(&self) -> HostCapabilities {
        HostCapabilities {
            frame_source: true,
            event_loop: self.proxy.is_some(),
            clipboard: true,
        }
    }

    /// The registry snapshot, CLONED out before formatting so the store lock is
    /// never held across a `Terminal` lock (the clone-then-release discipline
    /// mutually-driving agents depend on).
    ///
    /// With no registry this is EMPTY, which is the one place this host does not
    /// yet meet the trait's roster contract — closed by the dispatcher passing
    /// `store` ([`GuiHost::with_fleet`]), not by anything here.
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
        match selector {
            Selector::Local(n) => g.by_local(n),
            Selector::Id(id) => g.by_sid(&SessionId::new(id)),
        }
        .map(|h| h.local_id)
    }

    // This host owns ONE session; the dispatcher resolved the sid before building
    // it, so the id is not re-checked here.
    fn with_terminal<R>(&self, _sid: u64, f: impl FnOnce(&Terminal) -> R) -> Option<R> {
        Some(f(&term_lock(self.term)))
    }

    fn with_terminal_mut<R>(&self, _sid: u64, f: impl FnOnce(&mut Terminal) -> R) -> Option<R> {
        Some(f(&mut term_lock(self.term)))
    }

    /// Through the resolved target's ONE `SinkWriter` (whole-frame atomicity with
    /// the keyboard path), noting the input so a driven smoke still measures the
    /// input→present slice. A host built with no sink reports the write did NOT
    /// happen rather than a false `OK`.
    fn write_input(&self, _sid: u64, bytes: &[u8]) -> Option<bool> {
        let Some(sink) = self.sink else {
            return Some(false);
        };
        crate::metrics::note_input();
        Some(sink.write_frame(bytes).is_ok())
    }

    fn request_redraw(&self, sid: u64) {
        if let Some(proxy) = self.proxy {
            let _ = proxy.send_event(Wake::redraw(sid));
        }
    }

    fn subscribe(&self, sid: u64) -> Box<dyn ChangeWait + '_> {
        Box::new(SubscriberWait(SubscriberSet::register(
            self.subscribers,
            &[sid],
        )))
    }

    fn clipboard_set(&self, text: &str) -> bool {
        crate::control::pbcopy(text)
    }
}

/// Phase 1a's exit criterion: `aterm-control`'s verb matrix runs against THIS
/// host, not only against the `MemoryHost` it was written beside. A suite that
/// only ever passes against its own reference host proves nothing about the seam.
///
/// WHAT THESE TESTS DO NOT PROVE. The host under test is built with `proxy:
/// None`. `EventLoop::new` panics unless it runs on the main thread and libtest
/// runs every test on a spawned one, so no unit test in this crate can mint an
/// `EventLoopProxy<Wake>`. That is the WHOLE delta from the shipped host — the
/// dispatcher builds `GuiHost::new(term, Some(proxy), subscribers)` with the same
/// `store`/`sink` (`None`) these do — and it reaches exactly two trait methods:
/// [`SessionHost::request_redraw`] becomes a no-op, and `capabilities().event_loop`
/// reads false. So nothing below shows that a `select` actually repaints a window.
/// Closing that needs an `EventLoop` built on a real main thread — a harness
/// binary, not a `#[test]`. Everything else here is the shipped path: the real
/// engine, the real `term_lock` discipline, the real `Subscription` change-wait,
/// and the real clipboard capability.
#[cfg(test)]
mod tests {
    use super::*;
    use aterm_control::conformance;
    use aterm_control::selection::{cmd_select, cmd_selection};

    /// The full matrix passes against `GuiHost`, over a session carrying real
    /// OSC-133 blocks and a live selection — so the block checks are not satisfied
    /// by an empty session and the suite's save/restore runs on real state.
    #[test]
    fn the_gui_host_passes_the_verb_matrix() {
        let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
        term_lock(&term).process(
            b"\x1b]133;A\x07$ \x1b]633;E;echo hi\x07\x1b]133;B\x07echo hi\n\x1b]133;C\x07hi\n\x1b]133;D;0\x07",
        );
        let reg = crate::subscribe::new_registry();
        let host = GuiHost::new(&term, None, &reg);
        // Pins which arm `check_copy` takes: this host has a clipboard, so it runs
        // the OK-0 path, not the trivially-satisfied `ERR unsupported` one.
        assert!(host.capabilities().clipboard);

        assert_eq!(cmd_select(&host, 0, "0 0 0 4"), "OK\n");
        let before = cmd_selection(&host, 0);
        assert!(before.contains("$ ech"), "{before}");

        let outcomes = conformance::run_all(&host, 0);
        assert_eq!(outcomes.len(), 7, "the matrix lost a check");
        for o in &outcomes {
            assert!(o.passed(), "{}: {:?}", o.check, o.failure);
        }
        assert_eq!(
            cmd_selection(&host, 0),
            before,
            "the matrix left the selection moved"
        );
    }

    /// The read-only subset — the entry point safe to point at a REAL window
    /// someone is mid-drag in — writes nothing here either. Worth asserting on
    /// THIS host specifically: `with_terminal_mut` never refuses a sid, so a stray
    /// write would land rather than be declined.
    #[test]
    fn the_read_only_subset_leaves_the_gui_host_untouched() {
        let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
        term_lock(&term).process(b"hello world");
        let reg = crate::subscribe::new_registry();
        let host = GuiHost::new(&term, None, &reg);
        assert_eq!(cmd_select(&host, 0, "0 0 0 4"), "OK\n");
        let before = cmd_selection(&host, 0);

        let outcomes = conformance::run_read_only(&host, 0);
        assert_eq!(outcomes.len(), 4, "the read-only subset lost a check");
        for o in &outcomes {
            assert!(o.passed(), "{}: {:?}", o.check, o.failure);
        }
        assert_eq!(cmd_selection(&host, 0), before);
    }
}
