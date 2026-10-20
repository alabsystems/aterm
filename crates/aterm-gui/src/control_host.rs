// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! aterm-gui's [`SessionHost`]: a borrowed adapter over the `(term, proxy,
//! subscribers, …)` handles [`handle`](super::handle) has already resolved, so
//! the verb bodies in `aterm-control` drive the real window with no extra
//! ownership, allocation or lifetime.
//!
//! The terminal accessors go through [`term_lock`], which keeps the debug
//! lock-hold tripwire on this side of the seam (its `#[track_caller]` location
//! is this file).
//!
//! The fleet half (roster, selector resolution, input sink) needs the registry
//! and the target's `ctx.sink`, which only the dispatcher holds and passes via
//! [`GuiHost::with_fleet`], the only constructor in a non-test build. The
//! `cfg(test)` [`GuiHost::new`] serves tests holding a bare `Terminal`; it
//! declares `roster: false` / `input_sink: false` rather than answering an empty
//! roster that reads like an empty machine.
//!
//! aterm-gui's own `sessions`/`send`/`feed` verbs run their own paths.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use aterm_control::{
    ChangeWait, HostCapabilities, Selector, SessionEntry, SessionHost, SessionState,
};
use aterm_core::terminal::Terminal;
use aterm_session::SessionId;
#[cfg(test)]
use aterm_session::sink::SinkWriter;
use aterm_window::event_loop::EventLoopProxy;

use super::control_session;
use crate::session_store::Store;
use crate::subscribe::{SubscriberSet, Subscribers, Subscription};
use crate::{SessionCtx, Wake, term_lock};

pub(crate) struct GuiHost<'a> {
    /// The one session this host serves. Every per-session method refuses any
    /// other sid, so a fleet-wide `resolve` handed to `write_input` fails closed.
    sid: u64,
    term: &'a Arc<Mutex<Terminal>>,
    /// `None` when there is no event loop to nudge. Production passes `Some`;
    /// tests run off the main thread, where no `EventLoopProxy` can be built.
    proxy: Option<&'a EventLoopProxy<Wake>>,
    subscribers: &'a Subscribers,
    /// The process registry, for the fleet answers ([`SessionHost::sessions`],
    /// [`SessionHost::resolve`]). `None` only on [`GuiHost::new`], which reports
    /// `roster: false`.
    store: Option<&'a Store>,
    /// The resolved target: sink plus its echo tracker, so trait-served input
    /// keeps whole-frame ordering and per-session receipt attribution. `None`
    /// only on [`GuiHost::new`] (`input_sink: false`).
    ctx: Option<&'a SessionCtx>,
}

fn classify_raw_editor_write(
    result: std::io::Result<usize>,
    intended: usize,
) -> (bool, crate::input::Delivery) {
    let ok = result.is_ok();
    let delivery = match result {
        Ok(written) if written == intended => crate::input::Delivery::Full,
        Ok(accepted) if accepted > 0 => crate::input::Delivery::PartialInDoubt { accepted },
        _ => crate::input::Delivery::Failed,
    };
    (ok, delivery)
}

impl<'a> GuiHost<'a> {
    /// A session-scoped host: serves `sid` only, keeps no roster, holds no input
    /// sink, and declares both so its empty `sessions()` is not read as "no
    /// sessions exist". `cfg(test)` so the shipped binary cannot build one.
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
            ctx: None,
        }
    }

    /// The shipped host: [`GuiHost::new`] plus the registry the roster/selector
    /// verbs read and the resolved target's input sink. Both are required so a
    /// fleet host cannot answer with an empty roster or drop writes silently.
    pub(crate) fn with_fleet(
        sid: u64,
        term: &'a Arc<Mutex<Terminal>>,
        proxy: Option<&'a EventLoopProxy<Wake>>,
        subscribers: &'a Subscribers,
        store: &'a Store,
        ctx: &'a SessionCtx,
    ) -> Self {
        Self {
            sid,
            term,
            proxy,
            subscribers,
            store: Some(store),
            ctx: Some(ctx),
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
        // Sliced while a socket connection is being served, so the waiting
        // verb re-asks `caller_gone` often enough to give a dead caller's lane
        // back promptly.
        self.0.wait(crate::control::hangup_park(timeout))
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
            input_sink: self.ctx.is_some(),
        }
    }

    /// The registry snapshot, cloned out before formatting so the store lock is
    /// never held across a `Terminal` lock. Ascending by `local_id`
    /// (`SessionStore::snapshot` sorts), the sid order the wire lists. Empty only
    /// on a session-scoped host, which advertises `roster: false`.
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

    /// Window membership and running command, through the same hop and sanitizer
    /// as the `sessions` verb ([`control_session::placement_of`]). `None` (could
    /// not look, never "detached") without a registry, without an event loop, or
    /// when the hop fails.
    fn placements(&self) -> Option<Vec<aterm_control::host::SessionPlacement>> {
        let store = self.store?;
        let rows = control_session::session_window_rows(self.proxy).ok()?;
        let snapshot = {
            let g = store.read().unwrap_or_else(|p| p.into_inner());
            g.snapshot()
        };
        Some(
            snapshot
                .iter()
                .map(|h| control_session::placement_of(h, &rows))
                .collect(),
        )
    }

    fn resolve(&self, selector: Selector<'_>) -> Option<u64> {
        let g = self.store?.read().unwrap_or_else(|p| p.into_inner());
        match selector {
            Selector::Local(n) => g.by_local(n),
            Selector::Id(id) => g.by_sid(&SessionId::new(id)),
        }
        .map(|h| h.local_id)
    }

    // `sessions`/`resolve` answer fleet-wide, so a caller can hold a sibling's
    // sid; serving this session's engine for it would be a silent misroute.
    fn with_terminal<R>(&self, sid: u64, f: impl FnOnce(&Terminal) -> R) -> Option<R> {
        self.serves(sid).then(|| f(&term_lock(self.term)))
    }

    fn with_terminal_mut<R>(&self, sid: u64, f: impl FnOnce(&mut Terminal) -> R) -> Option<R> {
        self.serves(sid).then(|| f(&mut term_lock(self.term)))
    }

    /// Through the resolved target's one `SinkWriter` (whole-frame atomicity with
    /// the keyboard path), noting the input so a driven smoke still measures
    /// input→present. A sid this host does not serve is refused before the sink;
    /// a host with no sink reports the write did not happen.
    fn write_input(&self, sid: u64, bytes: &[u8]) -> Option<bool> {
        if !self.serves(sid) {
            return None;
        }
        let Some(ctx) = self.ctx else {
            return Some(false);
        };
        // An empty frame moves nothing, so it must not stamp an input time the
        // input→present measurement would then wait on a repaint for.
        if bytes.is_empty() {
            return Some(true);
        }
        crate::metrics::note_input();
        let write = ctx.output_echo.begin_raw_editor(bytes);
        let receipt = ctx.sink.write_frame_with_receipt(bytes);
        let accepted_order = match receipt.as_ref() {
            Ok(receipt) => receipt.order(),
            Err(error) => error.order(),
        };
        let (ok, delivery) = classify_raw_editor_write(
            receipt
                .map(|receipt| receipt.accepted())
                .map_err(|error| error.into_error()),
            bytes.len(),
        );
        write.finish_delivery(delivery, accepted_order, &ctx.sink);
        Some(ok)
    }

    /// Ended once the session's fabric endpoint is closed: its program exited
    /// or its tab closed (`SessionFabric::close`, then a subscriber notify).
    fn session_ended(&self, sid: u64) -> bool {
        self.serves(sid) && self.ctx.is_some_and(|ctx| ctx.fabric.endpoint_closed())
    }

    fn request_redraw(&self, sid: u64) {
        if !self.serves(sid) {
            return;
        }
        if let Some(proxy) = self.proxy {
            let _ = proxy.send_event(Wake::redraw(sid));
        }
    }

    /// The subscriber set is keyed by sid, so a sid this host does not serve
    /// registers on nothing and its wait can only time out.
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

    /// The GUI's one conversion ([`crate::cwd_native::native_path`]), so a
    /// block's `cwd=` names the directory exactly as `cwd`, `meta` and the tab
    /// strip do. The identity off Windows.
    fn native_cwd<'p>(&self, reported: &'p str) -> std::borrow::Cow<'p, str> {
        crate::cwd_native::native_path(reported)
    }
}

/// Runs `aterm-control`'s verb matrix against this host, not only `MemoryHost`.
///
/// The host under test is built with `proxy: None`: libtest runs tests off the
/// main thread, where no `EventLoopProxy<Wake>` can be built. That is the only
/// difference from the shipped host, and it touches two trait methods:
/// [`SessionHost::request_redraw`] is a no-op and `capabilities().event_loop`
/// reads false, so nothing here shows a `select` repainting a window.
/// Everything else is the shipped path: the real engine, `term_lock`,
/// `Subscription` change-wait, registry snapshot, `SinkWriter` and clipboard.
#[cfg(test)]
mod tests {
    use super::*;
    use aterm_control::conformance;
    use aterm_control::selection::{cmd_select, cmd_selection};
    use aterm_session::{EdgeTable, LaunchNonce, SessionId};

    use crate::session_store::{self, SessionHandle};

    /// What the shipped `GuiHost` claims, declared to the matrix so a host that
    /// dropped a facility cannot pass on the easy arm of each capability-gated
    /// check. `event_loop` is false only because no test can mint an
    /// `EventLoopProxy` (see the module doc).
    const GUI_HOST_PROFILE: HostCapabilities = HostCapabilities {
        frame_source: true,
        event_loop: false,
        clipboard: true,
        roster: true,
        input_sink: true,
    };

    #[test]
    fn raw_editor_write_classification_preserves_partial_acceptance_and_reply_shape() {
        assert_eq!(
            classify_raw_editor_write(Ok(4), 4),
            (true, crate::input::Delivery::Full)
        );
        assert_eq!(
            classify_raw_editor_write(Ok(2), 4),
            (true, crate::input::Delivery::PartialInDoubt { accepted: 2 }),
            "the host still replies from Result::is_ok while echo tracking retains the prefix"
        );
        assert_eq!(
            classify_raw_editor_write(Ok(0), 4),
            (true, crate::input::Delivery::Failed)
        );
        assert_eq!(
            classify_raw_editor_write(Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe)), 4,),
            (false, crate::input::Delivery::Failed)
        );
    }

    /// A session registered in a fresh registry as the GUI registers a tab:
    /// shared engine `Arc`, fabric identity, and a sink over `master` (a pipe
    /// write-end the test reads back, else the `-1` stub).
    fn registered(local_id: u64, term: &Arc<Mutex<Terminal>>, master: i32) -> SessionHandle {
        let sid = SessionId::generate();
        let nonce = LaunchNonce::generate();
        let ctx = Arc::new(crate::SessionCtx {
            sink: Arc::new(SinkWriter::new(master)),
            output_echo: Arc::new(crate::app_input::OutputEchoTracker::default()),
            modes: crate::mode_mirror_of(term),
            ui_waiting: Arc::default(),
            edges: std::sync::Arc::new(std::sync::Mutex::new(EdgeTable::new())),
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
            timeline: Arc::new(std::sync::Mutex::new(
                crate::session_timeline::SessionTimeline::default(),
            )),
            fabric: std::sync::Arc::default(),
            idem: Default::default(),
            rewrap_gauge: std::sync::Arc::default(),
            human_input: Default::default(),
            launch: Default::default(),
            reset_lane: Default::default(),
            crash_journal: Default::default(),
            typed: Default::default(),
            update_parked: Default::default(),
            witness: Default::default(),
            fenced_writes: Default::default(),
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
            identity: None,
        }
    }

    /// The full matrix passes against the shipped `GuiHost` (registry and sink
    /// wired) over a session with real OSC-133 blocks and a live selection. Run
    /// declared ([`GUI_HOST_PROFILE`]) so a host that stopped advertising its
    /// roster or sink cannot pass on the easy arms.
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
        let host = GuiHost::with_fleet(0, &term, None, &reg, &store, &handle.ctx);
        // Equality, not the declared run's floor: a capability gained or lost
        // must be restated in the profile.
        assert_eq!(host.capabilities(), GUI_HOST_PROFILE);

        assert_eq!(cmd_select(&host, 0, "0 0 0 4"), "OK\n");
        let before = cmd_selection(&host, 0);
        assert!(before.contains("$ ech"), "{before}");

        let outcomes = conformance::run_all_declared(&host, 0, GUI_HOST_PROFILE);
        assert!(!outcomes.is_empty());
        for o in &outcomes {
            assert!(o.passed(), "{}: {:?}", o.check, o.failure);
        }
        assert_eq!(
            cmd_selection(&host, 0),
            before,
            "the matrix left the selection moved"
        );
    }

    /// The read-only subset (safe against a real window in use) writes nothing.
    /// Witnessed with the session's text, since its shape checks alone pass on
    /// an empty `Terminal`.
    #[test]
    fn the_read_only_subset_leaves_the_gui_host_untouched() {
        let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
        term_lock(&term).process(b"hello world");
        let reg = crate::subscribe::new_registry();
        let store = session_store::new_store();
        let handle = registered(0, &term, -1);
        store.write().unwrap().register(handle.clone());
        let host = GuiHost::with_fleet(0, &term, None, &reg, &store, &handle.ctx);
        assert_eq!(cmd_select(&host, 0, "0 0 0 4"), "OK\n");
        let before = cmd_selection(&host, 0);

        let outcomes = conformance::run_read_only_witnessed(&host, 0, "hello world");
        assert!(!outcomes.is_empty());
        for o in &outcomes {
            assert!(o.passed(), "{}: {:?}", o.check, o.failure);
        }
        assert_eq!(cmd_selection(&host, 0), before);
    }

    /// Every per-session method declines a sibling's sid (the roster answers
    /// fleet-wide), so `resolve` + `write_input` cannot type into the wrong
    /// terminal.
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
        let host = GuiHost::with_fleet(0, &term, None, &reg, &store, &handle.ctx);

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

    /// `blocks` names a block's directory the way `cwd` does: on Windows an OSC 7
    /// path like `/C:/Users//x` becomes `C:\Users\x`; a native cmd path and a POSIX
    /// path from WSL or SSH pass through; on Unix nothing is converted (`/C:/` is
    /// a legal directory there).
    #[test]
    fn a_block_cwd_is_the_native_path_the_cwd_verb_prints() {
        use aterm_control::selection::{cmd_blocks, cmd_blocks_json};
        let blocks_cwd = |osc: &[u8]| {
            let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
            term_lock(&term).process(osc);
            term_lock(&term).process(b"\x1b]133;A\x07$ ");
            let reg = crate::subscribe::new_registry();
            let host = GuiHost::new(0, &term, None, &reg);
            (cmd_blocks(&host, 0, ""), cmd_blocks_json(&host, 0, ""))
        };

        let (listed, json) = blocks_cwd(b"\x1b]7;file:///C:/Users//m6-an\x07");
        #[cfg(windows)]
        {
            assert!(listed.contains(r" cwd=C:\Users\m6-an "), "{listed}");
            assert!(json.contains(r#""cwd":"C:\\Users\\m6-an""#), "{json}");
        }
        #[cfg(not(windows))]
        {
            assert!(listed.contains(" cwd=/C:/Users//m6-an "), "{listed}");
            assert!(json.contains(r#""cwd":"/C:/Users//m6-an""#), "{json}");
        }

        // cmd's `633;P;Cwd=$P` is native already: idempotent.
        let (listed, _) = blocks_cwd(b"\x1b]633;P;Cwd=C:\\Windows\\Temp\x07");
        assert!(listed.contains(r" cwd=C:\Windows\Temp "), "{listed}");

        // A WSL/SSH shell's POSIX cwd is nobody's drive path.
        let (listed, _) = blocks_cwd(b"\x1b]7;file:///home/user/proj\x07");
        assert!(listed.contains(" cwd=/home/user/proj "), "{listed}");
    }

    /// A cmd block's cwd is the directory its command ran in. cmd reports its
    /// cwd from the prompt (`633;P;Cwd=$P`) and the engine files it under the
    /// block in progress, so the injection sends it after `133;A` opens the new
    /// block (pinned by `aterm-shell-integration`'s
    /// `test_prepare_cmd_wraps_the_prompt_with_marks_and_cwd`). The reverse
    /// order labels a `cd` block with its destination.
    #[test]
    fn a_cmd_block_cwd_is_where_its_command_ran() {
        use aterm_control::selection::cmd_blocks;
        let cwds = |cwd_after_start: bool| {
            // One rendered cmd prompt, as cmd writes it for `$e]…$e\`.
            let prompt = |dir: &str| {
                let start = "\x1b]133;A\x1b\\";
                let cwd = format!("\x1b]633;P;Cwd={dir}\x1b\\");
                let marks = if cwd_after_start {
                    format!("{start}{cwd}")
                } else {
                    format!("{cwd}{start}")
                };
                format!("{marks}{dir}>\x1b]133;B\x1b\\")
            };
            let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
            term_lock(&term).process(prompt(r"C:\Users\m6-an\aterm").as_bytes());
            term_lock(&term).process(b"cd /d C:\\Windows\\Temp\r\n");
            term_lock(&term).process(prompt(r"C:\Windows\Temp").as_bytes());
            let reg = crate::subscribe::new_registry();
            let host = GuiHost::new(0, &term, None, &reg);
            cmd_blocks(&host, 0, "")
                .lines()
                .skip(1)
                .filter_map(|line| {
                    let (_, rest) = line.split_once(" cwd=")?;
                    rest.split(' ').next().map(str::to_owned)
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(cwds(true), [r"C:\Users\m6-an\aterm", r"C:\Windows\Temp"]);
        // Negative control: the reverse order.
        assert_eq!(cwds(false), [r"C:\Windows\Temp", r"C:\Windows\Temp"]);
    }

    /// The roster is the registry's, ascending by sid, with each session's stable
    /// id. A session-scoped host says `roster: false` and lists nothing, which is
    /// not "no sessions exist".
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
        let host = GuiHost::with_fleet(0, &term, None, &reg, &store, &handle.ctx);

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
        // Still serves its session: no index is not no session.
        assert!(cmd_selection(&scoped, 0).starts_with("OK "));

        // With no event loop to ask, both hosts answer `None` (could not look),
        // never an empty batch that would print as `window=none`.
        assert_eq!(host.placements(), None);
        assert_eq!(scoped.placements(), None);
    }

    /// Bytes for the served sid reach the target's real `SinkWriter`, which
    /// `check_write_input` does not assert against a live session. Unix-only
    /// (reads back through a pipe).
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
        let host = GuiHost::with_fleet(0, &term, None, &reg, &store, &handle.ctx);

        assert_eq!(host.write_input(0, b"echo hi\r"), Some(true));
        let mut buf = [0u8; b"echo hi\r".len()];
        rx.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"echo hi\r");
        let sample = handle
            .ctx
            .output_echo
            .sample(&handle.ctx.sink, std::time::Instant::now());
        assert!(sample.last_accepted_at.is_none());
        assert!(
            sample.last_boundary_at.is_some(),
            "a raw command ending in CR opens the submitted turn"
        );
        assert!(!sample.input_hot);

        assert_eq!(host.write_input(0, b"x"), Some(true));
        let mut byte = [0];
        rx.read_exact(&mut byte).unwrap();
        assert_eq!(&byte, b"x");
        let echo = handle
            .ctx
            .output_echo
            .sample(&handle.ctx.sink, std::time::Instant::now());
        assert!(
            echo.last_accepted_at.is_some(),
            "a raw editor frame without CR/LF keeps its echo shadow"
        );

        // A host with no sink reports the write did not happen, rather than `OK`
        // for bytes that went nowhere.
        let scoped = GuiHost::new(0, &term, None, &reg);
        assert!(!scoped.capabilities().input_sink);
        assert_eq!(scoped.write_input(0, b"echo hi\r"), Some(false));
    }

    /// A CLOSED TAB ENDS WHAT IS PARKED ON IT. A driver parks `await inbox` and
    /// `wait` on a tab, and the tab is closed. Both answer `ERR exited` at the
    /// close; before, they slept to their deadline (up to ten minutes) and then
    /// answered `OK timeout`, and all that time each lane's `SessionCtx` clone,
    /// whose sink owns the master, held the closed tab's PTY open. Once they
    /// answer, the lanes hold nothing of the session.
    #[test]
    fn closing_a_tab_ends_its_parked_waits_with_err_exited() {
        let mut app = crate::App::headless_for_test();
        let wid = crate::WindowId(0);
        let id = app.next_session_id;
        app.push_stub_tab(wid, crate::stub_session(id));
        let (term, ctx) = {
            let session = app.pool.get(id).expect("the tab's session");
            (session.term.clone(), session.ctx.clone())
        };
        let (answered, answers) = std::sync::mpsc::channel();
        let inbox = {
            let (ctx, answered) = (ctx.clone(), answered.clone());
            std::thread::spawn(move || {
                let reply = crate::fabric::cmd_await_inbox(&ctx, &["since=0"], 600_000);
                answered.send(("await inbox", reply)).unwrap();
            })
        };
        let wait = {
            let (term, ctx) = (term.clone(), ctx.clone());
            let (store, subscribers) = (app.store.clone(), app.subscribers.clone());
            std::thread::spawn(move || {
                let host = GuiHost::with_fleet(id, &term, None, &subscribers, &store, &ctx);
                let reply = aterm_control::selection::cmd_wait(&host, id, "600000");
                answered.send(("wait", reply)).unwrap();
            })
        };
        // `wait` is parked once its subscriber is registered: the close wakes it,
        // where an entry check would have answered it anyway.
        aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "`wait` parks on the tab",
            || (app.subscribers.lock().unwrap().watchers(id) == 1).then_some(()),
        );
        app.close_session_by_id(id).expect("the tab closes");
        let mut replies = Vec::new();
        for _ in 0..2 {
            // A hang detector, not a latency budget: the close is the event.
            replies.push(
                answers
                    .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
                    .expect("a wait parked on the closed tab never answered"),
            );
        }
        replies.sort_unstable();
        assert_eq!(
            replies,
            [
                ("await inbox", "ERR exited\n".to_string()),
                ("wait", "ERR exited\n".to_string())
            ]
        );
        inbox.join().unwrap();
        wait.join().unwrap();
        assert_eq!(
            Arc::strong_count(&ctx),
            1,
            "something still holds the closed tab's context, and with it its master"
        );
    }

    /// The exit half: a shell that exits under `--hold` keeps its pane, so no
    /// close ever comes, and the exit edge (`exit_session_logical`, the
    /// logical core of `Wake::Exit` and of a deferred exit's replay) must end
    /// what is parked on it by itself: `await inbox` from the fabric it closes,
    /// `wait` from the subscriber wake it sends.
    #[test]
    fn a_held_shells_exit_ends_its_parked_waits_with_err_exited() {
        let mut app = crate::App::headless_for_test();
        app.hold = true;
        let wid = crate::WindowId(0);
        let id = app.next_session_id;
        app.push_stub_tab(wid, crate::stub_session(id));
        let (term, ctx) = {
            let session = app.pool.get(id).expect("the tab's session");
            (session.term.clone(), session.ctx.clone())
        };
        let (answered, answers) = std::sync::mpsc::channel();
        let inbox = {
            let (ctx, answered) = (ctx.clone(), answered.clone());
            std::thread::spawn(move || {
                let reply = crate::fabric::cmd_await_inbox(&ctx, &["since=0"], 600_000);
                answered.send(("await inbox", reply)).unwrap();
            })
        };
        let wait = {
            let (term, ctx) = (term.clone(), ctx.clone());
            let (store, subscribers) = (app.store.clone(), app.subscribers.clone());
            std::thread::spawn(move || {
                let host = GuiHost::with_fleet(id, &term, None, &subscribers, &store, &ctx);
                let reply = aterm_control::selection::cmd_wait(&host, id, "600000");
                answered.send(("wait", reply)).unwrap();
            })
        };
        aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "`wait` parks on the tab",
            || (app.subscribers.lock().unwrap().watchers(id) == 1).then_some(()),
        );
        assert!(
            app.exit_session_logical(id).is_empty(),
            "`--hold` keeps the pane"
        );
        let mut replies = Vec::new();
        for _ in 0..2 {
            // A hang detector, not a latency budget: the exit is the event.
            replies.push(
                answers
                    .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
                    .expect("a wait parked on the exited shell never answered"),
            );
        }
        replies.sort_unstable();
        assert_eq!(
            replies,
            [
                ("await inbox", "ERR exited\n".to_string()),
                ("wait", "ERR exited\n".to_string())
            ]
        );
        inbox.join().unwrap();
        wait.join().unwrap();
        assert!(app.pool.get(id).is_some(), "the held pane stays");
    }
}
