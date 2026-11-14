// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `restart account=<label>` and `restart account-auto` against a real process on a real pty:
//! what an accepted ask stamps, signals and replies, that a refused one leaves nothing, and that
//! a move ends a session only at its own rest.

use super::*;
use aterm_agent::harness::accounts_pool;
use aterm_agent::harness::restart_stamp::{Cause, Stamp, Wanted};

const THREAD: &str = "0199f2a4-6c1e-7a52-9d3b-2f6c8e1a4b77";

/// One of these tests at a time: every ask is judged among at most `ACCOUNT_JUDGES` judgements
/// this process runs at once, and one more is answered `account-unread`, so tests that reach
/// their first judgement together would refuse each other.
fn serial() -> std::sync::MutexGuard<'static, ()> {
    static SERIAL: Mutex<()> = Mutex::new(());
    SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The fields of a codex-alab session record at its rest ([`self_claimed::idle_point`]).
const IDLE: &str = r#""status":"idle","draftEmpty":true,"state":"idle","exit":null"#;

/// The fields of a codex-alab session record whose turn runs.
const BUSY: &str = r#""status":"busy","draftEmpty":true,"state":"working","exit":null"#;

/// Writes the codex-alab session record of process `pid` at `path` as codex-alab writes it
/// (renamed into place): its version, `pid`, its thread and `fields`.
fn record_at(path: &std::path::Path, pid: i32, fields: &str) {
    let dir = path.parent().expect("its folder");
    std::fs::create_dir_all(dir).expect("the sessions folder");
    let tmp = dir.join(format!("{pid}.json.tmp"));
    let text = format!(r#"{{"v":1,"pid":{pid},"sessionId":"{THREAD}",{fields}}}"#);
    std::fs::write(&tmp, text).expect("the record");
    std::fs::rename(&tmp, path).expect("renamed into place");
}

/// Where `agent`'s codex-alab session record is: under the `CODEX_HOME` its copy was read at.
fn record_path(agent: &RealAgent) -> std::path::PathBuf {
    self_claimed::record_path(&agent.home.path().join(".codex"), agent.upid())
}

/// Writes `agent`'s own session record with `fields` ([`record_at`]).
fn record(agent: &RealAgent, fields: &str) {
    record_at(&record_path(agent), agent.pid, fields);
}

/// A real process leading a real pty that this tab's validated self-claim copy and the host's
/// published snapshot name as a codex-alab on this machine's own login, under the host's claim,
/// its own session record at its rest: what the restart act would carry. `shell` is the shell
/// the snapshot names under it.
fn codex_alab(local_id: u64, shell: u32) -> RealAgent {
    let agent = RealAgent::spawn(false, local_id);
    record(&agent, IDLE);
    let copy = self_claimed::Copy {
        lifetime: None,
        pid: agent.upid(),
        start: agent.start.clone(),
        home: agent.home.path().join(".codex"),
        seen: standing_record(),
        resume: Ok(self_claimed::Resume {
            argv: vec![
                "/u/.aterm/agents/codex".to_string(),
                "resume".to_string(),
                THREAD.to_string(),
            ],
            session: THREAD.to_string(),
            codex_home: None,
            codex_auth_path: None,
            cwd: None,
        }),
    };
    let resume = copy.resume.clone().expect("a line");
    let snap =
        aterm_agent::harness::relaunch::snapshot_of_copy(&agent.sid, &copy, &resume, shell, None);
    crate::self_claim::set_copy_for_test(&agent.sid, Some(copy));
    crate::harness_host::publish_snapshot_for_test(&agent.sid, Some(&snap));
    agent.claim();
    agent
}

/// Forget what [`codex_alab`] published for `agent`'s tab.
fn forget(agent: &RealAgent) {
    crate::harness_host::publish_snapshot_for_test(&agent.sid, None);
    crate::self_claim::set_copy_for_test(&agent.sid, None);
}

/// The `restart` verb with `rest` on `agent`, its stamps under `state`, where the pool answers
/// an account ask with `word`.
fn restart(
    agent: &RealAgent,
    state: &std::path::Path,
    rest: &str,
    word: Option<&'static str>,
) -> String {
    restart_judged(agent, state, rest, move |_, _| word)
}

/// [`restart`], where `judge` is the pool's word on an account ask.
fn restart_judged(
    agent: &RealAgent,
    state: &std::path::Path,
    rest: &str,
    judge: impl Fn(&crate::harness_host::RunningLaunch, &Wanted) -> Option<&'static str>
    + Send
    + Sync
    + 'static,
) -> String {
    let _judge = crate::restart_act::judge_for_test(Arc::new(judge), aterm_spec::wait::MUST_HAPPEN);
    crate::harness_host::with_harness_state(state, || {
        crate::control::restart_verb(agent.master, rest, &agent.ctx, aterm_slog::Actor::Ctl)
    })
}

/// The stamp standing for `agent` under `state`.
fn stamp_of(agent: &RealAgent, state: &std::path::Path) -> Option<Stamp> {
    restart_stamp::read(state, &agent.sid).filter(|s| s.names(agent.upid(), &agent.start))
}

/// AN ACCEPTED ACCOUNT RESTART STAMPS ITS DESTINATION, SIGNALS, AND SAYS WHAT IT ASKED: the
/// stamp its relaunch reads names the row, or the pool's choice, and the reply ends in that
/// word. A row labelled `auto` is asked as `account=auto` and answered so; the pool's choice is
/// the bare word `account-auto` both ways. A stalled session's reply keeps `cause=stall`.
/// CONTROL: a plain restart's stamp names no account and its reply is what it was.
#[test]
fn an_account_restart_stamps_its_destination_and_says_what_it_asked() {
    let _serial = serial();
    let state = aterm_tempfile::TempDir::new().expect("state");
    let run = |n: u64, rest: &str, stalled: bool| {
        let agent = codex_alab(7760 + n, 1);
        if stalled {
            agent.stall();
        }
        let _ends_on_its_term =
            crate::restart_act::deadline_for_test(aterm_spec::wait::MUST_HAPPEN);
        let reply = restart(&agent, state.path(), rest, None);
        let stamp = stamp_of(&agent, state.path()).expect("its stamp");
        until("it ended on the act's SIGTERM", || agent.ended().is_some());
        assert_eq!(agent.acts("restart"), 1, "{rest}: one act mark");
        forget(&agent);
        (reply, agent.pid, stamp.cause, stamp.wanted)
    };
    let row = |label: &str| Some(Wanted::Row(label.to_string()));
    let (reply, pid, cause, wanted) = run(0, "account=auto", false);
    assert_eq!(
        reply,
        format!("OK restarting pid {pid} cause=asked account=auto\n")
    );
    assert_eq!((cause, wanted), (Cause::Asked, row("auto")));
    let (reply, pid, cause, wanted) = run(1, "account-auto quiet=0", false);
    assert_eq!(
        reply,
        format!("OK restarting pid {pid} cause=asked account-auto\n")
    );
    assert_eq!((cause, wanted), (Cause::Asked, Some(Wanted::Pool)));
    let (reply, pid, cause, wanted) = run(2, "account=work", true);
    assert_eq!(
        reply,
        format!("OK restarting pid {pid} cause=stall account=work\n")
    );
    assert_eq!((cause, wanted), (Cause::Stall, row("work")));
    let (reply, pid, cause, wanted) = run(3, "", false);
    assert_eq!(
        reply,
        format!("OK restarting pid {pid} cause=asked\n"),
        "CONTROL: a plain restart"
    );
    assert_eq!((cause, wanted), (Cause::Asked, None));
}

/// AN ACCOUNT RESTART'S STAMP CARRIES THE RECEIPT BESIDE ITS DESTINATION: the stamp an
/// `account=<label>` act writes names the row and this host's input counter as the act read
/// it, the fence its resume line holds earlier typing to. CONTROL: a plain restart's stamp
/// carries the same receipt and names no account.
#[test]
fn an_account_restarts_stamp_carries_this_hosts_input_receipt() {
    let _serial = serial();
    let state = aterm_tempfile::TempDir::new().expect("state");
    let host = restart_stamp::input_host().expect("this host's incarnation reads");
    let run = |n: u64, rest: &str, typed: u64| {
        let agent = codex_alab(7770 + n, 1);
        for at in 1..=typed {
            agent
                .ctx
                .human_input
                .note(at, crate::human_input::Gesture::Typed);
        }
        let seq = agent.ctx.human_input.seq();
        let _ends_on_its_term =
            crate::restart_act::deadline_for_test(aterm_spec::wait::MUST_HAPPEN);
        let reply = restart(&agent, state.path(), rest, None);
        assert!(reply.starts_with("OK restarting"), "{rest}: {reply}");
        let stamp = stamp_of(&agent, state.path()).expect("its stamp");
        until("it ended on the act's SIGTERM", || agent.ended().is_some());
        forget(&agent);
        (seq, stamp.wanted, stamp.input)
    };
    let (seq, wanted, input) = run(0, "account=work quiet=0", 3);
    assert_eq!(seq, 3, "the typing counted");
    assert_eq!(wanted, Some(Wanted::Row("work".to_string())));
    assert_eq!(
        input,
        Some(restart_stamp::Input {
            host: host.clone(),
            seq,
        }),
        "an account ask keeps the receipt"
    );
    let (seq, wanted, input) = run(1, "quiet=0", 2);
    assert_eq!(
        (wanted, input),
        (None, Some(restart_stamp::Input { host, seq })),
        "CONTROL: a plain restart"
    );
}

/// A REFUSED ACCOUNT RESTART LEAVES NOTHING: no stamp, no act mark, no signal. Each word is
/// reached through the verb: the pool's own word, the act's `account-not-resumable` for a
/// session with no shell under it, `account-restart-pending` under a standing restart for
/// another destination (which stands as it was), and `account-unmanaged` for an agent the tab
/// holds no self-claim copy of. The act's own fences answer first: under another driver's lease
/// the reply is `driver-lease`, not the pool's word. CONTROL: the same session, asked for an
/// account the pool accepts, is restarted.
#[test]
fn a_refused_account_restart_writes_nothing_and_sends_nothing() {
    let _serial = serial();
    let state = aterm_tempfile::TempDir::new().expect("state");
    let agent = codex_alab(7770, 1);
    let untouched = |why: &str| {
        assert!(
            restart_stamp::read(state.path(), &agent.sid).is_none(),
            "{why}: no stamp"
        );
        assert_eq!(agent.acts("restart"), 0, "{why}: no act mark");
        assert!(
            agent.ended().is_none() && agent.record_left(),
            "{why}: nothing sent"
        );
    };
    let refused = |why: &str| format!("ERR restart unavailable: {why}\n");
    for word in [
        accounts_pool::MOVE_OFF,
        accounts_pool::MOVE_UNKNOWN,
        accounts_pool::MOVE_NO_CHOICE,
        accounts_pool::MOVE_CURRENT,
        accounts_pool::MOVE_SAME,
        accounts_pool::MOVE_NOT_ENROLLED,
        accounts_pool::MOVE_SIGNED_OUT,
        accounts_pool::MOVE_LIMITED,
        accounts_pool::MOVE_HELD,
        accounts_pool::MOVE_IDENTITY,
        accounts_pool::MOVE_UNREAD,
        accounts_pool::MOVE_UNMANAGED,
    ] {
        assert_eq!(
            restart(&agent, state.path(), "account=work", Some(word)),
            refused(word)
        );
        untouched(word);
    }
    *agent.ctx.turn_lease.lock().unwrap() = Some(crate::Lease::Drive {
        holder: "someone".to_string(),
        expires_us: crate::metrics::now_us() + 60_000_000,
        conn: None,
        hard: false,
        here: false,
        holder_pid: None,
    });
    assert_eq!(
        restart(
            &agent,
            state.path(),
            "account=work",
            Some(accounts_pool::MOVE_LIMITED)
        ),
        refused("driver-lease"),
        "the act's own fence answers before the pool's word"
    );
    untouched("a driver's lease");
    *agent.ctx.turn_lease.lock().unwrap() = None;
    let standing = Stamp {
        pid: agent.upid(),
        start: agent.start.clone(),
        cause: Cause::Asked,
        at: aterm_time::unix_secs().unwrap_or(0),
        wanted: Some(Wanted::Pool),
        input: None,
    };
    restart_stamp::write(state.path(), &agent.sid, &standing).expect("a standing restart");
    assert_eq!(
        restart(&agent, state.path(), "account=work", None),
        refused(crate::restart_act::ACCOUNT_RESTART_PENDING)
    );
    assert_eq!(
        restart_stamp::read(state.path(), &agent.sid),
        Some(standing),
        "the standing restart is as it was"
    );
    assert_eq!(agent.acts("restart"), 0, "no act mark");
    assert!(
        agent.ended().is_none() && agent.record_left(),
        "nothing sent"
    );
    let _ = restart_stamp::take_if(state.path(), &agent.sid, |_| true);
    untouched("the standing restart taken");
    let _ends_on_its_term = crate::restart_act::deadline_for_test(aterm_spec::wait::MUST_HAPPEN);
    assert_eq!(
        restart(&agent, state.path(), "account=work", None),
        format!("OK restarting pid {} cause=asked account=work\n", agent.pid),
        "CONTROL: an account the pool accepts"
    );
    until("it ended on the act's SIGTERM", || agent.ended().is_some());
    forget(&agent);

    let bare = codex_alab(7771, aterm_agent::harness::relaunch::NO_SHELL);
    assert_eq!(
        restart(&bare, state.path(), "account=work", None),
        refused(crate::restart_act::ACCOUNT_NOT_RESUMABLE),
        "the tab's own program"
    );
    assert!(restart_stamp::read(state.path(), &bare.sid).is_none());
    assert!(bare.ended().is_none() && bare.record_left());
    forget(&bare);

    // A Claude Code the host relaunches, of which the tab holds no self-claim copy.
    let claude = RealAgent::spawn(false, 7772);
    let (a, host) = claude.world();
    claude.claim();
    let _judge =
        crate::restart_act::judge_for_test(Arc::new(|_, _| None), aterm_spec::wait::MUST_HAPPEN);
    assert_eq!(
        claude.restart(&a, "account=work"),
        refused(accounts_pool::MOVE_UNMANAGED),
        "no self-claim copy"
    );
    assert!(restart_stamp::read(a.stamp_dir(), &claude.sid).is_none());
    assert!(claude.ended().is_none() && claude.record_left());
    host.shutdown_and_join();
}

/// A PLAIN RESTART THAT JOINS AN ACCOUNT'S STAMP KEEPS ITS DESTINATION: the stamp is the first
/// act's, the relaunch is the one both asked for, and the plain restart's reply is what a plain
/// restart's always was.
#[test]
fn a_plain_restart_joining_an_accounts_stamp_keeps_its_destination() {
    let _serial = serial();
    let state = aterm_tempfile::TempDir::new().expect("state");
    let agent = codex_alab(7775, 1);
    let work = Some(Wanted::Row("work".to_string()));
    let standing = Stamp {
        pid: agent.upid(),
        start: agent.start.clone(),
        cause: Cause::Asked,
        at: aterm_time::unix_secs().unwrap_or(0),
        wanted: work.clone(),
        input: None,
    };
    restart_stamp::write(state.path(), &agent.sid, &standing).expect("the account's stamp");
    let _ends_on_its_term = crate::restart_act::deadline_for_test(aterm_spec::wait::MUST_HAPPEN);
    assert_eq!(
        restart(&agent, state.path(), "", None),
        format!("OK restarting pid {} cause=asked\n", agent.pid)
    );
    assert_eq!(
        stamp_of(&agent, state.path()).and_then(|stamp| stamp.wanted),
        work,
        "the joined stamp, renewed, still names the account"
    );
    until("it ended on the act's SIGTERM", || agent.ended().is_some());
    forget(&agent);
}

/// THE ACT'S LOCK IS NOT HELD WHILE AN ACCOUNT IS JUDGED: while one tab's ask waits on a pool
/// that has not answered, another tab's plain restart is carried. Held under the lock, that
/// second restart would return only once the first's wait ran out and its ask had answered.
/// CONTROL: once the pool answers, the first ask is carried too.
#[test]
fn an_account_judgement_holds_no_other_restart() {
    let _serial = serial();
    let state = aterm_tempfile::TempDir::new().expect("state");
    let (asking, other) = (codex_alab(7780, 1), codex_alab(7781, 1));
    let (release, held) = std::sync::mpsc::channel::<()>();
    let held = Mutex::new(held);
    let (judging, began) = std::sync::mpsc::channel::<()>();
    let judging = Mutex::new(judging);
    let _ends_on_its_term = crate::restart_act::deadline_for_test(aterm_spec::wait::MUST_HAPPEN);
    let reply = std::thread::scope(|scope| {
        let waits = scope.spawn(|| {
            let _ends_on_its_term =
                crate::restart_act::deadline_for_test(aterm_spec::wait::MUST_HAPPEN);
            let _judge = crate::restart_act::judge_for_test(
                Arc::new(move |_, _| {
                    let _ = judging.lock().unwrap().send(());
                    let _ = held.lock().unwrap().recv();
                    None
                }),
                aterm_spec::wait::MUST_HAPPEN,
            );
            crate::harness_host::with_harness_state(state.path(), || {
                crate::control::restart_verb(
                    asking.master,
                    "account=work",
                    &asking.ctx,
                    aterm_slog::Actor::Ctl,
                )
            })
        });
        began
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .expect("the judgement began");
        assert_eq!(
            restart(&other, state.path(), "", None),
            format!("OK restarting pid {} cause=asked\n", other.pid),
            "another tab's restart is carried"
        );
        assert!(
            !waits.is_finished(),
            "and it was carried while the judgement still waited"
        );
        drop(release);
        waits.join().expect("the ask answered")
    });
    assert_eq!(
        reply,
        format!(
            "OK restarting pid {} cause=asked account=work\n",
            asking.pid
        ),
        "CONTROL: the pool answered"
    );
    until("both ended on their SIGTERMs", || {
        asking.ended().is_some() && other.ended().is_some()
    });
    forget(&asking);
    forget(&other);
}

/// A POOL THAT DOES NOT ANSWER IN TIME REFUSES THE ASK AS `account-unread`: nothing is stamped
/// and nothing sent.
#[test]
fn an_account_judgement_that_does_not_answer_refuses_the_ask() {
    let _serial = serial();
    let state = aterm_tempfile::TempDir::new().expect("state");
    let agent = codex_alab(7785, 1);
    let (release, held) = std::sync::mpsc::channel::<()>();
    let held = Mutex::new(held);
    let reply = {
        let _judge = crate::restart_act::judge_for_test(
            Arc::new(move |_, _| {
                let _ = held.lock().unwrap().recv();
                None
            }),
            Duration::from_millis(50),
        );
        crate::harness_host::with_harness_state(state.path(), || {
            crate::control::restart_verb(
                agent.master,
                "account=work",
                &agent.ctx,
                aterm_slog::Actor::Ctl,
            )
        })
    };
    drop(release);
    assert_eq!(
        reply,
        format!("ERR restart unavailable: {}\n", accounts_pool::MOVE_UNREAD)
    );
    assert!(restart_stamp::read(state.path(), &agent.sid).is_none());
    assert_eq!(agent.acts("restart"), 0);
    assert!(agent.ended().is_none() && agent.record_left());
    forget(&agent);
}

/// AN ACCOUNT IS JUDGED ONLY ON THE TAB'S COPY OF THE VERY PROCESS ASKED ABOUT: a self-claim copy
/// of an earlier process in the tab (same pid, another kernel start) names a launch this process
/// may not have had, so the pool is never asked about it and nothing is stamped or sent. CONTROL:
/// with the copy of this process, the pool is asked once.
#[test]
fn an_account_is_judged_only_on_the_copy_of_the_process_asked_about() {
    let _serial = serial();
    let state = aterm_tempfile::TempDir::new().expect("state");
    let agent = codex_alab(7790, 1);
    let mut earlier = crate::self_claim::copy_of_tab(&agent.sid).expect("its copy");
    let own = earlier.clone();
    earlier.start = format!("{}-earlier", agent.start);
    let asked = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let ask = |rest: &str| {
        let counted = Arc::clone(&asked);
        let _judge = crate::restart_act::judge_for_test(
            Arc::new(move |_, _| {
                counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Some(accounts_pool::MOVE_LIMITED)
            }),
            aterm_spec::wait::MUST_HAPPEN,
        );
        crate::harness_host::with_harness_state(state.path(), || {
            crate::control::restart_verb(agent.master, rest, &agent.ctx, aterm_slog::Actor::Ctl)
        })
    };
    crate::self_claim::set_copy_for_test(&agent.sid, Some(earlier));
    let reply = ask("account=work");
    assert!(reply.starts_with("ERR restart unavailable: "), "{reply}");
    assert_eq!(
        asked.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "the pool was asked about another process's launch: {reply}"
    );
    assert!(restart_stamp::read(state.path(), &agent.sid).is_none());
    assert!(agent.ended().is_none() && agent.record_left());
    crate::self_claim::set_copy_for_test(&agent.sid, Some(own));
    assert_eq!(
        ask("account=work"),
        format!("ERR restart unavailable: {}\n", accounts_pool::MOVE_LIMITED),
        "CONTROL: its own copy is judged"
    );
    assert_eq!(asked.load(std::sync::atomic::Ordering::SeqCst), 1);
    forget(&agent);
}

/// The reply of a move refused because the session was not at its own rest.
fn busy() -> String {
    format!(
        "ERR restart unavailable: {}\n",
        crate::restart_act::ACCOUNT_BUSY
    )
}

/// Asserts that a refused ask on `agent` left nothing: no stamp under `state`, no act mark, and
/// the program running.
fn untouched(agent: &RealAgent, state: &std::path::Path, why: &str) {
    assert!(
        restart_stamp::read(state, &agent.sid).is_none(),
        "{why}: no stamp"
    );
    assert_eq!(agent.acts("restart"), 0, "{why}: no act mark");
    assert!(
        agent.ended().is_none() && agent.record_left(),
        "{why}: nothing sent"
    );
}

/// A MOVE ENDS A SESSION ONLY AT ITS RECORD'S REST: the act reads the session's own record
/// before anything is written, and one whose turn runs, that waits on a question, that holds a
/// draft, that shows an exit, that is missing or unreadable, or that is another process's,
/// refuses the move as `account-busy`, leaving no stamp, no act mark and the program running.
/// CONTROL: the same session, its record at its rest, is moved.
#[test]
fn a_move_ends_a_session_only_at_its_records_rest() {
    let _serial = serial();
    let state = aterm_tempfile::TempDir::new().expect("state");
    let agent = codex_alab(7795, 1);
    let other_pid = format!(r#""v":1,"pid":{},{IDLE}"#, agent.pid + 1);
    for (why, fields) in [
        ("its turn runs", Some(BUSY)),
        (
            "a question waits",
            Some(r#""status":"waiting","draftEmpty":true,"state":"blocked","exit":null"#),
        ),
        (
            "a draft",
            Some(r#""status":"idle","draftEmpty":false,"state":"idle","exit":null"#),
        ),
        (
            "an exit",
            Some(r#""status":"idle","draftEmpty":true,"state":"idle","exit":"signal:15""#),
        ),
        ("no record", None),
    ] {
        match fields {
            Some(fields) => record(&agent, fields),
            None => std::fs::remove_file(record_path(&agent)).expect("the record removed"),
        }
        assert_eq!(
            restart(&agent, state.path(), "account=work", None),
            busy(),
            "{why}"
        );
        untouched(&agent, state.path(), why);
    }
    for (why, text) in [
        ("unreadable", "{".to_string()),
        ("another process's", format!("{{{other_pid}}}")),
    ] {
        std::fs::write(record_path(&agent), text).expect("the record");
        assert_eq!(
            restart(&agent, state.path(), "account=work", None),
            busy(),
            "{why}"
        );
        untouched(&agent, state.path(), why);
    }
    record(&agent, IDLE);
    let _ends_on_its_term = crate::restart_act::deadline_for_test(aterm_spec::wait::MUST_HAPPEN);
    assert_eq!(
        restart(&agent, state.path(), "account=work", None),
        format!("OK restarting pid {} cause=asked account=work\n", agent.pid),
        "CONTROL: at its rest"
    );
    until("it ended on the act's SIGTERM", || agent.ended().is_some());
    forget(&agent);
}

/// A TURN THAT BEGINS WHILE THE POOL JUDGES THE MOVE IS NOT ENDED BY IT: the record is read after
/// the judgement, so a session whose record went busy during it (a turn Autopilot or the person
/// started in those seconds) is refused as `account-busy` and left as it was. CONTROL: a
/// judgement during which the record stays at its rest is carried.
#[test]
fn a_turn_that_begins_while_the_pool_judges_is_not_ended_by_the_move() {
    let _serial = serial();
    let state = aterm_tempfile::TempDir::new().expect("state");
    let run = |n: u64, during: &'static str| {
        let agent = codex_alab(7796 + n, 1);
        let (path, pid) = (record_path(&agent), agent.pid);
        let _ends_on_its_term =
            crate::restart_act::deadline_for_test(aterm_spec::wait::MUST_HAPPEN);
        let reply = restart_judged(&agent, state.path(), "account=work", move |_, _| {
            record_at(&path, pid, during);
            None
        });
        (agent, reply)
    };
    let (agent, reply) = run(0, BUSY);
    assert_eq!(reply, busy(), "a turn began while the pool was judged");
    untouched(&agent, state.path(), "a turn began");
    forget(&agent);
    let (agent, reply) = run(1, IDLE);
    assert_eq!(
        reply,
        format!("OK restarting pid {} cause=asked account=work\n", agent.pid),
        "CONTROL: it stayed at its rest"
    );
    until("it ended on the act's SIGTERM", || agent.ended().is_some());
    forget(&agent);
}

/// A PERSON'S GESTURE THAT REACHES THE PROGRAM AFTER THE ASK ARRIVED REFUSES THE MOVE: read last
/// before the signal, it answers `account-busy` and the act takes back the stamp it wrote, so
/// nothing stands, nothing is marked and nothing is sent. CONTROL: gestures that reached the
/// program before the ask arrived (the person's own `/accounts` keys) do not.
#[test]
fn a_persons_gesture_after_the_ask_arrived_refuses_the_move() {
    let _serial = serial();
    let state = aterm_tempfile::TempDir::new().expect("state");
    let agent = codex_alab(7798, 1);
    let ctx = Arc::clone(&agent.ctx);
    let reply = restart_judged(&agent, state.path(), "account=work", move |_, _| {
        ctx.human_input
            .note(crate::metrics::now_us(), crate::human_input::Gesture::Typed);
        None
    });
    assert_eq!(reply, busy(), "a key reached it while the pool was judged");
    untouched(&agent, state.path(), "a key after the ask");
    for at in 1..=3 {
        agent
            .ctx
            .human_input
            .note(at, crate::human_input::Gesture::Typed);
    }
    let _ends_on_its_term = crate::restart_act::deadline_for_test(aterm_spec::wait::MUST_HAPPEN);
    assert_eq!(
        restart(&agent, state.path(), "account=work", None),
        format!("OK restarting pid {} cause=asked account=work\n", agent.pid),
        "CONTROL: gestures before the ask"
    );
    until("it ended on the act's SIGTERM", || agent.ended().is_some());
    forget(&agent);
}

/// Puts `agent`'s tty in raw mode, as a full-screen program does, and writes `bytes` into its
/// input, which nothing there reads.
fn unread(agent: &RealAgent, bytes: &[u8]) {
    // SAFETY: a zeroed `termios` is a valid out-slot for `tcgetattr` on the test's open master.
    let mut t: libc::termios = unsafe { std::mem::zeroed() };
    // SAFETY: the master is open and `t` is a valid out-param.
    assert_eq!(
        unsafe { libc::tcgetattr(agent.master, &mut t) },
        0,
        "tcgetattr"
    );
    // SAFETY: `t` is an initialized termios.
    unsafe { libc::cfmakeraw(&mut t) };
    // SAFETY: `t` is a valid termios for the open master's tty.
    assert_eq!(
        unsafe { libc::tcsetattr(agent.master, libc::TCSANOW, &t) },
        0,
        "tcsetattr"
    );
    // SAFETY: the master is open and `bytes` is a valid buffer.
    let n = unsafe { libc::write(agent.master, bytes.as_ptr().cast(), bytes.len()) };
    assert_eq!(usize::try_from(n).ok(), Some(bytes.len()), "write(master)");
}

/// INPUT THE SESSION HAS NOT READ REFUSES THE MOVE AND IS KEPT: the act reads the tty's unread
/// input before its signal, which would drop it unread, answers `account-busy`, takes back its
/// stamp, and the bytes are still there for the program. CONTROL: a plain restart of the same
/// session is carried as it always was, dropping them (`discarded=`).
#[test]
fn input_the_session_has_not_read_refuses_the_move_and_is_kept() {
    let _serial = serial();
    let state = aterm_tempfile::TempDir::new().expect("state");
    let agent = codex_alab(7799, 1);
    unread(&agent, b"k");
    assert_eq!(agent.ctx.sink.unread_input(), Some(1), "the key waits");
    assert_eq!(restart(&agent, state.path(), "account=work", None), busy());
    untouched(&agent, state.path(), "input unread");
    assert_eq!(
        agent.ctx.sink.unread_input(),
        Some(1),
        "the key is still there"
    );
    let _ends_on_its_term = crate::restart_act::deadline_for_test(aterm_spec::wait::MUST_HAPPEN);
    assert_eq!(
        restart(&agent, state.path(), "", None),
        format!("OK restarting pid {} cause=asked discarded=1\n", agent.pid),
        "CONTROL: a plain restart"
    );
    until("it ended on the act's SIGTERM", || agent.ended().is_some());
    forget(&agent);
}

/// A REST READ THAT DOES NOT ANSWER REFUSES THE MOVE AND HOLDS NO OTHER RESTART PAST ITS BOUND:
/// the record is read under the act's lock, so while it hangs another tab's restart waits, but
/// only until the read's bound: that restart is carried, and the ask answered `account-busy`
/// with nothing written, while the read itself still hangs.
#[test]
fn a_rest_read_that_does_not_answer_refuses_the_move_and_holds_no_other_restart_past_it() {
    let _serial = serial();
    let state = aterm_tempfile::TempDir::new().expect("state");
    let (asking, other) = (codex_alab(7802, 1), codex_alab(7803, 1));
    let (release, held) = std::sync::mpsc::channel::<()>();
    let held = Mutex::new(held);
    let (reading, began) = std::sync::mpsc::channel::<()>();
    let reading = Mutex::new(reading);
    let (dir, other_agent) = (state.path(), &other);
    std::thread::scope(|scope| {
        // Dropped before the scope joins, also on a failed assertion: the read lets go then.
        let release = release;
        let asks = scope.spawn(|| {
            let _rest = crate::restart_act::rest_for_test(
                Arc::new(move |_, _, _| {
                    let _ = reading.lock().unwrap().send(());
                    let _ = held.lock().unwrap().recv();
                    None
                }),
                Duration::from_millis(50),
            );
            restart(&asking, dir, "account=work", None)
        });
        began
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .expect("the rest read began");
        let (carry, carried) = std::sync::mpsc::channel();
        scope.spawn(move || {
            let _ends_on_its_term =
                crate::restart_act::deadline_for_test(aterm_spec::wait::MUST_HAPPEN);
            let _ = carry.send(restart(other_agent, dir, "", None));
        });
        assert_eq!(
            carried
                .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
                .expect("another tab's restart is carried while the read still hangs"),
            format!("OK restarting pid {} cause=asked\n", other.pid)
        );
        assert_eq!(asks.join().expect("the ask answered"), busy());
        drop(release);
    });
    untouched(&asking, state.path(), "the read did not answer");
    until("the other ended on its SIGTERM", || other.ended().is_some());
    forget(&asking);
    forget(&other);
}

/// THE REST IS READ BEFORE ANYTHING IS WRITTEN: no stamp of the process stands while the act reads
/// its record, so a program that ends during that read ends on its own: the host's readers of
/// its exit (`Acts::asked`, `Acts::asked_account`: [`restart_stamp::lookup`] and
/// [`restart_stamp::wanted_for`]) find no ask and no account for it as it ends or after, so its
/// exit is not the act's to relaunch on the asked account, and the ask is `account-busy`. So does
/// one that ends while the pool judges the move, before the act's lock, which the act then
/// refuses with nothing written. CONTROL: a program that stays at its rest across the read is
/// moved, its stamp written after.
#[test]
fn the_rest_is_read_before_anything_is_written() {
    let _serial = serial();
    let state = aterm_tempfile::TempDir::new().expect("state");
    // Whether the host's readers of `agent`'s exit would find an ask or an account for it.
    let asked = |dir: &std::path::Path, sid: &str, upid: u32, start: &str| {
        let now = aterm_time::unix_secs().unwrap_or(0);
        restart_stamp::lookup(dir, sid, upid, start, now).is_some()
            || restart_stamp::wanted_for(dir, sid, upid, start, now).is_some()
    };
    let run = |n: u64, end: Option<&'static str>| {
        let agent = codex_alab(7806 + n, 1);
        let stood = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let read_asked = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (seen, seen_asked, dir, sid) = (
            Arc::clone(&stood),
            Arc::clone(&read_asked),
            state.path().to_path_buf(),
            agent.sid.clone(),
        );
        let (pid, status) = (agent.pid, Arc::clone(&agent.status));
        let kill = move || {
            // SAFETY: `pid` is this test's own unreaped child.
            unsafe { libc::kill(pid, libc::SIGKILL) };
            until("it ended", || status.lock().unwrap().is_some());
        };
        let killer = kill.clone();
        let _rest = crate::restart_act::rest_for_test(
            Arc::new(move |home, upid, start| {
                seen.store(
                    restart_stamp::read(&dir, &sid).is_some(),
                    std::sync::atomic::Ordering::SeqCst,
                );
                if end == Some("read") {
                    killer();
                    seen_asked.store(
                        asked(&dir, &sid, upid, start),
                        std::sync::atomic::Ordering::SeqCst,
                    );
                }
                self_claimed::read_seen(home, upid, start)
            }),
            aterm_spec::wait::MUST_HAPPEN,
        );
        let _ends_on_its_term =
            crate::restart_act::deadline_for_test(aterm_spec::wait::MUST_HAPPEN);
        let reply = restart_judged(&agent, state.path(), "account=work", move |_, _| {
            if end == Some("judge") {
                kill();
            }
            None
        });
        (
            agent,
            reply,
            stood.load(std::sync::atomic::Ordering::SeqCst),
            read_asked.load(std::sync::atomic::Ordering::SeqCst),
        )
    };
    let (agent, reply, stood, read_asked) = run(0, Some("read"));
    assert_eq!(reply, busy(), "it ended during the read");
    assert!(!stood, "no stamp stood during the read");
    assert!(!read_asked, "its exit, read as it ended, is no ask");
    assert!(
        !asked(state.path(), &agent.sid, agent.upid(), &agent.start),
        "nor read after the act"
    );
    assert!(
        restart_stamp::read(state.path(), &agent.sid).is_none(),
        "no stamp names its end"
    );
    assert_eq!(agent.acts("restart"), 0, "no act mark");
    forget(&agent);
    let (agent, reply, _, _) = run(11, Some("judge"));
    assert!(
        reply.starts_with("ERR "),
        "it ended while the pool judged: {reply}"
    );
    assert!(
        !asked(state.path(), &agent.sid, agent.upid(), &agent.start),
        "its exit is no ask"
    );
    assert!(restart_stamp::read(state.path(), &agent.sid).is_none());
    assert_eq!(agent.acts("restart"), 0, "no act mark");
    forget(&agent);
    let (agent, reply, stood, _) = run(1, None);
    assert_eq!(
        reply,
        format!("OK restarting pid {} cause=asked account=work\n", agent.pid),
        "CONTROL: at its rest across the read"
    );
    assert!(!stood, "CONTROL: its stamp was written after the read");
    assert!(
        asked(state.path(), &agent.sid, agent.upid(), &agent.start),
        "CONTROL: the host reads its exit as the act's, on the asked account"
    );
    until("it ended on the act's SIGTERM", || agent.ended().is_some());
    forget(&agent);
}

/// THE REST FENCE IS THE MOVE'S ALONE: a plain restart of a session whose turn runs is carried as
/// it always was (the stall and memory remedies end a busy session on purpose), and an account
/// ask that joins a standing stamp for the same account is not judged again, since that stamp's
/// act already signalled: neither its record's rest before the stamp nor, as it comes to signal,
/// a person's gesture since it arrived (refused, it would say nothing restarts while the first
/// act's signal is already ending the session for that same account).
#[test]
fn the_rest_fence_is_the_moves_alone() {
    let _serial = serial();
    let state = aterm_tempfile::TempDir::new().expect("state");
    let plain = codex_alab(7808, 1);
    record(&plain, BUSY);
    let _ends_on_its_term = crate::restart_act::deadline_for_test(aterm_spec::wait::MUST_HAPPEN);
    assert_eq!(
        restart(&plain, state.path(), "", None),
        format!("OK restarting pid {} cause=asked\n", plain.pid),
        "a plain restart"
    );
    until("it ended on the act's SIGTERM", || plain.ended().is_some());
    forget(&plain);
    let joins = codex_alab(7809, 1);
    record(&joins, BUSY);
    let work = Some(Wanted::Row("work".to_string()));
    let standing = Stamp {
        pid: joins.upid(),
        start: joins.start.clone(),
        cause: Cause::Asked,
        at: aterm_time::unix_secs().unwrap_or(0),
        wanted: work.clone(),
        input: None,
    };
    restart_stamp::write(state.path(), &joins.sid, &standing).expect("the account's stamp");
    let ctx = Arc::clone(&joins.ctx);
    assert_eq!(
        restart_judged(&joins, state.path(), "account=work", move |_, _| {
            ctx.human_input
                .note(crate::metrics::now_us(), crate::human_input::Gesture::Typed);
            None
        }),
        format!("OK restarting pid {} cause=asked account=work\n", joins.pid),
        "an ask joining a standing stamp for its account, a key reaching it during the judgement"
    );
    assert_eq!(
        stamp_of(&joins, state.path()).and_then(|stamp| stamp.wanted),
        work
    );
    until("it ended on the act's SIGTERM", || joins.ended().is_some());
    forget(&joins);
}

/// A HAND THAT COMES DURING THE REST READ ANSWERS ITS OWN WORD: the read takes up to
/// `ACCOUNT_REST`, so the act reads its own fence again after it, and another driver's lease
/// taken meanwhile refuses the move as `driver-lease`, nothing written or sent, whatever the
/// record says (`account-busy` stays the last word). CONTROL: a read that nothing comes during
/// is moved.
#[test]
fn a_hand_that_comes_during_the_rest_read_answers_its_own_word() {
    let _serial = serial();
    let state = aterm_tempfile::TempDir::new().expect("state");
    let run = |n: u64, lease: bool, fields: &'static str| {
        let agent = codex_alab(7810 + n, 1);
        let (ctx, path, pid) = (Arc::clone(&agent.ctx), record_path(&agent), agent.pid);
        let _rest = crate::restart_act::rest_for_test(
            Arc::new(move |home, upid, start| {
                record_at(&path, pid, fields);
                if lease {
                    *ctx.turn_lease.lock().unwrap() = Some(crate::Lease::Drive {
                        holder: "another-driver".to_string(),
                        expires_us: crate::metrics::now_us() + 60_000_000,
                        conn: None,
                        hard: false,
                        here: false,
                        holder_pid: None,
                    });
                }
                self_claimed::read_seen(home, upid, start)
            }),
            aterm_spec::wait::MUST_HAPPEN,
        );
        let _ends_on_its_term =
            crate::restart_act::deadline_for_test(aterm_spec::wait::MUST_HAPPEN);
        let reply = restart(&agent, state.path(), "account=work", None);
        (agent, reply)
    };
    for (n, why, fields) in [(0, "at its rest", IDLE), (1, "its turn runs too", BUSY)] {
        let (agent, reply) = run(n, true, fields);
        assert_eq!(
            reply, "ERR restart unavailable: driver-lease\n",
            "a lease taken during the read, {why}"
        );
        untouched(&agent, state.path(), why);
        *agent.ctx.turn_lease.lock().unwrap() = None;
        forget(&agent);
    }
    let (agent, reply) = run(2, false, IDLE);
    assert_eq!(
        reply,
        format!("OK restarting pid {} cause=asked account=work\n", agent.pid),
        "CONTROL: no hand came during the read"
    );
    until("it ended on the act's SIGTERM", || agent.ended().is_some());
    forget(&agent);
}

/// `account-busy` IS THE LAST WORD: a move that another fence refuses answers that fence's word
/// even where the session's record is not at its rest, the pool's (`account-limited`) and a
/// restart standing for another destination (`account-restart-pending`) among them, with
/// nothing written or sent. CONTROL: with every other fence open, the same record is
/// `account-busy`.
#[test]
fn account_busy_is_the_last_word() {
    let _serial = serial();
    let state = aterm_tempfile::TempDir::new().expect("state");
    let agent = codex_alab(7813, 1);
    record(&agent, BUSY);
    assert_eq!(
        restart(
            &agent,
            state.path(),
            "account=work",
            Some(accounts_pool::MOVE_LIMITED)
        ),
        format!("ERR restart unavailable: {}\n", accounts_pool::MOVE_LIMITED),
        "the pool's word"
    );
    untouched(&agent, state.path(), "the pool's word");
    let other = Stamp {
        pid: agent.upid(),
        start: agent.start.clone(),
        cause: Cause::Asked,
        at: aterm_time::unix_secs().unwrap_or(0),
        wanted: Some(Wanted::Row("spare".to_string())),
        input: None,
    };
    restart_stamp::write(state.path(), &agent.sid, &other).expect("another account's stamp");
    assert_eq!(
        restart(&agent, state.path(), "account=work", None),
        format!(
            "ERR restart unavailable: {}\n",
            crate::restart_act::ACCOUNT_RESTART_PENDING
        ),
        "a restart standing for another account"
    );
    assert_eq!(
        stamp_of(&agent, state.path()),
        Some(other),
        "that stamp is kept"
    );
    assert!(restart_stamp::take_if(state.path(), &agent.sid, |_| true).is_some());
    untouched(
        &agent,
        state.path(),
        "a restart standing for another account",
    );
    assert_eq!(
        restart(&agent, state.path(), "account=work", None),
        busy(),
        "CONTROL: every other fence open"
    );
    untouched(&agent, state.path(), "CONTROL");
    forget(&agent);
}

/// A MOVE WHOSE PROGRAM LEFT UNDER ITS STAMP IS NOT CALLED REFUSED: where the act, refusing as it
/// came to signal (a person's gesture since the ask), finds that the program no longer leads its
/// tab, a reader of that exit may already have taken it for the act's while the stamp stood and
/// begun its relaunch on the asked account, so the act answers as one whose leader left before
/// anything was sent (`OK restart gone`), never `account-busy`. Its stamp is taken back either
/// way. CONTROL: the same refusal of a program that still leads its tab is `account-busy`.
#[test]
fn a_move_whose_program_left_under_its_stamp_is_not_called_refused() {
    let _serial = serial();
    let state = aterm_tempfile::TempDir::new().expect("state");
    let run = |n: u64, leaves: bool| {
        let agent = codex_alab(7814 + n, 1);
        let stamped = {
            let _judge = crate::restart_act::judge_for_test(
                Arc::new(|_, _| None),
                aterm_spec::wait::MUST_HAPPEN,
            );
            crate::restart_act::begin(
                agent.master,
                "account=work",
                &agent.ctx,
                Some(state.path()),
                aterm_slog::Actor::Ctl,
            )
        };
        let Ok(stamped) = stamped else {
            panic!("the move is stamped");
        };
        assert!(stamp_of(&agent, state.path()).is_some(), "its stamp stands");
        if leaves {
            // SAFETY: `agent.pid` is this test's own unreaped child.
            unsafe { libc::kill(agent.pid, libc::SIGKILL) };
            until("it left under the stamp", || agent.ended().is_some());
        }
        agent
            .ctx
            .human_input
            .note(crate::metrics::now_us(), crate::human_input::Gesture::Typed);
        let reply = stamped.signal();
        assert!(
            restart_stamp::read(state.path(), &agent.sid).is_none(),
            "its stamp is taken back"
        );
        assert_eq!(agent.acts("restart"), 0, "nothing sent");
        (agent, reply)
    };
    let (agent, reply) = run(0, true);
    assert_eq!(reply, format!("OK restart gone pid {}\n", agent.pid));
    forget(&agent);
    let (agent, reply) = run(1, false);
    assert_eq!(reply, busy(), "CONTROL: it still leads its tab");
    untouched(&agent, state.path(), "CONTROL");
    forget(&agent);
}

/// THE REST READS ARE COUNTED APART FROM THE POOL'S JUDGEMENTS: four rest reads that hang (a home
/// on a mount that does not answer) each refuse their move as `account-busy` once their bound
/// passes and keep their slot while they hang, and the next ask is still judged by the pool, so
/// it answers the pool's own word (`account-limited`), never `account-unread`. CONTROL: a fifth
/// rest read beside the four is refused at once (`account-busy`): the reads have their own bound.
#[test]
fn rest_reads_that_hang_hold_no_judgement_of_the_pool() {
    let _serial = serial();
    let state = aterm_tempfile::TempDir::new().expect("state");
    let agent = codex_alab(7816, 1);
    let (release, held) = std::sync::mpsc::channel::<()>();
    let held = Arc::new(Mutex::new(held));
    let returned = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let hung = 4;
    {
        // Dropped before the waits below, also on a failed assertion: the reads let go then.
        let release = release;
        for n in 0..hung {
            let (held, returned) = (Arc::clone(&held), Arc::clone(&returned));
            let _rest = crate::restart_act::rest_for_test(
                Arc::new(move |_, _, _| {
                    let _ = held.lock().unwrap().recv();
                    returned.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    None
                }),
                Duration::from_millis(50),
            );
            assert_eq!(
                restart(&agent, state.path(), "account=work", None),
                busy(),
                "hung read {n}"
            );
        }
        assert_eq!(
            restart(
                &agent,
                state.path(),
                "account=work",
                Some(accounts_pool::MOVE_LIMITED)
            ),
            format!("ERR restart unavailable: {}\n", accounts_pool::MOVE_LIMITED),
            "the pool is still judged while the reads hang"
        );
        let fifth = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let read = Arc::clone(&fifth);
        let _rest = crate::restart_act::rest_for_test(
            Arc::new(move |home, upid, start| {
                read.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                self_claimed::read_seen(home, upid, start)
            }),
            aterm_spec::wait::MUST_HAPPEN,
        );
        assert_eq!(
            restart(&agent, state.path(), "account=work", None),
            busy(),
            "CONTROL: a fifth read beside the four"
        );
        assert_eq!(
            fifth.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "CONTROL: it was never made"
        );
        drop(release);
    }
    until("the hung reads let go", || {
        returned.load(std::sync::atomic::Ordering::SeqCst) == hung
    });
    untouched(&agent, state.path(), "reads that hang");
    forget(&agent);
}
