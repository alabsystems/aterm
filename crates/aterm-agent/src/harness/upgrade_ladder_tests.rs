// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! THE LIVE UPGRADE'S LADDER, BOUND TO THE CODE (the owner's decision of
//! 2026-09-28: "Time ladder — prefer a natural pause; the longer it has been
//! behind, the less it waits … It never types over a draft, a dialog or
//! running work."). Three tests:
//!
//! * TIER-1: the derived `HarnessUpgradeLadder` machine against the REAL
//!   `upgrade::gate` (through `upgrade_codex::requested_step`, a daemon-mode
//!   Codex client — the incident's), over every state the machine reaches.
//!   Each model state becomes the screens, the person's stamps, the clock and
//!   the daemon's threads and clients of real looks, and the facts are built
//!   from them by the ONE ASSEMBLY the visit builds them by — `ladder_look`
//!   (the rung, the person, the one reader's idle reading and the
//!   repaint-proof settle: the same the Claude Code lane's `look` calls),
//!   `codex_daemon_turn` (the daemon's turns: the kernel's naming of the own
//!   conversation, the still run's placing of another session's root),
//!   `screen_turn`
//!   and `quiet_s` — never set by hand. Every `Buggy` member caught where the
//!   real rule disagrees with it, and every `#[refines]` anchor entered;
//! * the repaint-proof settle, looked at;
//! * the 2026-09-28 INCIDENT, REPLAYED through the same assembly on its own
//!   schedule: the pin overnight, nine hours of goal-mode turns in the
//!   daemon, the owner's Esc and the `»` input line — and again with one more
//!   thread on the daemon (the second review), and in the owner's measured
//!   shape of that evening, one conversation and three subagents it spawned
//!   under one TUI (the third review).

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use aterm_phase::codex::fixtures::{BOX_EXEC, GOAL_BUSY_0_158, GOAL_NEXT_TURN_0_158};
use aterm_phase::prompt::fixtures::screen as fixture;
use aterm_spec::derive::{Model, harness_upgrade_ladder_model};

use super::super::{IDLE_RUN_GAP_S, Screen, ladder_look, ladder_projection};
use super::*;
use crate::harness::upgrade::{DaemonTurn, QUIET_S, Request, Rung};
use crate::supervise::screen::HumanInput;

type State = BTreeMap<&'static str, i64>;

/// `[harness] human_grace_s` at its default.
const GRACE: u32 = 120;

/// A look's clock: the incident's own day (2026-09-28 15:04:17Z).
const NOW: u64 = 1_790_607_857;

/// The tab's Codex, another tab's Codex on the same daemon, and the daemon.
const TUI: u32 = 4_242;
const OTHER_TUI: u32 = 4_343;
const DAEMON_PID: u32 = 5_151;

/// This tab's conversation (its root) and a subagent it spawned; another
/// session's conversation and a subagent of that one; a root no tab shows.
const OWN: &str = "01a0dc36-1dc1-7ee2-be91-11bc323e377c";
const OWN_SUB: &str = "01a0e8a1-0000-7000-8000-000000000001";
const OTHERS: &str = "01a0dc3a-5c71-7143-bcee-58def0a15dfd";
const OTHERS_SUB: &str = "01a0e8a1-0000-7000-8000-000000000002";
const DETACHED: &str = "01a0dc3b-0000-7000-8000-000000000003";

/// The status line a look reads (the incident's Codex: agent unknown).
const STATUS: &str = "OK schema=1 agent=unknown agent_since_ms=5";

fn v(s: &str) -> Version {
    Version::parse(s).expect("a version")
}

fn reach(m: &Model) -> BTreeSet<State> {
    let mut seen: BTreeSet<State> = BTreeSet::new();
    let mut queue = VecDeque::from([m.init_state()]);
    while let Some(s) = queue.pop_front() {
        if !seen.insert(s.clone()) {
            continue;
        }
        for a in &m.actions {
            queue.extend(m.successors(a.name, &s));
        }
    }
    seen
}

/// The owner's tab as the incident showed it between two goal turns — the
/// measured 0.158.0 frame a second after a turn's end row, whose screen reads
/// ENDED while the goal's next turn already streams under it — the input
/// line drawn with `mark`, the last message `words`, the goal's counter in
/// the footer reading `minute`: what repaints between two looks while the
/// words stand still.
fn tab_screen(mark: char, words: &str, minute: u32) -> Vec<String> {
    let mut rows = fixture(GOAL_NEXT_TURN_0_158);
    let last = rows
        .iter()
        .position(|r| r.starts_with("• The last turn"))
        .expect("the last message");
    rows[last] = format!("• {words}");
    rows[last + 1] = String::new();
    rows[last + 2] = String::new();
    let caret = aterm_phase::codex::composer(&rows).expect("the input line");
    rows[caret] = rows[caret].replacen('»', &mark.to_string(), 1);
    let footer = rows.len() - 2;
    rows[footer] = rows[footer].replace("3h 14m)", &format!("3h {minute:02}m)"));
    rows
}

/// The same screen with NO GOAL in its footer: the footer's `Pursuing goal
/// (…)` cleared, the rest of the row kept.
fn without_goal(mut rows: Vec<String>) -> Vec<String> {
    let footer = rows.len() - 2;
    let pursuing = aterm_phase::anchors::anchor_text("codex.goal.pursuing");
    if let Some(at) = rows[footer].find(pursuing) {
        let kept = rows[footer][..at].trim_end().len();
        rows[footer].truncate(kept);
    }
    assert!(!cx::goal_on_screen(&rows), "no goal left: {rows:?}");
    rows
}

/// The same tab mid-turn: its status row over the input line.
fn busy_screen(mark: char) -> Vec<String> {
    let mut rows = fixture(GOAL_BUSY_0_158);
    let caret = aterm_phase::codex::composer(&rows).expect("the input line");
    rows[caret] = rows[caret].replacen('»', &mark.to_string(), 1);
    rows
}

/// One read of the tab's screen as `screen` returns it: `rows`, the cursor
/// on the input line — at its placeholder, or after a `drafted` text —
/// `seq`, and the person's stamp `human`.
fn read(rows: Vec<String>, drafted: bool, seq: u64, human: HumanInput) -> Screen {
    let cursor = aterm_phase::codex::composer(&rows).map(|c| (c, if drafted { 16 } else { 2 }));
    Screen {
        rows,
        cursor,
        seq,
        first: 0,
        generation: None,
        human,
    }
}

/// ONE READ OF THE DAEMON as the pass read it — on the managed build — by
/// which threads it holds, which of them run a turn, and whether another
/// Codex is attached (another tab's TUI).
#[derive(Clone, Copy, Debug, Default)]
struct Daemon {
    /// This tab's conversation's root, running.
    own: bool,
    /// A subagent that root spawned: held (`Some`), running or not.
    own_sub: Option<bool>,
    /// Another session's conversation's root: held, running or not.
    others: Option<bool>,
    /// A subagent of that conversation: held, running or not.
    others_sub: Option<bool>,
    /// A root no tab shows (run in the background): held, running or not.
    detached: Option<bool>,
    /// Another Codex attached to the daemon besides this tab's.
    attached: bool,
}

impl Daemon {
    fn view(self) -> DaemonView {
        let turn = |running: bool| {
            if running {
                TurnState::Busy
            } else {
                TurnState::Idle
            }
        };
        let loaded = |thread: &str, running: bool, lineage: cx::Lineage| cx::Loaded {
            thread: thread.to_string(),
            turn: turn(running),
            lineage,
        };
        let mut threads = vec![loaded(OWN, self.own, cx::Lineage::Root)];
        let spawned = |parent: &str| cx::Lineage::Spawned(parent.to_string());
        if let Some(running) = self.own_sub {
            threads.push(loaded(OWN_SUB, running, spawned(OWN)));
        }
        if let Some(running) = self.others {
            threads.push(loaded(OTHERS, running, cx::Lineage::Root));
        }
        if let Some(running) = self.others_sub {
            threads.push(loaded(OTHERS_SUB, running, spawned(OTHERS)));
        }
        if let Some(running) = self.detached {
            threads.push(loaded(DETACHED, running, cx::Lineage::Root));
        }
        DaemonView {
            running: Some(v("0.158.0")),
            wait: String::new(),
            pid: DAEMON_PID,
            threads,
        }
    }

    /// The Codex the kernel lists as attached to the daemon: this tab's, and
    /// another tab's where one is attached.
    fn clients(self) -> impl FnOnce(u32) -> Option<Vec<u32>> {
        move |pid| {
            assert_eq!(pid, DAEMON_PID, "asked of its own daemon");
            Some(if self.attached {
                vec![TUI, OTHER_TUI]
            } else {
                vec![TUI]
            })
        }
    }
}

/// ONE REAL LOOK of a daemon-mode Codex client at `t`: the facts its visit
/// builds of the screen `scr` and the daemon `d`, by the visit's own
/// assembly — [`quiet_s`] over the
/// status and the screen's `seq`, [`ladder_look`], [`screen_turn`] (no
/// rollout: a daemon-mode client's thread is its daemon's) and
/// [`codex_daemon_turn`]. The composer's placeholder cell reads `dim` unless
/// `drafted` (the `cell` verb the visit asks). With the look's ladder
/// reading, whose `idle_read` is where the host steps at all.
fn look(
    st: &mut St,
    scr: &Screen,
    drafted: bool,
    d: Daemon,
    t: u64,
) -> (Facts, super::super::LadderLook) {
    let quiet = quiet_s(Some(STATUS), st, scr.seq, t);
    let ladder = ladder_look(st, "codex", scr, TUI, GRACE, t);
    let (turn, busy) = screen_turn(&scr.rows, None);
    let f = Facts {
        status: turn.to_string(),
        status_age_s: quiet,
        composer_empty: cx::composer_is_empty(&scr.rows, scr.cursor, !drafted),
        approval_box: cx::box_on_screen(&scr.rows),
        busy_footer: busy,
        quiet_s: quiet,
        attended: ladder.attended,
        rung: ladder.rung,
        typing: ladder.typing,
        still_s: ladder.still_s,
        daemon_turn: codex_daemon_turn(
            st,
            TUI,
            &Mode::Daemon,
            &scr.rows,
            Some(&d.view()),
            &ladder,
            d.clients(),
            t,
        ),
        ..Facts::default()
    };
    (f, ladder)
}

/// Whose turn a model state's `work` 2 is — a turn this screen never draws
/// and the lane never places: a subagent of this tab's own conversation
/// (the owner's shape of 2026-09-28), another session's subagent, or a root
/// no tab shows with this TUI the daemon's only client.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Hidden {
    OwnSub,
    OthersSub,
    Detached,
}

/// ONE DAEMON HISTORY a model state admits: whose turn a `work` 2 is
/// (`hidden`); whether the turn `work` stands for ran at the earlier look of
/// the still run too (`work_earlier`: unseen — a goal's, or a hidden one);
/// whether another session's root ran there (`other_earlier`); and whether
/// another session is on the daemon at all, another Codex attached
/// (`shared`: its root idle where the state runs none — so the kernel names
/// no conversation).
#[derive(Clone, Copy, Debug)]
struct History {
    hidden: Hidden,
    work_earlier: bool,
    other_earlier: bool,
    shared: bool,
}

/// Every daemon history of `s` the bind looks at: the model's own (this
/// tab's turn idle at the earlier look, another session's root running there
/// as `other_seen` says, on the daemon where `other` or `other_seen` put
/// one); where this tab's turn runs, the same with another session on the
/// daemon, idle; for a turn the lane never places (`work` 2), each of its
/// kinds — this tab's subagent (named by the kernel, or not: another session
/// on the daemon), another session's subagent, a root no tab shows with no
/// other Codex attached — running at BOTH looks of a still run of two, the
/// third review's probe, where the rule that placed as it ran moved; and
/// under a GOAL the worst the state admits — this tab's own goal turn and
/// another session's running at both looks of the still run, and another
/// session on the daemon so the kernel names nothing: the second review's
/// probe, where the rule that placed through a goal moved.
fn histories(s: &State) -> Vec<History> {
    let model = History {
        hidden: Hidden::OwnSub,
        work_earlier: false,
        other_earlier: s["other_seen"] == 2,
        shared: false,
    };
    let mut out = vec![model];
    if s["work"] >= 1 {
        out.push(History {
            shared: true,
            ..model
        });
    }
    if s["work"] == 2 {
        let others_on = s["other"] == 1 || s["other_seen"] > 0;
        for hidden in [Hidden::OwnSub, Hidden::OthersSub, Hidden::Detached] {
            for shared in [false, true] {
                if hidden == Hidden::Detached && (shared || others_on) {
                    continue;
                }
                out.push(History {
                    hidden,
                    work_earlier: s["seen"] == 2,
                    shared,
                    ..model
                });
            }
        }
    }
    if s["goal"] == 1 && s["seen"] == 2 {
        out.push(History {
            hidden: Hidden::OwnSub,
            work_earlier: s["work"] == 1,
            other_earlier: s["other"] == 1,
            shared: true,
        });
    }
    out
}

/// The daemon of `s` under `h` at the EARLIER look of the still run
/// (`earlier`) or at this one: this tab's root running for `work` 1, the
/// turn a `work` 2 stands for running in the thread `h` says, another
/// session's root where another session is on it (running for `other`, and
/// at the earlier look as `h` says) — another Codex attached with it — the
/// turn `work` stands for running at the earlier look only as `h` says.
fn daemon_of(s: &State, h: History, earlier: bool) -> Daemon {
    let work_runs = !earlier || h.work_earlier;
    let mut d = Daemon {
        own: s["work"] == 1 && work_runs,
        ..Daemon::default()
    };
    if h.shared || s["other"] == 1 || s["other_seen"] > 0 {
        d.others = Some(if earlier {
            h.other_earlier
        } else {
            s["other"] == 1
        });
        d.attached = true;
    }
    if s["work"] == 2 {
        match h.hidden {
            Hidden::OwnSub => d.own_sub = Some(work_runs),
            Hidden::OthersSub => {
                d.others.get_or_insert(false);
                d.others_sub = Some(work_runs);
                d.attached = true;
            }
            Hidden::Detached => d.detached = Some(work_runs),
        }
    }
    d
}

/// A MODEL STATE AS REAL LOOKS, under the daemon history `h`: the rung as
/// how long the session has been behind; the tab's screen (a box, a draft,
/// or the goal's idle-looking frame with `mark`, its footer's `Pursuing goal
/// (…)` for `goal` alone); the person's stamp for `keys`; the daemon of
/// [`daemon_of`]; then the looks: for `seen` 2, one QUIET_S earlier at the
/// same words (the footer's counter repainted between them and the `seq`
/// moved unless `quiet`), the daemon as it was then; the last at NOW. The
/// last look's facts and ladder reading.
fn facts_of(s: &State, h: History) -> (Facts, super::super::LadderLook) {
    const BEHIND: [u64; 4] = [
        60,
        upgrade::RUNG_SETTLED_S,
        upgrade::RUNG_KEYS_S,
        upgrade::RUNG_LAND_S,
    ];
    let mark = if s["glyph"] == 1 { '»' } else { '›' };
    let drafted = s["draft"] == 1;
    let rows = |minute: u32| {
        let tab = if s["goal"] == 1 {
            tab_screen(mark, "Done.", minute)
        } else {
            without_goal(tab_screen(mark, "Done.", minute))
        };
        if s["dialog"] == 1 {
            fixture(BOX_EXEC)
        } else if drafted {
            let mut rows = tab;
            let caret = aterm_phase::codex::composer(&rows).expect("the input line");
            rows[caret] = format!("{mark} fix the footer");
            rows
        } else {
            tab
        }
    };
    let human = match s["keys"] {
        0 => HumanInput::Ago(600_000),
        1 => HumanInput::Ago(60_000),
        _ => HumanInput::Ago(1_000),
    };
    let mut st = St {
        pending_since: NOW - BEHIND[usize::try_from(s["rung"]).expect("a rung")],
        ..St::default()
    };
    // The screen first seen QUIET_S before the earlier look: the screen's own
    // quiet holds for `quiet` only where its `seq` never moved.
    let _ = quiet_s(Some(STATUS), &mut st, 7, NOW - 2 * QUIET_S);
    if s["seen"] == 2 {
        let earlier = read(rows(50), drafted, 7, human);
        let _ = look(
            &mut st,
            &earlier,
            drafted,
            daemon_of(s, h, true),
            NOW - QUIET_S,
        );
    }
    let now = read(
        rows(51),
        drafted,
        if s["quiet"] == 1 { 7 } else { 8 },
        human,
    );
    look(&mut st, &now, drafted, daemon_of(s, h, false), NOW)
}

/// The real client step of a daemon-mode Codex over those facts: the host
/// steps (the one reader's idle reading) and the step is the `/exit`.
fn real_moves(f: &Facts, trusted: bool, request: &Request) -> bool {
    trusted
        && cx::requested_step(
            request,
            &Mode::Daemon,
            &Phase::Pending,
            f,
            false,
            false,
            NOW,
            "0.158.0",
        ) == Step::Terminate
}

fn target() -> Target {
    Target {
        twin: PathBuf::from("/stand-in/agents/codex"),
        exe: PathBuf::from("/stand-in/store/codex/0.158.0/bin/codex"),
        version: v("0.158.0"),
    }
}

/// The real daemon rule over a model state: its tab due — for the move while
/// it is owed, else for the pin alone (`daemon_owes`, the pin not looked at
/// within PIN_LOOK_S) — and its daemon on the managed build, pinned or not,
/// this tab's and another session's threads running or not.
fn real_pins(s: &State) -> bool {
    let pinned = s["pinned"] == 1;
    let due = s["behind"] == 1
        || (daemon_owes(Some(v("0.158.0")), pinned, &target()) == Owes::Pin
            && pin_look_due(None, NOW));
    due && cx::daemon_step(&DaemonFacts {
        running: Some(v("0.158.0")),
        managed: v("0.158.0"),
        pinned,
        busy_threads: usize::from(s["work"] > 0) + usize::from(s["other"] == 1),
        terminals: 0,
        settling_threads: 0,
        attended: false,
        held: false,
        owner_held: false,
        unseen_clients: 0,
    }) == DaemonStep::Update
}

/// TIER-1: the derived `HarnessUpgradeLadder` against the REAL rule, over
/// EVERY state the committed machine reaches. The model's `Move` is enabled
/// exactly where a look the host takes would type the `/exit` — the rung,
/// the screen's quiet, the reader's run of looks, the person, the draft, the
/// box, the mark, this tab's turn and another session's in the daemon each
/// built from real looks by the visit's own assembly — and its `Pin` exactly
/// where the real daemon rule pins a tab that is due; the rungs are the real
/// `rung`'s; the daemon's turns are the ones `codex_daemon_turn` decides
/// (this tab's own conversation named by the kernel where every thread of
/// the daemon hangs from its root and it is the one client — a subagent's
/// turn included; another session's running ROOT placed by the still run,
/// another Codex attached — never under a goal; a turn the lane never
/// places, holding it for as long as it runs); and the real facts project
/// back onto the state they were built from
/// (`upgrade_drive::ladder_projection`). Each state is looked at under every
/// daemon history it admits ([`histories`]) — among them, under a goal, this
/// tab's own goal turn running unseen through both looks of the still run
/// with another session on the daemon, and a subagent's turn (this tab's
/// own, or another session's) or a root no tab shows running through both
/// looks, where `Move` is not enabled and the real rule must wait. Every
/// `#[refines]` anchor of the machine is ENTERED on the way (the evidence
/// window).
///
/// NEGATIVE CONTROLS — each `Buggy` member caught ALONE in a state the
/// machine reaches, where the real rule does otherwise: the owner's `--now`
/// that waived the person (`NowWaivesThePerson`: the real word waits a
/// keystroke within KEYS_GAP_S out), a move over a draft, a box or this
/// tab's own turn in the daemon (the real one waits), the still run placing
/// through a goal (`PlacedUnderAGoal`: the real one waits), the still run
/// placing as it ran (`PlacedAsItRan`: the real one waits), the seq settle
/// (`SeqSettle`: the real one moves where it waited), the grace that never
/// narrows, the reader blind to `»` and every thread of the daemon holding
/// the client (`OtherTurnHolds`: the real one moves where they, once
/// decided, cannot), and the pin that let a current tab go (`PinDropsDue`:
/// the real due question answers yes).
#[test]
fn the_real_ladder_is_the_derived_machine() {
    use aterm_spec::xref;
    assert!(xref::reset_entered_anchors(), "the evidence window opens");
    let model = harness_upgrade_ladder_model();
    let states = reach(&model);
    assert!(
        states.len() > 2_000,
        "the machine has a space: {}",
        states.len()
    );
    // NEGATIVE CONTROLS on the Buggy=1 machine, each mutant caught ALONE at
    // a state the committed machine reaches, where it is enabled and the real
    // rule does otherwise (a degraded lane: once it has decided, it refuses
    // the move the real rule takes there).
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let mut caught: BTreeSet<&str> = BTreeSet::new();
    let (mut moves, mut waits, mut placed, mut unseen, mut hidden) = (0, 0, 0, 0, 0);
    let mut kinds: BTreeSet<(&str, bool)> = BTreeSet::new();
    for s in &states {
        assert_eq!(model.action_enabled("Pin", s), real_pins(s), "{s:?}");
        if s["behind"] == 0 {
            if buggy.action_enabled("PinDropsDue", s)
                && daemon_owes(Some(v("0.158.0")), s["pinned"] == 1, &target()) == Owes::Pin
            {
                caught.insert("PinDropsDue");
            }
            continue;
        }
        for h in histories(s) {
            let (f, ladder) = facts_of(s, h);
            let real = real_moves(&f, ladder.idle_read, &Request::None);
            assert_eq!(model.action_enabled("Move", s), real, "{s:?}\n{h:?}\n{f:?}");
            if real {
                moves += 1;
            } else {
                waits += 1;
            }
            let goal = s["goal"] == 1;
            let placed_there = s["other"] == 1 && s["other_seen"] == 2 && !goal;
            let unplaced = s["other"] == 1 && !placed_there;
            // The kernel names this tab's conversation where nothing but it
            // is on the daemon — its root, the subagent it spawned — and it
            // is the one client.
            let named = !(h.shared || s["other"] == 1 || s["other_seen"] > 0)
                && (s["work"] != 2 || h.hidden == Hidden::OwnSub);
            // Where the host steps (the one reader's idle reading — a box on
            // the screen is none, and ends the still run), the daemon's turns
            // as the model says: a turn of this tab's own conversation holds
            // it — named by the kernel, a goal where its footer shows one; a
            // turn the lane never places holds it, named or not; another
            // session's root only until placed — never under a goal.
            if ladder.idle_read {
                let want = match (s["work"] > 0, named) {
                    (true, true) if goal => DaemonTurn::Goal,
                    (true, true) => DaemonTurn::Own,
                    (true, false) => DaemonTurn::Unplaced,
                    (false, _) if unplaced => DaemonTurn::Unplaced,
                    (false, _) => DaemonTurn::None,
                };
                assert_eq!(f.daemon_turn, want, "{s:?}\n{h:?}");
                if placed_there {
                    placed += 1;
                }
                if h.work_earlier && s["seen"] == 2 {
                    unseen += 1;
                    if s["work"] == 2 {
                        hidden += 1;
                        kinds.insert((
                            match h.hidden {
                                Hidden::OwnSub => "own subagent",
                                Hidden::OthersSub => "another's subagent",
                                Hidden::Detached => "a root no tab shows",
                            },
                            named,
                        ));
                    }
                }
                let mut want: BTreeMap<&str, i64> =
                    ["rung", "quiet", "seen", "keys", "draft", "dialog"]
                        .into_iter()
                        .map(|k| (k, s[k]))
                        .collect();
                // One look is no run: the projection reads none, as the gate does.
                want.insert("seen", if s["seen"] == 2 { 2 } else { 0 });
                want.insert("work", i64::from(s["work"] > 0 || unplaced));
                assert_eq!(ladder_projection(&f), want, "{s:?}\n{h:?}");
            }
            for mutant in [
                "MoveOverADraft",
                "MoveOverABox",
                "MoveMidTurn",
                "PlacedUnderAGoal",
                "PlacedAsItRan",
            ] {
                if buggy.action_enabled(mutant, s) && !real {
                    caught.insert(mutant);
                }
            }
            if buggy.action_enabled("NowWaivesThePerson", s)
                && !real_moves(&f, ladder.idle_read, &Request::Now)
            {
                caught.insert("NowWaivesThePerson");
            }
            if buggy.action_enabled("SeqSettle", s) && real {
                caught.insert("SeqSettle");
            }
            // The degraded lanes, once decided: refused where the real rule
            // moves.
            for mutant in ["AttendedWithoutLadder", "BlindReader", "OtherTurnHolds"] {
                let mut decided = s.clone();
                if buggy.fire(mutant, &mut decided)
                    && !buggy.action_enabled("Move", &decided)
                    && real
                {
                    caught.insert(mutant);
                }
            }
        }
    }
    assert!(
        moves > 0 && waits > 0 && placed > 0 && unseen > 0 && hidden > 0,
        "{moves} {waits} {placed} {unseen} {hidden}"
    );
    // Every kind of turn the lane never places ran through a still run of
    // two looks, this tab's own subagent both named by the kernel and not.
    assert_eq!(
        kinds,
        BTreeSet::from([
            ("a root no tab shows", false),
            ("another's subagent", false),
            ("own subagent", false),
            ("own subagent", true),
        ])
    );
    // The rungs are the real clock's.
    for (behind, rung) in [
        (0, Rung::Prefer),
        (upgrade::RUNG_SETTLED_S, Rung::Settled),
        (upgrade::RUNG_KEYS_S, Rung::KeysOnly),
        (upgrade::RUNG_LAND_S, Rung::Land),
    ] {
        assert_eq!(upgrade::rung(behind), rung);
    }
    let every: BTreeSet<&str> = buggy
        .actions
        .iter()
        .map(|a| a.name)
        .filter(|name| !aterm_spec::interp::fired_actions(&model).contains(name))
        .collect();
    assert_eq!(caught, every, "every Buggy member caught by the real rule");

    // The anchors are linked, name the one projection, and were entered.
    let mut anchored: Vec<(&str, &str)> = xref::refinements()
        .filter(|a| a.machine == "HarnessUpgradeLadder")
        .map(|a| {
            assert_eq!(
                a.project,
                "aterm_agent::harness::upgrade_drive::ladder_projection"
            );
            (a.action, a.rust_method)
        })
        .collect();
    anchored.sort_unstable();
    assert_eq!(
        anchored,
        [
            ("Advance", "rung"),
            ("Look", "still"),
            ("Move", "daemon_turn"),
            ("Move", "gate"),
            ("Pin", "daemon_step"),
        ]
    );
    let entered = xref::entered_anchor_ids();
    for id in [
        "HarnessUpgradeLadder::Advance @ rung",
        "HarnessUpgradeLadder::Look @ still",
        "HarnessUpgradeLadder::Move @ daemon_turn",
        "HarnessUpgradeLadder::Move @ gate",
        "HarnessUpgradeLadder::Pin @ daemon_step",
    ] {
        assert!(entered.contains(id), "{id} entered: {entered:?}");
    }
}

/// THE REPAINT-PROOF SETTLE, looked at (`ladder_look`, `St::still`): two
/// looks QUIET_S apart at the same last words stand QUIET_S however the
/// screen repainted; new words, a look not idle, or a look past
/// `IDLE_RUN_GAP_S` begin the run again. NEGATIVE CONTROL: the screen's own
/// quiet (`quiet_s`, the verdict `unknown`) restarts at each repaint.
#[test]
fn the_still_run_counts_looks_at_the_same_words_and_not_repaints() {
    let mut st = St::default();
    let never = HumanInput::Never;
    let at = |st: &mut St, rows: Vec<String>, t: u64| {
        ladder_look(st, "codex", &read(rows, false, t, never), 7, GRACE, t).still_s
    };
    assert_eq!(
        at(&mut st, tab_screen('»', "Done.", 51), 1_000),
        0,
        "one look is no run"
    );
    assert_eq!(quiet_s(Some(STATUS), &mut st, 1, 1_000), 0);
    assert_eq!(at(&mut st, tab_screen('»', "Done.", 52), 1_030), 30);
    assert_eq!(
        quiet_s(Some(STATUS), &mut st, 2, 1_030),
        0,
        "the screen's own quiet restarts at the repaint"
    );
    // New words begin it again.
    assert_eq!(at(&mut st, tab_screen('»', "Now the tests.", 52), 1_060), 0);
    // A look not idle ends it.
    assert_eq!(at(&mut st, busy_screen('»'), 1_090), 0);
    let busy = ladder_look(
        &mut st,
        "codex",
        &read(busy_screen('»'), false, 1, never),
        7,
        GRACE,
        1_095,
    );
    assert!(!busy.idle_read, "a busy screen is no idle reading");
    assert_eq!(at(&mut st, tab_screen('»', "Now the tests.", 52), 1_120), 0);
    assert_eq!(
        at(&mut st, tab_screen('»', "Now the tests.", 53), 1_150),
        30
    );
    // A look after a long gap begins it again.
    assert_eq!(
        at(
            &mut st,
            tab_screen('»', "Now the tests.", 53),
            1_150 + IDLE_RUN_GAP_S + 1
        ),
        0
    );
}

/// One look of the incident's day: the tab's screen and its `seq`, whether
/// this tab's thread runs a turn in the daemon, and the owner's last
/// keystroke (unix s).
struct DayLook {
    rows: Vec<String>,
    seq: u64,
    running: bool,
    human_at: Option<u64>,
}

impl DayLook {
    /// The person stamp a look at `t` reads.
    fn human(&self, t: u64) -> HumanInput {
        self.human_at
            .map_or(HumanInput::Never, |at| HumanInput::Ago((t - at) * 1_000))
    }
}

/// The incident's GOAL LOOKS, 06:09:22Z to 14:59:53Z, a look every 200 s:
/// every fourth one mid-turn (the status row up), the rest idle-looking
/// between goal turns — this tab's thread running its goal's next turn at
/// every one — the words new every fourth look, the goal's counter
/// repainting at each.
fn goal_looks() -> Vec<(u64, DayLook)> {
    (0..)
        .map(|i: u64| 1_790_575_762 + i * 200)
        .take_while(|t| *t <= 1_790_607_593)
        .enumerate()
        .map(|(i, t)| {
            let rows = if i % 4 == 3 {
                busy_screen('›')
            } else {
                tab_screen(
                    '›',
                    &format!("Goal step {} done; continuing.", i / 4),
                    u32::try_from(i % 60).expect("a minute"),
                )
            };
            (
                t,
                DayLook {
                    rows,
                    seq: t,
                    running: true,
                    human_at: None,
                },
            )
        })
        .collect()
}

/// 15:04:17Z onward: the goal paused, the thread idle, the `»` line, the
/// owner's last keystroke at 15:02:39Z; the host's re-looks at +20 s, +80 s,
/// +380 s.
fn paused_looks() -> Vec<(u64, DayLook)> {
    [1_790_607_857, 1_790_607_877, 1_790_607_937, 1_790_608_237]
        .into_iter()
        .map(|t| {
            (
                t,
                DayLook {
                    rows: tab_screen('»', "Luna was the rate-limit nudge's pick.", 0),
                    seq: 99,
                    running: false,
                    human_at: Some(1_790_607_759),
                },
            )
        })
        .collect()
}

/// THE 2026-09-28 INCIDENT, REPLAYED through the visit's own assembly on its
/// own schedule (the journal of tab `s-0920…`, the rollout, the ledger). One
/// Codex session on its daemon, one tab attached: the kernel names the
/// thread as this tab's.
///
/// * 20:39Z–05:12Z: Codex 0.157.1 CURRENT on its current, UNPINNED daemon —
///   due for the PIN alone (main's rule, kept: that daemon's armed updater
///   restarted it mid-turn at 06:05:45Z). Looked at for it once an hour
///   (PIN_LOOK_S), each look `wait:pin:busy-thread` on the goal's thread,
///   owning no turn end — where every idle point of the goal's turns looked
///   before.
/// * 05:21:08Z 0.158.0 installed (behind from then). 06:09Z–14:59Z: a
///   goal-mode turn in the daemon at every look — the next one began within
///   14 ms of each end — its screen idle-looking between the busy rows, the
///   goal's counter repainting, the words changing turn by turn. The real
///   rule types NOTHING through Settled, KeysOnly and Land: this tab's own
///   thread runs its goal (`goal`, the wait the owner's "Pause the goal
///   briefly" will lift), and `/exit` might stop the turn. A step that
///   ignored the daemon's turn (`MoveMidTurn`, the report's proposed
///   repaint-proof move of a daemon-mode client) would have ended the client
///   at 06:12Z, mid-goal.
/// * 15:01:13Z the owner's Esc pauses the goal; 15:02:39Z they send a
///   message, answered by 15:04:04Z; the thread idles until 15:18:42Z; the
///   input line is drawn `»` from 15:02Z. The real rule moves at the first
///   look of that pause, 15:04:17Z (the journal's own idle event): the Land
///   rung (9 h 43 m behind), the last keystroke 98 s old, the `»` line read
///   by the one reader. The reader of the incident (`BlindReader`, `›`
///   alone) never steps there — the owner quit and resumed Codex by hand at
///   15:27Z; the grace that never narrows (`AttendedWithoutLadder`, 120 s)
///   waits two more looks.
///
/// So it lands at the first pause the goal gave, 7 h 43 m past the Land rung
/// (07:21:08Z): no rung moves a client over its own running turn, and a goal
/// that never pauses gives none until aterm may pause it (the owner's
/// decision of 2026-09-28, not built here). The tab read
/// `integration=degraded` with no blocks at 15:35Z: the typed `/exit` then
/// names its thread from the KERNEL — the daemon's one thread, this TUI its
/// one client (`kernel_thread`) — as this replay's daemon is.
#[test]
fn the_incident_replayed_moves_at_the_first_pause_its_goal_gave() {
    const PENDING: u64 = 1_790_572_868; // 05:21:08Z
    const LAND_AT: u64 = PENDING + upgrade::RUNG_LAND_S; // 07:21:08Z

    // Overnight, before 0.158.0: current on a current, unpinned daemon — due
    // for the pin alone, looked at once an hour, never owning a turn end.
    let before = Target {
        version: v("0.157.1"),
        ..target()
    };
    assert_eq!(daemon_owes(Some(v("0.157.1")), false, &before), Owes::Pin);
    let (mut last, mut pin_looks, mut idle_points) = (None, 0, 0);
    for t in (1_790_541_540..=1_790_572_320).step_by(200) {
        // 20:39Z to 05:12Z, an idle point every 200 s.
        idle_points += 1;
        if pin_look_due(last, t) {
            pin_looks += 1;
            last = Some(t);
            let step = cx::daemon_step(&DaemonFacts {
                running: Some(v("0.157.1")),
                managed: v("0.157.1"),
                pinned: false,
                busy_threads: 1,
                terminals: 0,
                settling_threads: 0,
                attended: false,
                held: false,
                owner_held: false,
                unseen_clients: 0,
            });
            assert_eq!(step, DaemonStep::Wait("busy-thread"));
            let word = pin_word("wait:busy-thread");
            assert_eq!(word, "wait:pin:busy-thread");
            assert!((0..8).all(|n| !super::super::owns_turn_ends(&word, n, 120)));
        }
    }
    assert!(
        idle_points > 150 && pin_looks == 9,
        "{idle_points} {pin_looks}"
    );

    // One look of the day at `t` ([`DayLook`]), and the mutant's knobs.
    #[derive(Clone, Copy, PartialEq)]
    enum Lane {
        Real,
        MoveMidTurn,
        BlindReader,
        AttendedWithoutLadder,
    }
    let step = |st: &mut St, t: u64, l: &DayLook, lane: Lane| -> Step {
        let scr = read(l.rows.clone(), false, l.seq, l.human(t));
        let d = Daemon {
            own: l.running,
            ..Daemon::default()
        };
        let (mut f, ladder) = look(st, &scr, false, d, t);
        let trusted = match lane {
            // The reader of the incident knew `›` alone.
            Lane::BlindReader => {
                ladder.idle_read
                    && aterm_phase::codex::composer(&l.rows)
                        .is_some_and(|c| l.rows[c].starts_with('›'))
            }
            _ => ladder.idle_read,
        };
        match lane {
            Lane::MoveMidTurn => f.daemon_turn = DaemonTurn::None,
            Lane::AttendedWithoutLadder => f.typing = f.attended,
            _ => {}
        }
        if !trusted {
            return Step::Wait("no-step");
        }
        cx::next_step(&Mode::Daemon, &Phase::Pending, &f, false, false, t)
    };

    let goal_looks = goal_looks();
    assert!(goal_looks.len() > 150, "{}", goal_looks.len());
    let paused = paused_looks();

    let run = |lane: Lane| -> (Option<(u64, Rung, bool)>, Vec<String>) {
        let mut st = St {
            pending_since: PENDING,
            ..St::default()
        };
        let mut waits = Vec::new();
        for (t, l) in goal_looks.iter().chain(&paused) {
            match step(&mut st, *t, l, lane) {
                Step::Terminate => return (Some((*t, st.rung_at(*t), l.running)), waits),
                Step::Wait(why) => waits.push(why.to_string()),
                other => panic!("{other:?}"),
            }
        }
        (None, waits)
    };

    // THE REAL RULE: nothing through nine hours of the goal's turns, at every
    // rung — the looks between turns wait `goal`, this tab's own goal named
    // by the kernel; the `/exit` at the first look of the owner's pause.
    let (moved, waits) = run(Lane::Real);
    assert_eq!(
        moved,
        Some((1_790_607_857, Rung::Land, false)),
        "at 15:04:17Z, on the Land rung, the daemon's thread idle"
    );
    const { assert!(1_790_607_857 > LAND_AT) };
    assert!(waits.iter().any(|w| w == "goal"), "{waits:?}");
    assert!(
        !waits.iter().any(|w| w == "daemon-busy"),
        "never a turn it cannot place: the kernel names this tab's own"
    );
    // MoveMidTurn: the report's repaint-proof client move, blind to the
    // daemon's turn — mid-goal, three minutes into the Settled rung's run.
    let (mid, _) = run(Lane::MoveMidTurn);
    let mid = mid.expect("it moves");
    assert!(mid.0 < 1_790_577_000 && mid.2, "mid-turn: {mid:?}");
    assert_eq!(mid.1, Rung::Settled);
    // BlindReader: no step at the `»` line — never, in the replay.
    assert_eq!(run(Lane::BlindReader).0, None);
    // AttendedWithoutLadder: the person 98 s and 118 s ago still holds it.
    assert_eq!(
        run(Lane::AttendedWithoutLadder).0.map(|(t, _, _)| t),
        Some(1_790_607_937),
        "two looks later"
    );
}

/// THE RULE OF THE ROUND BEFORE THE THIRD REVIEW (2026-09-28), kept here as
/// the replays' negative-control lane: the kernel named a client's thread
/// only where its daemon held ONE writer lock and it was the one client —
/// so a conversation with subagents was never named — and a running thread
/// the still run placed (`elsewhere`) held nothing, whatever it was — a
/// subagent, a root with no other Codex attached — unless a goal showed on
/// this screen (`goal`).
fn round_before(
    threads: &[cx::Loaded],
    clients: &[u32],
    elsewhere: &[String],
    goal: bool,
) -> DaemonTurn {
    if let ([only], [pid]) = (threads, clients)
        && *pid == TUI
    {
        return match (only.running(), goal) {
            (false, _) => DaemonTurn::None,
            (true, true) => DaemonTurn::Goal,
            (true, false) => DaemonTurn::Own,
        };
    }
    let elsewhere: &[String] = if goal { &[] } else { elsewhere };
    if threads
        .iter()
        .any(|l| l.running() && !elsewhere.contains(&l.thread))
    {
        DaemonTurn::Unplaced
    } else {
        DaemonTurn::None
    }
}

/// The still run's placing as a look of `ladder` at `t` computes it over the
/// daemon `d` (`record`: the run's record, kept across looks) — the threads
/// found running through QUIET_S of it (`cx::still_through`).
fn placed_by(
    record: &mut Vec<(String, u64)>,
    d: &DaemonView,
    ladder: &super::super::LadderLook,
    t: u64,
) -> Vec<String> {
    let running: Vec<String> = d
        .threads
        .iter()
        .filter(|l| l.running())
        .map(|l| l.thread.clone())
        .collect();
    let in_run = ladder.idle_read && ladder.still_s > 0;
    let (kept, placed) = if ladder.idle_read {
        cx::still_through(record, &running, in_run, t)
    } else {
        (Vec::new(), Vec::new())
    };
    *record = kept;
    placed
}

/// THE SECOND REVIEW'S PROBE (2026-09-28), REPLAYED: the incident's own goal
/// looks — this tab's thread running its goal at every one of them — with
/// ONE MORE thread on the daemon, idle: another tab's Codex session (two
/// clients), or a thread no tab shows (this TUI the daemon's only client).
/// The kernel names no conversation, so this tab's screen is all the lane
/// has, and it reads the goal's own turns ENDED at the same last words for
/// 200 s at a time. The real rule places nothing under the goal its footer
/// shows — nor, with this TUI the only client, anything at all: it types
/// NOTHING through the nine hours — `daemon-busy` between the busy rows,
/// never `goal` (the thread is not proven this tab's) — and moves at the
/// first look of the owner's pause, as the one-thread replay does.
///
/// NEGATIVE CONTROLS, each ending the client MID-GOAL: `MoveMidTurn` (the
/// daemon's turn ignored), and `PlacedUnderAGoal` — the rule of the rounds
/// before, the still run placing through the goal ([`placed_by`] handed to
/// [`round_before`] as though no goal showed), which the review's probe
/// caught typing `/exit` at 06:12:42Z, the second goal look.
#[test]
fn a_second_thread_on_the_daemon_never_lets_the_goal_be_ended() {
    const PENDING: u64 = 1_790_572_868; // 05:21:08Z
    #[derive(Clone, Copy, PartialEq, Debug)]
    enum Lane {
        Real,
        MoveMidTurn,
        PlacedUnderAGoal,
    }
    let goal_looks = goal_looks();
    let paused = paused_looks();
    for (label, two_clients) in [("two clients", true), ("a detached thread", false)] {
        let run = |lane: Lane| -> (Option<(u64, bool)>, Vec<String>) {
            let mut st = St {
                pending_since: PENDING,
                ..St::default()
            };
            // The round before's own record of the still run's threads.
            let mut record: Vec<(String, u64)> = Vec::new();
            let mut waits = Vec::new();
            for (t, l) in goal_looks.iter().chain(&paused) {
                let scr = read(l.rows.clone(), false, l.seq, l.human(*t));
                let d = if two_clients {
                    Daemon {
                        own: l.running,
                        others: Some(false),
                        attached: true,
                        ..Daemon::default()
                    }
                } else {
                    Daemon {
                        own: l.running,
                        detached: Some(false),
                        ..Daemon::default()
                    }
                };
                let (mut f, ladder) = look(&mut st, &scr, false, d, *t);
                match lane {
                    Lane::Real => {}
                    Lane::MoveMidTurn => f.daemon_turn = DaemonTurn::None,
                    Lane::PlacedUnderAGoal => {
                        let view = d.view();
                        let placed = placed_by(&mut record, &view, &ladder, *t);
                        let clients = d.clients()(DAEMON_PID).expect("clients");
                        f.daemon_turn = round_before(&view.threads, &clients, &placed, false);
                    }
                }
                if !ladder.idle_read {
                    waits.push("no-step".to_string());
                    continue;
                }
                match cx::next_step(&Mode::Daemon, &Phase::Pending, &f, false, false, *t) {
                    Step::Terminate => return (Some((*t, l.running)), waits),
                    Step::Wait(why) => waits.push(why.to_string()),
                    other => panic!("{label}: {other:?}"),
                }
            }
            (None, waits)
        };

        let (moved, waits) = run(Lane::Real);
        assert_eq!(
            moved,
            Some((1_790_607_857, false)),
            "{label}: at 15:04:17Z, the goal paused, every thread idle"
        );
        assert!(
            waits.iter().any(|w| w == "daemon-busy"),
            "{label}: {waits:?}"
        );
        assert!(
            !waits.iter().any(|w| w == "goal"),
            "{label}: the thread is never proven this tab's"
        );
        // MoveMidTurn: the daemon's turn ignored.
        let (mid, _) = run(Lane::MoveMidTurn);
        let mid = mid.expect("it moves");
        assert!(mid.1 && mid.0 < 1_790_577_000, "{label}: mid-goal: {mid:?}");
        // The round before: placed through the goal, and ended at 06:12:42Z.
        let (placed, _) = run(Lane::PlacedUnderAGoal);
        assert_eq!(
            placed,
            Some((1_790_575_962, true)),
            "{label}: the probe's `/exit`, mid-goal"
        );
    }
}

/// THE THIRD REVIEW'S SHAPE (2026-09-28), REPLAYED: the owner's daemon as it
/// was measured that evening, read-only — ONE conversation, the root this
/// tab's one TUI resumed, and three subagents it spawned, their turns begun
/// on their own after the spawn and drawn nowhere on the tab's screen, which
/// stands idle at the same last words look after look. The looks: every
/// 200 s from the Land rung on, a subagent running at each of the first
/// eight (its turn a few minutes long, the next one's beginning as the last
/// ends), the root idle — then, at the last two, every thread idle. With the
/// goal footer (`goal`) or without it.
///
/// THE REAL RULE: the kernel names the conversation — every thread hangs
/// from the root, and this TUI is the one client — so a subagent's turn is
/// the tab's own: it waits `daemon-turn` (`goal` under the footer) at every
/// look a subagent runs, types nothing, and moves at the first look with
/// every thread idle. NEGATIVE CONTROLS: the rule of the round before
/// ([`round_before`] over [`placed_by`]) named nothing — four locks — and
/// placed the running subagent after 20 s of the still screen: `/exit` at
/// the second look, the subagent mid-turn (without the footer; under it, it
/// waited `daemon-busy`, never `goal`). And the same looks with ANOTHER
/// session on the daemon, another Codex attached (the kernel names nothing
/// now): the real rule never places a subagent, `daemon-busy` until it ends.
#[test]
fn the_owners_shape_holds_while_a_subagent_of_its_own_conversation_runs() {
    const PENDING: u64 = 1_790_572_868; // 05:21:08Z
    let looks: Vec<(u64, bool)> = (0..10)
        .map(|i| (PENDING + upgrade::RUNG_LAND_S + 60 + i * 200, i < 8))
        .collect();
    for goal in [false, true] {
        for shared in [false, true] {
            let run = |before: bool| -> (Option<(u64, bool)>, Vec<String>) {
                let mut st = St {
                    pending_since: PENDING,
                    ..St::default()
                };
                let mut record: Vec<(String, u64)> = Vec::new();
                let mut waits = Vec::new();
                for (t, sub_running) in &looks {
                    let words = tab_screen('»', "Spawned three reviewers; waiting on them.", 0);
                    let rows = if goal { words } else { without_goal(words) };
                    let scr = read(rows, false, 99, HumanInput::Never);
                    let d = Daemon {
                        own_sub: Some(*sub_running),
                        others: shared.then_some(false),
                        attached: shared,
                        ..Daemon::default()
                    };
                    let (mut f, ladder) = look(&mut st, &scr, false, d, *t);
                    if before {
                        let view = d.view();
                        let placed = placed_by(&mut record, &view, &ladder, *t);
                        let clients = d.clients()(DAEMON_PID).expect("clients");
                        f.daemon_turn = round_before(&view.threads, &clients, &placed, goal);
                    }
                    assert!(ladder.idle_read, "the tab reads idle throughout");
                    match cx::next_step(&Mode::Daemon, &Phase::Pending, &f, false, false, *t) {
                        Step::Terminate => return (Some((*t, *sub_running)), waits),
                        Step::Wait(why) => waits.push(why.to_string()),
                        other => panic!("{goal} {shared}: {other:?}"),
                    }
                }
                (None, waits)
            };
            let label = format!("goal={goal} shared={shared}");
            let (moved, waits) = run(false);
            assert_eq!(
                moved,
                Some((looks[8].0, false)),
                "{label}: at the first look with every thread idle: {waits:?}"
            );
            let want = match (shared, goal) {
                (false, true) => "goal",
                (false, false) => "daemon-turn",
                (true, _) => "daemon-busy",
            };
            assert_eq!(waits, vec![want.to_string(); 8], "{label}");
            if shared {
                continue;
            }
            // The round before: four locks named nothing, and the subagent
            // that ran through 20 s of the still screen was placed.
            let (before, waits) = run(true);
            if goal {
                assert_eq!(before, Some((looks[8].0, false)), "{label}");
                assert!(
                    waits.iter().all(|w| w == "daemon-busy"),
                    "{label}: {waits:?}"
                );
            } else {
                assert_eq!(
                    before,
                    Some((looks[1].0, true)),
                    "{label}: `/exit` at the second look, the subagent mid-turn"
                );
            }
        }
    }
}

// The goal pause's Tier-1 bind and its replay of the incident.
#[path = "upgrade_goal_tests.rs"]
mod goal;
