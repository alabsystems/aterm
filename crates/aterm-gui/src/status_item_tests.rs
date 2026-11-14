// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use aterm_agent::harness::resume::AfterRestart;

use crate::needs::{AgentFact, NeedFacts};

fn row_of(title: &str, facts: &NeedFacts) -> SessionRow {
    SessionRow {
        title: title.to_string(),
        need: crate::needs::of(facts).need,
        ..SessionRow::default()
    }
}

fn agent_facts(word: &'static str, rev: u64) -> NeedFacts {
    NeedFacts {
        agent: Some(AgentFact {
            word,
            detail: (word == "prompt").then(|| "bash:not-read-only".to_string()),
            rev,
            subject: (word == "prompt").then(|| "rm -rf build".to_string()),
        }),
        ..NeedFacts::default()
    }
}

fn agent_row(title: &str, word: &'static str, rev: u64) -> SessionRow {
    row_of(title, &agent_facts(word, rev))
}

const OWN: &str = "claude --model opus --resume 5f1c2d3e-4b5a-4c6d-8e7f-0a1b2c3d4e5f";

fn stall_row(
    fact: &crate::input_stall::InputStallFact,
    program: Option<&str>,
    after: &AfterRestart,
    clock: Option<&str>,
    waited: std::time::Duration,
) -> InputStallRow {
    InputStallRow {
        text: crate::input_stall::attention_text(
            fact,
            program,
            after,
            "s-b7cf523445a1b0d8658e",
            clock,
            waited,
        ),
        key: crate::input_stall::episode_key(fact),
        stopped: fact.stopped,
    }
}

fn incident_stall() -> crate::input_stall::InputStallFact {
    crate::input_stall::InputStallFact {
        word: aterm_session::input_backlog::InputWord::Stalled,
        since: std::time::Instant::now(),
        since_unix: None,
        bytes: 1,
        stopped: false,
        footprint_mb: Some(39_731),
        restart: crate::input_stall::Restart::default(),
        act: None,
    }
}

fn stalled_agent(
    fact: &crate::input_stall::InputStallFact,
    row: InputStallRow,
    rev: u64,
) -> NeedFacts {
    NeedFacts {
        stall: Some(crate::needs::StallFact {
            text: row.text,
            key: row.key,
            stopped: row.stopped,
            act: fact.act.is_some(),
            hand_back_frees: false,
        }),
        agent: Some(AgentFact {
            word: "wall:unresponsive",
            detail: fact.stopped.then(|| "stopped".to_string()),
            rev,
            subject: None,
        }),
        ..NeedFacts::default()
    }
}

#[test]
fn a_stall_is_one_need_per_episode_while_its_text_moves() {
    let fact = incident_stall();
    let row_at = |fact: &crate::input_stall::InputStallFact, secs: u64| {
        let row = stall_row(
            fact,
            Some("claude"),
            &AfterRestart::Command(OWN.to_string()),
            None,
            std::time::Duration::from_secs(secs),
        );
        row_of("worker", &stalled_agent(fact, row, secs))
    };
    let need_of = |row: &SessionRow| row.need.clone().expect("a stall row");
    let first_row = row_at(&fact, 10);
    let later_row = row_at(
        &crate::input_stall::InputStallFact {
            bytes: 7,
            ..fact.clone()
        },
        95,
    );
    let (first, later) = (need_of(&first_row), need_of(&later_row));
    assert_ne!(first.what, later.what, "the text moved");
    assert_eq!(first.key, later.key, "the episode did not");
    let turned = crate::input_stall::InputStallFact {
        word: aterm_session::input_backlog::InputWord::Stopped,
        stopped: true,
        ..fact.clone()
    };
    let stopped = need_of(&row_at(&turned, 120));
    assert_eq!(stopped.key, first.key);
    assert_eq!(stopped.word, NeedWord::Stopped);

    let mut h = NeedAges::default();
    assert!(h.observe(5, Some(&first_row), 100));
    assert!(
        h.observe(5, Some(&later_row), 140),
        "the moved text moves the row"
    );
    assert_eq!(
        h.need_since(5),
        Some(100),
        "one episode, one need: its age stands"
    );
    let next = crate::input_stall::InputStallFact {
        since: fact.since + std::time::Duration::from_secs(600),
        ..fact.clone()
    };
    let again_row = row_at(&next, 10);
    assert_ne!(need_of(&again_row).key, first.key);
    assert!(h.observe(5, Some(&again_row), 160));
    assert_eq!(h.need_since(5), Some(160), "a new episode is a new need");
}

#[test]
fn a_stall_on_an_agent_is_its_row_frozen_or_stopped() {
    use crate::input_stall::{InputStallFact, StallAct};
    let fact = incident_stall();
    let worker = |fact: &InputStallFact, after: &AfterRestart, rev: u64| {
        let row = stall_row(
            fact,
            Some("claude"),
            after,
            Some("14:02"),
            std::time::Duration::from_secs(9660),
        );
        NeedFacts {
            supervised: true,
            attention: vec![crate::needs::AttentionFact {
                owner: aterm_agent::supervise::ATTENTION_OWNER.to_string(),
                text: "answer this box: 4. Chat about this".to_string(),
                kind: None,
            }],
            ..stalled_agent(fact, row, rev)
        }
    };
    let hosted = InputStallFact {
        act: Some(StallAct {
            cmd: aterm_agent::harness::resume::restart_command(
                Some("s-b7cf523445a1b0d8658e"),
                None,
            ),
            promised: true,
        }),
        ..fact.clone()
    };
    let frozen_row = row_of(
        "\u{2733} worker",
        &worker(&hosted, &AfterRestart::Relaunch, 9),
    );
    assert_eq!(
        frozen_row.need.as_ref().map(|n| n.word),
        Some(NeedWord::Frozen)
    );
    let frozen = frozen_row.need.clone().expect("a stall row");
    assert_eq!(frozen.key, crate::input_stall::episode_key(&fact));
    assert!(
        frozen.what.ends_with(
            "claude is frozen: not reading input since 14:02 (1 B queued, footprint 38.8 GB)"
        ),
        "{}",
        frozen.what
    );
    let entry = worker(&hosted, &AfterRestart::Relaunch, 9)
        .stall
        .expect("a published stall")
        .text;
    assert!(
        entry.contains("restart if-stalled"),
        "the entry keeps it: {entry}"
    );
    assert!(!frozen.what.contains("aterm ctl"), "{}", frozen.what);
    let stopped_fact = InputStallFact {
        word: aterm_session::input_backlog::InputWord::Stopped,
        stopped: true,
        ..fact.clone()
    };
    for supervised in [true, false] {
        let facts = NeedFacts {
            supervised,
            ..worker(&stopped_fact, &AfterRestart::Command(OWN.to_string()), 10)
        };
        let stopped_row = row_of("\u{2733} worker", &facts);
        assert_eq!(
            stopped_row.need.as_ref().map(|n| n.word),
            Some(NeedWord::Stopped),
            "supervised={supervised}"
        );
        let stopped = stopped_row.need.clone().expect("a stopped row");
        assert_eq!(stopped.key, frozen.key, "one episode");
        assert!(
            stopped
                .what
                .ends_with("claude is stopped with input queued since 14:02"),
            "{}",
            stopped.what
        );
        assert!(!stopped.what.contains("signal cont"), "{}", stopped.what);
    }
    let mut facts = worker(&fact, &AfterRestart::Relaunch, 11);
    facts.stall = None;
    facts.agent.as_mut().unwrap().word = "prompt";
    assert_eq!(
        row_of("worker", &facts).need.map(|n| n.word),
        Some(NeedWord::Ask)
    );
}

#[test]
fn presence_band_and_the_published_need_say_a_box_and_a_wall_alike() {
    let mut bare = agent_facts("prompt", 1);
    bare.agent.as_mut().unwrap().subject = None;
    for (detail, word, label) in [
        ("question", NeedWord::Question, "question"),
        ("other", NeedWord::Approval, "approval"),
        ("plan-exit", NeedWord::Approval, "plan approval"),
    ] {
        bare.agent.as_mut().unwrap().detail = Some(detail.into());
        let need = row_of("t", &bare).need.expect("a box");
        assert_eq!(need.word, word, "{detail}");
        assert_eq!(need.what, label, "{detail}");
        assert_eq!(
            crate::presence::prompt_band_word(Some(detail)),
            label,
            "{detail}"
        );
    }
    let overloaded = crate::presence::AgentPhase::Wall {
        kind: aterm_phase::WallKind::Overloaded,
        reset: None,
        until: None,
    };
    let walled = agent_row("w", "wall:overloaded", 1).need.expect("a wall");
    assert_eq!(walled.what, overloaded.band_word());
}

/// The state a test's raises are asked over: a level file and a fleet held.
const BASIS: Basis = Basis {
    stored: Some(aterm_agent::fleet_access::Stamp {
        ino: 1,
        modified: (0, 0),
    }),
    fleet: Some(1),
};

/// The click sequences of `docs/DESIGN-fleet-access-lock-2026-11-10.md` §6.3, the clock
/// handed in. `wrote` stands for the App's write of what the face told it to.
#[test]
fn the_lock_waits_on_a_raise_and_writes_a_lower_level_at_once() {
    use FleetAccess::{Full, ReadOnly, ReadWrite};
    let t0 = std::time::Instant::now();
    let at = |ms: u64| t0 + std::time::Duration::from_millis(ms);
    let settle = u64::try_from(SETTLE.as_millis()).expect("a short settle");

    // One click from Read Only: the icon rests on Read and Write and names it, and nothing
    // is written until the settle has passed.
    let mut face = LockFace::new(ReadOnly);
    assert_eq!(face.next_deadline(), None);
    assert_eq!(
        face.click(at(0), false),
        None,
        "a raise is not written by its click"
    );
    assert_eq!(
        (face.on_disk(), face.shown(), face.flash(at(1))),
        (ReadOnly, ReadWrite, Some(ReadWrite))
    );
    assert_eq!(face.next_deadline(), Some(at(settle)));
    assert_eq!(face.due(at(settle - 1)), None, "not yet");
    assert_eq!(face.shown(), ReadWrite);
    assert_eq!(face.due(at(settle)), Some(ReadWrite));
    face.wrote(ReadWrite, BASIS);
    assert_eq!(
        (face.on_disk(), face.shown(), face.flash(at(settle))),
        (ReadWrite, ReadWrite, None),
        "in force, and the name gone from the title"
    );
    assert_eq!(face.next_deadline(), None);
    assert_eq!(face.due(at(9 * settle)), None, "written once");

    // Full Access from Read Only is two clicks and the rest; each click starts the rest
    // again.
    let mut face = LockFace::new(ReadOnly);
    assert_eq!(face.click(at(0), false), None);
    assert_eq!(face.click(at(500), false), None);
    assert_eq!((face.on_disk(), face.shown()), (ReadOnly, Full));
    assert_eq!(
        face.due(at(settle)),
        None,
        "the first click's time is not the second's"
    );
    assert_eq!(face.due(at(500 + settle)), Some(Full));

    // Clicking on round to Read Only inside the settle writes nothing at all.
    let mut face = LockFace::new(ReadOnly);
    assert_eq!(face.click(at(0), false), None);
    assert_eq!(face.click(at(300), false), None);
    assert_eq!(
        face.click(at(600), false),
        None,
        "back where it is: nothing to write"
    );
    assert_eq!((face.on_disk(), face.shown()), (ReadOnly, ReadOnly));
    assert_eq!(
        face.due(at(10 * settle)),
        None,
        "and no raise is left to fall due"
    );

    // Passing over Full Access on the way down never opens it.
    let mut face = LockFace::new(ReadWrite);
    assert_eq!(face.click(at(0), false), None, "Full Access waits");
    assert_eq!(face.shown(), Full);
    assert_eq!(
        face.click(at(400), false),
        Some(ReadOnly),
        "a lower level is written now"
    );
    face.wrote(ReadOnly, BASIS);
    assert_eq!((face.on_disk(), face.shown()), (ReadOnly, ReadOnly));
    assert_eq!(
        face.flash(at(400 + settle - 1)),
        Some(ReadOnly),
        "the click's level is still named"
    );
    assert_eq!(
        face.due(at(10 * settle)),
        None,
        "Full Access was never written"
    );
    assert_eq!(
        face.next_deadline(),
        None,
        "and the name went when its time came"
    );

    // From Full Access one click closes it at once.
    let mut face = LockFace::new(Full);
    assert_eq!(face.click(at(0), false), Some(ReadOnly));
    face.wrote(ReadOnly, BASIS);
    assert_eq!(
        face.next_deadline(),
        Some(at(settle)),
        "only the name's time is left"
    );
    assert_eq!(face.due(at(settle)), None);
    assert_eq!(face.next_deadline(), None);
}

#[test]
fn a_menu_pick_the_menus_open_and_a_change_on_disk_each_cancel_a_pending_raise() {
    use FleetAccess::{Full, ReadOnly, ReadWrite};
    let t0 = std::time::Instant::now();
    let later = t0 + 10 * SETTLE;
    let pending = || {
        let mut face = LockFace::new(ReadOnly);
        assert!(
            !face.seen(ReadOnly, BASIS),
            "the state is read before a click, as the App reads it"
        );
        assert_eq!(face.click(t0, false), None);
        assert_eq!(face.shown(), ReadWrite, "PRECONDITION: a raise is pending");
        face
    };

    // A pick is written at once, whatever was pending; the level in force is no write.
    let mut face = pending();
    assert_eq!(face.pick(Full), Some(Full));
    face.wrote(Full, BASIS);
    assert_eq!(
        (face.on_disk(), face.shown(), face.flash(t0)),
        (Full, Full, None)
    );
    assert_eq!(face.due(later), None);
    let mut face = pending();
    assert_eq!(face.pick(ReadOnly), None);
    assert_eq!((face.shown(), face.due(later)), (ReadOnly, None));

    // The menu's open.
    let mut face = pending();
    face.menu_opened();
    assert_eq!(
        (face.shown(), face.flash(t0), face.due(later)),
        (ReadOnly, None, None)
    );
    assert_eq!(face.next_deadline(), None);

    // Another writer's level replaces the one in force and cancels the raise; the state
    // this face already holds (its own write, read back) changes nothing.
    let mut face = pending();
    assert!(!face.seen(ReadOnly, BASIS), "the state it holds");
    assert_eq!(face.shown(), ReadWrite, "the raise still waits");
    assert!(face.seen(Full, BASIS));
    assert_eq!(
        (face.on_disk(), face.shown(), face.due(later)),
        (Full, Full, None)
    );
    let mut face = pending();
    assert!(
        face.seen(ReadWrite, BASIS),
        "even the level the raise was going to"
    );
    assert_eq!(face.due(later), None, "it is in force by another's hand");
    assert_eq!(face.on_disk(), ReadWrite);

    // The state moved under the raise with the level as it was: the level stated again
    // elsewhere (another file), the fleet left, another fleet joined. Each drops the raise.
    let restated = Basis {
        stored: Some(aterm_agent::fleet_access::Stamp {
            ino: 7,
            modified: (1, 0),
        }),
        ..BASIS
    };
    let left = Basis {
        fleet: None,
        ..BASIS
    };
    let moved = Basis {
        fleet: Some(9),
        ..BASIS
    };
    for basis in [restated, left, moved] {
        let mut face = pending();
        assert!(face.seen(ReadOnly, basis), "{basis:?}");
        assert_eq!(
            (
                face.on_disk(),
                face.shown(),
                face.raising(),
                face.due(later)
            ),
            (ReadOnly, ReadOnly, false, None),
            "{basis:?}"
        );
        assert_eq!(face.basis(), basis, "the next raise is asked over it");
    }
    // CONTROL: with no raise pending, a state that moved is only taken note of.
    let mut face = LockFace::new(ReadOnly);
    assert!(!face.seen(ReadOnly, BASIS) && !face.seen(ReadOnly, restated));
    assert_eq!(face.basis(), restated);
}

/// A multi-click's second or later release lowers or cancels and never raises: a
/// double-click from Read Only moves one step, and a quick second click from Read and
/// Write, over Full Access and round to Read Only, closes the lock at once. CONTROL: the
/// same second click counted as a click of its own raises a second step.
#[test]
fn a_repeated_click_lowers_or_cancels_and_never_raises() {
    use FleetAccess::{Full, ReadOnly, ReadWrite};
    let t0 = std::time::Instant::now();
    let soon = t0 + SETTLE / 10;
    let later = t0 + 10 * SETTLE;

    let mut face = LockFace::new(ReadOnly);
    assert_eq!(face.click(t0, false), None);
    assert_eq!(face.click(soon, true), None, "no second step up");
    assert_eq!(
        (face.shown(), face.next_deadline()),
        (ReadWrite, Some(t0 + SETTLE)),
        "the first click's raise and its clock stand"
    );
    assert_eq!(face.due(later), Some(ReadWrite));
    let mut control = LockFace::new(ReadOnly);
    assert_eq!(control.click(t0, false), None);
    assert_eq!(control.click(soon, false), None);
    assert_eq!(
        control.shown(),
        Full,
        "CONTROL: a click of its own steps on"
    );

    // Read and Write, click, click: Read Only, written by the second click.
    let mut face = LockFace::new(ReadWrite);
    assert_eq!(face.click(t0, false), None);
    assert_eq!(face.shown(), Full, "PRECONDITION: Full Access waits");
    assert_eq!(face.click(soon, true), Some(ReadOnly));
    face.wrote(ReadOnly, BASIS);
    assert_eq!((face.on_disk(), face.shown()), (ReadOnly, ReadOnly));
    assert_eq!(
        face.click(soon, true),
        None,
        "a third release raises nothing"
    );
    assert_eq!(face.due(later), None, "Full Access was never written");

    // A repeat that comes round to the level in force cancels the raise and writes nothing.
    let mut face = LockFace::new(ReadOnly);
    assert_eq!(face.click(t0, false), None);
    assert_eq!(face.click(t0 + SETTLE / 2, false), None);
    assert_eq!(face.shown(), Full);
    assert_eq!(face.click(t0 + SETTLE / 2 + SETTLE / 10, true), None);
    assert_eq!((face.shown(), face.raising()), (ReadOnly, false));
    assert_eq!(face.due(later), None);

    // From Full Access the first click closes it; its repeat opens nothing.
    let mut face = LockFace::new(Full);
    assert_eq!(face.click(t0, false), Some(ReadOnly));
    face.wrote(ReadOnly, BASIS);
    assert_eq!(face.click(soon, true), None);
    assert_eq!((face.shown(), face.due(later)), (ReadOnly, None));
}

/// Only a pointer's plain left release in a fleet is the lock's click, and it says whether
/// AppKit counted it as a multi-click's second or later. Every other press opens the menu:
/// a right release, a Control-click, a press with no event of its own or a stale one (an
/// accessibility press), a key's (Control-F8, then Space), and every press out of a fleet.
/// CONTROL: the same left release rotates in a fleet.
#[test]
fn only_a_plain_left_release_in_a_fleet_rotates_and_every_other_press_opens_the_menu() {
    use aterm_types::status_click::{CONTROL, EventKey, LEFT_MOUSE_UP, RIGHT_MOUSE_UP};
    let key = |kind, flags, at, clicks| EventKey {
        kind,
        flags,
        at,
        clicks,
    };
    let left = key(LEFT_MOUSE_UP, 0, 1.0, 1);
    let second = key(LEFT_MOUSE_UP, 0, 1.2, 2);
    let third = key(LEFT_MOUSE_UP, 0, 1.4, 3);
    let right = key(RIGHT_MOUSE_UP, 0, 1.0, 1);
    let control = key(LEFT_MOUSE_UP, CONTROL, 1.0, 1);
    // `NSEventTypeKeyDown`: Space on the focused item.
    let space = key(10, 0, 1.0, 0);
    let once = ClickAct::Rotate { repeated: false };
    let again = ClickAct::Rotate { repeated: true };
    assert_eq!(click_act(Some(left), None, true), once, "CONTROL");
    assert_eq!(click_act(Some(second), Some(left), true), again);
    assert_eq!(click_act(Some(third), Some(second), true), again);
    for (event, stale) in [
        (Some(right), None),
        (Some(control), None),
        (None, None),
        (None, Some(left)),
        (Some(left), Some(left)),
        (Some(right), Some(right)),
        (Some(space), None),
        (Some(space), Some(left)),
    ] {
        assert_eq!(
            click_act(event, stale, true),
            ClickAct::Menu,
            "{event:?} after {stale:?}"
        );
    }
    // Out of a fleet no lock shows, and every press opens the menu.
    for event in [Some(left), Some(second), Some(right), Some(space), None] {
        assert_eq!(click_act(event, None, false), ClickAct::Menu, "{event:?}");
    }
}

#[test]
fn the_tab_name_is_the_role_then_the_title_without_status_glyphs() {
    for frame in [
        "\u{25d0} Summarise the fleet",
        "\u{25d1} Summarise the fleet",
        "\u{2733} Summarise the fleet",
        "\u{280b} Summarise the fleet",
        "  \u{2819} Summarise the fleet ",
    ] {
        assert_eq!(
            tab_name(None, None, frame).as_deref(),
            Some("Summarise the fleet")
        );
    }
    assert_eq!(
        tab_name(Some("lab-sweep"), Some("mine"), "osc").as_deref(),
        Some("lab-sweep")
    );
    assert_eq!(
        tab_name(Some("  "), Some("mine"), "osc").as_deref(),
        Some("mine")
    );
    assert_eq!(
        tab_name(None, None, "\u{2733}fleet").as_deref(),
        Some("\u{2733}fleet")
    );
    assert_eq!(tab_name(None, Some(" "), " \u{2733} ").as_deref(), None);
}

#[test]
fn the_need_ages_remember_since_when_a_need_and_a_hold_stand() {
    let asked = |key: u64, hold: Option<HoldWord>| SessionRow {
        title: "zsh".to_string(),
        agent: "busy",
        need: Some(crate::needs::Need {
            word: NeedWord::Ask,
            key,
            what: "look".to_string(),
            harness: None,
            report: None,
        }),
        held: hold,
        ..SessionRow::default()
    };
    let mut h = NeedAges::default();
    assert!(h.observe(1, Some(&asked(7, None)), 100));
    assert!(!h.observe(1, Some(&asked(7, None)), 130), "the same row");
    assert!(h.observe(1, Some(&asked(7, Some(HoldWord::Local))), 160));
    let held_since = |h: &NeedAges| h.carry_of(1).held.map(|(_, at)| at);
    assert_eq!((h.need_since(1), held_since(&h)), (Some(100), Some(160)));
    assert!(h.observe(1, Some(&asked(8, Some(HoldWord::Local))), 200));
    assert_eq!((h.need_since(1), held_since(&h)), (Some(200), Some(160)));
    assert!(h.observe(1, Some(&asked(8, Some(HoldWord::Person))), 300));
    assert_eq!(held_since(&h), Some(300), "another hold is a new one");
    let mut answered = asked(8, Some(HoldWord::Person));
    answered.agent = "prompt";
    assert!(!h.observe(1, Some(&answered), 310), "one class");
    assert!(h.observe(1, None, 400));
    assert_eq!((h.need_since(1), held_since(&h)), (None, None));
    assert!(h.since.is_empty());
}

#[test]
fn the_status_carry_row_reads_back_a_part_at_a_time() {
    let full = StatusCarry {
        need: Some((NeedWord::Frozen, 100)),
        held: Some((HoldWord::Local, 90)),
        done: Some(1_000_000),
    };
    assert_eq!(StatusCarry::parse(&full.row().expect("a row")), full);
    let unheld = StatusCarry { held: None, ..full };
    assert_eq!(StatusCarry::parse(&unheld.row().expect("a row")), unheld);
    assert_eq!(StatusCarry::default().row(), None);
    assert_eq!(
        StatusCarry::parse("need=frozen@100 told=1 told_ids=1f,a box=permission@7 done=5"),
        StatusCarry {
            need: Some((NeedWord::Frozen, 100)),
            done: Some(5),
            ..StatusCarry::default()
        },
        "an older parent's told=, told_ids= and box= parts are read past"
    );
    assert_eq!(
        StatusCarry::parse("need=zapped@100 hold=local@x later=1 done=-3"),
        StatusCarry::default()
    );
}

#[test]
fn an_adopted_standing_need_keeps_its_age() {
    let mut before = NeedAges::default();
    let boxed = agent_row("builder", "prompt", 5);
    let _ = before.observe(4, Some(&boxed), 100);
    let carry = StatusCarry::parse(&before.carry_of(4).row().expect("a row"));
    let reboxed = agent_row("builder", "prompt", 1);
    let successor = |carry: StatusCarry, at: i64| {
        let mut h = NeedAges::default();
        h.adopt(9, carry, 1_000);
        let _ = h.observe(9, Some(&reboxed), at);
        h
    };
    let mut h = successor(carry.clone(), 1_010);
    assert_eq!(h.need_since(9), Some(100), "its age carried");
    let _ = h.observe(9, Some(&agent_row("builder", "prompt", 2)), 1_020);
    assert_eq!(
        h.need_since(9),
        Some(1_020),
        "a new box after it is a new need"
    );

    let other = StatusCarry {
        need: Some((NeedWord::Ask, 100)),
        ..carry.clone()
    };
    assert_eq!(
        successor(other, 1_010).need_since(9),
        Some(1_010),
        "another need"
    );
    let settled = 1_000 + i64::try_from(ADOPT_SETTLE.as_secs()).expect("seconds");
    assert_eq!(
        successor(carry, settled).need_since(9),
        Some(settled),
        "past the settle"
    );
}

#[test]
fn a_carried_need_is_matched_by_its_word_not_its_kind() {
    let carried = |word: NeedWord| StatusCarry {
        need: Some((word, 100)),
        ..StatusCarry::default()
    };
    let first_age = |carry: StatusCarry, verdict: &'static str| {
        let mut h = NeedAges::default();
        h.adopt(9, carry, 1_000);
        let _ = h.observe(9, Some(&agent_row("builder", verdict, 1)), 1_010);
        h.need_since(9)
    };
    assert_eq!(
        agent_row("b", "wall:auth", 1).need.map(|n| n.word),
        Some(NeedWord::Login),
        "PRECONDITION"
    );
    assert_eq!(
        agent_row("b", "wall:memory", 1).need.map(|n| n.word),
        Some(NeedWord::Memory),
        "PRECONDITION"
    );
    assert_eq!(
        first_age(carried(NeedWord::Login), "wall:auth"),
        Some(100),
        "the carried word: its age"
    );
    assert_eq!(
        first_age(carried(NeedWord::Login), "wall:memory"),
        Some(1_010),
        "the same kind, another word: a new need"
    );
    assert_eq!(
        first_age(carried(NeedWord::Wall), "wall:auth"),
        Some(1_010),
        "a carried wall says nothing of a login"
    );
}

const NEED_A_CUT: &str = "grant Screen Recording to com.aterm.aterm in System Settings \u{25b8} \
                          Priv\u{2026}";

/// A `NEEDS YOU:` report of Claude Code's, as its harness writes the needs entry.
fn harness_row(title: &str, first: &str) -> SessionRow {
    let facts = NeedFacts {
        attention: vec![crate::needs::AttentionFact {
            owner: crate::harness_host::NEEDS_ATTENTION_OWNER.to_string(),
            text: format!(
                "{} needs you: {first} (+1 more) \u{2014} carrying on",
                aterm_phase::Program::Claude.name()
            ),
            kind: Some(aterm_types::needs::AttentionKind::Ask),
        }],
        ..NeedFacts::default()
    };
    row_of(title, &facts)
}

#[test]
fn the_shown_key_folds_the_harness_words() {
    let a = harness_row("t", NEED_A_CUT);
    assert!(
        a.need.as_ref().is_some_and(|n| n.harness.is_some()),
        "{a:?}"
    );
    let mut b = a.clone();
    if let Some(need) = b.need.as_mut()
        && let Some(ask) = need.harness.as_mut()
    {
        ask.more = 7;
    }
    assert_ne!(shown_key(&a), shown_key(&b));
}

/// A fixed "now" the traffic tests add their hours up at.
const NOW_MS: u64 = 1_800_000_000_000;

/// This Mac in a fleet at `access`, named and placed, with `traffic` as the reader left it.
fn in_fleet(access: FleetAccess, traffic: Option<Summary>) -> Glance {
    Glance {
        machine: Some("m9 Yellow Seahorse".into()),
        in_fleet: true,
        network: Some("ALab LAN".into()),
        access,
        access_shown: access,
        access_flash: None,
        blocked: blocked_unseen(traffic.as_ref(), 0, NOW_MS),
        traffic,
    }
}

/// `n` requests of `what` between this Mac and `host`, the latest `ago_min` minutes ago.
fn asked(
    way: aterm_types::fleet_activity::Way,
    host: &str,
    what: aterm_types::fleet_activity::Plain,
    n: u32,
    ago_min: u64,
    blocked: bool,
) -> aterm_types::fleet_activity::Row {
    use aterm_types::fleet_activity::{Outcome, Row};
    Row {
        at_ms: NOW_MS - ago_min * 60_000,
        way,
        // A node id is 32 hex digits; the host's own bytes make each Mac's distinct.
        node: format!("{:0>32}", aterm_hash::fnv1a_64(host.as_bytes())),
        host: Some(host.to_owned()),
        what,
        outcome: if blocked {
            Outcome::Blocked {
                level: FleetAccess::ReadOnly,
                needs: FleetAccess::Full,
            }
        } else {
            Outcome::Admitted
        },
        by: None,
        n,
    }
}

fn hour(rows: &[aterm_types::fleet_activity::Row]) -> Summary {
    aterm_types::fleet_activity::summarize(rows, NOW_MS, aterm_types::fleet_activity::WINDOW_MS)
}

/// The menu's grey lines in order, each with its mark.
fn infos(rows: &[StatusRow]) -> Vec<(&str, Mark)> {
    rows.iter()
        .filter_map(|row| match row {
            StatusRow::Info { text, mark } => Some((text.as_str(), *mark)),
            _ => None,
        })
        .collect()
}

/// The menu's clickable rows in order: what each does and whether it is checked.
fn actions(rows: &[StatusRow]) -> Vec<(StatusAction, bool)> {
    rows.iter()
        .filter_map(|row| match row {
            StatusRow::Action {
                action, checked, ..
            } => Some((*action, *checked)),
            _ => None,
        })
        .collect()
}

#[test]
fn every_action_rides_its_own_tag_and_a_retired_tag_is_inert() {
    let all = [
        StatusAction::RotateFleetAccess { repeated: false },
        StatusAction::RotateFleetAccess { repeated: true },
        StatusAction::AllActivity,
        StatusAction::SetFleetAccess(FleetAccess::ReadOnly),
        StatusAction::SetFleetAccess(FleetAccess::ReadWrite),
        StatusAction::SetFleetAccess(FleetAccess::Full),
    ];
    let tags: std::collections::BTreeSet<isize> = all.iter().map(|a| a.tag()).collect();
    assert_eq!(tags.len(), all.len(), "one tag each");
    for action in all {
        assert_eq!(StatusAction::from_tag(action.tag()), Some(action));
    }
    // The numbers and bands an earlier menu's rows rode in, and a level past the last: a
    // menu left painted by an older build posts nothing here.
    for retired in (0..=13).chain([16, 18]) {
        assert_eq!(StatusAction::from_tag(retired), None, "tag {retired}");
    }
    for band in 1..=15 {
        assert_eq!(
            StatusAction::from_tag(band * TAG_BAND + 1),
            None,
            "band {band}"
        );
    }
    assert_eq!(StatusAction::from_tag(SET_ACCESS_BAND * TAG_BAND + 3), None);
    assert_eq!(StatusAction::from_tag(-1), None);
}

#[test]
fn out_of_a_fleet_the_menu_is_this_mac_alone_and_no_lock_shows() {
    let g = Glance {
        machine: Some("m9 Yellow Seahorse".into()),
        network: Some("ALab LAN".into()),
        // A level file and a refusal left from a fleet this Mac was in mean nothing now.
        access: FleetAccess::Full,
        access_shown: FleetAccess::Full,
        traffic: Some(a_blocked_hour(NOW_MS)),
        blocked: true,
        ..Glance::default()
    };
    let rows = compose_status_menu(&g, NOW_MS);
    assert_eq!(
        rows,
        vec![info("m9 Yellow Seahorse \u{b7} not in a fleet", Mark::None)]
    );
    assert_eq!(
        g.button_face(),
        ButtonFace {
            title: "\u{276f}".into(),
            lock: None,
            blocked: false,
        }
    );
    assert_eq!(g.accessibility_label(), "aterm");
    // Before the machine's title was read the line still names something.
    let unnamed = compose_status_menu(&Glance::default(), NOW_MS);
    assert_eq!(infos(&unnamed).len(), 1);
    assert!(infos(&unnamed)[0].0.ends_with("not in a fleet"));
}

#[test]
fn in_a_fleet_the_menu_names_this_mac_and_offers_the_three_levels() {
    for access in FleetAccess::ALL {
        let g = in_fleet(access, None);
        let rows = compose_status_menu(&g, NOW_MS);
        assert_eq!(
            infos(&rows),
            vec![(
                format!(
                    "m9 Yellow Seahorse \u{b7} {} fleet \u{b7} ALab LAN",
                    crate::build_info::COMPANY
                )
                .as_str(),
                Mark::None
            )],
            "until the record is read the traffic has no line"
        );
        assert_eq!(
            actions(&rows),
            FleetAccess::ALL
                .map(|level| (StatusAction::SetFleetAccess(level), level == access))
                .to_vec(),
            "the three levels, rising, the one in force checked"
        );
        assert_eq!(rows[1], StatusRow::Separator);
    }
    // A raise that has not taken effect checks the level still in force.
    let raising = Glance {
        access_shown: FleetAccess::Full,
        ..in_fleet(FleetAccess::ReadOnly, None)
    };
    assert_eq!(
        actions(&compose_status_menu(&raising, NOW_MS))
            .iter()
            .filter(|(_, checked)| *checked)
            .map(|(action, _)| *action)
            .collect::<Vec<_>>(),
        vec![StatusAction::SetFleetAccess(FleetAccess::ReadOnly)]
    );
    // No network read yet: the line stops at the fleet.
    let unplaced = Glance {
        network: None,
        ..in_fleet(FleetAccess::ReadOnly, None)
    };
    let rows = compose_status_menu(&unplaced, NOW_MS);
    assert!(infos(&rows)[0].0.ends_with(" fleet"), "{rows:?}");
}

#[test]
fn an_hour_with_no_request_is_one_line() {
    use aterm_types::fleet_activity::{Plain, Way};
    // An empty record, and one whose latest request left the hour.
    for (traffic, line) in [
        (hour(&[]), "Quiet"),
        (
            hour(&[asked(Way::In, "m3", Plain::Typed, 4, 180, false)]),
            "Quiet for 3h",
        ),
    ] {
        let rows = compose_status_menu(&in_fleet(FleetAccess::ReadOnly, Some(traffic)), NOW_MS);
        assert_eq!(infos(&rows)[1..], [(line, Mark::None)]);
        assert_eq!(actions(&rows).len(), 3, "no record to open: {rows:?}");
    }
}

#[test]
fn a_busy_hour_has_a_line_for_each_mac_and_way_busiest_first() {
    use aterm_types::fleet_activity::{Plain, Way};
    let traffic = hour(&[
        asked(Way::Out, "m85", Plain::Read, 9, 20, false),
        asked(Way::Out, "m85", Plain::NewJob, 3, 20, false),
        asked(Way::In, "m86", Plain::Typed, 40, 0, false),
        asked(Way::In, "m86", Plain::Read, 2, 4, false),
    ]);
    let g = in_fleet(FleetAccess::Full, Some(traffic));
    assert!(!g.blocked);
    let rows = compose_status_menu(&g, NOW_MS);
    assert_eq!(
        infos(&rows)[1..],
        [
            // The headline keeps the mark's room, so it starts where the lines under it do.
            ("Last hour \u{b7} in from 1 Mac \u{b7} out to 1", Mark::Room),
            (
                "m86 \u{2190} 42 \u{b7} mostly typing \u{b7} now",
                Mark::Room
            ),
            ("m85 \u{2192} 12 \u{b7} 3 new jobs \u{b7} 20m", Mark::Room),
        ]
    );
    assert_eq!(
        actions(&rows).last(),
        Some(&(StatusAction::AllActivity, false)),
        "the whole record is one pick away"
    );
}

#[test]
fn a_refusal_marks_the_headline_and_the_line_that_counts_it() {
    use aterm_types::fleet_activity::{Plain, Way};
    // This Mac's lock refused m3; m85's lock refused this Mac; m86 was let through.
    let traffic = hour(&[
        asked(Way::In, "m3", Plain::NewJob, 2, 35, true),
        asked(Way::Out, "m85", Plain::NewJob, 1, 10, true),
        asked(Way::Out, "m85", Plain::NewJob, 2, 12, false),
        asked(Way::In, "m86", Plain::Typed, 6, 1, false),
    ]);
    let rows = compose_status_menu(&in_fleet(FleetAccess::ReadOnly, Some(traffic)), NOW_MS);
    assert_eq!(
        infos(&rows)[1..],
        [
            (
                "Last hour \u{b7} in from 2 Macs \u{b7} out to 1 \u{b7} 3 blocked",
                Mark::Blocked
            ),
            ("m86 \u{2190} 6 \u{b7} typing \u{b7} 1m", Mark::Room),
            (
                "m85 \u{2192} 3 \u{b7} 3 new jobs, 1 blocked \u{b7} 10m",
                Mark::Blocked
            ),
            (
                "m3 \u{2190} 2 \u{b7} 2 new jobs, both blocked \u{b7} 35m",
                Mark::Blocked
            ),
        ]
    );
    // The fixture the windowed drive paints from is such an hour.
    let fixture = compose_status_menu(
        &in_fleet(FleetAccess::ReadOnly, Some(a_blocked_hour(NOW_MS))),
        NOW_MS,
    );
    let marks: Vec<Mark> = infos(&fixture)[1..].iter().map(|(_, mark)| *mark).collect();
    assert_eq!(marks, [Mark::Blocked, Mark::Room, Mark::Blocked]);
}

#[test]
fn the_traffic_is_five_lines_at_most_and_a_few_reads_have_none() {
    use aterm_types::fleet_activity::{Plain, ROUTINE_READS, SHOWN_PAIRS, Way};
    let reads = u32::try_from(ROUTINE_READS).expect("a small number");
    let mut asks: Vec<_> = (0..12_u32)
        .map(|i| asked(Way::In, &format!("m{i}"), Plain::Typed, 20 + i, 5, false))
        .collect();
    // A Mac that only read this one a few times, and one that read it more than a few.
    asks.push(asked(Way::In, "reader", Plain::Read, reads, 2, false));
    asks.push(asked(Way::In, "watcher", Plain::Read, 500, 2, false));
    let traffic = hour(&asks);
    let rows = compose_status_menu(&in_fleet(FleetAccess::Full, Some(traffic.clone())), NOW_MS);
    let lines = infos(&rows);
    assert_eq!(
        lines.len(),
        2 + SHOWN_PAIRS,
        "identity, headline, five Macs"
    );
    assert!(
        lines[2].0.starts_with("watcher "),
        "busiest first: {lines:?}"
    );
    assert!(
        lines[1].0.contains("in from 14 Macs"),
        "every Mac is counted: {lines:?}"
    );
    // An hour of routine reads alone: counted in the headline, no line of its own.
    let routine = hour(&[asked(Way::In, "reader", Plain::Read, reads, 2, false)]);
    let rows = compose_status_menu(&in_fleet(FleetAccess::Full, Some(routine)), NOW_MS);
    assert_eq!(infos(&rows).len(), 2, "{rows:?}");
    assert_eq!(
        actions(&rows).last(),
        Some(&(StatusAction::AllActivity, false))
    );
    // One read more and the Mac has its line.
    let more = hour(&[asked(Way::In, "reader", Plain::Read, reads + 1, 2, false)]);
    let rows = compose_status_menu(&in_fleet(FleetAccess::Full, Some(more)), NOW_MS);
    assert_eq!(infos(&rows).len(), 3, "{rows:?}");
}

/// THE MENU'S ROWS, BY KIND AND IN ORDER, in each state it has: a grey line (`i`), a separator
/// (`-`), a level (`L`) and All activity… (`A`). Out of a fleet it is the identity line alone;
/// in one, the three levels follow, then the record once it was read: its headline alone for
/// a quiet hour, else the headline, a line for each Mac and way, and All activity…. A row of
/// any other kind, a row out of place or a row too many fails the shape.
#[test]
fn the_menu_is_this_mac_its_levels_and_the_record_in_that_order() {
    use aterm_types::fleet_activity::{Plain, Way};
    let shape = |g: &Glance| -> String {
        compose_status_menu(g, NOW_MS)
            .iter()
            .map(|row| match row {
                StatusRow::Info { .. } => 'i',
                StatusRow::Separator => '-',
                StatusRow::Action {
                    action: StatusAction::SetFleetAccess(_),
                    ..
                } => 'L',
                StatusRow::Action {
                    action: StatusAction::AllActivity,
                    ..
                } => 'A',
                StatusRow::Action { .. } => '?',
            })
            .collect()
    };
    let busy = hour(&[
        asked(Way::In, "m3", Plain::NewJob, 2, 35, true),
        asked(Way::Out, "m85", Plain::Hold, 4, 3, false),
    ]);
    assert_eq!(shape(&Glance::default()), "i");
    // The record of a Mac in no fleet is not shown, whatever it holds.
    let alone = Glance {
        traffic: Some(busy.clone()),
        ..Glance::default()
    };
    assert_eq!(shape(&alone), "i");
    assert_eq!(shape(&in_fleet(FleetAccess::ReadOnly, None)), "i-LLL");
    assert_eq!(
        shape(&in_fleet(FleetAccess::ReadWrite, Some(hour(&[])))),
        "i-LLL-i"
    );
    assert_eq!(
        shape(&in_fleet(FleetAccess::Full, Some(busy))),
        "i-LLL-iiiA"
    );
}

#[test]
fn the_bar_wears_the_mark_for_a_refusal_here_until_the_menu_opens_or_it_ages_out() {
    use aterm_types::fleet_activity::{Plain, WINDOW_MS, Way};
    let refused_at = NOW_MS - 35 * 60_000;
    let traffic = hour(&[asked(Way::In, "m3", Plain::NewJob, 2, 35, true)]);
    assert_eq!(traffic.last_blocked_in_ms, Some(refused_at));
    assert!(blocked_unseen(Some(&traffic), 0, NOW_MS), "never opened");
    assert!(
        blocked_unseen(Some(&traffic), refused_at - 1, NOW_MS),
        "opened before it"
    );
    assert!(
        !blocked_unseen(Some(&traffic), refused_at, NOW_MS),
        "opened since"
    );
    // A later refusal marks the bar again.
    let again = hour(&[
        asked(Way::In, "m3", Plain::NewJob, 2, 35, true),
        asked(Way::In, "m3", Plain::NewJob, 1, 1, true),
    ]);
    assert!(blocked_unseen(Some(&again), refused_at, NOW_MS));
    // The same summary read an hour on: the refusal left the hour, and the mark with it.
    assert!(blocked_unseen(
        Some(&traffic),
        0,
        refused_at + WINDOW_MS - 1
    ));
    assert!(!blocked_unseen(Some(&traffic), 0, refused_at + WINDOW_MS));
    // Another Mac's lock refusing this one is the menu's line, never the bar's mark.
    let outgoing = hour(&[asked(Way::Out, "m85", Plain::NewJob, 1, 2, true)]);
    assert_eq!(outgoing.blocked(), 1);
    assert!(!blocked_unseen(Some(&outgoing), 0, NOW_MS));
    assert!(!blocked_unseen(None, 0, NOW_MS), "no record read");
}

#[test]
fn the_face_the_hover_and_the_label_say_the_mark() {
    let clear = in_fleet(FleetAccess::ReadOnly, Some(hour(&[])));
    let marked = in_fleet(FleetAccess::ReadOnly, Some(a_blocked_hour(NOW_MS)));
    assert!(marked.blocked && !clear.blocked);
    assert_eq!(
        marked.button_face(),
        ButtonFace {
            title: "\u{276f}".into(),
            lock: Some(FleetAccess::ReadOnly),
            blocked: true,
        }
    );
    assert_eq!(
        clear.button_face(),
        ButtonFace {
            blocked: false,
            ..marked.button_face()
        }
    );
    // The title stays the caret: the mark is the lock's, and a fresh click names its level.
    assert_eq!(marked.button_title(), clear.button_title());
    let clicked = Glance {
        access_flash: Some(FleetAccess::ReadWrite),
        access_shown: FleetAccess::ReadWrite,
        ..marked.clone()
    };
    assert_eq!(clicked.button_face().title, "\u{276f} Read and Write");
    assert_eq!(clicked.button_face().lock, Some(FleetAccess::ReadWrite));
    // A screen reader and the hover say what the picture says.
    assert_ne!(marked.accessibility_label(), clear.accessibility_label());
    assert!(
        marked
            .accessibility_label()
            .starts_with(&clear.accessibility_label())
    );
    assert_ne!(marked.tooltip(), clear.tooltip());
    assert!(clicked.tooltip().contains("becoming Read and Write"));
    // The face is set again only where it differs from the one shown.
    assert_eq!(button_face_change(Some(&clear.button_face()), &clear), None);
    assert_eq!(
        button_face_change(Some(&clear.button_face()), &marked),
        Some(marked.button_face())
    );
    assert_eq!(button_face_change(None, &clear), Some(clear.button_face()));
}

#[test]
fn the_fingerprint_moves_with_everything_the_item_shows() {
    use aterm_types::fleet_activity::{Plain, Way};
    let base = in_fleet(
        FleetAccess::ReadOnly,
        Some(hour(&[asked(Way::In, "m86", Plain::Typed, 6, 1, false)])),
    );
    let moved = [
        Glance {
            machine: Some("m8 Heron".into()),
            ..base.clone()
        },
        Glance {
            in_fleet: false,
            ..base.clone()
        },
        Glance {
            network: Some("away \u{b7} no internet".into()),
            ..base.clone()
        },
        Glance {
            access: FleetAccess::Full,
            ..base.clone()
        },
        Glance {
            access_shown: FleetAccess::Full,
            ..base.clone()
        },
        Glance {
            access_flash: Some(FleetAccess::ReadOnly),
            ..base.clone()
        },
        Glance {
            traffic: Some(hour(&[asked(Way::In, "m86", Plain::Typed, 7, 1, false)])),
            ..base.clone()
        },
        Glance {
            traffic: None,
            ..base.clone()
        },
        Glance {
            blocked: true,
            ..base.clone()
        },
    ];
    assert_eq!(base.fingerprint(), base.clone().fingerprint());
    let prints: std::collections::BTreeSet<u64> = moved
        .iter()
        .chain([&base])
        .map(Glance::fingerprint)
        .collect();
    assert_eq!(prints.len(), moved.len() + 1);
}

#[test]
fn the_menu_reads_as_text_a_row_a_line_and_a_place_names_its_row() {
    let g = in_fleet(FleetAccess::ReadWrite, Some(a_blocked_hour(NOW_MS)));
    let rows = compose_status_menu(&g, NOW_MS);
    let lines = menu_lines(&rows);
    assert_eq!(lines.len(), rows.len());
    for (i, (line, row)) in lines.iter().zip(&rows).enumerate() {
        assert!(line.starts_with("statusmenu "), "{line}");
        assert!(line.ends_with(&format!(" at={i}")), "{line}");
        let at = MenuAt::parse(&i.to_string()).expect("a place");
        assert_eq!(row_at(&rows, &at), Some(row));
        assert_eq!(at.to_string(), i.to_string());
        let (clickable, blocked, checked) = match row {
            StatusRow::Info { mark, .. } => (false, *mark == Mark::Blocked, false),
            StatusRow::Separator => {
                assert_eq!(*line, format!("statusmenu --- at={i}"));
                continue;
            }
            StatusRow::Action { checked, .. } => (true, false, *checked),
        };
        assert_eq!(line.contains(" enabled=yes"), clickable, "{line}");
        assert_eq!(line.contains(" enabled=no"), !clickable, "{line}");
        assert_eq!(line.contains(" blocked=yes"), blocked, "{line}");
        assert_eq!(line.contains(" checked=yes"), checked, "{line}");
    }
    assert_eq!(
        lines.iter().filter(|l| l.contains(" checked=yes")).count(),
        1
    );
    assert_eq!(
        lines.iter().filter(|l| l.contains(" blocked=yes")).count(),
        2
    );
    let past = MenuAt::parse(&rows.len().to_string()).expect("a place");
    assert_eq!(row_at(&rows, &past), None);
    for bad in ["", "-1", "1.0", "a", "+2", " 3"] {
        assert_eq!(MenuAt::parse(bad), None, "{bad:?}");
    }
}
