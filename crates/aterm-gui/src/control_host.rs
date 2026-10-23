// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! aterm-gui's [`SessionHost`]: a borrowed adapter over the `(term, proxy, watchers, …)` handles [`dispatch_session`](super::dispatch_session) has already resolved, so the verb bodies in `aterm-control` drive the real window with no extra ownership, allocation or lifetime.
//! Its terminal accessors hand the verbs a whole frame ([`term_lock_whole`]); `blocks` and `wait` read the shell's marks as they stand.

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

    /// The shipped host: [`GuiHost::new`] plus the registry the roster/selector verbs read and the resolved target's context, whose watchers it parks on.
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

    /// Whether this host serves `sid` at all — the fail-closed gate every
    /// per-session method takes first.
    const fn serves(&self, sid: u64) -> bool {
        sid == self.sid
    }
}

/// A [`Subscription`] as a [`ChangeWait`]. Owning it in the box preserves the
/// RAII deregistration: the slot on the session's list lives exactly as long
/// as the handle.
struct SubscriberWait(Subscription);

impl ChangeWait for SubscriberWait {
    /// Sliced while a socket connection is being served ([`crate::park`]): a
    /// slice that ends with no wake asks only after the caller, so the waiting
    /// verb re-reads its session on a wake or at `timeout`, and a hang-up ends
    /// the wait early (`false`), for the verb to ask `caller_gone`.
    fn wait(&self, timeout: Duration) -> bool {
        let until = std::time::Instant::now() + timeout;
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

    /// The registry snapshot, cloned out before formatting so the store lock is
    /// never held across a `Terminal` lock.
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

    /// The registry's one selector lookup (`SessionStore::by_selector`), the one
    /// the dispatcher resolves a request's `@<selector>` through, so the trait
    /// and the wire cannot read a selector two ways.
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

    /// Ended once the registry reads the session `Exited` or its fabric
    /// endpoint is closed.
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

    /// A sid this host does not serve registers on nothing, and its wait can
    /// only time out.
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

    /// The GUI's one conversion ([`crate::cwd_native::native_path`]), so a
    /// block's `cwd=` names the directory exactly as `cwd`, `meta` and the tab
    /// strip do.
    fn native_cwd<'p>(&self, reported: &'p str) -> std::borrow::Cow<'p, str> {
        crate::cwd_native::native_path(reported)
    }
}

/// Whether a verb acts on the hold [`term_lock_whole`] gave it: never on half a frame its caller hung up waiting for (`copy`, `select`, `mainscreen` act, and nobody reads the reply).
fn still_wanted(t: &Terminal) -> bool {
    !(crate::control::caller_hung_up() && crate::frame_unfinished_now(t))
}

/// Runs `aterm-control`'s verb matrix against this host, not only `MemoryHost`.
#[cfg(test)]
mod tests {
    use super::*;
    use aterm_control::conformance;
    use aterm_control::selection::{
        cmd_blocks, cmd_blocks_json, cmd_select, cmd_selection, cmd_wait,
    };
    use aterm_session::{EdgeTable, LaunchNonce, SessionId};

    use crate::session_store::{self, SessionHandle};

    /// What the shipped `GuiHost` claims, declared to the matrix so a host that
    /// dropped a facility cannot pass on the easy arm of each capability-gated
    /// check.
    const GUI_HOST_PROFILE: HostCapabilities = HostCapabilities {
        frame_source: true,
        event_loop: false,
        clipboard: true,
        roster: true,
    };

    /// A session registered in a fresh registry as the GUI registers a tab:
    /// shared engine `Arc`, fabric identity, and a sink over the `-1` master stub.
    fn registered(local_id: u64, term: &Arc<Mutex<Terminal>>) -> SessionHandle {
        let sid = SessionId::generate();
        let nonce = LaunchNonce::generate();
        let observed = crate::session_watch::Observed::default();
        let ctx = Arc::new(crate::SessionCtx {
            sink: Arc::new(SinkWriter::new(-1)),
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
            turns: observed.turns,
            meta: std::sync::Mutex::new(crate::session_timeline::SessionMeta::default()),
            timeline: observed.timeline,
            fabric: std::sync::Arc::default(),
            idem: Default::default(),
            rewrap_gauge: std::sync::Arc::default(),
            human_input: Default::default(),
            licence: Default::default(),
            launch: Default::default(),
            reset_lane: Default::default(),
            crash_journal: Default::default(),
            typed: Default::default(),
            bells: Default::default(),
            screen_moves: Default::default(),
            update_parked: Default::default(),
            witness: Default::default(),
            fenced_writes: Default::default(),
            watchers: observed.watchers,
            frame: Default::default(),
        });
        SessionHandle {
            sid,
            nonce,
            local_id,
            parent: None,
            state: crate::session_store::SessionState::Alive,
            title: format!("tab-{local_id}"),
            term: term.clone(),
            master: -1,
            ctx,
        }
    }

    /// The full matrix passes, run declared ([`GUI_HOST_PROFILE`]), against the shipped
    /// `GuiHost` (registry and sink wired) over real OSC-133 blocks and a live selection.
    #[test]
    fn the_gui_host_passes_the_verb_matrix() {
        let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
        term_lock(&term).process(
            b"\x1b]133;A\x07$ \x1b]633;E;echo hi\x07\x1b]133;B\x07echo hi\n\x1b]133;C\x07hi\n\x1b]133;D;0\x07",
        );
        let store = session_store::new_store();
        let handle = registered(0, &term);
        store.write().unwrap().register(handle.clone());
        let host = GuiHost::with_fleet(0, &term, None, &store, &handle.ctx);
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
    #[test]
    fn the_read_only_subset_leaves_the_gui_host_untouched() {
        let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
        term_lock(&term).process(b"hello world");
        let store = session_store::new_store();
        let handle = registered(0, &term);
        store.write().unwrap().register(handle.clone());
        let host = GuiHost::with_fleet(0, &term, None, &store, &handle.ctx);
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
    /// fleet-wide), so a sid `resolve` hands out cannot reach the wrong
    /// terminal.
    #[test]
    fn a_sibling_sid_is_refused_by_the_session_scoped_host() {
        let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
        term_lock(&term).process(b"hello world");
        let sibling_term = Arc::new(Mutex::new(Terminal::new(24, 80)));
        let store = session_store::new_store();
        let handle = registered(0, &term);
        let sibling = registered(1, &sibling_term);
        store.write().unwrap().register(handle.clone());
        store.write().unwrap().register(sibling.clone());
        let host = GuiHost::with_fleet(0, &term, None, &store, &handle.ctx);

        assert_eq!(host.resolve(Selector::Local(1)), Some(1));
        assert_eq!(host.resolve(Selector::Id(sibling.sid.as_str())), Some(1));
        assert!(host.with_terminal(1, |t: &Terminal| t.rows()).is_none());
        assert!(
            host.with_terminal_mut(1, |t: &mut Terminal| t.text_selection_mut().clear())
                .is_none()
        );
        assert!(
            host.with_terminal_as_it_stands(1, |t: &Terminal| t.rows())
                .is_none()
        );
        assert_eq!(cmd_selection(&host, 1), "ERR no such session\n");
        assert_eq!(cmd_select(&host, 1, "clear"), "ERR no such session\n");
        assert_eq!(cmd_blocks(&host, 1, ""), "ERR no such session\n");
        assert_eq!(cmd_blocks_json(&host, 1, ""), "ERR no such session\n");
        assert_eq!(cmd_wait(&host, 1, "0"), "ERR no such session\n");
        assert!(cmd_selection(&host, 0).starts_with("OK "));
    }

    /// `blocks` names a block's directory the way `cwd` does: on Windows an OSC 7
    /// path like `/C:/Users//x` becomes `C:\Users\x`; a native cmd path and a POSIX
    /// path from WSL or SSH pass through; on Unix nothing is converted (`/C:/` is
    /// a legal directory there).
    #[test]
    fn a_block_cwd_is_the_native_path_the_cwd_verb_prints() {
        let blocks_cwd = |osc: &[u8]| {
            let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
            term_lock(&term).process(osc);
            term_lock(&term).process(b"\x1b]133;A\x07$ ");
            let watchers = Arc::<crate::session_watch::Watchers>::default();
            let host = GuiHost::new(0, &term, None, &watchers);
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

        let (listed, _) = blocks_cwd(b"\x1b]633;P;Cwd=C:\\Windows\\Temp\x07");
        assert!(listed.contains(r" cwd=C:\Windows\Temp "), "{listed}");

        let (listed, _) = blocks_cwd(b"\x1b]7;file:///home/user/proj\x07");
        assert!(listed.contains(" cwd=/home/user/proj "), "{listed}");
    }

    /// A cmd block's cwd is the directory its command ran in. cmd reports its
    /// cwd from the prompt (`633;P;Cwd=$P`) and the engine files it under the
    /// block in progress, so the injection sends it after `133;A` opens the new
    /// block (pinned by `aterm-shell-integration`'s
    /// `test_prepare_cmd_wraps_the_prompt_with_marks_and_cwd`).
    #[test]
    fn a_cmd_block_cwd_is_where_its_command_ran() {
        let cwds = |cwd_after_start: bool| {
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
            let watchers = Arc::<crate::session_watch::Watchers>::default();
            let host = GuiHost::new(0, &term, None, &watchers);
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
        assert_eq!(cwds(false), [r"C:\Windows\Temp", r"C:\Windows\Temp"]);
    }

    /// The roster is the registry's, ascending by sid, with each session's stable
    /// id.
    #[test]
    fn the_roster_is_the_registry_and_an_absent_one_says_so() {
        let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
        let sibling_term = Arc::new(Mutex::new(Terminal::new(24, 80)));
        let watchers = Arc::<crate::session_watch::Watchers>::default();
        let store = session_store::new_store();
        let handle = registered(0, &term);
        let sibling = registered(1, &sibling_term);
        store.write().unwrap().register(sibling.clone());
        store.write().unwrap().register(handle.clone());
        let host = GuiHost::with_fleet(0, &term, None, &store, &handle.ctx);

        let roster = host.sessions();
        assert_eq!(roster.iter().map(|e| e.sid).collect::<Vec<_>>(), vec![0, 1]);
        assert_eq!(roster[0].id, handle.sid.as_str());
        assert_eq!(roster[1].id, sibling.sid.as_str());
        assert_eq!(roster[0].state, SessionState::Alive);
        assert_eq!(roster[0].title, "tab-0");
        assert!(!roster[0].has_meta);

        let scoped = GuiHost::new(0, &term, None, &watchers);
        assert!(!scoped.capabilities().roster);
        assert!(scoped.sessions().is_empty());
        assert_eq!(scoped.resolve(Selector::Local(0)), None);
        assert!(cmd_selection(&scoped, 0).starts_with("OK "));
    }

    /// Closing a tab answers `await inbox` and `wait` parked on it with `ERR exited` at the
    /// close, after which the lanes hold no `SessionCtx` clone (whose sink owns the PTY
    /// master).
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
            let store = app.store.clone();
            std::thread::spawn(move || {
                let host = GuiHost::with_fleet(id, &term, None, &store, &ctx);
                let reply = aterm_control::selection::cmd_wait(&host, id, "600000");
                answered.send(("wait", reply)).unwrap();
            })
        };
        aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "`wait` parks on the tab",
            || (ctx.watchers.count() == 1).then_some(()),
        );
        app.close_session_by_id(id).expect("the tab closes");
        let mut replies = Vec::new();
        for _ in 0..2 {
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
    /// `wait` from the registry's `Exited`, whose record rings it.
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
            let store = app.store.clone();
            std::thread::spawn(move || {
                let host = GuiHost::with_fleet(id, &term, None, &store, &ctx);
                let reply = aterm_control::selection::cmd_wait(&host, id, "600000");
                answered.send(("wait", reply)).unwrap();
            })
        };
        aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "`wait` parks on the tab",
            || (ctx.watchers.count() == 1).then_some(()),
        );
        assert!(
            app.exit_session_logical(id).is_empty(),
            "`--hold` keeps the pane"
        );
        let mut replies = Vec::new();
        for _ in 0..2 {
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

    /// THE EXIT'S RING ENDS A PARKED `wait` BY ITSELF.
    #[test]
    fn an_exited_state_alone_ends_a_parked_wait() {
        let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
        let store = session_store::new_store();
        let handle = registered(0, &term);
        store.write().unwrap().register(handle.clone());
        let (answered, answer) = std::sync::mpsc::channel();
        let wait = {
            let (term, store, ctx) = (term.clone(), store.clone(), handle.ctx.clone());
            std::thread::spawn(move || {
                let host = GuiHost::with_fleet(0, &term, None, &store, &ctx);
                let reply = aterm_control::selection::cmd_wait(&host, 0, "600000");
                answered.send(reply).unwrap();
            })
        };
        aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "`wait` parks on the session",
            || (handle.ctx.watchers.count() == 1).then_some(()),
        );
        store
            .write()
            .unwrap()
            .set_state(0, session_store::SessionState::Exited);
        assert_eq!(
            answer
                .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
                .expect("a wait parked on the exited session never answered"),
            "ERR exited\n"
        );
        assert!(
            !handle.ctx.fabric.endpoint_closed(),
            "the answer came from the registry's state, not the fabric"
        );
        wait.join().unwrap();
    }
}
