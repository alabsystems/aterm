// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for [`super::run`]: the `end` verb over a fake view, a fake re-read, and an
//! [`EndControl`] that records every signal; no kernel, clock or window is reached.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};

use aterm_messages::need::Need;
use aterm_messages::tags;
use aterm_uds::process::Birth;
use aterm_vitals::left::{ClosedTab, RunRow};

use super::*;

const SIGTERM: i32 = 15;
const SIGKILL: i32 = 9;

/// Now, unix ms, for the records' words.
const NOW: u64 = 1_792_200_000_000;

/// The target session of an edge's requests: tab key 7, `s-a7`.
const EDGE: Caller<'static> = Caller {
    owner: false,
    key: 7,
    sid: "s-a7",
};

/// An owner-class connection aimed at tab key 1.
const OWNER: Caller<'static> = Caller {
    owner: true,
    key: 1,
    sid: "s-a1",
};

/// The dead verify run that rode in tab 7.
const RUN: &str = "41983-1792163800331520";

/// `pid`'s identity on a kernel that keeps pid versions.
fn ident(pid: i32) -> Identity {
    Identity {
        pid,
        birth: Birth(1_792_098_840_004_410),
        idversion: Some(331),
    }
}

fn token(pid: i32) -> String {
    ident(pid).token()
}

/// A row the view offers, from `from`: W-session's proof, no run roots, the executable judged
/// `/s/sleep`.
fn offered(pid: i32, from: Source) -> LeftRow {
    LeftRow {
        pid: u32::try_from(pid).expect("a pid"),
        state: State::Left,
        peer: None,
        check_ms: None,
        started_ms: Some(60_000),
        cpu_ms: Some(10),
        exe_judged: Some("/s/sleep".into()),
        tier: Tier::Offer,
        why: Why::OwnerGone,
        proof: vec![Proof::Env, Proof::Time],
        listen: false,
        from,
        teller: None,
        token: Some(token(pid)),
        foreground: false,
        after_agent: false,
    }
}

/// A row the view names with no token.
fn named(pid: i32, state: State, tier: Tier, why: Why) -> LeftRow {
    LeftRow {
        state,
        tier,
        why,
        token: None,
        ..offered(pid, Source::Tab(7))
    }
}

fn view(rows: Vec<LeftRow>) -> LeftView {
    LeftView {
        rows,
        runs: vec![RunRow {
            id: RUN.into(),
            dead: true,
            dead_ms: Some(3_600_000),
            sid: Some("s-a7".into()),
            roots: vec!["/w/wt".into()],
            left: 1,
            next_verify: false,
        }],
        closed: vec![ClosedTab {
            key: 5,
            sid: "s-gone".into(),
            label: "Tab 3".into(),
            number: Some(3),
            shell_uniqueid: 1_000,
            horizon: 2_000,
            closed_ms: NOW - 7_200_000,
            agent_spans: Vec::new(),
            left: 1,
            restored: false,
            told: false,
        }],
        ..LeftView::default()
    }
}

/// The fake kernel: each live pid's identity; a `SIGTERM` ends a process unless it is deaf, a
/// `SIGKILL` always; a refused pid takes no signal.
#[derive(Default)]
struct Procs {
    live: HashMap<i32, Identity>,
    deaf: HashSet<i32>,
    refused: HashSet<i32>,
    signals: Vec<(i32, i32)>,
}

fn procs(pids: &[i32]) -> RefCell<Procs> {
    RefCell::new(Procs {
        live: pids.iter().map(|&pid| (pid, ident(pid))).collect(),
        ..Procs::default()
    })
}

struct Control<'a>(&'a RefCell<Procs>);

impl EndControl for Control<'_> {
    fn identity(&mut self, pid: i32) -> Option<Identity> {
        self.0.borrow().live.get(&pid).copied()
    }
    fn signal(&mut self, token: &Identity, sig: i32) -> std::io::Result<()> {
        let mut p = self.0.borrow_mut();
        if p.refused.contains(&token.pid) {
            return Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied));
        }
        p.signals.push((token.pid, sig));
        if sig == SIGKILL || !p.deaf.contains(&token.pid) {
            p.live.remove(&token.pid);
        }
        Ok(())
    }
    fn nap(&mut self, _: Duration) {}
}

/// What one request answered, what it posted, and how often it read the view.
struct Answer {
    reply: String,
    posted: Vec<Message>,
    views: usize,
}

/// `end <rest>` for `caller` against `v`, `p`'s processes and the re-read `reread`, in the kernel
/// shape `versioned`, with no grace, and with every pid the fake holds no identity for absent.
fn end_with(
    caller: Caller<'_>,
    rest: &str,
    v: &LeftView,
    p: &RefCell<Procs>,
    reread: &dyn Fn(u32) -> Option<LeftRow>,
    versioned: bool,
) -> Answer {
    let shown = Arc::new(v.clone());
    let views = Cell::new(0);
    let mut read = || {
        views.set(views.get() + 1);
        Some(Arc::clone(&shown))
    };
    let mut posted = Vec::new();
    let mut post = |m: Message| posted.push(m);
    let fresh = |pid| {
        reread(pid).map(|row| LeftView {
            rows: vec![row],
            ..v.clone()
        })
    };
    let reply = run(
        &caller,
        rest,
        World {
            versioned,
            view: &mut read,
            reread: &fresh,
            authorized: &|| true,
            presence: &|_| ProcessPresence::Absent,
            control: Control(p),
            grace: Duration::ZERO,
            now_ms: NOW,
            post: &mut post,
            hung_up: &|| false,
        },
    );
    Answer {
        reply,
        posted,
        views: views.get(),
    }
}

/// [`end_with`] whose re-read judges each pid as the view does.
fn end(caller: Caller<'_>, rest: &str, v: &LeftView, p: &RefCell<Procs>) -> Answer {
    let same = |pid: u32| v.rows.iter().find(|r| r.pid == pid).cloned();
    end_with(caller, rest, v, p, &same, true)
}

/// The real end loop with a host transition immediately after its successful TERM. No grace
/// clock or actual process is involved: a deaf target stays alive for the second re-proof.
fn end_changing(
    caller: Caller<'_>,
    v: &LeftView,
    p: &RefCell<Procs>,
    reread: &dyn Fn(u32) -> Option<LeftView>,
    authorized: &dyn Fn() -> bool,
    after_term: &dyn Fn(),
) -> Answer {
    struct Changing<'a> {
        control: Control<'a>,
        after_term: &'a dyn Fn(),
    }
    impl EndControl for Changing<'_> {
        fn identity(&mut self, pid: i32) -> Option<Identity> {
            self.control.identity(pid)
        }
        fn signal(&mut self, token: &Identity, signal: i32) -> std::io::Result<()> {
            self.control.signal(token, signal)?;
            if signal == SIGTERM {
                (self.after_term)();
            }
            Ok(())
        }
        fn nap(&mut self, _: Duration) {}
    }
    let shown = Arc::new(v.clone());
    let views = Cell::new(0);
    let mut read = || {
        views.set(views.get() + 1);
        Some(Arc::clone(&shown))
    };
    let mut posted = Vec::new();
    let reply = run(
        &caller,
        v.rows[0].token.as_deref().expect("offered token"),
        World {
            versioned: true,
            view: &mut read,
            reread,
            authorized,
            presence: &|_| ProcessPresence::Absent,
            control: Changing {
                control: Control(p),
                after_term,
            },
            grace: Duration::ZERO,
            now_ms: NOW,
            post: &mut |message| posted.push(message),
            hung_up: &|| false,
        },
    );
    Answer {
        reply,
        posted,
        views: views.get(),
    }
}

#[derive(Clone, Copy)]
enum Transition {
    Unchanged,
    CloseSession,
    Foreground,
    ForegroundDuringRead,
    Revoke,
}

/// Uses the actual Store close transition, fresh-context collector and leftover judge. Only
/// the PTY/kernel readings and signal delivery are injected; a cached live tab would let the
/// closed-session case incorrectly send KILL, and a cached foreground would do the same when
/// the target becomes the current job.
fn end_during_transition(transition: Transition) -> (Answer, Vec<(i32, i32)>) {
    use crate::session_store::{ExitActor, ExitReason};
    use aterm_vitals::left::{self, Ctx, Facts, Lin, LiveTab, Marks};

    let store = crate::session_store::new_store();
    let mut handle = crate::session_store::test_handle_on(7, -1);
    handle.shell_identity = aterm_pty::ShellIdentity::from_parts(600, Some(700), true);
    let sid = handle.sid.as_str().to_owned();
    store.write().unwrap().register(handle);
    let initial = LeftView {
        live: vec![LiveTab {
            key: 7,
            sid: sid.clone(),
            shell_pid: 600,
            shell_uniqueid: Some(1_000),
            fg_pgid: Some(600),
            agent_spans: Vec::new(),
        }],
        ..LeftView::default()
    };
    let facts = Facts {
        pid: 4100,
        ppid: 1,
        lineage: Some(Lin {
            uniqueid: 1_500,
            parent: 1,
            pgid: 2_600,
            launcher_version: Some(7),
            tty: None,
        }),
        token: Some(token(4100)),
        exe: Some("/s/sleep".into()),
        thread_ran: Some(true),
        marks: Some(Marks {
            session: Some(sid.clone()),
            ..Marks::default()
        }),
        listener: Some(false),
        sandboxed: Some(false),
        by_design: Some(false),
        ..Facts::default()
    };
    let judge = |live: &[LiveTab], closed: &[ClosedTab]| {
        left::judge(
            &facts,
            &Ctx {
                launchd: Some(1),
                live,
                closed,
                spared: &[],
                tellers: &[],
            },
        )
    };
    let v = LeftView {
        rows: vec![judge(&initial.live, &[]).expect("live orphan")],
        ..initial
    };
    assert_eq!(v.rows[0].tier, Tier::Offer);
    assert_eq!(v.rows[0].from, Source::Tab(7));
    let foreground = Cell::new(600);
    let permitted = Cell::new(true);
    let readings = Cell::new(0);
    let reread = |_: u32| {
        readings.set(readings.get() + 1);
        reread_current(
            &store,
            &|| {
                fresh_context_with(
                    &store,
                    &v,
                    &|identity| {
                        (identity.pid == 600 && identity.birth == Some(700)).then_some(1_000)
                    },
                    &|_| {
                        assert!(store.try_write().is_ok(), "no Store guard during PTY read");
                        Some(foreground.get())
                    },
                )
            },
            &|live, closed| {
                if readings.get() == 2 && matches!(transition, Transition::ForegroundDuringRead) {
                    // The first capture already saw the old foreground. Membership is unchanged,
                    // and the row still judges Offer against that old context. Only the second
                    // host capture prevents KILL after this handoff during the expensive read.
                    foreground.set(2_600);
                }
                Reread::Read(judge(live, closed).map(|row| LeftView {
                    rows: vec![row],
                    live: live.to_vec(),
                    closed: closed.to_vec(),
                    ..LeftView::default()
                }))
            },
        )
        .view()
    };
    let after_term = || {
        let mut current = store.try_write().expect("no Store guard during signal");
        match transition {
            Transition::Unchanged | Transition::ForegroundDuringRead => {}
            Transition::CloseSession => {
                assert!(
                    current
                        .deregister_closing(7, ExitReason::UiClose, ExitActor::Human)
                        .is_some()
                );
            }
            Transition::Foreground => foreground.set(2_600),
            Transition::Revoke => permitted.set(false),
        }
    };
    let p = procs(&[4100]);
    p.borrow_mut().deaf.insert(4100);
    let a = end_changing(
        Caller { sid: &sid, ..EDGE },
        &v,
        &p,
        &reread,
        &|| permitted.get(),
        &after_term,
    );
    assert_eq!(
        readings.get(),
        2,
        "TERM and KILL each need a fresh host judgment"
    );
    let signals = p.into_inner().signals;
    (a, signals)
}

#[test]
fn closing_the_session_after_term_revokes_the_signal_edges_kill() {
    let (a, signals) = end_during_transition(Transition::CloseSession);
    assert_eq!(signals, [(4100, SIGTERM)]);
    assert!(
        a.reply.contains("term=1 gone=0 killed=0 refused=0"),
        "{}",
        a.reply
    );
}

#[test]
fn entering_the_foreground_after_term_prevents_kill() {
    let (_, signals) = end_during_transition(Transition::Foreground);
    assert_eq!(signals, [(4100, SIGTERM)]);
}

#[test]
fn entering_the_foreground_during_the_candidate_reread_prevents_kill() {
    let (_, signals) = end_during_transition(Transition::ForegroundDuringRead);
    assert_eq!(signals, [(4100, SIGTERM)]);
}

#[test]
fn revoking_current_authorization_after_term_prevents_kill() {
    let (_, signals) = end_during_transition(Transition::Revoke);
    assert_eq!(signals, [(4100, SIGTERM)]);
}

#[test]
fn unchanged_fresh_session_and_foreground_allow_the_deaf_targets_kill() {
    let (a, signals) = end_during_transition(Transition::Unchanged);
    assert_eq!(signals, [(4100, SIGTERM), (4100, SIGKILL)]);
    assert!(
        a.reply.contains("term=1 gone=1 killed=1 refused=0"),
        "{}",
        a.reply
    );
}

#[test]
fn a_runs_current_session_is_rechecked_before_kill() {
    let v = view(vec![offered(4100, Source::Run(RUN.into()))]);
    let p = procs(&[4100]);
    p.borrow_mut().deaf.insert(4100);
    let owned = Cell::new(true);
    let reread = |_: u32| {
        let mut fresh = v.clone();
        if !owned.get() {
            fresh.runs[0].sid = Some("s-other".into());
        }
        Some(fresh)
    };
    let a = end_changing(EDGE, &v, &p, &reread, &|| true, &|| owned.set(false));
    assert_eq!(p.borrow().signals, [(4100, SIGTERM)]);
    assert!(a.reply.contains("killed=0"), "{}", a.reply);
}

#[test]
fn an_offered_token_is_ended_with_one_term_and_reported() {
    let v = view(vec![offered(4100, Source::Tab(7))]);
    let p = procs(&[4100]);
    let a = end(EDGE, &token(4100), &v, &p);
    assert_eq!(
        a.reply,
        format!(
            "OK 1 end term=1 gone=1 killed=0 refused=0\nended {} signal=term gone=1\n",
            token(4100)
        )
    );
    assert_eq!(p.borrow().signals, [(4100, SIGTERM)]);
    assert_eq!(a.views, 1);
}

/// The pid now names another process (its birth moved): the re-read just before the signal says
/// so, and nothing is sent. NEGATIVE CONTROL: signal the token's pid without the re-proof, and the
/// other process takes a `SIGTERM`.
#[test]
fn a_token_whose_process_moved_is_refused_stale_and_never_signalled() {
    let v = view(vec![offered(4100, Source::Tab(7))]);
    let p = procs(&[4100]);
    p.borrow_mut().live.insert(
        4100,
        Identity {
            birth: Birth(1_792_198_840_000_001),
            ..ident(4100)
        },
    );
    let a = end(EDGE, &token(4100), &v, &p);
    assert_eq!(
        a.reply,
        format!(
            "OK 1 end term=0 gone=0 killed=0 refused=1\nrefused {} why=stale\n",
            token(4100)
        )
    );
    assert!(p.borrow().signals.is_empty(), "{:?}", p.borrow().signals);
    assert!(a.posted.is_empty());
}

/// Where the kernel keeps no pid version an exec keeps the identity: a session's row (no run's
/// folders vouch for it) whose executable moved since the offer is another program now, and is
/// refused stale. NEGATIVE CONTROL: drop the executable comparison, and it is signalled.
#[test]
fn a_session_row_whose_executable_changed_since_the_offer_is_refused() {
    let linux = Identity {
        pid: 4100,
        birth: Birth(555_000),
        idversion: None,
    };
    let row = LeftRow {
        token: Some(linux.token()),
        ..offered(4100, Source::Tab(7))
    };
    let v = view(vec![row.clone()]);
    let p = procs(&[]);
    p.borrow_mut().live.insert(4100, linux);
    let execd = move |_: u32| {
        Some(LeftRow {
            exe_judged: Some("/s/other".into()),
            ..row.clone()
        })
    };
    let a = end_with(EDGE, &linux.token(), &v, &p, &execd, false);
    assert_eq!(
        a.reply,
        "OK 1 end term=0 gone=0 killed=0 refused=1\nrefused 4100@555000 why=stale\n"
    );
    assert!(p.borrow().signals.is_empty(), "{:?}", p.borrow().signals);
}

/// A process that outlives its `SIGTERM` is judged again before the `SIGKILL`; once its tier is
/// lost there, it takes no `SIGKILL`. NEGATIVE CONTROL: judge only before the `SIGTERM`, and the
/// `SIGKILL` goes.
#[test]
fn a_row_whose_tier_is_lost_before_the_kill_gets_no_kill() {
    let row = offered(4100, Source::Tab(7));
    let v = view(vec![row.clone()]);
    let p = procs(&[4100]);
    p.borrow_mut().deaf.insert(4100);
    let asked = Cell::new(0);
    let reread = |_: u32| {
        asked.set(asked.get() + 1);
        Some(if asked.get() == 1 {
            row.clone()
        } else {
            LeftRow {
                tier: Tier::Name,
                token: Some(token(4100)),
                ..row.clone()
            }
        })
    };
    let a = end_with(EDGE, &token(4100), &v, &p, &reread, true);
    assert_eq!(
        a.reply,
        format!(
            "OK 1 end term=1 gone=0 killed=0 refused=0\nended {} signal=term gone=0\n",
            token(4100)
        )
    );
    assert_eq!(p.borrow().signals, [(4100, SIGTERM)]);
    assert_eq!(asked.get(), 2, "judged before each signal");
}

/// The same, with the tier kept: the `SIGKILL` follows, and the reply says so.
#[test]
fn a_term_deaf_row_still_proven_is_killed() {
    let v = view(vec![offered(4100, Source::Tab(7))]);
    let p = procs(&[4100]);
    p.borrow_mut().deaf.insert(4100);
    let a = end(EDGE, &token(4100), &v, &p);
    assert_eq!(
        a.reply,
        format!(
            "OK 1 end term=1 gone=1 killed=1 refused=0\nended {} signal=kill gone=1\n",
            token(4100)
        )
    );
    assert_eq!(p.borrow().signals, [(4100, SIGTERM), (4100, SIGKILL)]);
}

#[test]
fn a_held_row_is_refused_held_with_no_signal() {
    let v = view(vec![named(
        4001,
        State::ProgramCheck,
        Tier::Name,
        Why::Live,
    )]);
    let p = procs(&[4001]);
    let a = end(EDGE, &token(4001), &v, &p);
    assert_eq!(
        a.reply,
        format!(
            "OK 1 end term=0 gone=0 killed=0 refused=1\nrefused {} why=held\n",
            token(4001)
        )
    );
    assert!(p.borrow().signals.is_empty());
}

#[test]
fn a_daemon_row_is_refused_daemon() {
    let v = view(vec![named(4200, State::Left, Tier::Name, Why::Daemon)]);
    let p = procs(&[4200]);
    let a = end(EDGE, &token(4200), &v, &p);
    assert!(
        a.reply
            .ends_with(&format!("\nrefused {} why=daemon\n", token(4200))),
        "{}",
        a.reply
    );
    assert!(p.borrow().signals.is_empty());
}

/// Every refusal word is read off the row, or, for a pid the view has no row of, off the
/// identity and presence reads; none sends a signal.
#[test]
fn each_refusal_word_comes_from_the_rows_facts() {
    let fg = LeftRow {
        foreground: true,
        ..named(4300, State::Orphaned, Tier::Never, Why::Live)
    };
    let never = named(4301, State::Orphaned, Tier::Never, Why::Live);
    let unproven = named(4302, State::Orphaned, Tier::Name, Why::Orphaned);
    let v = view(vec![fg, never, unproven]);
    let p = procs(&[4300, 4301, 4302, 4400]);
    let rest = [4300, 4301, 4302, 4400, 4500].map(token).join(" ");
    let a = end(OWNER, &rest, &v, &p);
    assert_eq!(
        a.reply,
        format!(
            "OK 5 end term=0 gone=0 killed=0 refused=5\n\
             refused {} why=foreground\nrefused {} why=never\nrefused {} why=not-proven\n\
             refused {} why=not-proven\nrefused {} why=stale\n",
            token(4300),
            token(4301),
            token(4302),
            token(4400),
            token(4500)
        )
    );
    assert!(p.borrow().signals.is_empty());

    let shown = Arc::new(view(Vec::new()));
    let mut read = || Some(Arc::clone(&shown));
    let mut post = |_: Message| {};
    let p = procs(&[]);
    let reply = run(
        &OWNER,
        &token(4600),
        World {
            versioned: true,
            view: &mut read,
            reread: &|_| None,
            authorized: &|| true,
            presence: &|_| ProcessPresence::Present,
            control: Control(&p),
            grace: Duration::ZERO,
            now_ms: NOW,
            post: &mut post,
            hung_up: &|| false,
        },
    );
    assert!(reply.ends_with("why=another-uid\n"), "{reply}");
}

/// A signal the kernel refuses for a process that still reads as the token's is `never`.
#[test]
fn a_signal_the_kernel_refuses_is_never() {
    let v = view(vec![offered(4100, Source::Tab(7))]);
    let p = procs(&[4100]);
    p.borrow_mut().refused.insert(4100);
    let a = end(EDGE, &token(4100), &v, &p);
    assert!(a.reply.ends_with("why=never\n"), "{}", a.reply);
    assert!(a.posted.is_empty());
}

/// An edge ends its own session's rows (W-session's tab, or a verify run that rode in it), and a
/// request naming one row of another session is denied whole, before anything is signalled.
/// NEGATIVE CONTROL: gate each token only after the one before it was ended, and the edge's own
/// row is signalled before the denial.
#[test]
fn an_edge_ends_its_own_sessions_row_and_is_denied_anothers_with_nothing_signalled() {
    let mine = offered(4100, Source::Tab(7));
    let theirs = offered(4200, Source::Tab(8));
    let run_row = LeftRow {
        proof: vec![Proof::Run, Proof::Time, Proof::Tree],
        ..offered(4300, Source::Run(RUN.into()))
    };
    let v = view(vec![mine, theirs, run_row]);
    let p = procs(&[4100, 4200, 4300]);
    let both = format!("{} {}", token(4100), token(4200));
    let a = end(EDGE, &both, &v, &p);
    assert_eq!(
        a.reply,
        format!("ERR denied end: {} is not this session's\n", token(4200))
    );
    assert!(p.borrow().signals.is_empty(), "{:?}", p.borrow().signals);
    assert!(a.posted.is_empty());

    let own = format!("{} {}", token(4100), token(4300));
    let a = end(EDGE, &own, &v, &p);
    assert!(
        a.reply
            .starts_with("OK 2 end term=2 gone=2 killed=0 refused=0\n"),
        "{}",
        a.reply
    );
    assert_eq!(p.borrow().signals, [(4100, SIGTERM), (4300, SIGTERM)]);
}

#[test]
fn a_closed_tabs_row_needs_owner() {
    let v = view(vec![offered(2606, Source::Closed(5))]);
    let p = procs(&[2606]);
    let a = end(EDGE, &token(2606), &v, &p);
    assert_eq!(
        a.reply,
        format!("ERR denied end: {} is not this session's\n", token(2606))
    );
    assert!(p.borrow().signals.is_empty());
    let a = end(OWNER, &token(2606), &v, &p);
    assert!(
        a.reply
            .starts_with("OK 1 end term=1 gone=1 killed=0 refused=0\n"),
        "{}",
        a.reply
    );
    assert_eq!(p.borrow().signals, [(2606, SIGTERM)]);
}

/// `end` names every row it ends: no `all`, no wildcard, no bare pid, no pattern, no token twice
/// and no more than 16; each is usage, answered before the view is read. NEGATIVE CONTROL: take
/// `all` as every offered row, and it is ended.
#[test]
fn end_all_a_wildcard_or_a_bare_pid_is_usage() {
    let v = view(vec![offered(4100, Source::Tab(7))]);
    let p = procs(&[4100]);
    let twice = format!("{} {}", token(4100), token(4100));
    let many: Vec<String> = (2..=18).map(token).collect();
    let many = many.join(" ");
    let with_all = format!("{} all", token(4100));
    for rest in [
        "", "all", "*", "4100", "4100@", "41*", "-1", "--all", &twice, &many, &with_all,
    ] {
        let a = end(OWNER, rest, &v, &p);
        assert_eq!(a.reply, USAGE, "{rest:?}");
        assert_eq!(a.views, 0, "{rest:?}: usage reads no view");
    }
    assert!(p.borrow().signals.is_empty(), "{:?}", p.borrow().signals);
}

/// Each signalled end is one `proc.` record with the row's proof words, its source, the signals
/// and the outcome; a refused one posts none. NEGATIVE CONTROL: drop the post, and none is made.
#[test]
fn each_end_posts_one_proc_record_with_its_proof_and_outcome() {
    let v = view(vec![
        offered(2606, Source::Closed(5)),
        named(4200, State::Left, Tier::Name, Why::Daemon),
    ]);
    let p = procs(&[2606, 4200]);
    let rest = format!("{} {}", token(2606), token(4200));
    let a = end(OWNER, &rest, &v, &p);
    assert!(a.reply.starts_with("OK 2 end term=1"), "{}", a.reply);
    let [record] = a.posted.as_slice() else {
        panic!("one record: {:?}", a.posted);
    };
    assert_eq!(record.key.as_deref(), Some("proc.end"));
    assert!(
        record
            .key
            .as_deref()
            .is_some_and(|k| k.starts_with("proc."))
    );
    assert_eq!(record.tag, tags::SYSTEM);
    assert_eq!(*record.need(), Need::Record);
    assert_eq!(record.title, "Ended sleep (pid 2606)");
    assert_eq!(
        record.detail,
        [
            "it outlived Tab 3 by 2 h 00 m",
            "proof: env,time (tier offer)",
            "SIGTERM; it is gone",
        ]
    );
}

/// Where the kernel keeps no pid version an exec keeps the identity. A row a run's folders
/// vouched for at the offer (C-tree) that now runs a program outside them is another program,
/// and is refused stale, though its session's tie still offers it from the same closed tab;
/// while the roots vouch for it on both readings, an exec within them is still the run's build.
/// NEGATIVE CONTROL: skip the executable comparison whenever the offer had C-tree, and the
/// program outside the roots is signalled.
#[test]
fn a_row_whose_roots_no_longer_vouch_for_its_executable_is_refused_stale() {
    let linux = Identity {
        pid: 4100,
        birth: Birth(555_000),
        idversion: None,
    };
    let row = LeftRow {
        token: Some(linux.token()),
        proof: vec![Proof::Env, Proof::Run, Proof::Time, Proof::Tree],
        exe_judged: Some("/w/wt/target/debug/deps/aterm_link-1f2e".into()),
        ..offered(4100, Source::Closed(5))
    };
    let v = view(vec![row.clone()]);
    let p = procs(&[]);
    p.borrow_mut().live.insert(4100, linux);
    let outside = row.clone();
    let execd = move |_: u32| {
        Some(LeftRow {
            proof: vec![Proof::Env, Proof::Run, Proof::Time],
            exe_judged: Some("/usr/bin/ssh-agent".into()),
            ..outside.clone()
        })
    };
    let a = end_with(OWNER, &linux.token(), &v, &p, &execd, false);
    assert_eq!(
        a.reply,
        "OK 1 end term=0 gone=0 killed=0 refused=1\nrefused 4100@555000 why=stale\n"
    );
    assert!(p.borrow().signals.is_empty(), "{:?}", p.borrow().signals);

    let within = move |_: u32| {
        Some(LeftRow {
            exe_judged: Some("/w/wt/target/debug/aterm".into()),
            ..row.clone()
        })
    };
    let a = end_with(OWNER, &linux.token(), &v, &p, &within, false);
    assert!(
        a.reply
            .starts_with("OK 1 end term=1 gone=1 killed=0 refused=0\n"),
        "{}",
        a.reply
    );
    assert_eq!(p.borrow().signals, [(4100, SIGTERM)]);
}

/// A dead run's stage leader and a member still in its group, named leader first: once the
/// leader is gone its group proves nothing (W-group), and the member is still the run's by the
/// run id it carries (W-run with C-tree), so it is ended, and its record names the proof that
/// held at the signal. NEGATIVE CONTROL: require the witnesses the view showed, and the member
/// is refused not-proven.
#[test]
fn a_member_named_after_its_ended_group_leader_is_ended_on_its_run_witness() {
    let run_row = |pid: i32| LeftRow {
        proof: vec![Proof::Group, Proof::Run, Proof::Time, Proof::Tree],
        why: Why::LauncherDead,
        exe_judged: Some("/w/wt/target/debug/deps/aterm_link-1f2e".into()),
        ..offered(pid, Source::Run(RUN.into()))
    };
    let v = view(vec![run_row(4500), run_row(4501)]);
    let p = procs(&[4500, 4501]);
    let reread = |pid: u32| {
        let leader_gone = !p.borrow().live.contains_key(&4500);
        let mut row = v.rows.iter().find(|r| r.pid == pid).cloned()?;
        if pid == 4501 && leader_gone {
            row.proof.retain(|w| *w != Proof::Group);
        }
        Some(row)
    };
    let rest = format!("{} {}", token(4500), token(4501));
    let a = end_with(OWNER, &rest, &v, &p, &reread, true);
    assert_eq!(
        a.reply,
        format!(
            "OK 2 end term=2 gone=2 killed=0 refused=0\nended {} signal=term gone=1\nended {} \
             signal=term gone=1\n",
            token(4500),
            token(4501)
        )
    );
    assert_eq!(p.borrow().signals, [(4500, SIGTERM), (4501, SIGTERM)]);
    let [_, member] = a.posted.as_slice() else {
        panic!("two records: {:?}", a.posted);
    };
    assert_eq!(
        member.detail.get(1).map(String::as_str),
        Some("proof: run,time,tree (tier offer)"),
        "{:?}",
        member.detail
    );
}

/// An edge ends a verify run's rows only when that run rode in its own session (`sid=`): a run
/// another session launched is not its, and the request is denied whole. NEGATIVE CONTROL: let
/// an edge end any run's row, and the other session's run member is signalled.
#[test]
fn an_edge_is_denied_the_row_of_a_run_another_session_launched() {
    const THEIRS: &str = "52001-1792163900000000";
    let mut v = view(vec![LeftRow {
        proof: vec![Proof::Run, Proof::Time, Proof::Tree],
        ..offered(4400, Source::Run(THEIRS.into()))
    }]);
    v.runs.push(RunRow {
        id: THEIRS.into(),
        sid: Some("s-b9".into()),
        ..v.runs[0].clone()
    });
    let p = procs(&[4400]);
    let a = end(EDGE, &token(4400), &v, &p);
    assert_eq!(
        a.reply,
        format!("ERR denied end: {} is not this session's\n", token(4400))
    );
    assert!(p.borrow().signals.is_empty(), "{:?}", p.borrow().signals);
}

/// A row the fresh reading ties to another source than the view did (a closed tab's row that
/// now reads as a run's) is not the row the caller was shown, and is refused not-proven with no
/// signal. NEGATIVE CONTROL: drop the source comparison, and it is signalled.
#[test]
fn a_row_whose_source_changed_since_the_offer_is_refused_not_proven() {
    let row = offered(2606, Source::Closed(5));
    let v = view(vec![row.clone()]);
    let p = procs(&[2606]);
    let elsewhere = move |_: u32| {
        Some(LeftRow {
            from: Source::Run(RUN.into()),
            ..row.clone()
        })
    };
    let a = end_with(OWNER, &token(2606), &v, &p, &elsewhere, true);
    assert_eq!(
        a.reply,
        format!(
            "OK 1 end term=0 gone=0 killed=0 refused=1\nrefused {} why=not-proven\n",
            token(2606)
        )
    );
    assert!(p.borrow().signals.is_empty(), "{:?}", p.borrow().signals);
}

/// A caller that hung up while its request ran is told nothing more and ends nothing more: the
/// token being ended finishes, and no later one is signalled. NEGATIVE CONTROL: ignore the
/// hang-up, and the second token is signalled for nobody.
#[test]
fn a_caller_that_hung_up_ends_no_further_token() {
    let v = view(vec![
        offered(4100, Source::Tab(7)),
        offered(4101, Source::Tab(7)),
    ]);
    let p = procs(&[4100, 4101]);
    let shown = Arc::new(v.clone());
    let mut read = || Some(Arc::clone(&shown));
    let mut post = |_: Message| {};
    let same = |pid: u32| {
        v.rows
            .iter()
            .find(|r| r.pid == pid)
            .cloned()
            .map(|row| LeftView {
                rows: vec![row],
                ..v.clone()
            })
    };
    let hung_up = || !p.borrow().signals.is_empty();
    let reply = run(
        &EDGE,
        &format!("{} {}", token(4100), token(4101)),
        World {
            versioned: true,
            view: &mut read,
            reread: &same,
            authorized: &|| true,
            presence: &|_| ProcessPresence::Absent,
            control: Control(&p),
            grace: Duration::ZERO,
            now_ms: NOW,
            post: &mut post,
            hung_up: &hung_up,
        },
    );
    assert_eq!(p.borrow().signals, [(4100, SIGTERM)]);
    assert_eq!(
        reply,
        format!(
            "OK 1 end term=1 gone=1 killed=0 refused=0\nended {} signal=term gone=1\n",
            token(4100)
        )
    );
}

/// A RESTORED CLOSE STANDS FOR `end`'S RE-PROOF: this process's close index never held a close an
/// earlier aterm recorded, so a restored certificate (under a key no session here has) stands on
/// its own, while one of this process's that the index does not confirm is dropped. NEGATIVE
/// CONTROL: confirm every certificate by the index alone, and the restored one is dropped.
#[test]
fn a_restored_close_stands_without_this_processes_close_index() {
    let store = crate::session_store::new_store();
    let shown = view(Vec::new());
    let restored = ClosedTab {
        key: crate::vitals_left::RESTORED_KEYS,
        sid: "s-0000000000000000a007".into(),
        restored: true,
        ..shown.closed[0].clone()
    };
    let offered = LeftView {
        closed: vec![shown.closed[0].clone(), restored.clone()],
        ..shown
    };
    let (handles, live, closed) =
        fresh_context_with(&store, &offered, &|_| None, &|_| None).expect("an empty roster");
    assert!(handles.is_empty() && live.is_empty());
    assert_eq!(closed, [restored]);
}

/// THE LEFT DOOR RE-PROVES EACH ROW AT THE YIELD (the harness's `IdleHost::leftover`, over a fake
/// view and a fake re-read): of a whole view, the agent's rows that re-prove as the same token
/// from the same source hold, its tab's and its dead run's; a row whose identity moved since the
/// view, or that is gone, holds nothing; a `name` row and another session's are never the
/// agent's; a partial view, or none, is unchecked, and so is a whole one where any row's read
/// could not be made.
/// NEGATIVE CONTROL: skip the re-read, and the moved row holds; read an unread row as gone, and the
/// whole view reads two rows.
#[test]
fn the_left_door_reproves_each_row_at_the_yield() {
    use aterm_agent::harness::leftover::LeftRead;
    use aterm_vitals::left::LiveTab;
    let tab = |key: u64, sid: &str| LiveTab {
        key,
        sid: sid.into(),
        shell_pid: 40 + u32::try_from(key).expect("a key"),
        shell_uniqueid: Some(key),
        fg_pgid: None,
        agent_spans: Vec::new(),
    };
    let live = vec![tab(7, "s-a7"), tab(1, "s-a1")];
    let whole = |rows: Vec<LeftRow>| LeftView {
        live: live.clone(),
        complete: true,
        ..view(rows)
    };
    let name = named(105, State::Left, Tier::Name, Why::Daemon);
    let rows = vec![
        offered(101, Source::Tab(7)),
        offered(102, Source::Tab(7)),
        offered(103, Source::Tab(7)),
        offered(104, Source::Run(RUN.into())),
        name.clone(),
        offered(106, Source::Tab(1)),
    ];
    let reread = |pid: u32| -> Reread {
        let row = match pid {
            101 => offered(101, Source::Tab(7)),
            // Another process now holds the pid: its birth moved.
            102 => LeftRow {
                token: Some(
                    Identity {
                        birth: Birth(1_792_198_840_004_410),
                        ..ident(102)
                    }
                    .token(),
                ),
                ..offered(102, Source::Tab(7))
            },
            104 => offered(104, Source::Run(RUN.into())),
            _ => return Reread::Read(None),
        };
        Reread::Read(Some(whole(vec![row])))
    };
    assert_eq!(
        left_at_yield(Some(&whole(rows.clone())), "s-a7", &reread),
        LeftRead::Rows(vec![ident(101), ident(104)])
    );
    assert_eq!(
        left_at_yield(
            Some(&whole(vec![name, offered(106, Source::Tab(1))])),
            "s-a7",
            &reread
        ),
        LeftRead::Rows(Vec::new()),
        "a name row and another session's are never the agent's"
    );
    let unread = |pid: u32| {
        if pid == 103 {
            Reread::Unread
        } else {
            reread(pid)
        }
    };
    assert_eq!(
        left_at_yield(Some(&whole(rows.clone())), "s-a7", &unread),
        LeftRead::Unchecked,
        "a row whose read could not be made is no row gone"
    );
    for partial in [
        LeftView {
            complete: false,
            ..whole(rows.clone())
        },
        LeftView {
            bounded: true,
            ..whole(rows.clone())
        },
    ] {
        assert_eq!(
            left_at_yield(Some(&partial), "s-a7", &reread),
            LeftRead::Unchecked
        );
    }
    assert_eq!(left_at_yield(None, "s-a7", &reread), LeftRead::Unchecked);
}

/// THE LEFT DOOR'S READ OF A ROW IS UNREAD WHERE THE HOST'S TABS ARE (`reread_current`): a context
/// that could not be captured, before or after the row's read, or one that changed between the
/// two captures, makes the row's read unread, and the yield over it unchecked rather than clean;
/// two captures that agree pass the read through. NEGATIVE CONTROL: read a first capture that
/// failed as the row gone (`Reread::Read(None)`), and its case reads gone, not unread.
#[test]
fn a_row_read_over_tabs_that_could_not_be_captured_leaves_the_yield_unchecked() {
    use aterm_agent::harness::leftover::LeftRead;
    use aterm_vitals::left::LiveTab;
    let store = crate::session_store::new_store();
    let tab = |fg: u32| LiveTab {
        key: 7,
        sid: "s-a7".into(),
        shell_pid: 47,
        shell_uniqueid: Some(7),
        fg_pgid: Some(fg),
        agent_spans: Vec::new(),
    };
    let row = offered(101, Source::Tab(7));
    let fresh = LeftView {
        live: vec![tab(47)],
        complete: true,
        ..view(vec![row.clone()])
    };
    let captures = Cell::new(0);
    // Which capture fails (`Some(n)`: the n-th of a row's two), and from which capture the
    // foreground moves.
    let read_with = |fails: Option<u32>, moves_from: Option<u32>| {
        captures.set(0);
        reread_current(
            &store,
            &|| {
                captures.set(captures.get() + 1);
                let n = captures.get();
                if fails == Some(n) {
                    return None;
                }
                let fg = if moves_from.is_some_and(|from| n >= from) {
                    2_600
                } else {
                    47
                };
                Some((Vec::new(), vec![tab(fg)], Vec::new()))
            },
            &|_, _| Reread::Read(Some(fresh.clone())),
        )
    };
    assert_eq!(read_with(None, None), Reread::Read(Some(fresh.clone())));
    for (what, fails, moves) in [
        ("no first capture", Some(1), None),
        ("no second capture", Some(2), None),
        ("a foreground handoff during the read", None, Some(2)),
    ] {
        assert_eq!(read_with(fails, moves), Reread::Unread, "{what}");
        let at_yield = left_at_yield(
            Some(&LeftView {
                live: vec![tab(47)],
                complete: true,
                ..view(vec![row.clone()])
            }),
            "s-a7",
            &|_| read_with(fails, moves),
        );
        assert_eq!(at_yield, LeftRead::Unchecked, "{what}");
    }
}

/// THE LEFT DOOR READS ONLY A FRESH VIEW (`fresh_left`): of the snapshot `SYS.fresh` answers, a
/// fresh one's left view is read, and a stale or warming one's is none, so the yield over it is
/// unchecked: a view older than a row born since would read that row's absence as clean.
/// NEGATIVE CONTROL: take the view whatever its freshness, and the stale view is read.
#[test]
fn the_left_door_reads_only_a_fresh_view() {
    use aterm_agent::harness::leftover::LeftRead;
    use aterm_vitals::sys::Freshness;
    let empty = LeftView {
        complete: true,
        ..LeftView::default()
    };
    let held = || {
        Some(crate::sys_host::Held {
            snap: Arc::new(aterm_vitals::model::Snapshot {
                left: Some(Arc::new(empty.clone())),
                ..aterm_vitals::model::Snapshot::default()
            }),
            unix_ms: 0,
            at: std::time::Instant::now(),
            jobs_at: None,
            left_at: None,
        })
    };
    let gone = |_: u32| Reread::Read(None);
    let fresh = fresh_left((held(), Freshness::Fresh));
    assert_eq!(fresh.as_deref(), Some(&empty));
    assert_eq!(
        left_at_yield(fresh.as_deref(), "s-a7", &gone),
        LeftRead::Rows(Vec::new())
    );
    for state in [Freshness::Stale, Freshness::Warming] {
        let old = fresh_left((held(), state));
        assert_eq!(old, None, "{state:?}");
        assert_eq!(
            left_at_yield(old.as_deref(), "s-a7", &gone),
            LeftRead::Unchecked,
            "{state:?}"
        );
    }
    assert_eq!(fresh_left((None, Freshness::Fresh)), None);
}
