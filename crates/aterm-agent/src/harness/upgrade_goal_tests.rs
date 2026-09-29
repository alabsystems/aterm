// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! THE GOAL PAUSE, BOUND TO THE CODE (the owner's decision of 2026-09-28:
//! "Pause the goal briefly — once the upgrade is due, aterm types `/goal
//! pause` (or presses Esc the instant a new goal turn starts, before it has
//! done anything), moves Codex onto the new build, then types `/goal
//! resume`. The goal carries on where it was; no running tool call is ever
//! cut off."). Two tests:
//!
//! * TIER-1: the derived `HarnessUpgradeGoalPause` machine against the REAL
//!   `upgrade_codex::goal_step`, `goal_owed` and `daemon_turn`, over every
//!   state the machine reaches — each state built into the look the visit
//!   builds ([`cx::GoalLook`]: the tab's goal record as a real
//!   [`Hold`], the footer's goal, the goal thread's head, the switch, a
//!   person's hand) and projected back (`upgrade_drive::goal_projection`);
//!   the ladder's clock read through the real `St::rung_at`, and the
//!   hold's bound and its rest aged through the real rule; every `Buggy`
//!   member caught where the real rule disagrees, every anchor entered;
//! * the 2026-09-28 INCIDENT, REPLAYED: a goal-mode daemon client whose next
//!   turn begins 14 ms after each one ends, looked at on its own schedule
//!   through the visit's assembly (`ladder_look`, `codex_daemon_turn`, the
//!   real gate) and the real `goal_step` — `wait:goal` until the Land rung,
//!   the pause there, the paused goal's last turn run to its end, the move,
//!   and the resume.

use aterm_spec::derive::harness_upgrade_goal_pause_model;

use super::*;
use crate::harness::goal_hold::{Hold, How, Owner, Stage};
use crate::harness::upgrade::Gate;
use crate::harness::upgrade_drive::goal_projection;
use aterm_phase::codex::CodexGoal;

/// The real hold record a model state's `held` stands for, at [`NOW`]: none
/// (or one rested); a typed pause past its take window, not seen; a pause
/// seen, within its bound; a resume made (after the move, or without it —
/// resting); released to a person's hand (resting).
fn hold_of(s: &State) -> Option<Hold> {
    let paused = |stage: Stage| Hold {
        stage,
        took_at: NOW - 10,
        ..Hold::pausing(Owner::Upgrade, How::Typed, TUI, "0.158.0", NOW - 20)
    };
    match s["held"] {
        1 => Some(Hold::pausing(
            Owner::Upgrade,
            How::Typed,
            TUI,
            "0.158.0",
            NOW - cx::PAUSE_TAKE_S,
        )),
        2 => Some(paused(Stage::Paused)),
        3 => Some(Hold {
            resume_at: NOW - 5,
            resumes: 1,
            why: if s["behind"] == 0 {
                "moved".into()
            } else {
                "abandoned:failed".into()
            },
            ..paused(Stage::Resumed)
        }),
        4 => Some(Hold {
            why: "by-hand".into(),
            ..paused(Stage::Released)
        }),
        _ => None,
    }
}

/// How long the upgrade a model state stands for has been behind at
/// [`NOW`]: at the ladder's Land rung for `land`, else at each rung short of
/// it (the ladder bind's `BEHIND`, up to a second before Land).
fn behind_of(land: i64) -> &'static [u64] {
    if land == 1 {
        &[upgrade::RUNG_LAND_S]
    } else {
        &[
            60,
            upgrade::RUNG_SETTLED_S,
            upgrade::RUNG_KEYS_S,
            upgrade::RUNG_LAND_S - 1,
        ]
    }
}

/// Whether an upgrade `behind` seconds behind at [`NOW`] stands at the
/// ladder's Land rung, as the visit reads it for the goal look's `land`
/// (`upgrade_codex_drive`: `facts.rung == Rung::Land`, the owner's `--now`
/// aside): the REAL rung, through the state's own `St::rung_at`
/// (`upgrade::rung`).
fn land_at(behind: u64) -> bool {
    St {
        pending_since: NOW - behind,
        ..St::default()
    }
    .rung_at(NOW)
        == Rung::Land
}

/// THE LOOK a model state stands for (`upgrade_codex::GoalLook`), with the
/// record `hold`: the footer's goal, the goal's own turn holding the move
/// while it is pursued, the ladder's clock at its Land rung or short of it
/// (the real rung, [`land_at`]), the gate otherwise open, the head read, the
/// composer free and nobody typing, a person's hand as `person` says.
fn look_of<'a>(s: &State, hold: Option<&'a Hold>) -> cx::GoalLook<'a> {
    cx::GoalLook {
        hold,
        behind: s["behind"] == 1,
        goal: Some(if s["goal"] == 1 {
            CodexGoal::Pursuing
        } else {
            CodexGoal::Paused
        }),
        footer: true,
        land: land_at(*behind_of(s["land"]).last().expect("an age")),
        goal_holds: s["goal"] == 1 && s["turn"] > 0,
        waived: Gate::Go,
        switch: s["switch"] == 1,
        moved: s["behind"] == 0,
        abandoned: (s["overdue"] == 1).then_some("abandoned:failed"),
        head: Some(s["turn"] == 1),
        free: true,
        esc_free: true,
        typing: false,
        person_since: s["person"] == 1,
        sandboxed: false,
        now: NOW,
    }
}

/// The ordinary waits a visit can end on at a HELD goal's idle point — a
/// turn, the daemon behind its client (its settle, its busy thread), a
/// person near the tab, the ladder's settle — each worded by the real
/// `goal_wait_word` as the visit words it, and whether the host then owns
/// the session's turn end (`upgrade_drive::owns_turn_ends`) for every one.
fn every_held_wait_owns_the_turn_end() -> bool {
    [
        "wait:goal",
        "wait:daemon-turn",
        "wait:not-idle",
        "wait:busy",
        "wait:settling",
        "wait:attended",
        "wait:held",
        "wait:daemon-first:settling",
        "wait:daemon-first:busy-thread",
        "wait:status-stale",
    ]
    .iter()
    .all(|w| {
        let r = Report {
            pid: TUI,
            tab: "t".into(),
            session: "s".into(),
            from: String::new(),
            to: String::new(),
            step: (*w).to_string(),
        };
        let word = super::super::goal_wait_word(r, true, false).step;
        crate::harness::upgrade_drive::owns_turn_ends(&word, 0, 120)
    })
}

/// The lane's acts the real `goal_step` decides at one look: each of the
/// model's lane actions, and whether the real step is it.
fn lane_of(real: &cx::GoalStep) -> [(&'static str, bool); 5] {
    [
        ("GoalPause", *real == cx::GoalStep::Pause),
        ("GoalEsc", *real == cx::GoalStep::Esc),
        ("Took", *real == cx::GoalStep::Took),
        ("Release", matches!(real, cx::GoalStep::Release(_))),
        ("GoalResume", matches!(real, cx::GoalStep::Resume(_))),
    ]
}

/// The real daemon's answer for this tab's own conversation — its root, the
/// daemon's only thread, this TUI its one client — running a turn as `turn`
/// says, under the goal the footer shows.
fn real_daemon_turn(s: &State) -> DaemonTurn {
    let threads = [cx::Loaded {
        thread: OWN.to_string(),
        turn: if s["turn"] > 0 {
            TurnState::Busy
        } else {
            TurnState::Idle
        },
        lineage: cx::Lineage::Root,
    }];
    cx::daemon_turn(TUI, &threads, Some(&[TUI]), &[], s["goal"] == 1)
}

/// TIER-1: the derived `HarnessUpgradeGoalPause` against the REAL rule, over
/// EVERY state the committed machine reaches where the lane still looks:
/// each lane action — `GoalPause`, `GoalEsc`, `Took`, `Release`,
/// `GoalResume` — is enabled exactly where the real `goal_step` decides it;
/// `LetGo` exactly where the real `goal_owed` owes nothing on a current tab;
/// `LadderMove` exactly where the real `daemon_turn` holds nothing (the ladder's
/// gate is `HarnessUpgradeLadder`'s); and the look projects back onto the
/// state it was built from.
///
/// THE CLOCK'S THREE STEPS, waived as the world's (the lane only reads the
/// clock) but aged through the real reader where the model takes them:
/// `Advance` — enabled exactly where the real `St::rung_at`
/// (`upgrade::rung`) stands short of Land at every rung before it, and
/// after it at Land; `MoveOverdue` — the paused hold aged to its bound
/// (`GOAL_HOLD_BOUND_S`, the real `goal_hold_past_bound`), the look projects
/// onto the model's successor (`overdue` through the bound, nothing
/// abandoned) and the real `goal_step` decides there what the model enables
/// at it, its resume `bound` (and not what it decided before, where the goal
/// shows paused); and `Rested` — the rest (`GOAL_REST_S`) run out, the real
/// `goal_rest_until` ends it and the look projects no hold.
///
/// NEGATIVE CONTROLS — each `Buggy` member caught ALONE in a state the
/// machine reaches, where the real rule does otherwise: the rule before the
/// owner's decision (`GoalHoldsTheTab`: once decided it never pauses, where
/// the real one pauses), the Esc past a head (the real one waits), the pause
/// under a switch or before Land (the real one passes), the resume made
/// again or over a person (the real one owes nothing), the host letting
/// a current tab go with its goal paused (the real `goal_owed` keeps it),
/// and a turn typed onto the paused thread (every wait the real visit words
/// at a held goal owns the turn end).
#[test]
fn the_real_goal_pause_is_the_derived_machine() {
    use aterm_spec::xref;
    assert!(xref::reset_entered_anchors(), "the evidence window opens");
    let model = harness_upgrade_goal_pause_model();
    let states = reach(&model);
    assert!(
        states.len() > 100,
        "the machine has a space: {}",
        states.len()
    );
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let mut caught: BTreeSet<&str> = BTreeSet::new();
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    for s in states.iter().filter(|s| s["looked"] == 1) {
        let hold = hold_of(s);
        let look = look_of(s, hold.as_ref());
        let real = cx::goal_step(&look);
        for (action, decided) in lane_of(&real) {
            assert_eq!(
                model.action_enabled(action, s),
                decided,
                "{action} at {s:?}: real {real:?}"
            );
            if decided {
                seen.insert(action);
            }
        }
        let owed = cx::goal_owed(hold.as_ref());
        assert_eq!(
            model.action_enabled("LetGo", s),
            s["behind"] == 0 && !owed,
            "LetGo at {s:?}"
        );
        assert_eq!(
            model.action_enabled("LadderMove", s),
            s["behind"] == 1 && real_daemon_turn(s) == DaemonTurn::None,
            "Move at {s:?}"
        );
        // ADVANCE, the ladder's clock: enabled exactly where the real rung
        // stands short of Land at every age this state stands for, and past
        // it the real rung stands at Land.
        let short = behind_of(s["land"]).iter().all(|&b| !land_at(b));
        assert_eq!(
            model.action_enabled("Advance", s),
            short,
            "Advance at {s:?}"
        );
        for next in model.successors("Advance", s) {
            assert!(
                behind_of(next["land"]).iter().all(|&b| land_at(b)),
                "Advance to {next:?}"
            );
            seen.insert("Advance");
        }
        // MOVE OVERDUE, the ladder's other floors and the clock: the paused
        // hold aged to its bound — exactly, by the real reader — is the
        // model's successor: its look projects onto it (`overdue` through
        // the bound, nothing abandoned), and the real rule decides there what
        // the model enables at it, a resume for the bound's own reason — and
        // not the step before it where the goal still shows paused (the
        // bound is what makes the resume due).
        for next in model.successors("MoveOverdue", s) {
            let held = hold.clone().expect("a paused goal's hold");
            let bound = NOW - cx::GOAL_HOLD_BOUND_S;
            let aged = Hold {
                at: bound - 20,
                tried_at: bound - 20,
                took_at: bound,
                ..held
            };
            assert!(
                cx::goal_hold_past_bound(&aged, NOW) && !cx::goal_hold_past_bound(&aged, NOW - 1),
                "aged to the bound exactly: {aged:?}"
            );
            let aged_look = look_of(s, Some(&aged));
            assert_eq!(aged_look.abandoned, None, "overdue by the bound alone");
            let p = goal_projection(&aged_look);
            for k in [
                "behind", "land", "goal", "held", "overdue", "switch", "person",
            ] {
                assert_eq!(p[k], next[k], "MoveOverdue: {k} of the aged look at {s:?}");
            }
            if s["goal"] == 1 {
                assert_eq!(p["turn"], next["turn"], "MoveOverdue: turn at {s:?}");
            }
            let at_bound = cx::goal_step(&aged_look);
            for (action, decided) in lane_of(&at_bound) {
                assert_eq!(
                    model.action_enabled(action, &next),
                    decided,
                    "MoveOverdue: {action} at {next:?}: real {at_bound:?} at the bound"
                );
            }
            if let cx::GoalStep::Resume(why) = at_bound {
                assert_eq!(why, "bound", "the resume is the bound's: {s:?}");
            }
            if s["goal"] == 2 {
                assert_ne!(real, at_bound, "the bound makes the resume due: {s:?}");
            }
            seen.insert("MoveOverdue");
        }
        // RESTED, the clock: a hold that ended without its move rests now,
        // and once `GOAL_REST_S` has passed the real `goal_rest_until` ends
        // the rest and the look projects no hold.
        for next in model.successors("Rested", s) {
            let h = hold.as_ref().expect("a hold that ended");
            let until = cx::goal_rest_until(h, NOW).expect("resting now");
            assert_eq!(cx::goal_rest_until(h, until), None, "the rest runs out");
            let rested = cx::GoalLook {
                now: until,
                ..look_of(s, Some(h))
            };
            assert_eq!(
                goal_projection(&rested)["held"],
                next["held"],
                "Rested at {s:?}"
            );
            seen.insert("Rested");
        }
        // The look projects back onto the state it was built from.
        let p = goal_projection(&look);
        for k in [
            "behind", "land", "goal", "held", "overdue", "switch", "person",
        ] {
            assert_eq!(p[k], s[k], "{k} of {s:?}");
        }
        if s["goal"] == 1 {
            assert_eq!(p["turn"], s["turn"], "turn of {s:?}");
        }
        // NEGATIVE CONTROLS.
        if buggy.action_enabled("EscMidWork", s) && real != cx::GoalStep::Esc {
            caught.insert("EscMidWork");
        }
        for mutant in ["PauseUnderSwitch", "PauseAtFirstSight"] {
            if buggy.action_enabled(mutant, s) && real != cx::GoalStep::Pause {
                caught.insert(mutant);
            }
        }
        for mutant in ["ResumeTwice", "ResumeOverPerson"] {
            if buggy.action_enabled(mutant, s) && !matches!(real, cx::GoalStep::Resume(_)) {
                caught.insert(mutant);
            }
        }
        if buggy.action_enabled("LetGoPaused", s) && owed {
            caught.insert("LetGoPaused");
        }
        // A turn typed onto the paused thread: at every look the real rule
        // HOLDS, every wait the visit words there owns the session's turn
        // end, so no supervisor continues it.
        if buggy.action_enabled("ContinuePaused", s)
            && real == cx::GoalStep::Hold
            && every_held_wait_owns_the_turn_end()
        {
            caught.insert("ContinuePaused");
        }
        let mut decided = s.clone();
        if buggy.fire("GoalHoldsTheTab", &mut decided)
            && !buggy.action_enabled("GoalPause", &decided)
            && real == cx::GoalStep::Pause
        {
            caught.insert("GoalHoldsTheTab");
        }
    }
    assert_eq!(
        seen,
        BTreeSet::from([
            "Advance",
            "GoalEsc",
            "GoalPause",
            "GoalResume",
            "MoveOverdue",
            "Release",
            "Rested",
            "Took"
        ]),
        "every lane act decided somewhere, every step of the clock taken"
    );
    let every: BTreeSet<&str> = buggy
        .actions
        .iter()
        .map(|a| a.name)
        .filter(|name| !aterm_spec::interp::fired_actions(&model).contains(name))
        .collect();
    assert_eq!(caught, every, "every Buggy member caught by the real rule");

    // The anchors are linked, name the one projection, and were entered.
    let mut anchored: Vec<(&str, &str)> = xref::refinements()
        .filter(|a| a.machine == "HarnessUpgradeGoalPause")
        .map(|a| {
            assert_eq!(
                a.project,
                "aterm_agent::harness::upgrade_drive::goal_projection"
            );
            (a.action, a.rust_method)
        })
        .collect();
    anchored.sort_unstable();
    assert_eq!(
        anchored,
        [
            ("GoalEsc", "goal_step"),
            ("GoalPause", "goal_step"),
            ("GoalResume", "goal_step"),
            ("LadderMove", "daemon_turn"),
            ("LetGo", "goal_owed"),
            ("Release", "goal_step"),
            ("Took", "goal_step"),
        ]
    );
    let entered = xref::entered_anchor_ids();
    for id in [
        "HarnessUpgradeGoalPause::GoalEsc @ goal_step",
        "HarnessUpgradeGoalPause::GoalPause @ goal_step",
        "HarnessUpgradeGoalPause::GoalResume @ goal_step",
        "HarnessUpgradeGoalPause::LadderMove @ daemon_turn",
        "HarnessUpgradeGoalPause::LetGo @ goal_owed",
        "HarnessUpgradeGoalPause::Release @ goal_step",
        "HarnessUpgradeGoalPause::Took @ goal_step",
    ] {
        assert!(entered.contains(id), "{id} entered: {entered:?}");
    }
}

/// The goal's own record through the incident's schedule, one visit's view
/// of it: the tab's hold as the pause, its take and its resume leave it.
struct Goal {
    hold: Option<Hold>,
}

impl Goal {
    /// The look the visit builds of the goal at `t`, over the facts `f` its
    /// assembly built of the screen `rows` and the daemon.
    fn look<'a>(&'a self, rows: &[String], f: &Facts, behind: bool, t: u64) -> cx::GoalLook<'a> {
        let land = f.rung == Rung::Land;
        cx::GoalLook {
            hold: self.hold.as_ref(),
            behind,
            goal: aterm_phase::codex::goal_state(rows),
            footer: aterm_phase::codex::footer_status(rows).is_some(),
            land,
            goal_holds: f.daemon_turn == DaemonTurn::Goal,
            waived: upgrade::gate_announce(&Facts {
                status: "idle".to_string(),
                daemon_turn: DaemonTurn::None,
                ..f.clone()
            }),
            switch: false,
            moved: !behind,
            abandoned: None,
            head: None,
            free: f.composer_empty && !f.approval_box && !f.busy_footer,
            esc_free: f.composer_empty && !f.approval_box,
            typing: f.typing,
            person_since: false,
            sandboxed: false,
            now: t,
        }
    }
}

/// `rows` with the footer's goal PAUSED (`Goal paused (/goal resume)` where
/// `Pursuing goal (…)` was).
fn paused_footer(mut rows: Vec<String>) -> Vec<String> {
    let footer = rows.len() - 2;
    let pursuing = aterm_phase::anchors::anchor_text("codex.goal.pursuing");
    if let Some(at) = rows[footer].find(pursuing) {
        rows[footer].truncate(at);
        rows[footer].push_str(aterm_phase::anchors::anchor_text("codex.goal.paused"));
    }
    assert_eq!(
        aterm_phase::codex::goal_state(&rows),
        Some(CodexGoal::Paused),
        "{rows:?}"
    );
    rows
}

/// THE 2026-09-28 INCIDENT, REPLAYED WITH THE GOAL PAUSE: the owner's
/// goal-mode daemon client (0.158.0's `»` input line, the goal's footer),
/// whose goal starts its next turn 14 ms after each one ends, so every look
/// the host takes — at the idle-looking frame a goal turn shows while its
/// first message streams — finds its own conversation's turn running in the
/// daemon. The looks go on through the ladder's rungs: `wait:goal` at every
/// one before Land (never a pause at first sight), and at the first look at
/// Land the real rule PAUSES the goal (`GoalStep::Pause`). The pause lets the
/// running turn finish — looks while it runs HOLD (the ordinary step, which
/// waits on the turn: `daemon-turn`) — and no turn follows it: the thread
/// idle, the real gate moves (`Terminate`, the `/exit`). The relaunched
/// Codex is current, its goal paused: the real rule RESUMES it, once — and a
/// host restarted between the resume and its showing reads the record and
/// never types it twice. NEGATIVE CONTROL: the same schedule with no pause
/// (the rule before the owner's decision) waits at every look to the end of
/// the replay.
#[test]
fn the_goal_mode_incident_now_moves_at_land_and_resumes_the_goal() {
    // Ten minutes behind at the first look, one look every twenty minutes.
    let start = NOW - 2 * 3_600;
    let mut st = St {
        pending_since: start - 600,
        ..St::default()
    };
    let mut goal = Goal { hold: None };
    let running = Daemon {
        own: true,
        ..Daemon::default()
    };
    let idle = Daemon::default();
    // A look at each goal turn's start (the incident's looks were minutes
    // apart): the rungs go by.
    let mut t = start;
    let mut minute = 10;
    let mut paused_at = None;
    let mut before_land = 0;
    while paused_at.is_none() {
        assert!(t < NOW + 3_600, "the pause never came");
        let rows = tab_screen('»', "Continuing toward the goal.", minute);
        let scr = read(rows.clone(), false, u64::from(minute), HumanInput::Never);
        let (f, ladder) = look(&mut st, &scr, false, running, t);
        assert!(ladder.idle_read, "a goal turn's first moments read idle");
        assert_eq!(f.daemon_turn, DaemonTurn::Goal);
        let step = cx::goal_step(&goal.look(&rows, &f, true, t));
        let ordinary = cx::requested_step(
            &Request::None,
            &Mode::Daemon,
            &Phase::Pending,
            &f,
            false,
            false,
            t,
            "0.158.0",
        );
        if f.rung < Rung::Land {
            assert_eq!(
                step,
                cx::GoalStep::Pass,
                "no pause at first sight, {:?}",
                f.rung
            );
            assert_eq!(ordinary, Step::Wait("goal"));
            before_land += 1;
        } else {
            assert_eq!(step, cx::GoalStep::Pause, "at Land");
            // The claim and the key, as `goal_do` makes them.
            goal.hold = Some(Hold::pausing(Owner::Upgrade, How::Typed, TUI, "0.158.0", t));
            paused_at = Some(t);
        }
        t += 20 * 60;
        minute += 20;
    }
    assert_eq!(before_land, 6, "Prefer, Settled and KeysOnly looked at");
    let paused_at = paused_at.expect("paused");

    // The footer shows the pause: taken. The goal's turn still runs (its
    // tool calls included): the ordinary step waits on it, and the look
    // holds.
    let t = paused_at + 30;
    let rows = paused_footer(tab_screen('»', "Continuing toward the goal.", 11));
    let scr = read(rows.clone(), false, 90, HumanInput::Never);
    let (f, _) = look(&mut st, &scr, false, running, t);
    assert_eq!(
        cx::goal_step(&goal.look(&rows, &f, true, t)),
        cx::GoalStep::Took
    );
    if let Some(h) = goal.hold.as_mut() {
        h.stage = Stage::Paused;
        h.took_at = t;
    }
    let t = paused_at + 300;
    let scr = read(rows.clone(), false, 91, HumanInput::Never);
    let (f, _) = look(&mut st, &scr, false, running, t);
    assert_eq!(
        f.daemon_turn,
        DaemonTurn::Own,
        "the paused goal's last turn"
    );
    assert_eq!(
        cx::goal_step(&goal.look(&rows, &f, true, t)),
        cx::GoalStep::Hold
    );
    assert_eq!(
        cx::requested_step(
            &Request::None,
            &Mode::Daemon,
            &Phase::Pending,
            &f,
            false,
            false,
            t,
            "0.158.0",
        ),
        Step::Wait("daemon-turn"),
        "worded goal-held by the visit"
    );

    // Its turn ends, and no turn follows a paused goal's last: the move.
    let t = paused_at + 900;
    let scr = read(rows.clone(), false, 92, HumanInput::Never);
    let (f, _) = look(&mut st, &scr, false, idle, t);
    assert_eq!(f.daemon_turn, DaemonTurn::None);
    assert_eq!(
        cx::goal_step(&goal.look(&rows, &f, true, t)),
        cx::GoalStep::Hold
    );
    assert_eq!(
        cx::requested_step(
            &Request::None,
            &Mode::Daemon,
            &Phase::Pending,
            &f,
            false,
            false,
            t,
            "0.158.0",
        ),
        Step::Terminate,
        "the move, at the paused goal's idle point"
    );

    // The relaunched Codex, current, its goal paused: resumed.
    let t = paused_at + 960;
    let mut current = St {
        pending_since: 0,
        ..St::default()
    };
    let scr = read(rows.clone(), false, 93, HumanInput::Never);
    let (f, _) = look(&mut current, &scr, false, idle, t);
    assert_eq!(
        cx::goal_step(&goal.look(&rows, &f, false, t)),
        cx::GoalStep::Resume("moved")
    );
    if let Some(h) = goal.hold.as_mut() {
        h.stage = Stage::Resuming;
        h.resume_at = t;
        h.resumes = 1;
        h.why = "moved".into();
    }
    // A HOST RESTARTED between the resume and its showing reads the record:
    // no second resume while it may still show, and once the footer shows it
    // pursued, resumed — owed nothing more.
    let restarted = Goal {
        hold: goal.hold.clone(),
    };
    assert_eq!(
        cx::goal_step(&restarted.look(&rows, &f, false, t + 5)),
        cx::GoalStep::Wait("goal-resuming")
    );
    let pursued = tab_screen('»', "Continuing toward the goal.", 12);
    let scr = read(pursued.clone(), false, 94, HumanInput::Never);
    let (f, _) = look(&mut current, &scr, false, running, t + 10);
    assert_eq!(
        cx::goal_step(&restarted.look(&pursued, &f, false, t + 10)),
        cx::GoalStep::Resumed
    );
    let done = Hold {
        stage: Stage::Resumed,
        ..restarted.hold.clone().expect("held")
    };
    assert!(!cx::goal_owed(Some(&done)), "the host may let the tab go");
    let after = Goal { hold: Some(done) };
    assert_eq!(
        cx::goal_step(&after.look(&pursued, &f, false, t + 3_600)),
        cx::GoalStep::Pass,
        "a current Codex's goal is never paused again"
    );

    // NEGATIVE CONTROL: the rule before the owner's decision — no pause —
    // waits at every look of the same schedule, however long.
    let mut st = St {
        pending_since: NOW - 10 * 3_600,
        ..St::default()
    };
    for k in 0..9_u32 {
        let t = NOW + u64::from(k) * 60;
        let rows = tab_screen('»', "Continuing toward the goal.", 20 + k);
        let scr = read(rows, false, 200 + u64::from(k), HumanInput::Never);
        let (f, _) = look(&mut st, &scr, false, running, t);
        assert_eq!(
            cx::requested_step(
                &Request::None,
                &Mode::Daemon,
                &Phase::Pending,
                &f,
                false,
                false,
                t,
                "0.158.0",
            ),
            Step::Wait("goal"),
            "the goal holds the tab for as long as it runs"
        );
    }
}

/// THE PAUSED GOAL BEHIND ITS DAEMON (the goal-pause review of 2026-09-28,
/// its probe made real): the incident's schedule with the Codex DAEMON
/// behind too — every managed Codex update leaves it behind, and daemon mode
/// is the owner's — and the paused goal's thread still SETTLING (written in
/// the last 20 s) at the idle point after its last turn. The real rule
/// HOLDS; the client's ordinary step waits on its daemon, which waits on the
/// settle; and the visit's word for that wait is `goal-held`, which OWNS the
/// session's turn end — so the supervisor types nothing into the Codex whose
/// goal aterm paused, the thread settles, and the daemon moves. Until that
/// day the word stayed `wait:daemon-first:settling`, unowned: the supervisor
/// continued the paused goal's session, its turn kept the thread busy, and
/// the goal was resumed at the hour's bound without its move.
#[test]
fn a_paused_goal_behind_its_daemon_keeps_its_turn_ends() {
    let start = NOW - 2 * 3_600;
    let mut st = St {
        pending_since: start - 600,
        ..St::default()
    };
    let hold = Hold {
        stage: Stage::Paused,
        took_at: NOW,
        ..Hold::pausing(Owner::Upgrade, How::Typed, TUI, "0.158.0", NOW)
    };
    let goal = Goal { hold: Some(hold) };
    let t = NOW + 900;
    let rows = paused_footer(tab_screen('»', "Continuing toward the goal.", 11));
    let scr = read(rows.clone(), false, 92, HumanInput::Never);
    let (f, _) = look(&mut st, &scr, false, Daemon::default(), t);
    assert_eq!(f.daemon_turn, DaemonTurn::None, "its last turn ended");
    assert_eq!(
        cx::goal_step(&goal.look(&rows, &f, true, t)),
        cx::GoalStep::Hold
    );
    // The daemon behind (the build moves it first): the client waits on it.
    let ordinary = cx::requested_step(
        &Request::None,
        &Mode::Daemon,
        &Phase::Pending,
        &f,
        false,
        true,
        t,
        "0.158.0",
    );
    assert_eq!(ordinary, Step::Wait("daemon-first"));
    // The daemon's own step, the paused goal's thread just written.
    let d = cx::DaemonFacts {
        running: crate::harness::upgrade::Version::parse("0.157.1"),
        managed: crate::harness::upgrade::Version::parse("0.158.0").expect("a version"),
        pinned: true,
        busy_threads: 0,
        terminals: 0,
        settling_threads: 1,
        attended: false,
        held: false,
        owner_held: false,
        unseen_clients: 0,
    };
    assert_eq!(cx::daemon_step(&d), cx::DaemonStep::Wait("settling"));
    // The visit's word for it, and the host's ownership of the turn end.
    let r = Report {
        pid: TUI,
        tab: "t".into(),
        session: "s".into(),
        from: String::new(),
        to: String::new(),
        step: "wait:daemon-first:settling".into(),
    };
    let word = super::super::goal_wait_word(r.clone(), true, false).step;
    assert_eq!(word, "wait:goal-held");
    assert!(crate::harness::upgrade_drive::owns_turn_ends(&word, 0, 120));
    // NEGATIVE CONTROL: no goal held, the daemon's own word stands, and the
    // turn end is the supervisor's.
    let free = super::super::goal_wait_word(r, false, false).step;
    assert_eq!(free, "wait:daemon-first:settling");
    assert!(!crate::harness::upgrade_drive::owns_turn_ends(
        &free, 0, 120
    ));
}
