// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A RUN'S END SAYS WHO ASKED FOR IT (fix E2; findings E2E-2, E2E-17): an end aterm asked for
//! (the restart act's signals, the `signal` verb's ending signals) is written before the signal
//! (`act word=restart|signal by=<asker>`) and closes `out=signal by=<asker>`, raising no
//! `failure`; and a program or agent run that held the front until its command ended takes that
//! command's end, in both orders the sweep reads them in (the D first, or the front back at the
//! shell first).

use std::sync::Mutex;

use aterm_slog::{Actor, Node};

use super::tests::{Hand, WALL0, blocks, finished, lines_of, opened, running};
use super::*;
use crate::session_timeline::SessionTimeline;

/// The two orders a command's end is read in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Order {
    /// ORDER A: the block's D first, its run still in front (one `end_cmd` closes both).
    DFirst,
    /// ORDER B: the front back at the shell first (the sweep saw the shell before it read the D).
    FrontFirst,
}

const ORDERS: [Order; 2] = [Order::DFirst, Order::FrontFirst];

/// [`ASKED_END_STANDS`] in ms.
fn stands_ms() -> u64 {
    u64::try_from(ASKED_END_STANDS.as_millis()).expect("ms")
}

/// `name` in front (group `pgid`) under command `r1` (block 1, begun at 5 ms): its run `r2`,
/// opened at 20 ms.
fn in_front(name: &str, pgid: i32) -> (Mutex<SessionTimeline>, Hand) {
    let (tl, hand) = opened();
    blocks(&tl, &hand, 10, &running(1, 5, name));
    {
        let mut g = tl.lock().unwrap();
        hand.set(20);
        assert!(g.note_foreground_group(pgid));
        assert!(g.set_program(pgid, Some(name.into())));
    }
    blocks(&tl, &hand, 30, &running(1, 5, name));
    let got = lines_of(&tl);
    assert!(find(&got, &[" open r2 ", " par=r1 "]).is_some(), "{got:?}");
    (tl, hand)
}

/// `by` asks aterm at `ms` to end group `pgid` by `word` (the hook the act and the verb call).
fn ask(tl: &Mutex<SessionTimeline>, hand: &Hand, ms: u64, pgid: i32, word: AskWord, by: Actor) {
    hand.set(ms);
    EndAsker { timeline: tl, by }.ask(pgid, word);
}

/// Command `r1`'s block ends at `end_ms` with status `code`, read at `read_ms` in `order`.
fn end(
    tl: &Mutex<SessionTimeline>,
    hand: &Hand,
    order: Order,
    code: Option<i32>,
    (end_ms, read_ms): (u64, u64),
) {
    hand.set(read_ms);
    let read = finished(1, code, 5, end_ms, "x");
    let mut g = tl.lock().unwrap();
    match order {
        Order::DFirst => {
            let _ = g.sweep_slog(&read, false, 100, 100);
        }
        Order::FrontFirst => {
            assert!(g.note_foreground_group(100));
            let now = g.slog().now();
            g.slog_mut().shell_blocks(&read, None, now);
        }
    }
}

/// The index of the one record of `got` holding every piece of `what`.
fn find(got: &[String], what: &[&str]) -> Option<usize> {
    let mut hits = got
        .iter()
        .enumerate()
        .filter(|(_, l)| what.iter().all(|w| l.contains(w)));
    let first = hits.next().map(|(i, _)| i);
    assert!(hits.next().is_none(), "two records hold {what:?}: {got:?}");
    first
}

/// The end the log holds as asked: its group, its asker, and how long past the ask it stands.
fn asked(tl: &Mutex<SessionTimeline>) -> Option<(i32, Actor, u64)> {
    tl.lock().unwrap().slog().asked_end()
}

/// The nodes the fold holds a `failure` need for.
fn failures(tl: &Mutex<SessionTimeline>) -> Vec<Node> {
    let g = tl.lock().unwrap();
    g.slog()
        .log()
        .state()
        .needs
        .iter()
        .flatten()
        .filter(|n| n.word == "failure")
        .map(|n| n.node)
        .collect()
}

/// Whether any record `tl` holds raises a need.
fn any_raises(tl: &Mutex<SessionTimeline>) -> bool {
    let g = tl.lock().unwrap();
    g.slog()
        .log()
        .window()
        .any(|r| aterm_slog::need_raised(r).is_some())
}

/// 1a. AN ASKED RESTART, in both orders: `mark r2 act word=restart by=ctl` comes before the
/// agent run's `out=signal by=ctl` and the command's `out=signal code=143 by=ctl conf=strong`,
/// and no record raises a need.
#[test]
fn an_asked_restart_is_marked_first_and_its_ends_say_who_asked() {
    for order in ORDERS {
        let (tl, hand) = in_front("claude", 300);
        ask(&tl, &hand, 100, 300, AskWord::Restart, Actor::Ctl);
        end(&tl, &hand, order, Some(143), (900, 1_000));
        let got = lines_of(&tl);
        let mark = find(
            &got,
            &[&format!(
                " mark r2 act word=restart by=ctl src=aterm conf=exact t={}",
                WALL0 + 100
            )],
        );
        let agent = find(
            &got,
            &[
                " close r2 agent out=signal ",
                " by=ctl src=status conf=strong ",
            ],
        );
        let cmd = find(
            &got,
            &[
                " close r1 cmd out=signal ",
                " code=143 by=ctl src=shell conf=strong ",
            ],
        );
        assert!(
            mark.is_some() && mark < agent && agent < cmd,
            "{order:?}: the act's mark, then the agent's end, then the command's: {got:?}"
        );
        assert!(find(&got, &[" amend r2 "]).is_none(), "{order:?}: {got:?}");
        assert!(failures(&tl).is_empty(), "{order:?}: {got:?}");
        assert!(
            !any_raises(&tl),
            "{order:?}: no record raises a need: {got:?}"
        );
    }
}

/// 1b. THE ASK LAPSES: the same end [`ASKED_END_STANDS`] after the ask still reads asked, and
/// one millisecond later is the program's own: `out=fail` and one `failure`.
#[test]
fn an_end_past_the_asks_window_is_the_programs_own() {
    for order in ORDERS {
        for late in [0, 1] {
            let (tl, hand) = in_front("claude", 300);
            ask(&tl, &hand, 100, 300, AskWord::Restart, Actor::Ctl);
            let t = 100 + stands_ms() + late;
            end(&tl, &hand, order, Some(143), (t, t));
            let got = lines_of(&tl);
            let (cmd, agent) = if late == 0 {
                ("out=signal", "out=signal")
            } else {
                ("out=fail", "out=gone")
            };
            assert!(
                find(&got, &[&format!(" close r1 cmd {cmd} "), " code=143 "]).is_some(),
                "{order:?} late={late}: {got:?}"
            );
            let run = match (order, late) {
                (Order::FrontFirst, 1) => find(
                    &got,
                    &[&format!(" amend r2 agent {agent} src=shell conf=strong ")],
                )
                .and(find(&got, &[" close r2 agent out=unknown "])),
                _ => find(&got, &[&format!(" close r2 agent {agent} ")]),
            };
            assert!(run.is_some(), "{order:?} late={late}: {got:?}");
            assert_eq!(
                failures(&tl),
                if late == 0 {
                    vec![]
                } else {
                    vec![Node::new(aterm_slog::Lvl::Run, 1)]
                },
                "{order:?} late={late}: {got:?}"
            );
        }
    }
}

/// 1c. AN ASK IS ITS GROUP'S: an end asked of group 300 while group 400 holds the front leaves
/// group 400's command, which ends 1, its own end: `out=fail`, its run `gone`, one `failure`.
#[test]
fn an_end_of_another_group_is_that_programs_own() {
    for order in ORDERS {
        let (tl, hand) = in_front("vim", 400);
        ask(&tl, &hand, 100, 300, AskWord::Signal, Actor::Ctl);
        end(&tl, &hand, order, Some(1), (900, 1_000));
        let got = lines_of(&tl);
        assert!(
            find(
                &got,
                &[" close r1 cmd out=fail ", " code=1 src=shell conf=exact "]
            )
            .is_some(),
            "{order:?}: {got:?}"
        );
        let run = match order {
            Order::DFirst => find(&got, &[" close r2 prog out=gone "]),
            Order::FrontFirst => find(&got, &[" amend r2 prog out=gone src=shell conf=strong "]),
        };
        assert!(run.is_some(), "{order:?}: {got:?}");
        assert!(!got.iter().any(|l| l.contains("out=signal")), "{got:?}");
        assert_eq!(failures(&tl).len(), 1, "{order:?}: {got:?}");
    }
}

/// 1d. AN ASKED PROGRAM THAT EXITS 0: its command reads `ok` (an exit status of 0 is never
/// rewritten), its run `signal` by its asker.
#[test]
fn an_asked_program_that_exits_zero_leaves_its_command_ok() {
    for order in ORDERS {
        let (tl, hand) = in_front("less", 200);
        ask(&tl, &hand, 100, 200, AskWord::Signal, Actor::Harness);
        end(&tl, &hand, order, Some(0), (900, 1_000));
        let got = lines_of(&tl);
        assert!(
            find(
                &got,
                &[" mark r2 act word=signal by=harness src=aterm conf=exact "]
            )
            .is_some(),
            "{order:?}: {got:?}"
        );
        assert!(
            find(
                &got,
                &[" close r1 cmd out=ok ", " code=0 src=shell conf=exact "]
            )
            .is_some(),
            "{order:?}: {got:?}"
        );
        assert!(
            find(
                &got,
                &[" close r2 prog out=signal ", " by=harness src=status "]
            )
            .is_some(),
            "{order:?}: {got:?}"
        );
        assert!(failures(&tl).is_empty(), "{order:?}: {got:?}");
    }
}

/// 1e. A CLEAN EXIT, in both orders, of an agent run and a program run: read with the D first,
/// the run closes `ok`; read with the front back at the shell first, it closes `unknown` and its
/// command's end amends it `ok` before the command closes.
#[test]
fn a_run_that_held_the_front_until_its_command_ended_takes_its_end() {
    for (name, kind) in [("claude", "agent"), ("less", "prog")] {
        for order in ORDERS {
            let (tl, hand) = in_front(name, 300);
            end(&tl, &hand, order, Some(0), (900, 1_000));
            let got = lines_of(&tl);
            let cmd = find(&got, &[" close r1 cmd out=ok "]);
            match order {
                Order::DFirst => {
                    let run = find(&got, &[&format!(" close r2 {kind} out=ok ")]);
                    assert!(run.is_some() && run < cmd, "{name} {order:?}: {got:?}");
                }
                Order::FrontFirst => {
                    let closed = find(&got, &[&format!(" close r2 {kind} out=unknown ")]);
                    let amend = find(
                        &got,
                        &[&format!(
                            " amend r2 {kind} out=ok src=shell conf=strong t={}",
                            WALL0 + 1_000
                        )],
                    );
                    assert!(
                        closed.is_some() && closed < amend && amend < cmd,
                        "{name} {order:?}: {got:?}"
                    );
                }
            }
            assert!(failures(&tl).is_empty(), "{name} {order:?}: {got:?}");
        }
    }
}

/// 1f. A CRASH (code 137), in both orders: the run reads `gone` (its close, or its command's
/// amend), the command alone carries `code=` and the one `failure`, and once a relaunch's run
/// opens the harness's look lifts it (`mark - seen by=harness`).
#[test]
fn a_crashed_runs_command_carries_the_one_failure_the_relaunch_lifts() {
    for order in ORDERS {
        let (tl, hand) = in_front("claude", 300);
        end(&tl, &hand, order, Some(137), (900, 1_000));
        let got = lines_of(&tl);
        let run = match order {
            Order::DFirst => find(&got, &[" close r2 agent out=gone "]),
            Order::FrontFirst => find(&got, &[" amend r2 agent out=gone src=shell conf=strong "]),
        };
        assert!(run.is_some(), "{order:?}: {got:?}");
        assert!(
            find(&got, &[" close r1 cmd out=fail ", " code=137 "]).is_some(),
            "{order:?}: {got:?}"
        );
        assert_eq!(
            failures(&tl),
            vec![Node::new(aterm_slog::Lvl::Run, 1)],
            "{order:?}: one failure, the command's: {got:?}"
        );
        blocks(&tl, &hand, 1_200, &running(2, 1_100, "claude"));
        let seen = {
            let mut g = tl.lock().unwrap();
            let now = g.slog().now();
            g.slog_mut().harness_saw_failure(now)
        };
        let got = lines_of(&tl);
        assert!(
            seen && got
                .last()
                .is_some_and(|l| l.contains(" mark - seen by=harness ")),
            "{order:?}: {got:?}"
        );
        assert!(failures(&tl).is_empty(), "{order:?}: {got:?}");
    }
}

/// 1g. THE FRONT LEAVES FOR ANOTHER GROUP (500, not the shell's) before the D: the run's close
/// stays `unknown` and its command's end amends nothing.
#[test]
fn a_run_that_left_the_front_for_another_group_keeps_its_unknown() {
    let (tl, hand) = in_front("claude", 300);
    {
        let mut g = tl.lock().unwrap();
        hand.set(500);
        assert!(g.note_foreground_group(500));
    }
    blocks(&tl, &hand, 1_000, &finished(1, Some(0), 5, 900, "x"));
    let got = lines_of(&tl);
    assert!(
        find(&got, &[" close r2 agent out=unknown "]).is_some()
            && find(&got, &[" close r1 cmd out=ok "]).is_some(),
        "{got:?}"
    );
    assert!(find(&got, &[" amend r2 "]).is_none(), "{got:?}");
}

/// 1h. `signal tstp` AND `signal cont` ASK NO END: no act mark, and a stopped job that leaves the
/// front closes `unknown`; `signal term`, the control, marks and closes `signal`.
#[cfg(unix)]
#[test]
fn a_stop_or_a_resume_asks_no_end() {
    for (sig, asks) in [
        (libc::SIGTSTP, false),
        (libc::SIGCONT, false),
        (libc::SIGTERM, true),
    ] {
        let (tl, hand) = in_front("vim", 400);
        hand.set(100);
        let asker = EndAsker {
            timeline: &tl,
            by: Actor::Ctl,
        };
        assert_eq!(
            crate::control::ask_signal_end(Some(asker), sig, 400).is_some(),
            asks,
            "{sig}"
        );
        {
            let mut g = tl.lock().unwrap();
            hand.set(200);
            assert!(g.note_foreground_group(100));
        }
        let got = lines_of(&tl);
        assert_eq!(
            find(&got, &[" mark r2 act word=signal by=ctl "]).is_some(),
            asks,
            "{sig}: {got:?}"
        );
        let out = if asks { "signal" } else { "unknown" };
        assert!(
            find(&got, &[&format!(" close r2 prog out={out} ")]).is_some(),
            "{sig}: {got:?}"
        );
    }
}

/// 1i. A COMMAND THE SWEEP HAD YET TO SEE: the asked group led the terminal at the ask, so while
/// the log still has the shell (group 100) in front, the command whose own stamps span the ask is
/// that group's and takes the asked end: one the sweep never saw running (its D alone), and one
/// it saw running whose group it had yet to read. A command whose D was still unread but that
/// ended before the ask keeps its own end, and so does one begun after the ask.
#[test]
fn a_command_the_sweep_had_yet_to_see_takes_the_asked_end() {
    // Never seen running: the D alone, begun at 50, ended at 900 by the signal.
    let (tl, hand) = opened();
    ask(&tl, &hand, 100, 300, AskWord::Signal, Actor::Ctl);
    blocks(&tl, &hand, 1_000, &finished(1, Some(143), 50, 900, "sleep"));
    let got = lines_of(&tl);
    let mark = find(
        &got,
        &[" mark - act word=signal by=ctl src=aterm conf=exact "],
    );
    let close = find(
        &got,
        &[
            " close r1 cmd out=signal ",
            " code=143 by=ctl src=shell conf=strong ",
        ],
    );
    assert!(mark.is_some() && close.is_some(), "never seen: {got:?}");
    assert!(!any_raises(&tl), "never seen: {got:?}");

    // Seen running (block 1 begun at 50), its group never read.
    let (tl, hand) = opened();
    blocks(&tl, &hand, 60, &running(1, 50, "sleep"));
    ask(&tl, &hand, 100, 300, AskWord::Signal, Actor::Ctl);
    blocks(&tl, &hand, 1_000, &finished(1, Some(143), 50, 900, "sleep"));
    let got = lines_of(&tl);
    assert!(
        find(&got, &[" mark r1 act word=signal by=ctl "]).is_some()
            && find(&got, &[" close r1 cmd out=signal ", " code=143 by=ctl "]).is_some(),
        "seen running: {got:?}"
    );
    assert!(!any_raises(&tl), "seen running: {got:?}");

    // A command whose D is unread (it ended at 60, before the ask at 100) keeps its own end; the
    // job asked, block 2 (begun at 80), takes the asked end.
    let (tl, hand) = in_front("claude", 300);
    {
        let mut g = tl.lock().unwrap();
        hand.set(70);
        assert!(g.note_foreground_group(100));
    }
    ask(&tl, &hand, 100, 400, AskWord::Signal, Actor::Ctl);
    blocks(&tl, &hand, 1_000, &finished(1, Some(1), 5, 60, "x"));
    blocks(&tl, &hand, 1_100, &finished(2, Some(143), 80, 900, "sleep"));
    let got = lines_of(&tl);
    assert!(
        find(
            &got,
            &[" close r1 cmd out=fail ", " code=1 src=shell conf=exact "]
        )
        .is_some(),
        "the unread D's own end: {got:?}"
    );
    assert!(
        find(&got, &[" cmd out=signal ", " code=143 by=ctl "]).is_some(),
        "the job asked: {got:?}"
    );
    assert_eq!(
        failures(&tl),
        vec![Node::new(aterm_slog::Lvl::Run, 1)],
        "{got:?}"
    );

    // Begun after the ask (at 200), it is another's: `out=fail`.
    let (tl, hand) = opened();
    ask(&tl, &hand, 100, 300, AskWord::Signal, Actor::Ctl);
    blocks(&tl, &hand, 1_000, &finished(1, Some(1), 200, 900, "false"));
    let got = lines_of(&tl);
    assert!(
        find(
            &got,
            &[" close r1 cmd out=fail ", " code=1 src=shell conf=exact "]
        )
        .is_some(),
        "begun after: {got:?}"
    );
    assert_eq!(failures(&tl).len(), 1, "begun after: {got:?}");
}

/// How a run's command ends in [`a_run_closed_before_its_commands_end_takes_the_ask_or_that_end`].
#[derive(Clone, Copy, Debug)]
enum Ends {
    /// Asked at 100 by ctl (a restart), then 143.
    Asked,
    /// Killed: 137, nothing asked.
    Crashed,
    /// A clean exit: 0.
    Clean,
}

impl Ends {
    const ALL: [Self; 3] = [Self::Asked, Self::Crashed, Self::Clean];

    const fn code(self) -> i32 {
        match self {
            Self::Asked => 143,
            Self::Crashed => 137,
            Self::Clean => 0,
        }
    }
}

/// Whether `got` reads run `r<n>` (`kind`) ended as `ends` says, when a path that is not its
/// command's end closed it at `closed_ms`: under the ask, that close is `signal by=ctl` and no
/// amend follows; else it is `unknown`, and an amend after it and before the command's close
/// gives that end (`gone` for a failure, `ok` for 0).
fn closed_early_then_reads(got: &[String], n: u32, kind: &str, closed_ms: u64, ends: Ends) -> bool {
    let cmd = find(got, &[" close r1 cmd "]);
    let early = |out: &str| {
        find(
            got,
            &[
                &format!(" close r{n} {kind} out={out} "),
                &format!(" t={}", WALL0 + closed_ms),
            ],
        )
    };
    let amend = |out: &str| {
        find(
            got,
            &[&format!(
                " amend r{n} {kind} out={out} src=shell conf=strong "
            )],
        )
    };
    match ends {
        Ends::Asked => {
            early("signal").is_some_and(|i| got[i].contains(" by=ctl "))
                && find(got, &[&format!(" amend r{n} ")]).is_none()
        }
        Ends::Crashed => {
            let (closed, amended) = (early("unknown"), amend("gone"));
            closed.is_some() && closed < amended && amended < cmd
        }
        Ends::Clean => {
            let (closed, amended) = (early("unknown"), amend("ok"));
            closed.is_some() && closed < amended && amended < cmd
        }
    }
}

/// 1j. A RUN CLOSED BEFORE ITS COMMAND'S END IS READ, ON EVERY SUCH PATH (fix E2 review), in both
/// orders: its group's leader goes unnamed (the resolver can no longer read it: a dying leader
/// before the shell takes the front back, or a pipeline's leader gone first), or the screen no
/// longer shows the agent it alone named. Under an ask the run closes `signal by=ctl`; else it
/// closes `unknown`, and its command's end amends it (`gone` with one `failure`, on the
/// command, for a crash; `ok` for a clean exit).
#[test]
fn a_run_closed_before_its_commands_end_takes_the_ask_or_that_end() {
    for order in ORDERS {
        for ends in Ends::ALL {
            for (name, kind) in [("claude", "agent"), ("less", "prog")] {
                let (tl, hand) = in_front(name, 300);
                if matches!(ends, Ends::Asked) {
                    ask(&tl, &hand, 100, 300, AskWord::Restart, Actor::Ctl);
                }
                {
                    let mut g = tl.lock().unwrap();
                    hand.set(150);
                    assert!(g.set_program(300, None));
                }
                end(&tl, &hand, order, Some(ends.code()), (900, 1_000));
                let got = lines_of(&tl);
                assert!(
                    closed_early_then_reads(&got, 2, kind, 150, ends),
                    "unnamed {name} {order:?} {ends:?}: {got:?}"
                );
                let cmd = match ends {
                    Ends::Asked => " close r1 cmd out=signal ",
                    Ends::Crashed => " close r1 cmd out=fail ",
                    Ends::Clean => " close r1 cmd out=ok ",
                };
                assert!(
                    find(&got, &[cmd]).is_some(),
                    "unnamed {name} {order:?} {ends:?}: {got:?}"
                );
                assert_eq!(
                    failures(&tl),
                    if matches!(ends, Ends::Crashed) {
                        vec![Node::new(aterm_slog::Lvl::Run, 1)]
                    } else {
                        vec![]
                    },
                    "unnamed {name} {order:?} {ends:?}: {got:?}"
                );
            }

            // An agent the screen alone named (r3, under the program run r2 of `node`) that the
            // screen no longer shows at 150.
            let (tl, hand) = in_front("node", 300);
            {
                let mut g = tl.lock().unwrap();
                hand.set(40);
                g.publish_agent(
                    "busy",
                    None,
                    None,
                    Some(aterm_phase::Program::Claude),
                    super::tests::stamp(1),
                );
            }
            assert!(
                find(
                    &lines_of(&tl),
                    &[" open r3 agent par=r2 agent=claude src=screen "]
                )
                .is_some(),
                "{:?}",
                lines_of(&tl)
            );
            if matches!(ends, Ends::Asked) {
                ask(&tl, &hand, 100, 300, AskWord::Restart, Actor::Ctl);
            }
            {
                let mut g = tl.lock().unwrap();
                hand.set(150);
                g.publish_agent("-", None, None, None, super::tests::stamp(2));
            }
            end(&tl, &hand, order, Some(ends.code()), (900, 1_000));
            let got = lines_of(&tl);
            assert!(
                closed_early_then_reads(&got, 3, "agent", 150, ends),
                "screen {order:?} {ends:?}: {got:?}"
            );
            assert_eq!(
                failures(&tl).len(),
                usize::from(matches!(ends, Ends::Crashed)),
                "screen {order:?} {ends:?}: {got:?}"
            );
        }
    }
}

/// 1k. AN ASK STOOD, RENEWED OR TAKEN BACK (the restart act's `carry`, its `SIGKILL`'s renewal,
/// and a signal that was never delivered), in both orders:
/// - `carry`'s stand (`EndAsker::stand`, by=aterm) writes no mark, and the end closes
///   `signal by=aterm`;
/// - an end past the ask's window but inside a renewal made at 30 s (`renew_ask`, the act's
///   `SIGKILL`) still reads asked, and without the renewal (or with one for another group) is the
///   program's own;
/// - a signal whose `killpg` refused (`EndAsker::delivered` with a nonzero return) takes its ask
///   back: the mark stays and the end is the program's own; one delivered keeps it.
#[test]
fn an_ask_stood_renewed_or_taken_back_reads_as_it_stands() {
    /// The command and its agent run read `signal by=<by>` and no failure, or `fail` and `gone`
    /// with one failure.
    fn reads(tl: &Mutex<SessionTimeline>, order: Order, by: Option<&str>, what: &str) {
        let got = lines_of(tl);
        match by {
            Some(by) => {
                assert!(
                    find(
                        &got,
                        &[" close r2 agent out=signal ", &format!(" by={by} ")]
                    )
                    .is_some()
                        && find(
                            &got,
                            &[" close r1 cmd out=signal ", &format!(" by={by} src=shell ")]
                        )
                        .is_some(),
                    "{what} {order:?}: {got:?}"
                );
                assert!(failures(tl).is_empty(), "{what} {order:?}: {got:?}");
            }
            None => {
                let run = match order {
                    Order::DFirst => find(&got, &[" close r2 agent out=gone "]),
                    Order::FrontFirst => {
                        find(&got, &[" amend r2 agent out=gone src=shell conf=strong "])
                    }
                };
                assert!(
                    run.is_some() && find(&got, &[" close r1 cmd out=fail "]).is_some(),
                    "{what} {order:?}: {got:?}"
                );
                assert_eq!(failures(tl).len(), 1, "{what} {order:?}: {got:?}");
            }
        }
    }
    for order in ORDERS {
        // Stood by the carrier: no mark, the end is aterm's ask.
        let (tl, hand) = in_front("claude", 300);
        hand.set(100);
        EndAsker {
            timeline: &tl,
            by: Actor::Aterm,
        }
        .stand(300);
        assert_eq!(
            asked(&tl),
            Some((300, Actor::Aterm, stands_ms())),
            "{order:?}"
        );
        end(&tl, &hand, order, Some(143), (900, 1_000));
        assert!(
            find(&lines_of(&tl), &[" act "]).is_none(),
            "{order:?}: {:?}",
            lines_of(&tl)
        );
        reads(&tl, order, Some("aterm"), "stood");

        // Renewed at 30 s by the act's SIGKILL; an end 500 ms past the first window.
        for (renews, by) in [(Some(300), Some("ctl")), (Some(400), None), (None, None)] {
            let (tl, hand) = in_front("claude", 300);
            ask(&tl, &hand, 100, 300, AskWord::Restart, Actor::Ctl);
            if let Some(pgid) = renews {
                hand.set(30_100);
                renew_ask(&tl, pgid);
            }
            let renewed = if renews == Some(300) { 30_000 } else { 0 };
            assert_eq!(
                asked(&tl),
                Some((300, Actor::Ctl, renewed + stands_ms())),
                "{order:?} renewed for {renews:?}"
            );
            let t = 100 + stands_ms() + 500;
            end(&tl, &hand, order, Some(137), (t, t));
            reads(&tl, order, by, &format!("renewed for {renews:?}"));
        }

        // A signal never delivered takes its ask back; one delivered keeps it.
        for (rc, by) in [(-1, None), (0, Some("ctl"))] {
            let (tl, hand) = in_front("claude", 300);
            ask(&tl, &hand, 100, 300, AskWord::Signal, Actor::Ctl);
            let asker = EndAsker {
                timeline: &tl,
                by: Actor::Ctl,
            };
            assert_eq!(asker.delivered(300, rc), rc == 0, "rc={rc}");
            assert_eq!(
                asked(&tl).is_some(),
                rc == 0,
                "{order:?} rc={rc}: taken back only when not delivered"
            );
            end(&tl, &hand, order, Some(143), (900, 1_000));
            assert!(
                find(&lines_of(&tl), &[" mark r2 act word=signal by=ctl "]).is_some(),
                "rc={rc} {order:?}: the mark stays: {:?}",
                lines_of(&tl)
            );
            reads(&tl, order, by, &format!("rc={rc}"));
        }
    }
}
