// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `restart account=<label>` and `restart account-auto` against a real process on a real pty:
//! what an accepted ask stamps, signals and replies, and that a refused one leaves nothing.

use super::*;
use aterm_agent::harness::accounts_pool;
use aterm_agent::harness::restart_stamp::{Cause, Stamp, Wanted};

const THREAD: &str = "0199f2a4-6c1e-7a52-9d3b-2f6c8e1a4b77";

/// A real process leading a real pty that this tab's validated self-claim copy and the host's
/// published snapshot name as a codex-alab on this machine's own login, under the host's claim:
/// what the restart act would carry. `shell` is the shell the snapshot names under it.
fn codex_alab(local_id: u64, shell: u32) -> RealAgent {
    let agent = RealAgent::spawn(false, local_id);
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
    let _judge = crate::restart_act::judge_for_test(
        Arc::new(move |_, _| word),
        aterm_spec::wait::MUST_HAPPEN,
    );
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
