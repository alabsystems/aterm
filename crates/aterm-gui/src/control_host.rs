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
//! holds. It passes them ([`GuiHost::with_fleet`]), so the SHIPPED host answers
//! `sessions`/`resolve`/`write_input` for real — and `with_fleet` is the only
//! constructor that survives a non-test build, so a rosterless, sink-less host is
//! not merely discouraged in production but unbuildable. The `cfg(test)`
//! [`GuiHost::new`] serves tests holding a bare `Terminal`; it declares
//! `roster: false` / `input_sink: false` rather than answering an empty roster
//! that reads like an empty machine.
//!
//! aterm-gui's own `sessions`/`send`/`feed` verbs still run their pre-seam paths,
//! so wiring the handles moved no wire byte.

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
    /// The ONE session this host serves — the target the dispatcher resolved
    /// before building it. Every per-session method refuses any other sid, so a
    /// fleet-wide `resolve` handed to `write_input` fails closed instead of
    /// landing on this session under a borrowed number.
    sid: u64,
    term: &'a Arc<Mutex<Terminal>>,
    /// `None` when there is no event loop to nudge. Production always passes
    /// `Some`; an `EventLoopProxy` is not buildable off the main thread, so this
    /// is what lets the block/selection verbs be tested on a worker thread.
    proxy: Option<&'a EventLoopProxy<Wake>>,
    subscribers: &'a Subscribers,
    /// The process registry, for the FLEET answers ([`SessionHost::sessions`] /
    /// [`SessionHost::resolve`]) a per-target adapter cannot give from one
    /// session. `None` only on the session-scoped [`GuiHost::new`], which then
    /// reports `roster: false`.
    store: Option<&'a Store>,
    /// The RESOLVED target's PTY sink — the same `ctx.sink` `send`/`feed` write
    /// through, so trait-served input keeps whole-frame atomicity with the
    /// keyboard path. `None` only on [`GuiHost::new`] (`input_sink: false`).
    sink: Option<&'a SinkWriter>,
}

impl<'a> GuiHost<'a> {
    /// A SESSION-SCOPED host: serves `sid` and nothing else, keeps no roster, holds
    /// no input sink — and DECLARES both, so its empty `sessions()` cannot be read
    /// as "no sessions exist".
    ///
    /// `cfg(test)`, which is the real guarantee: the SHIPPED binary has no way to
    /// build a host that answers the fleet verbs with nothing. Tests that own a
    /// bare `Terminal` and no registry keep it.
    #[cfg(test)]
    pub(crate) fn new(
        sid: u64,
        term: &'a Arc<Mutex<Terminal>>,
        proxy: Option<&'a EventLoopProxy<Wake>>,
        subscribers: &'a Subscribers,
    ) -> Self {
        Self {
            sid,
            term,
            proxy,
            subscribers,
            store: None,
            sink: None,
        }
    }

    /// The SHIPPED host: [`GuiHost::new`] plus the two handles only the dispatcher
    /// holds — the registry the roster/selector verbs read, and the resolved
    /// target's input sink. Both are required, not optional: a "fleet" host built
    /// without them is exactly the empty-roster, silently-dropped-write lie this
    /// pair exists to prevent.
    pub(crate) fn with_fleet(
        sid: u64,
        term: &'a Arc<Mutex<Terminal>>,
        proxy: Option<&'a EventLoopProxy<Wake>>,
        subscribers: &'a Subscribers,
        store: &'a Store,
        sink: &'a SinkWriter,
    ) -> Self {
        Self {
            sid,
            term,
            proxy,
            subscribers,
            store: Some(store),
            sink: Some(sink),
        }
    }

    /// Whether this host serves `sid` at all — the fail-closed gate every
    /// per-session method takes first.
    fn serves(&self, sid: u64) -> bool {
        sid == self.sid
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
            roster: self.store.is_some(),
            input_sink: self.sink.is_some(),
        }
    }

    /// The registry snapshot, CLONED out before formatting so the store lock is
    /// never held across a `Terminal` lock (the clone-then-release discipline
    /// mutually-driving agents depend on). Already ascending by `local_id`
    /// (`SessionStore::snapshot` sorts), which is the sid order the wire lists.
    ///
    /// EMPTY only on a session-scoped host, which advertises `roster: false` —
    /// the trait's one legal empty roster.
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

    // This host owns ONE session. The sid IS re-checked: `sessions`/`resolve`
    // answer fleet-wide, so a caller can arrive holding a sibling's sid, and
    // serving this session's engine for it would be a silent misroute.
    fn with_terminal<R>(&self, sid: u64, f: impl FnOnce(&Terminal) -> R) -> Option<R> {
        self.serves(sid).then(|| f(&term_lock(self.term)))
    }

    fn with_terminal_mut<R>(&self, sid: u64, f: impl FnOnce(&mut Terminal) -> R) -> Option<R> {
        self.serves(sid).then(|| f(&mut term_lock(self.term)))
    }

    /// Through the resolved target's ONE `SinkWriter` (whole-frame atomicity with
    /// the keyboard path), noting the input so a driven smoke still measures the
    /// input→present slice. A sid this host does not serve is refused BEFORE the
    /// sink; a host built with no sink reports the write did NOT happen rather
    /// than a false `OK`.
    fn write_input(&self, sid: u64, bytes: &[u8]) -> Option<bool> {
        if !self.serves(sid) {
            return None;
        }
        let Some(sink) = self.sink else {
            return Some(false);
        };
        // An empty frame moves nothing, so it must not stamp an input time the
        // input→present measurement would then wait on a repaint for.
        if bytes.is_empty() {
            return Some(true);
        }
        crate::metrics::note_input();
        Some(sink.write_frame(bytes).is_ok())
    }

    fn request_redraw(&self, sid: u64) {
        if !self.serves(sid) {
            return;
        }
        if let Some(proxy) = self.proxy {
            let _ = proxy.send_event(Wake::redraw(sid));
        }
    }

    /// Registered against the process-wide subscriber set, which is keyed by sid —
    /// so a sid this host does not serve registers on NOTHING and its wait can only
    /// time out, rather than waking on this session's output.
    fn subscribe(&self, sid: u64) -> Box<dyn ChangeWait + '_> {
        let one = [sid];
        let watched: &[u64] = if self.serves(sid) { &one } else { &[] };
        Box::new(SubscriberWait(SubscriberSet::register(
            self.subscribers,
            watched,
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
/// dispatcher builds [`GuiHost::with_fleet`] with the same registered `store` and
/// target `sink` these do — and it reaches exactly two trait methods:
/// [`SessionHost::request_redraw`] becomes a no-op, and `capabilities().event_loop`
/// reads false. So nothing below shows that a `select` actually repaints a window.
/// Closing that needs an `EventLoop` built on a real main thread — a harness
/// binary, not a `#[test]`. Everything else here is the shipped path: the real
/// engine, the real `term_lock` discipline, the real `Subscription` change-wait,
/// the real registry snapshot, the real `SinkWriter`, and the real clipboard
/// capability.
#[cfg(test)]
mod tests {
    use super::*;
    use aterm_control::conformance;
    use aterm_control::selection::{cmd_select, cmd_selection};
    use aterm_session::{EdgeTable, LaunchNonce, SessionId};

    use crate::session_store::{self, SessionHandle};

    /// What the SHIPPED `GuiHost` claims. DECLARED to the matrix, because every
    /// capability-gated check otherwise reads `capabilities()` and a host that
    /// advertises nothing takes the easy arm of each — so an undeclared run cannot
    /// tell this host from one that dropped a facility.
    ///
    /// `event_loop` is the one false, and that is the HARNESS talking rather than
    /// the host: no `#[test]` here can mint an `EventLoopProxy` (see this module's
    /// doc), so the host under test is built `proxy: None`.
    const GUI_HOST_PROFILE: HostCapabilities = HostCapabilities {
        frame_source: true,
        event_loop: false,
        clipboard: true,
        roster: true,
        input_sink: true,
    };

    /// A session registered in a fresh registry, exactly as the GUI registers a
    /// tab: shared engine `Arc`, fabric identity, and a sink over `master` (a pipe
    /// write-end where a test reads the bytes back, else the `-1` stub). The host
    /// under test is then the SHIPPED shape, not a stripped one.
    fn registered(local_id: u64, term: &Arc<Mutex<Terminal>>, master: i32) -> SessionHandle {
        let sid = SessionId::generate();
        let nonce = LaunchNonce::generate();
        let ctx = Arc::new(crate::SessionCtx {
            sink: Arc::new(SinkWriter::new(master)),
            edges: std::sync::Mutex::new(EdgeTable::new()),
            turn_lease: std::sync::Mutex::new(None),
            self_id: sid.clone(),
            nonce,
            cast: Arc::new(std::sync::Mutex::new(crate::cast::CastRecorder::new(
                80, 24,
            ))),
            temporal: Arc::new(std::sync::Mutex::new(
                crate::temporal::TemporalRecorder::new(),
            )),
            byte_fanout: Arc::new(crate::cast::ByteFanout::new()),
            turns: Arc::new(std::sync::Mutex::new(
                crate::turn_ledger::TurnLedger::default(),
            )),
            meta: std::sync::Mutex::new(crate::session_timeline::SessionMeta::default()),
            app_kitty: std::sync::Mutex::new(crate::app_kitty::AppKittySlot::default()),
            timeline: Arc::new(std::sync::Mutex::new(
                crate::session_timeline::SessionTimeline::default(),
            )),
        });
        SessionHandle {
            sid,
            nonce,
            local_id,
            parent: None,
            state: crate::session_store::SessionState::Alive,
            title: format!("tab-{local_id}"),
            term: term.clone(),
            master,
            ctx,
        }
    }

    /// The full matrix passes against the SHIPPED `GuiHost` — registry and sink
    /// wired, over a session carrying real OSC-133 blocks and a live selection, so
    /// the block checks are not satisfied by an empty session, the roster checks
    /// read a real registry, and the suite's save/restore runs on real state.
    ///
    /// Run DECLARED ([`GUI_HOST_PROFILE`]): the shipped host's shape is known here,
    /// so it is held to it. Plain `run_all` would let a host that stopped
    /// advertising its roster or sink pass on the easy arm of every gated check —
    /// pinned by `dropping_a_shipped_capability_only_fails_the_declared_run`.
    #[test]
    fn the_gui_host_passes_the_verb_matrix() {
        let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
        term_lock(&term).process(
            b"\x1b]133;A\x07$ \x1b]633;E;echo hi\x07\x1b]133;B\x07echo hi\n\x1b]133;C\x07hi\n\x1b]133;D;0\x07",
        );
        let reg = crate::subscribe::new_registry();
        let store = session_store::new_store();
        let handle = registered(0, &term, -1);
        store.write().unwrap().register(handle.clone());
        let host = GuiHost::with_fleet(0, &term, None, &reg, &store, &handle.ctx.sink);
        // EQUALITY, where the declared run enforces only a floor: this host's shape
        // is fully known here, so a capability gained or lost has to be RESTATED in
        // the profile rather than quietly widening what the matrix is held to.
        assert_eq!(host.capabilities(), GUI_HOST_PROFILE);

        assert_eq!(cmd_select(&host, 0, "0 0 0 4"), "OK\n");
        let before = cmd_selection(&host, 0);
        assert!(before.contains("$ ech"), "{before}");

        let outcomes = conformance::run_all_declared(&host, 0, GUI_HOST_PROFILE);
        assert_eq!(outcomes.len(), 11, "the matrix lost a check");
        for o in &outcomes {
            assert!(o.passed(), "{}: {:?}", o.check, o.failure);
        }
        assert_eq!(
            cmd_selection(&host, 0),
            before,
            "the matrix left the selection moved"
        );
    }

    /// WHY THE MATRIX ABOVE RUNS DECLARED. A session-scoped host is HONEST about
    /// keeping no roster and holding no sink, so plain `run_all` passes it — each
    /// gated check reads `capabilities()` and takes its easy arm (empty roster,
    /// `Some(false)` write). Naming the shipped profile is the whole difference
    /// between that and a failure, so this is the same host, the same suite, and
    /// only the entry point changing the verdict.
    ///
    /// One profile per capability because `check_declared_capabilities` reports the
    /// FIRST mismatch: a single run would prove `roster` and never reach the sink.
    #[test]
    fn dropping_a_shipped_capability_only_fails_the_declared_run() {
        let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
        term_lock(&term).process(b"hello world");
        let reg = crate::subscribe::new_registry();
        // Neither handle `with_fleet` supplies: roster and input_sink are gone.
        let scoped = GuiHost::new(0, &term, None, &reg);
        assert_eq!(
            scoped.capabilities(),
            HostCapabilities {
                roster: false,
                input_sink: false,
                ..GUI_HOST_PROFILE
            }
        );

        for o in conformance::run_all(&scoped, 0) {
            assert!(
                o.passed(),
                "undeclared, a host that says so is legal: {}: {:?}",
                o.check,
                o.failure
            );
        }

        for (capability, declared) in [
            ("roster", GUI_HOST_PROFILE),
            (
                "input_sink",
                HostCapabilities {
                    roster: false,
                    ..GUI_HOST_PROFILE
                },
            ),
        ] {
            let outcomes = conformance::run_all_declared(&scoped, 0, declared);
            let failure = outcomes
                .iter()
                .find(|o| !o.passed())
                .unwrap_or_else(|| panic!("{capability} declared but absent, still passed"));
            let detail = failure.failure.as_deref().unwrap_or_default();
            assert!(
                detail.contains(capability),
                "{capability} declared but absent, reported as: {detail}"
            );
        }
    }

    /// The read-only subset — the entry point safe to point at a REAL window
    /// someone is mid-drag in — writes nothing here either.
    ///
    /// WITNESSED, because plain `run_read_only` is six shape checks a host serving
    /// a fresh empty `Terminal` also passes: naming the text this session is holding
    /// is what makes the gate say the shipped host answers from the REAL session.
    #[test]
    fn the_read_only_subset_leaves_the_gui_host_untouched() {
        let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
        term_lock(&term).process(b"hello world");
        let reg = crate::subscribe::new_registry();
        let store = session_store::new_store();
        let handle = registered(0, &term, -1);
        store.write().unwrap().register(handle.clone());
        let host = GuiHost::with_fleet(0, &term, None, &reg, &store, &handle.ctx.sink);
        assert_eq!(cmd_select(&host, 0, "0 0 0 4"), "OK\n");
        let before = cmd_selection(&host, 0);

        let outcomes = conformance::run_read_only_witnessed(&host, 0, "hello world");
        assert_eq!(
            outcomes.len(),
            7,
            "the witnessed read-only subset lost a check"
        );
        for o in &outcomes {
            assert!(o.passed(), "{}: {:?}", o.check, o.failure);
        }
        assert_eq!(cmd_selection(&host, 0), before);
    }

    /// THE MISROUTE, refused. The roster answers fleet-wide, so a caller can hold a
    /// SIBLING's sid; every per-session method must decline it rather than serve
    /// this host's session — otherwise `resolve` + `write_input` types into the
    /// wrong terminal.
    #[test]
    fn a_sibling_sid_is_refused_by_the_session_scoped_host() {
        let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
        term_lock(&term).process(b"hello world");
        let sibling_term = Arc::new(Mutex::new(Terminal::new(24, 80)));
        let reg = crate::subscribe::new_registry();
        let store = session_store::new_store();
        let handle = registered(0, &term, -1);
        let sibling = registered(1, &sibling_term, -1);
        store.write().unwrap().register(handle.clone());
        store.write().unwrap().register(sibling.clone());
        let host = GuiHost::with_fleet(0, &term, None, &reg, &store, &handle.ctx.sink);

        // The sibling IS resolvable — this host just does not serve it.
        assert_eq!(host.resolve(Selector::Local(1)), Some(1));
        assert_eq!(host.resolve(Selector::Id(sibling.sid.as_str())), Some(1));
        assert_eq!(host.write_input(1, b"rm -rf /\r"), None);
        assert!(host.with_terminal(1, |t: &Terminal| t.rows()).is_none());
        assert!(
            host.with_terminal_mut(1, |t: &mut Terminal| t.text_selection_mut().clear())
                .is_none()
        );
        // …and the verbs on top of it answer the wire's fail-closed error.
        assert_eq!(cmd_selection(&host, 1), "ERR no such session\n");
        assert_eq!(cmd_select(&host, 1, "clear"), "ERR no such session\n");
        // The served sid still works, so the refusal is the sid check and not a
        // dead host.
        assert!(cmd_selection(&host, 0).starts_with("OK "));
    }

    /// The roster is the REGISTRY's, ascending by sid, carrying each session's
    /// stable id — the answer a daemon-side `sessions` client parses. A
    /// session-scoped host (no registry) says `roster: false` and lists nothing,
    /// which is a different claim from "no sessions exist".
    #[test]
    fn the_roster_is_the_registry_and_an_absent_one_says_so() {
        let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
        let sibling_term = Arc::new(Mutex::new(Terminal::new(24, 80)));
        let reg = crate::subscribe::new_registry();
        let store = session_store::new_store();
        let handle = registered(0, &term, -1);
        let sibling = registered(1, &sibling_term, -1);
        // Registered out of order: the wire's ascending listing is the store's
        // doing, not the insertion order's.
        store.write().unwrap().register(sibling.clone());
        store.write().unwrap().register(handle.clone());
        let host = GuiHost::with_fleet(0, &term, None, &reg, &store, &handle.ctx.sink);

        let roster = host.sessions();
        assert_eq!(roster.iter().map(|e| e.sid).collect::<Vec<_>>(), vec![0, 1]);
        assert_eq!(roster[0].id, handle.sid.as_str());
        assert_eq!(roster[1].id, sibling.sid.as_str());
        assert_eq!(roster[0].state, SessionState::Alive);
        assert_eq!(roster[0].title, "tab-0");
        assert!(!roster[0].has_meta);

        let scoped = GuiHost::new(0, &term, None, &reg);
        assert!(!scoped.capabilities().roster);
        assert!(scoped.sessions().is_empty());
        assert_eq!(scoped.resolve(Selector::Local(0)), None);
        // Still SERVES its session: no index is not no session.
        assert!(cmd_selection(&scoped, 0).starts_with("OK "));
    }

    /// Bytes for the served sid reach the target's REAL `SinkWriter` — the half
    /// `check_write_input` deliberately will not assert against a live session (it
    /// writes only empty frames). Unix-only: it needs a pipe to read back.
    #[test]
    #[cfg(unix)]
    fn write_input_reaches_the_targets_sink() {
        use std::io::Read;
        use std::os::fd::FromRawFd;

        let mut fds = [0i32; 2];
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0, "pipe");
        let mut rx = unsafe { std::fs::File::from_raw_fd(fds[0]) };
        let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
        let reg = crate::subscribe::new_registry();
        let store = session_store::new_store();
        let handle = registered(0, &term, fds[1]);
        store.write().unwrap().register(handle.clone());
        let host = GuiHost::with_fleet(0, &term, None, &reg, &store, &handle.ctx.sink);

        assert_eq!(host.write_input(0, b"echo hi\r"), Some(true));
        let mut buf = [0u8; b"echo hi\r".len()];
        rx.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"echo hi\r");

        // A host with no sink reports the write did not happen, rather than `OK`
        // for bytes that went nowhere.
        let scoped = GuiHost::new(0, &term, None, &reg);
        assert!(!scoped.capabilities().input_sink);
        assert_eq!(scoped.write_input(0, b"echo hi\r"), Some(false));
    }
}
