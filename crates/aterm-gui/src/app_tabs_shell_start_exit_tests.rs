// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::time::{Duration, Instant};

fn run_to_zombie(script: &str) -> i32 {
    let child = aterm_uds::spawnfd::command("/bin/sh")
        .args(["-c", script])
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .stdout(aterm_uds::spawnfd::Stdio::null())
        .stderr(aterm_uds::spawnfd::Stdio::null())
        .spawn()
        .expect("spawn the stand-in shell");
    let pid = i32::try_from(child.id()).expect("pid fits");
    std::mem::forget(child);
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        // SAFETY: a zeroed `siginfo_t` out-param for a non-reaping peek.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        // SAFETY: `WNOWAIT` leaves the child waitable; `WNOHANG` never blocks.
        let rc = unsafe {
            libc::waitid(
                libc::P_PID,
                pid as libc::id_t,
                &mut info,
                libc::WEXITED | libc::WNOWAIT | libc::WNOHANG,
            )
        };
        assert_eq!(rc, 0, "waitid on our own child");
        // SAFETY: `waitid` either filled a `CLD_*` siginfo or, under `WNOHANG` with nothing waitable, left it zeroed; `si_pid` is defined in both.
        let exited = unsafe { info.si_pid() };
        if exited == pid {
            return pid;
        }
        assert!(Instant::now() < deadline, "the stand-in shell did not exit");
        std::thread::sleep(Duration::from_millis(5));
    }
}

type Start = Option<(&'static str, Instant)>;

fn started(name: &'static str, at: Instant) -> Start {
    Some((name, at))
}

fn eof_at_start(at: Instant) -> Option<Instant> {
    Some(at + Duration::from_millis(100))
}

fn app_with(pid: i32, start: Start, closed: Option<Instant>) -> (App, WindowId, u64) {
    let sink = std::sync::Arc::new(aterm_session::sink::SinkWriter::new(-1));
    app_with_sink(pid, start, closed, sink)
}

fn app_with_sink(
    pid: i32,
    start: Start,
    closed: Option<Instant>,
    sink: std::sync::Arc<aterm_session::sink::SinkWriter>,
) -> (App, WindowId, u64) {
    let mut app = App::headless_for_test();
    let id = app.next_session_id;
    let mut session = crate::stub_session_with_sink(id, sink);
    session.pid = pid;
    session.shell_identity = aterm_pty::record_spawned_shell(pid);
    session.shell_start = start.map(|(name, at)| crate::spawn::ShellStart {
        at,
        name: name.to_string(),
        input: session.ctx.sink.driven_input(),
    });
    if let Some(at) = closed {
        let _ = session.pty_closed_at.set(at);
    }
    App::register_session(&app.store, &session, None);
    let wid = app.insert_logical_window(session, 24, 80);
    (app, wid, id)
}

fn ledger_code(app: &App, session: u64) -> Option<i32> {
    app.store
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .noted_exit_code(session)
}

fn reader_eof(app: &mut App, session: u64) -> Vec<WindowId> {
    app.note_session_exit(session);
    app.retire_input_watch(session);
    app.exit_session_logical(session)
}

fn pane_text(app: &App, session: u64) -> String {
    term_lock(&app.pool.get(session).expect("the pane's session").term).visible_content()
}

fn log_close_due(app: &App, session: u64) -> bool {
    app.pool.get(session).is_some_and(|s| {
        s.ctx
            .timeline
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .slog()
            .close_due()
    })
}

fn assert_kept(app: &App, wid: WindowId, session: u64, to_close: &[WindowId], line: &str) {
    assert!(to_close.is_empty(), "nothing closes: {to_close:?}");
    assert!(
        app.window_contains_session(wid, session),
        "the pane stays in its window"
    );
    assert!(
        pane_text(app, session).contains(line),
        "the pane says why: {:?}",
        pane_text(app, session)
    );
    let reaped = app
        .pool
        .get(session)
        .expect("still pooled")
        .child_reaped
        .load(std::sync::atomic::Ordering::Acquire);
    assert!(reaped, "the exit path reaped the child");
    assert_eq!(
        app.store
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .by_local(session)
            .map(|h| h.state),
        Some(session_store::SessionState::Exited),
        "the registry still says the session exited"
    );
    assert!(
        !log_close_due(app, session),
        "the log knows no close is coming"
    );
}

fn parked_close_wait(app: &App, session: u64) -> std::sync::mpsc::Receiver<String> {
    let pooled = app.pool.get(session).expect("the pane's session");
    let (term, ctx) = (pooled.term.clone(), std::sync::Arc::clone(&pooled.ctx));
    let (ep, head) = {
        let tl = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
        (tl.slog().log().ep(), tl.slog().log().last_seq())
    };
    let store = app.store.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(crate::control::cmd_await(
            &term,
            &store,
            session,
            &ctx,
            &format!("log close=session since={head} ep={ep} timeout=600000"),
        ));
    });
    rx
}

#[test]
fn an_escalated_exit_keeps_the_close_due_until_its_window_closes() {
    let pid = run_to_zombie("exit 0");
    let (mut app, wid, id) = app_with(pid, None, Some(Instant::now()));
    let waiter = parked_close_wait(&app, id);
    assert_eq!(reader_eof(&mut app, id), vec![wid], "the window closes");
    assert!(log_close_due(&app, id), "its close comes with its window's");
    let _ = app.close_window_logical(wid);
    app.settle_log_close(id);
    let reply = waiter
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .expect("answered by the window's close");
    assert!(
        reply.contains(" close ") && reply.contains(" out=shell-exit"),
        "{reply}"
    );

    let pid = run_to_zombie("exit 0");
    let (mut app, wid, id) = app_with(pid, None, Some(Instant::now()));
    let waiter = parked_close_wait(&app, id);
    assert_eq!(reader_eof(&mut app, id), vec![wid], "the window closes");
    app.settle_log_close(id);
    assert!(!log_close_due(&app, id), "no close is coming");
    assert_eq!(
        waiter
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .expect("answered by the settle"),
        "ERR exited\n"
    );
}

fn close_probe(app: &App, session: u64) -> String {
    let pooled = app.pool.get(session).expect("the pane's session");
    crate::control::cmd_await(
        &pooled.term,
        &app.store,
        session,
        &pooled.ctx,
        "log close=session timeout=0",
    )
}

#[test]
fn an_exit_a_handoff_deferred_keeps_its_close_due_until_the_replay() {
    let pid = run_to_zombie("exit 0");
    let (mut app, wid, id) = app_with(pid, None, Some(Instant::now()));
    let waiter = parked_close_wait(&app, id);
    app.pending_update_handoff = Some(crate::messages_host::tests::parked());
    assert!(
        reader_eof(&mut app, id).is_empty(),
        "the handoff holds the close"
    );
    app.settle_log_close(id);
    assert!(
        log_close_due(&app, id),
        "the replay's retire writes the close"
    );
    assert_eq!(
        close_probe(&app, id),
        "OK timeout\n",
        "the close is still to come"
    );
    let teardown = app
        .pending_update_handoff
        .take()
        .expect("the attempt's record")
        .teardown;
    let deferred = std::mem::take(&mut app.log_closes_deferred);
    assert_eq!(deferred, std::collections::BTreeSet::from([id]));
    assert_eq!(
        teardown,
        crate::DeferredHandoffTeardown::mutation(crate::DeferredHandoffMutation::ExitSession(id)),
        "the exit is what the handoff deferred"
    );
    assert_eq!(
        app.exit_session_logical(id),
        vec![wid],
        "the replay closes the window"
    );
    let _ = app.close_window_logical(wid);
    for session in deferred {
        app.settle_log_close(session);
    }
    let reply = waiter
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .expect("answered by the replay's close");
    assert!(
        reply.contains(" close ") && reply.contains(" out=shell-exit"),
        "{reply}"
    );

    let pid = run_to_zombie("exit 0");
    let (mut app, _wid, id) = app_with(pid, None, Some(Instant::now()));
    let waiter = parked_close_wait(&app, id);
    app.pending_update_handoff = Some(crate::messages_host::tests::parked());
    assert!(reader_eof(&mut app, id).is_empty());
    assert!(
        app.defer_pending_update_handoff_teardown(crate::DeferredHandoffTeardown::QuitRequested),
        "a quit dominates the deferred exit"
    );
    assert!(log_close_due(&app, id), "still the attempt's to settle");
    let teardown = app
        .pending_update_handoff
        .take()
        .expect("the attempt's record")
        .teardown;
    let deferred = std::mem::take(&mut app.log_closes_deferred);
    assert_eq!(teardown, crate::DeferredHandoffTeardown::QuitRequested);
    for session in deferred {
        app.settle_log_close(session);
    }
    assert!(!log_close_due(&app, id), "no close is coming");
    assert_eq!(
        waiter
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .expect("answered by the completion's settle"),
        "ERR exited\n"
    );
}

#[test]
fn a_shell_that_exits_3_at_start_keeps_its_pane_with_the_line() {
    let at = Instant::now();
    let pid = run_to_zombie("exit 3");
    let (mut app, wid, id) = app_with(pid, started("zsh", at), eof_at_start(at));

    let to_close = reader_eof(&mut app, id);

    assert_kept(&app, wid, id, &to_close, "[zsh ended at start: exit 3]");
    assert_eq!(
        app.session_status.status(id).map(|s| s.last_outcome),
        Some(crate::session_status::Outcome::Failure { exit_code: 3 }),
        "the classification is unchanged"
    );
    assert_eq!(ledger_code(&app, id), Some(3), "the ledger has the code");
    for look in [ExitLook::First, ExitLook::Retry] {
        assert_eq!(
            app.exit_status(id, look),
            Some(aterm_pty::ChildExit::Code(3)),
            "{look:?} reads the status the first look kept"
        );
    }
    assert_eq!(
        app.close_session_by_id(id),
        Ok(CloseProgress::Deferred(wid)),
        "an explicit close still takes it"
    );
    let _ = app.close_window_logical(wid);
    assert!(app.pool.get(id).is_none(), "and the session is retired");
}

#[test]
fn a_shell_that_ends_at_start_keeps_its_pane_whoever_reads_the_status() {
    for (script, shell, tab_status, slow, line, code) in [
        (
            "kill -KILL $$",
            "fish",
            None,
            false,
            "[fish ended at start: signal 9]",
            None,
        ),
        (
            "kill -KILL $$",
            "fish",
            Some(false),
            false,
            "[fish ended at start: signal 9]",
            None,
        ),
        (
            "exit 4",
            "zsh",
            Some(false),
            false,
            "[zsh ended at start: exit 4]",
            Some(4),
        ),
        (
            "exit 3",
            "zsh",
            None,
            true,
            "[zsh ended at start: exit 3]",
            Some(3),
        ),
    ] {
        let pid = run_to_zombie(script);
        let (at, closed) = if slow {
            let at = Instant::now()
                .checked_sub(Duration::from_secs(3))
                .expect("the clock reaches back 3 s");
            (at, Some(at + Duration::from_secs(1)))
        } else {
            let at = Instant::now();
            (at, eof_at_start(at))
        };
        let (mut app, wid, id) = app_with(pid, started(shell, at), closed);
        app.config.tab_status = tab_status;

        let to_close = reader_eof(&mut app, id);

        assert_kept(&app, wid, id, &to_close, line);
        assert_eq!(
            ledger_code(&app, id),
            code,
            "{script} tab_status={tab_status:?} slow={slow}"
        );
    }
}

#[test]
fn a_shell_that_got_input_closes_as_before_even_on_exit_1() {
    let at = Instant::now();
    let pid = run_to_zombie("exit 1");
    let mut fds = [0; 2];
    // SAFETY: a fresh two-slot out-param for `pipe(2)`.
    assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0, "pipe");
    let sink = std::sync::Arc::new(aterm_session::sink::SinkWriter::new(fds[1]));
    let (mut app, wid, id) = app_with_sink(pid, started("zsh", at), eof_at_start(at), sink.clone());
    sink.write_frame(b"\x04")
        .expect("the Ctrl-D reaches the PTY");

    assert_eq!(reader_eof(&mut app, id), vec![wid], "the window closes");
    assert!(!pane_text(&app, id).contains("ended at start"));
    assert_eq!(ledger_code(&app, id), Some(1), "the ledger is unchanged");

    drop(app);
    drop(sink);
    // SAFETY: both ends of the pipe opened above; the borrowed sink that used the write end is gone.
    unsafe {
        libc::close(fds[0]);
        libc::close(fds[1]);
    }
}

#[test]
fn a_shell_that_was_only_answered_keeps_its_pane() {
    let at = Instant::now();
    let pid = run_to_zombie("exit 1");
    let mut fds = [0; 2];
    // SAFETY: a fresh two-slot out-param for `pipe(2)`.
    assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0, "pipe");
    let sink = std::sync::Arc::new(aterm_session::sink::SinkWriter::new(fds[1]));
    let (mut app, wid, id) =
        app_with_sink(pid, started("fish", at), eof_at_start(at), sink.clone());
    let (lane, _writer) = crate::spawn::spawn_reply_writer_for_test(sink.clone());
    assert!(
        lane.submit(std::sync::Arc::from(&b"\x1b[?62;22c"[..])),
        "queued"
    );
    let mut ready = libc::pollfd {
        fd: fds[0],
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: one live pollfd; a minute is a hang detector, not a budget.
    assert_eq!(
        unsafe { libc::poll(&mut ready, 1, 60_000) },
        1,
        "the reply arrives"
    );

    let to_close = reader_eof(&mut app, id);

    assert_kept(&app, wid, id, &to_close, "[fish ended at start: exit 1]");
    drop((app, lane, sink));
    // SAFETY: both ends of the pipe opened above; the borrowed sink that used the write end is gone.
    unsafe {
        libc::close(fds[0]);
        libc::close(fds[1]);
    }
}

#[test]
fn an_exit_that_is_no_failure_at_start_closes_as_today() {
    let now = Instant::now();
    let before_window = now
        .checked_sub(SHELL_START_WINDOW + Duration::from_millis(500))
        .expect("the clock reaches back past the start window");
    for (case, script, start, closed, prompt) in [
        (
            "clean exit",
            "exit 0",
            started("zsh", now),
            eof_at_start(now),
            false,
        ),
        ("no recorded start", "exit 1", None, Some(now), false),
        (
            "after the window",
            "exit 3",
            started("zsh", before_window),
            Some(now),
            false,
        ),
        (
            "drew its prompt",
            "kill -KILL $$",
            started("zsh", now),
            eof_at_start(now),
            true,
        ),
    ] {
        let pid = run_to_zombie(script);
        let (mut app, wid, id) = app_with(pid, start, closed);
        if prompt {
            term_lock(&app.pool.get(id).expect("the pane's session").term)
                .process(b"\x1b]133;A\x07% \x1b]133;B\x07");
        }

        assert_eq!(
            reader_eof(&mut app, id),
            vec![wid],
            "{case}: the window closes"
        );
        assert!(!pane_text(&app, id).contains("ended at start"), "{case}");
        assert!(
            log_close_due(&app, id),
            "{case}: its close is coming with its window's"
        );
    }
}

#[test]
fn hold_keeps_its_own_behaviour() {
    let at = Instant::now();
    let pid = run_to_zombie("exit 3");
    let (mut app, wid, id) = app_with(pid, started("zsh", at), eof_at_start(at));
    app.hold = true;

    assert!(reader_eof(&mut app, id).is_empty());
    assert!(app.window_contains_session(wid, id), "held, as always");
    assert!(!pane_text(&app, id).contains("ended at start"));
    assert!(!log_close_due(&app, id), "the log knows no close is coming");
}

#[test]
fn a_shared_shell_that_fails_at_start_stays_in_every_viewer() {
    let at = Instant::now();
    let pid = run_to_zombie("exit 1");
    let (mut app, wid, id) = app_with(pid, started("zsh", at), eof_at_start(at));
    app.frontmost_window = Some(wid);
    let second = app
        .open_active_session_in_new_window_logical()
        .expect("share the pane into a second window");

    let to_close = reader_eof(&mut app, id);

    assert_kept(&app, wid, id, &to_close, "[zsh ended at start: exit 1]");
    assert!(!log_close_due(&app, id), "the log knows no close is coming");
    assert!(app.window_contains_session(second, id));
    assert_eq!(
        pane_text(&app, id).matches("ended at start").count(),
        1,
        "one line, not one per viewer"
    );
}

#[test]
fn an_adopted_session_never_reaps_the_child_its_freed_pid_now_names() {
    let pid = run_to_zombie("exit 3");
    let (mut app, _wid, id) = app_with(pid, None, None);
    app.pool
        .sessions
        .get_mut(&id)
        .expect("pooled")
        .session
        .shell_identity = aterm_pty::ShellIdentity::from_parts(pid, None, false);

    let _ = reader_eof(&mut app, id);

    for look in [ExitLook::First, ExitLook::Retry] {
        assert_eq!(app.exit_status(id, look), None, "{look:?}: unknown");
    }
    assert_eq!(
        ledger_code(&app, id),
        None,
        "exit_code=-, never the stranger's 3"
    );
    let mut status: libc::c_int = 0;
    // SAFETY: a non-blocking wait on this test's own zombie child.
    let reaped = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
    assert_eq!(reaped, pid, "the zombie was left for its owner");
    assert_eq!(
        libc::WEXITSTATUS(status),
        3,
        "whose wait reads its own status"
    );
}

#[cfg(target_vendor = "apple")]
#[test]
fn an_adopted_shell_s_exit_status_comes_from_its_exit_watch() {
    use std::io::Write as _;
    let mut shell = aterm_uds::spawnfd::command("/bin/sh")
        .args(["-c", "read _; exit 5"])
        .stdin(aterm_uds::spawnfd::Stdio::piped())
        .stdout(aterm_uds::spawnfd::Stdio::null())
        .stderr(aterm_uds::spawnfd::Stdio::null())
        .spawn()
        .expect("spawn the shell");
    let pid = i32::try_from(shell.id()).expect("pid fits");
    let (mut app, _wid, id) = app_with(pid, None, None);
    {
        let session = &mut app.pool.sessions.get_mut(&id).expect("pooled").session;
        let birth = aterm_pty::record_spawned_shell(pid).birth;
        assert!(
            birth.is_some(),
            "PRECONDITION: the live shell's birth reads"
        );
        session.shell_identity = aterm_pty::ShellIdentity::from_parts(pid, birth, false);
        session.exit_watch = crate::spawn::adopted_exit_watch(&session.shell_identity);
        assert!(
            session.exit_watch.is_some(),
            "the live adopted shell is watched"
        );
    }
    shell
        .stdin
        .take()
        .expect("stdin")
        .write_all(b"\n")
        .expect("release the shell");
    assert_eq!(shell.wait().expect("its parent reaps it").code(), Some(5));

    let _ = reader_eof(&mut app, id);

    assert_eq!(ledger_code(&app, id), Some(5), "the ledger has the code");
    assert_eq!(
        app.session_status.status(id).map(|s| s.last_outcome),
        Some(crate::session_status::Outcome::Failure { exit_code: 5 }),
        "the classifier has the failure"
    );
    for look in [ExitLook::First, ExitLook::Retry] {
        assert_eq!(
            app.exit_status(id, look),
            Some(aterm_pty::ChildExit::Code(5)),
            "{look:?} reads the witnessed status"
        );
    }
}
