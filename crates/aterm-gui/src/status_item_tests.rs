// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use aterm_agent::harness::resume::AfterRestart;

use crate::needs::{AgentFact, NeedFacts};

fn local_glance(
    rows: &[SessionRow],
    siblings: &[SiblingInstance],
    now_unix: i64,
    api_since: Option<i64>,
) -> Glance {
    Glance::build(rows, siblings, None, unix_ms(now_unix), api_since)
}

fn unix_ms(secs: i64) -> u64 {
    u64::try_from(secs).unwrap_or(0) * 1000
}

fn row_of(id: u64, title: &str, facts: &NeedFacts) -> SessionRow {
    SessionRow {
        id,
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

fn agent_row(id: u64, title: &str, word: &'static str, rev: u64) -> SessionRow {
    row_of(id, title, &agent_facts(word, rev))
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
        row_of(5, "worker", &stalled_agent(fact, row, secs))
    };
    let need_of = |row: &SessionRow| row.need.clone().expect("a stall row");
    let label = |row: &SessionRow| need_label(&row.title, None, &need_of(row).what);
    let first_row = row_at(&fact, 10);
    let later_row = row_at(
        &crate::input_stall::InputStallFact {
            bytes: 7,
            ..fact.clone()
        },
        95,
    );
    let (first, later) = (need_of(&first_row), need_of(&later_row));
    assert_ne!(label(&first_row), label(&later_row), "the text moved");
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
        3,
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
    let g = local_glance(
        &[
            row(1, "zsh", Some("deploy needs a human")),
            agent_row(2, "builder", "prompt", 5),
            frozen_row.clone(),
        ],
        &[],
        0,
        None,
    );
    assert_eq!(g.local_need_sessions()[0], 3, "{:?}", g.needs);
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
        let stopped_row = row_of(3, "\u{2733} worker", &facts);
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
        let label = need_label(&stopped_row.title, None, &stopped.what);
        assert!(!label.contains("signal cont"), "{label}");
    }
    let mut facts = worker(&fact, &AfterRestart::Relaunch, 11);
    facts.stall = None;
    facts.agent.as_mut().unwrap().word = "prompt";
    assert_eq!(
        row_of(3, "worker", &facts).need.map(|n| n.word),
        Some(NeedWord::Ask)
    );
}

#[test]
fn row_text_is_one_clipped_line() {
    assert_eq!(fold_clip("rm -rf\n  build\t/x", 64), "rm -rf build /x");
    assert_eq!(fold_clip("  lead", 64), "lead");
    let long = "x".repeat(100);
    let clipped = fold_clip(&long, 20);
    assert!(
        clipped.len() <= 20 && clipped.ends_with('\u{2026}'),
        "{clipped}"
    );
    let wide = "\u{00e9}".repeat(30);
    assert!(fold_clip(&wide, 11).ends_with('\u{2026}'));
    assert_eq!(
        fold_clip("rm -rf /tmp/\u{202e}gol.txt\u{2066}x\u{2069}\u{200b}y", 64),
        "rm -rf /tmp/gol.txtxy"
    );
    let mut spoof = agent_facts("prompt", 1);
    spoof.agent.as_mut().unwrap().subject = Some("ls \u{202e}fr- mr".to_string());
    let what = row_of(1, "t", &spoof).need.expect("a box").what;
    assert!(!need_label("t", None, &what).contains('\u{202e}'), "{what}");
    assert_eq!(
        fold_clip("caf\u{00e9} \u{1f469}\u{200d}\u{1f4bb}", 64),
        "caf\u{00e9} \u{1f469}\u{200d}\u{1f4bb}"
    );
    let mut bare = agent_facts("prompt", 1);
    bare.agent.as_mut().unwrap().subject = None;
    assert_eq!(
        need_label("t", None, &row_of(1, "t", &bare).need.expect("a box").what),
        "t \u{2014} bash approval"
    );
}

#[test]
fn presence_band_and_menu_bar_say_a_box_and_a_wall_alike() {
    let mut bare = agent_facts("prompt", 1);
    bare.agent.as_mut().unwrap().subject = None;
    for (detail, word, label) in [
        ("question", NeedWord::Question, "question"),
        ("other", NeedWord::Approval, "approval"),
        ("plan-exit", NeedWord::Approval, "plan approval"),
    ] {
        bare.agent.as_mut().unwrap().detail = Some(detail.into());
        let need = row_of(1, "t", &bare).need.expect("a box");
        assert_eq!(need.word, word, "{detail}");
        assert_eq!(
            need_label("t", None, &need.what),
            format!("t \u{2014} {label}"),
            "{detail}"
        );
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
    let walled = agent_row(2, "w", "wall:overloaded", 1)
        .need
        .expect("a wall");
    assert_eq!(
        need_label("w", None, &walled.what),
        format!("w \u{2014} {}", overloaded.band_word())
    );
}

fn row(id: u64, title: &str, attention: Option<&str>) -> SessionRow {
    let facts = NeedFacts {
        attention: attention
            .map(|text| crate::needs::AttentionFact {
                owner: "-".to_string(),
                text: text.to_string(),
                kind: Some(aterm_types::needs::AttentionKind::Ask),
            })
            .into_iter()
            .collect(),
        ..NeedFacts::default()
    };
    row_of(id, title, &facts)
}

fn worker(id: u64, title: &str, from: &str, state: TaskState, result: u64) -> SessionRow {
    SessionRow {
        id,
        sid: format!("s-{id:020}"),
        title: title.to_string(),
        agent: "busy",
        task: Some(TaskRow {
            state,
            why: (state == TaskState::Stalled).then_some("startup"),
            from: from.to_string(),
            entered: 7,
            since_unix_ms: 1_000_000,
            result_bytes: result,
            result_path: (result > 0).then(|| std::path::PathBuf::from("/tmp/r.md")),
            seen: false,
            finished: false,
            own_report: false,
        }),
        ..SessionRow::default()
    }
}

/// `row`, whose task finished before its agent left.
fn finished(mut row: SessionRow) -> SessionRow {
    row.task.as_mut().unwrap().finished = true;
    row
}

#[derive(Debug, PartialEq, Eq)]
enum Shape {
    Info,
    Sep,
    Act(StatusAction),
    Sub(Vec<Shape>),
    Fleet,
}

fn shape(rows: &[StatusRow]) -> Vec<Shape> {
    rows.iter()
        .map(|row| match row {
            StatusRow::Info(_) => Shape::Info,
            StatusRow::Separator => Shape::Sep,
            StatusRow::Action { action, .. } => Shape::Act(*action),
            StatusRow::Submenu { rows, .. } => Shape::Sub(shape(rows)),
            StatusRow::FleetSubmenu { .. } => Shape::Fleet,
        })
        .collect()
}

#[test]
fn a_fleet_need_on_a_mac_that_lets_this_one_drive_it_offers_take_over() {
    let driven = PeerRef {
        dial: "lab-mini.local".to_string(),
        name: "lab-mini".to_string(),
        host: Some("lab-mini.local".to_string()),
        sid: "s-ab12".to_string(),
        drive: true,
        access: Some(FleetAccess::Full),
    };
    let rows = peer_rows(&driven, pick(0));
    assert_eq!(
        shape(&rows),
        vec![
            Shape::Act(StatusAction::PeerRead(pick(0))),
            Shape::Act(StatusAction::PeerTakeOver(pick(0))),
            Shape::Act(StatusAction::PeerHandBack(pick(0))),
            Shape::Act(StatusAction::PeerScreen(pick(0))),
            Shape::Act(StatusAction::PeerSsh(pick(0))),
            Shape::Info,
        ]
    );
    let labels: Vec<&str> = rows
        .iter()
        .filter_map(|row| match row {
            StatusRow::Action { label, .. } => Some(label.as_str()),
            StatusRow::Info(line) => Some(line.as_str()),
            _ => None,
        })
        .collect();
    assert!(labels.contains(&"Take Over on lab-mini"), "{labels:?}");
    assert!(labels.contains(&"Hand Back on lab-mini"), "{labels:?}");
    assert!(
        labels
            .last()
            .is_some_and(|l| l.contains("person's Take Over wins")),
        "{labels:?}"
    );
    let read_only = peer_rows(
        &PeerRef {
            drive: false,
            ..driven.clone()
        },
        pick(0),
    );
    assert_eq!(
        shape(&read_only),
        vec![
            Shape::Act(StatusAction::PeerRead(pick(0))),
            Shape::Act(StatusAction::PeerScreen(pick(0))),
            Shape::Act(StatusAction::PeerSsh(pick(0))),
            Shape::Info,
        ]
    );
    assert!(
        matches!(read_only.last(), Some(StatusRow::Info(line))
            if line.starts_with("Read-only from here") && line.contains("allow drive")),
        "{read_only:?}"
    );
    // Every (drive, access) pair: Take Over and Hand Back are offered exactly where the
    // other Mac would take them, and each pair's closing line is its own.
    let acts_on = |rows: &[StatusRow]| {
        rows.iter().any(|row| {
            matches!(
                row,
                StatusRow::Action {
                    action: StatusAction::PeerTakeOver(_) | StatusAction::PeerHandBack(_),
                    ..
                }
            )
        })
    };
    let mut closing = std::collections::BTreeSet::new();
    for drive in [false, true] {
        for access in [
            None,
            Some(FleetAccess::ReadOnly),
            Some(FleetAccess::ReadWrite),
            Some(FleetAccess::Full),
        ] {
            let peer = PeerRef {
                drive,
                access,
                ..driven.clone()
            };
            let want = drive && access != Some(FleetAccess::ReadOnly);
            let rows = peer_rows(&peer, pick(0));
            assert_eq!(acts_on(&rows), want, "need rows at {drive} {access:?}");
            assert_eq!(
                shape(&rows).first(),
                Some(&Shape::Act(StatusAction::PeerRead(pick(0)))),
                "a read is offered at {drive} {access:?}"
            );
            let held = held_there_rows(&peer, pick(0));
            assert_eq!(acts_on(&held), want, "held rows at {drive} {access:?}");
            assert_eq!(shape(&held).last(), Some(&Shape::Info));
            if drive {
                closing.insert(match rows.last() {
                    Some(StatusRow::Info(line)) => line.clone(),
                    other => panic!("a closing line: {other:?}"),
                });
            }
        }
    }
    assert_eq!(
        closing.len(),
        3,
        "read only, read and write, and full or unread: {closing:?}"
    );
}

#[test]
fn a_hold_this_mac_placed_elsewhere_keeps_its_hand_back() {
    let now = 10_000_i64;
    let read_ms = u64::try_from((now - 20) * 1000).unwrap();
    let held = crate::fleet_glance::FleetMac {
        drive: true,
        held_here: vec![crate::fleet_glance::FleetHeld {
            sid: "s-ab12".to_string(),
            who: "review".to_string(),
        }],
        ..mac("lab-mini", 0, 0, false)
    };
    let g = Glance::build(
        &[],
        &[],
        Some(&fleet_slot(vec![held.clone()], read_ms)),
        unix_ms(now),
        None,
    );
    assert_eq!((g.needs.len(), g.need_count()), (0, 0), "a hold is no need");
    assert_eq!(g.held_there.len(), 1);
    let peer = &g.peers[usize::from(g.held_there[0].at)];
    assert_eq!(
        (peer.dial.as_str(), peer.sid.as_str()),
        ("lab-mini", "s-ab12")
    );
    let menu = compose_status_menu(&g, now);
    let at = menu
        .iter()
        .position(|r| matches!(r, StatusRow::Info(line) if line == "Held from here"))
        .expect("a Held from here group");
    let pick = Pick {
        generation: g.generation,
        index: g.held_there[0].at,
    };
    assert_eq!(
        shape(&menu[at + 1..=at + 1]),
        vec![Shape::Sub(vec![
            Shape::Act(StatusAction::PeerHandBack(pick)),
            Shape::Act(StatusAction::PeerRead(pick)),
            Shape::Info,
        ])]
    );
    let revoked = crate::fleet_glance::FleetMac {
        drive: false,
        ..held
    };
    let g = Glance::build(
        &[],
        &[],
        Some(&fleet_slot(vec![revoked], read_ms)),
        unix_ms(now),
        None,
    );
    let menu = compose_status_menu(&g, now);
    assert!(
        !menu.iter().any(|r| matches!(r, StatusRow::Submenu { rows, .. }
            if rows.iter().any(|r| matches!(r, StatusRow::Action { action: StatusAction::PeerHandBack(_), .. })))),
        "{menu:?}"
    );
    let g = Glance::build(
        &[],
        &[],
        Some(&fleet_slot(vec![mac("lab-mini", 0, 0, false)], read_ms)),
        unix_ms(now),
        None,
    );
    assert!(g.held_there.is_empty());
    assert!(
        !compose_status_menu(&g, now)
            .iter()
            .any(|r| matches!(r, StatusRow::Info(line) if line == "Held from here"))
    );
}

fn flat(rows: &[StatusRow], fleet: &[StatusRow], depth: usize) -> Vec<(usize, Option<String>)> {
    let mut out = Vec::new();
    for row in rows {
        match row {
            StatusRow::Separator => out.push((depth, None)),
            StatusRow::Info(label) | StatusRow::Action { label, .. } => {
                out.push((depth, Some(label.clone())));
            }
            StatusRow::Submenu { label, rows } => {
                out.push((depth, Some(label.clone())));
                out.extend(flat(rows, fleet, depth + 1));
            }
            StatusRow::FleetSubmenu { label } => {
                out.push((depth, Some(label.clone())));
                out.extend(flat(fleet, &[], depth + 1));
            }
        }
    }
    out
}

fn worded(lines: Vec<String>) -> Vec<String> {
    lines
        .into_iter()
        .map(|line| match line.rsplit_once(" at=") {
            Some((words, _)) => words.to_string(),
            None => line,
        })
        .collect()
}

#[test]
fn every_row_path_chrome_prints_names_its_row() {
    let now = 10_000_i64;
    let read_ms = u64::try_from((now - 20) * 1000).unwrap();
    let slot = fleet_slot(
        vec![crate::fleet_glance::FleetMac {
            drive: true,
            ..mac("lab-mini", 1, 1, false)
        }],
        read_ms,
    );
    let g = Glance::build(&[], &[], Some(&slot), unix_ms(now), None);
    let (rows, fleet) = (compose_status_menu(&g, now), compose_fleet_menu(&slot, now));
    let lines = menu_lines(&rows, &fleet);
    let flat = flat(&rows, &fleet, 0);
    assert_eq!(lines.len(), flat.len(), "{lines:#?}");
    let mut depths = std::collections::BTreeSet::new();
    for (line, (depth, label)) in lines.iter().zip(&flat) {
        let (words, at) = line
            .rsplit_once(" at=")
            .unwrap_or_else(|| panic!("no path: {line}"));
        assert!(!at.contains(' '), "the path is the last word: {line}");
        let path = MenuAt::parse(at).unwrap_or_else(|| panic!("{at} is no path"));
        assert_eq!(path.to_string(), at, "printed as it reads");
        assert_eq!(path.0.len(), depth + 1, "a level a path index: {line}");
        let row = row_at(&rows, &fleet, &path).unwrap_or_else(|| panic!("{line}: no row"));
        let named = match row {
            StatusRow::Separator => None,
            StatusRow::Info(label)
            | StatusRow::Action { label, .. }
            | StatusRow::Submenu { label, .. }
            | StatusRow::FleetSubmenu { label } => Some(label.clone()),
        };
        assert_eq!(&named, label, "{line}");
        assert!(label.is_some() || words.ends_with("---"), "{line}");
        depths.insert(*depth);
    }
    assert!(
        depths.contains(&1),
        "PRECONDITION: a submenu's rows were read: {lines:#?}"
    );
    let fleet_child = lines
        .iter()
        .find(|l| l.contains("\"Refresh Now\""))
        .expect("the Fleet row's own rows");
    assert!(
        fleet_child.starts_with("statusmenu   "),
        "PRECONDITION: one level in: {fleet_child}"
    );
    assert_eq!(row_at(&rows, &fleet, &MenuAt(vec![rows.len()])), None);
    let leaf = rows
        .iter()
        .position(|r| matches!(r, StatusRow::Action { .. }))
        .expect("an action row");
    assert_eq!(row_at(&rows, &fleet, &MenuAt(vec![leaf, 0])), None);
    for junk in ["", ".", "1.", "a", "-1", "1.x", "0.0.0.0.0", "1 2"] {
        assert_eq!(MenuAt::parse(junk), None, "{junk:?}");
    }
    assert_eq!(MenuAt::parse("3.0"), Some(MenuAt(vec![3, 0])));
}

#[test]
fn the_menu_dump_is_the_builders_rows_in_order() {
    let now = 10_000_i64;
    let read_ms = u64::try_from((now - 20) * 1000).unwrap();
    let driven = crate::fleet_glance::FleetMac {
        drive: true,
        held_here: vec![crate::fleet_glance::FleetHeld {
            sid: "s-ab12".to_string(),
            who: "review".to_string(),
        }],
        ..mac("lab-mini", 1, 1, false)
    };
    let dump = |mac: crate::fleet_glance::FleetMac| {
        let slot = fleet_slot(vec![mac], read_ms);
        let g = Glance::build(&[], &[], Some(&slot), unix_ms(now), None);
        let (rows, fleet) = (compose_status_menu(&g, now), compose_fleet_menu(&slot, now));
        (worded(menu_lines(&rows, &fleet)), flat(&rows, &fleet, 0))
    };
    let (lines, rows) = dump(driven.clone());
    assert_eq!(lines.len(), rows.len(), "{lines:#?}");
    for (line, (depth, label)) in lines.iter().zip(&rows) {
        let at = format!("statusmenu {}", "  ".repeat(*depth));
        let row = line.strip_prefix(&at).unwrap_or_else(|| panic!("{line}"));
        assert!(!row.starts_with(' '), "{line}");
        match label {
            None => assert_eq!(row, "---"),
            Some(label) => assert!(row.starts_with(&format!("{label:?} ")), "{line}"),
        }
    }
    assert!(
        lines
            .contains(&"statusmenu   \"Take Over on lab-mini\" enabled=yes checked=no".to_string()),
        "{lines:#?}"
    );
    assert!(
        lines.contains(&"statusmenu \"Held from here\" enabled=no".to_string()),
        "{lines:#?}"
    );
    assert!(
        lines.contains(&"statusmenu   \"Refresh Now\" enabled=yes checked=no".to_string()),
        "the Fleet row's own rows: {lines:#?}"
    );
    let (lines, _) = dump(crate::fleet_glance::FleetMac {
        drive: false,
        ..driven
    });
    assert!(
        !lines.iter().any(|line| line.contains("Take Over on")),
        "{lines:#?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("\"Read-only from here: lab-mini")),
        "{lines:#?}"
    );
}

#[test]
fn a_mac_whose_key_is_not_proven_is_listed_and_never_counted_or_driven() {
    let now = 10_000_i64;
    let read_ms = u64::try_from((now - 20) * 1000).unwrap();
    let studio = crate::fleet_glance::FleetMac {
        drive: true,
        unproven: true,
        held_here: vec![crate::fleet_glance::FleetHeld {
            sid: "s-ab12".to_string(),
            who: "review".to_string(),
        }],
        ..mac("studio", 1, 1, false)
    };
    let read = |mac: crate::fleet_glance::FleetMac| {
        let slot = fleet_slot(vec![mac], read_ms);
        let g = Glance::build(&[], &[], Some(&slot), unix_ms(now), None);
        let fleet = compose_fleet_menu(&slot, now);
        let lines = worded(menu_lines(&compose_status_menu(&g, now), &fleet));
        (g, fleet, lines)
    };
    let (g, fleet, lines) = read(studio.clone());
    assert_eq!((g.needs.len(), g.need_count()), (0, 0), "{:?}", g.needs);
    assert!(g.held_there.is_empty() && g.peers.is_empty(), "{g:?}");
    assert_eq!(g.button_title(), "\u{276f}");
    let fold = fleet
        .iter()
        .find_map(|row| match row {
            StatusRow::Submenu { label, rows }
                if label == "1 Mac's key not proven (not counted: rejoin it)" =>
            {
                Some(rows)
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("its fold: {fleet:?}"));
    let [StatusRow::Submenu { label, rows }] = fold.as_slice() else {
        panic!("one Mac in it: {fold:?}");
    };
    assert_eq!(
        label,
        "studio \u{2014} key not proven \u{b7} 1 need \u{b7} 1 busy"
    );
    let infos: Vec<&str> = rows
        .iter()
        .filter_map(|row| match row {
            StatusRow::Info(line) => Some(line.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        infos.contains(&"\u{270b} review \u{2014} held from here: its person hands it back there"),
        "{infos:?}"
    );
    assert!(
        infos.last().is_some_and(|line| line
            == &"Update aterm on studio and join it again (`aterm fleet join`); \
                 `aterm fleet peers` shows its KEY"),
        "{infos:?}"
    );
    assert!(
        !lines
            .iter()
            .any(|line| line.contains("Take Over on") || line.contains("Hand Back on")),
        "{lines:#?}"
    );
    assert!(
        lines.contains(
            &"statusmenu   \"1 Mac's key not proven (not counted: rejoin it)\" enabled=yes \
              submenu"
                .to_string()
        ),
        "{lines:#?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("studio's key is not proven")),
        "{lines:#?}"
    );
    let (g, fleet, lines) = read(crate::fleet_glance::FleetMac {
        unproven: false,
        ..studio
    });
    assert_eq!((g.needs.len(), g.need_count()), (1, 1));
    assert_eq!(g.held_there.len(), 1);
    assert!(
        lines.contains(&"statusmenu   \"Take Over on studio\" enabled=yes checked=no".to_string()),
        "{lines:#?}"
    );
    assert!(
        fleet
            .iter()
            .any(|row| matches!(row, StatusRow::Submenu { label, .. }
            if label == "studio \u{2014} 1 needs you \u{b7} 1 busy")),
        "{fleet:?}"
    );
    assert!(
        !lines.iter().any(|line| line.contains("not proven")),
        "{lines:#?}"
    );
}

#[test]
fn where_this_mac_binds_no_key_to_its_issuer_its_own_join_proves_them() {
    let now = 10_000_i64;
    let read_at = u64::try_from((now - 20) * 1000).unwrap();
    let unproven = |name| crate::fleet_glance::FleetMac {
        unproven: true,
        ..mac(name, 1, 0, false)
    };
    let read = |unbound: Option<&str>| {
        let mut slot = fleet_slot(vec![unproven("lab-mini"), unproven("studio")], read_at);
        slot.book.minted = false;
        slot.book.unbound_issuer = unbound.map(str::to_string);
        let g = Glance::build(&[], &[], Some(&slot), unix_ms(now), None);
        assert_eq!(g.need_count(), 0, "nothing of theirs counts either way");
        worded(menu_lines(
            &compose_status_menu(&g, now),
            &compose_fleet_menu(&slot, now),
        ))
    };
    let lines = read(Some("lab-mini"));
    let says = |lines: &[String], text: &str| lines.iter().filter(|l| l.contains(text)).count();
    let join_here = "This Mac binds no key to its issuer lab-mini: join again here once it has \
                     one (`aterm fleet init` there)";
    assert_eq!(
        says(&lines, join_here),
        3,
        "the menu, under its Fleet row, and both Macs' rows; the Fleet submenu repeats \
         none: {lines:#?}"
    );
    assert_eq!(
        says(
            &lines,
            "\"2 Macs' keys not proven (not counted: join again here)\" enabled=yes submenu"
        ),
        1,
        "{lines:#?}"
    );
    assert_eq!(says(&lines, "Update aterm on"), 0, "{lines:#?}");
    let lines = read(None);
    assert_eq!(says(&lines, "binds no key to its issuer"), 0, "{lines:#?}");
    assert_eq!(
        says(
            &lines,
            "\"2 Macs' keys not proven (not counted: rejoin them)\" enabled=yes submenu"
        ),
        1,
        "{lines:#?}"
    );
    for name in ["lab-mini", "studio"] {
        assert_eq!(
            says(
                &lines,
                &format!(
                    "Update aterm on {name} and join it again (`aterm fleet join`); `aterm \
                     fleet peers` shows its KEY"
                )
            ),
            1,
            "{name}: {lines:#?}"
        );
    }
}

fn acts() -> [Shape; 3] {
    [
        Shape::Sep,
        Shape::Act(StatusAction::Overview),
        Shape::Act(StatusAction::ReadThisMac),
    ]
}

fn pick(index: u16) -> Pick {
    Pick {
        generation: 0,
        index,
    }
}

#[test]
fn needs_are_listed_stuck_first_then_oldest_and_each_goes_to_its_tab() {
    let mut limits = agent_row(1, "limits", "wall:usage-session", 3);
    limits.need_since = Some(500);
    let mut asker = agent_row(3, "asker", "question", 2);
    asker.need_since = Some(100);
    let mut builder = agent_row(4, "builder", "prompt", 5);
    builder.need_since = Some(50);
    let mut zsh = row(5, "zsh", Some("deploy needs a human"));
    zsh.need_since = Some(200);
    let g = local_glance(
        &[
            limits,
            agent_row(2, "tests", "busy", 1),
            asker,
            builder,
            zsh,
            agent_row(6, "idle one", "idle", 1),
        ],
        &[],
        1000,
        None,
    );
    assert_eq!(g.local_need_sessions(), vec![1, 4, 3, 5]);
    assert_eq!(g.need_count(), 4);
    assert_eq!(g.button_title(), "\u{276f}");
    assert!(g.stuck(), "the wall is stuck");
    let menu = compose_status_menu(&g, 1000);
    let go = |session: u64| {
        let at =
            g.go.iter()
                .position(|s| *s == session)
                .expect("a go target");
        Shape::Act(StatusAction::Go(pick(u16::try_from(at).unwrap())))
    };
    assert_eq!(
        shape(&menu[..6]),
        vec![Shape::Info, go(1), go(4), go(3), go(5), Shape::Sep]
    );
    let calm = local_glance(&[agent_row(2, "tests", "busy", 1)], &[], 1000, None);
    assert!(calm.needs.is_empty());
    assert_eq!(calm.button_title(), "\u{276f}");
}

#[test]
fn local_and_sibling_diagnostics_keep_the_bar_quiet() {
    let sibling = SiblingInstance {
        pid: 900,
        sock: "/tmp/aterm-900.sock".to_string(),
        sessions: 2,
        needs: vec![SiblingNeed {
            sid: "s-sib".to_string(),
            word: NeedWord::Frozen,
            label: "claude".to_string(),
            age_ms: Some(60_000),
            claim: None,
            carries_on: false,
        }],
        scanned_ms: unix_ms(990),
    };
    let decide = local_glance(&[row(1, "zsh", Some("look"))], &[], 1000, None);
    assert_eq!((decide.need_count(), decide.stuck()), (1, false));
    let both = local_glance(
        &[row(1, "zsh", Some("look"))],
        std::slice::from_ref(&sibling),
        1000,
        None,
    );
    assert_eq!(both.button_title(), "\u{276f}");
    assert!(both.stuck(), "the sibling's frozen agent is stuck");
    assert_eq!(both.stuck_count(), 1);
    assert_eq!(both.needs[0].origin, Origin::Sibling(900));
    assert_eq!(both.needs[0].at, NeedAt::Raise(0));
    assert_eq!(both.raises[0].sid, "s-sib");
    assert_eq!(both.here.elsewhere, 2);
    let quiet = SiblingInstance {
        needs: Vec::new(),
        ..sibling
    };
    let g = local_glance(&[], &[quiet], 1000, None);
    assert_eq!((g.need_count(), g.here.elsewhere), (0, 2));
}

#[test]
fn a_sibling_need_counts_only_from_a_young_scan() {
    let scanned_ms = 1_000_000_000_500;
    let fresh = u64::try_from(SIBLING_FRESH.as_millis()).unwrap();
    let sibling = SiblingInstance {
        pid: 900,
        sock: "/tmp/aterm-900.sock".to_string(),
        sessions: 1,
        needs: vec![SiblingNeed {
            sid: "s-sib".to_string(),
            word: NeedWord::Stalled,
            label: "claude".to_string(),
            age_ms: Some(60_000),
            claim: None,
            carries_on: false,
        }],
        scanned_ms,
    };
    let at = |now_ms: u64| Glance::build(&[], std::slice::from_ref(&sibling), None, now_ms, None);
    let young = at(scanned_ms + fresh);
    assert_eq!(
        (young.need_count(), young.stuck()),
        (1, true),
        "the control"
    );
    assert_eq!(young.needs[0].read_at_ms, None);
    let born = i64::try_from(scanned_ms / 1000).unwrap() - 60;
    assert_eq!(young.needs[0].since_unix, Some(born));
    let old = at(scanned_ms + fresh + 1);
    assert_eq!((old.needs.len(), old.need_count()), (1, 0));
    assert_eq!(old.needs[0].read_at_ms, Some(scanned_ms));
    assert_eq!(old.needs[0].since_unix, Some(born), "aged from the scan");
    assert_eq!(old.button_title(), "\u{276f}");
    assert_eq!(young.fingerprint(), at(scanned_ms + 1).fingerprint());
    assert_ne!(young.fingerprint(), old.fingerprint(), "uncounted moves it");
}

#[test]
fn the_dot_means_something_for_later_and_never_hides_a_need() {
    let mut g = local_glance(&[], &[], 0, None);
    assert_eq!(g.button_title(), "\u{276f}");
    g.exceptions = vec![(11, "macOS keeps asking for access".into())];
    assert_eq!(g.button_title(), "\u{276f}");
    let done = local_glance(
        &[worker(
            2,
            "summary",
            crate::task_record::FROM_CTL,
            TaskState::Done,
            10,
        )],
        &[],
        0,
        None,
    );
    assert_eq!(done.button_title(), "\u{276f}", "a result for you");
    let mut asked = local_glance(&[row(3, "zsh", Some("look"))], &[], 0, None);
    asked.exceptions = g.exceptions.clone();
    assert_eq!(asked.button_title(), "\u{276f}", "a need wins the title");
}

#[test]
fn sections_appear_only_with_data_in_order() {
    let empty = compose_status_menu(&local_glance(&[], &[], 0, None), 0);
    let mut want = vec![Shape::Info];
    want.extend(acts());
    assert_eq!(shape(&empty), want);

    let manager = SessionRow {
        id: 1,
        sid: "s-manager".to_string(),
        title: "lab-sweep".to_string(),
        agent: "busy",
        ..SessionRow::default()
    };
    let mut waiting = agent_row(5, "limited", "wall:usage-session", 1);
    waiting.need = None;
    waiting.waiting = Some(crate::needs::Waiting {
        word: WaitWord::Limit,
        wall: Some("usage-session"),
        reset: Some("7pm".to_string()),
        entry: None,
    });
    let mut held = agent_row(6, "bench", "busy", 1);
    held.held = Some(HoldWord::Person);
    let rows = [
        manager,
        worker(2, "bench", "s-manager", TaskState::Working, 0),
        worker(3, "lint", "s-manager", TaskState::Done, 2100),
        worker(
            4,
            "summary",
            crate::task_record::FROM_CTL,
            TaskState::Done,
            0,
        ),
        waiting,
        held,
        row(7, "zsh", Some("look at the build")),
    ];
    let mut g = local_glance(&rows, &[], 2000, None);
    g.exceptions = vec![(41, "a fact for later".into())];
    let menu = compose_status_menu(&g, 2000);
    let go = |session: u64| {
        let at =
            g.go.iter()
                .position(|s| *s == session)
                .expect("a go target");
        Shape::Act(StatusAction::Go(pick(u16::try_from(at).unwrap())))
    };
    let mut want = vec![
        Shape::Info,
        go(7),
        Shape::Sep,
        Shape::Info,
        Shape::Sub(vec![
            go(1),
            Shape::Sep,
            go(2),
            go(3),
            Shape::Sep,
            Shape::Act(StatusAction::ReadResult(pick(0))),
        ]),
        Shape::Sep,
        Shape::Info,
        go(4),
        Shape::Sep,
        Shape::Info,
        Shape::Sub(vec![go(5)]),
        Shape::Sep,
        Shape::Info,
        go(6),
    ];
    want.extend([
        Shape::Act(StatusAction::HandBackAll),
        Shape::Sep,
        Shape::Info,
        Shape::Act(StatusAction::ShowException(41)),
        Shape::Act(StatusAction::DismissExceptions),
    ]);
    want.extend(acts());
    assert_eq!(shape(&menu), want);
    assert_eq!(g.reads[0].session, 3, "lint's result");
    assert_eq!(
        (g.here.working, g.here.done, g.here.shells),
        (2, 2, 1),
        "the manager and bench work; lint and the summary are done"
    );
    assert_eq!(
        (g.here.need_you, g.here.waiting),
        (1, 1),
        "zsh's ask needs you; the limited tab waits"
    );
    assert_eq!(
        here_line(&g.here, g.machine.as_deref()),
        "This Mac \u{2014} 2 working \u{b7} 1 need you \u{b7} 1 waiting \u{b7} 2 done \u{b7} 1 shell"
    );
}

/// §14.6: a Land session that holds counts as `landing` on This Mac, the word the page's Every
/// session row says for it, and a need it carries counts first; the Off session at rest stays
/// `resting`. The row reaches the menu as composed (AppKit's own item is `verify appkit`'s
/// stage 11, on the main thread AppKit's menus need).
#[test]
fn a_held_landing_counts_as_landing_on_this_mac() {
    let rows = a_land_desk_at_rest();
    let g = local_glance(&rows, &[], 2000, None);
    assert_eq!(
        (
            g.here.need_you,
            g.here.landing,
            g.here.resting,
            g.here.landed
        ),
        (1, 6, 1, 5),
        "{:?}",
        g.here
    );
    let line = here_line(&g.here, g.machine.as_deref());
    assert_eq!(
        line,
        "This Mac \u{2014} 1 need you \u{b7} 6 landing \u{b7} 1 resting \u{b7} 5 landed \u{b7} 3 shells"
    );
    let menu = compose_status_menu(&g, 2000);
    let lines = menu_lines(&menu, &[]);
    assert_eq!(
        lines
            .iter()
            .filter(|l| l.starts_with(&format!("statusmenu {line:?} enabled=no")))
            .count(),
        1,
        "{lines:#?}"
    );
}

/// §14.6: the landing arm comes after the waiting arm, as after the need arm, so a held Land
/// that aterm waits out a limit for counts `waiting`, never `landing` as well.
#[test]
fn a_held_land_that_waits_counts_waiting_not_landing() {
    let mut rows = a_land_desk_at_rest();
    let held = rows
        .iter()
        .find(|r| r.landing && r.need.is_none())
        .cloned()
        .expect("a held Land row");
    rows.push(SessionRow {
        id: 40,
        sid: format!("s-{:020}", 40),
        title: "land-w".to_string(),
        waiting: Some(crate::needs::Waiting {
            word: WaitWord::Limit,
            wall: Some("usage-session"),
            reset: Some("7pm".to_string()),
            entry: None,
        }),
        ..held
    });
    let g = local_glance(&rows, &[], 2000, None);
    assert_eq!(
        (
            g.here.need_you,
            g.here.waiting,
            g.here.landing,
            g.here.resting
        ),
        (1, 1, 6, 1),
        "{:?}",
        g.here
    );
}

#[test]
fn the_you_hold_row_counts_the_refused_writes_and_names_who_waits() {
    let manager = SessionRow {
        id: 1,
        sid: "s-manager".to_string(),
        title: "lab-sweep".to_string(),
        agent: "busy",
        ..SessionRow::default()
    };
    let held = |refused: u64, hold: HoldWord| {
        let mut bench = worker(2, "bench", "s-manager", TaskState::Working, 0);
        bench.held = Some(hold);
        bench.held_since = Some(2000 - 240);
        bench.person_refusals = refused;
        bench
    };
    let glance = |bench: SessionRow| local_glance(&[manager.clone(), bench], &[], 2000, None);
    let line = |g: &Glance| held_line(&g.held[0], 2000);

    let quiet = glance(held(0, HoldWord::Person));
    assert_eq!(quiet.held[0].refused, 0);
    assert_eq!(quiet.held[0].worker_of.as_deref(), Some("lab-sweep"));
    assert_eq!(
        line(&quiet),
        "\u{270b} bench (worker of lab-sweep) \u{2014} yours 4m"
    );
    let refused = glance(held(1, HoldWord::Person));
    assert_eq!(refused.held[0].refused, 1);
    assert_eq!(
        line(&refused),
        "\u{270b} bench (worker of lab-sweep) \u{2014} yours 4m \u{b7} refused 1 write"
    );
    assert_ne!(refused.fingerprint(), quiet.fingerprint());
    assert!(line(&glance(held(3, HoldWord::Person))).ends_with("refused 3 writes"));
    assert_eq!(
        glance(held(0, HoldWord::Person)).fingerprint(),
        quiet.fingerprint()
    );
    let client = glance(held(5, HoldWord::Local));
    assert_eq!(client.held[0].refused, 0);
    assert!(!line(&client).contains("refused"), "{}", line(&client));

    // What the hold costs is the hover of the held row and of Hand Back All, and no row of
    // its own.
    let menu = compose_status_menu(&refused, 2000);
    let cost = aterm_types::control_verbs::HOLD_COST;
    let hovers: Vec<&StatusAction> = menu
        .iter()
        .filter_map(|r| match r {
            StatusRow::Action { action, tip, .. } if tip.as_deref() == Some(cost) => Some(action),
            _ => None,
        })
        .collect();
    assert!(
        matches!(
            hovers.as_slice(),
            [StatusAction::Go(_), StatusAction::HandBackAll]
        ),
        "{hovers:?}"
    );
    assert!(
        !menu
            .iter()
            .any(|r| matches!(r, StatusRow::Info(text) if cost.starts_with(text.as_str()))),
        "{menu:?}"
    );
}

#[test]
fn more_needs_and_managers_than_fit_are_counted_on_one_line_each() {
    let mut rows: Vec<SessionRow> = (0..13).map(|id| row(id, "zsh", Some("look"))).collect();
    for id in 0..9 {
        let sid = format!("s-m{id}");
        rows.push(SessionRow {
            id: 100 + id,
            sid: sid.clone(),
            title: format!("m{id}"),
            ..SessionRow::default()
        });
        rows.push(worker(200 + id, "w", &sid, TaskState::Working, 0));
    }
    let g = local_glance(&rows, &[], 0, None);
    let menu = compose_status_menu(&g, 0);
    let overviews = menu
        .iter()
        .filter(|r| {
            matches!(
                r,
                StatusRow::Action {
                    action: StatusAction::Overview,
                    ..
                }
            )
        })
        .count();
    assert_eq!(overviews, 1, "the footer's act alone opens the Overview");
    for (more, of) in [
        (13 - NEEDS_SHOWN, "needs"),
        (9 - MANAGERS_SHOWN, "managers"),
    ] {
        assert_eq!(
            menu.iter()
                .filter(|r| **r == more_in_overview(more))
                .count(),
            1,
            "{of}: {menu:?}"
        );
    }
    let need_rows = menu
        .iter()
        .filter(|r| {
            matches!(
                r,
                StatusRow::Action {
                    action: StatusAction::Go(_),
                    ..
                }
            )
        })
        .count();
    assert_eq!(need_rows, NEEDS_SHOWN);
    let managers = menu
        .iter()
        .filter(|r| matches!(r, StatusRow::Submenu { .. }))
        .count();
    assert_eq!(managers, MANAGERS_SHOWN);
}

#[test]
fn done_for_you_lists_what_nobody_here_waits_on() {
    let manager = SessionRow {
        id: 1,
        sid: "s-manager".to_string(),
        title: "lab-sweep".to_string(),
        ..SessionRow::default()
    };
    let mut seen = worker(5, "seen", crate::task_record::FROM_CTL, TaskState::Done, 5);
    seen.task.as_mut().unwrap().seen = true;
    let g = local_glance(
        &[
            manager,
            worker(2, "managed", "s-manager", TaskState::Done, 5),
            worker(3, "orphan", "s-gone", TaskState::Done, 5),
            worker(4, "bare", crate::task_record::FROM_CTL, TaskState::Done, 0),
            seen,
            worker(
                6,
                "busy",
                crate::task_record::FROM_CTL,
                TaskState::Working,
                0,
            ),
        ],
        &[],
        0,
        None,
    );
    let labels: Vec<&str> = g.done.iter().map(|d| d.label.as_str()).collect();
    assert_eq!(labels, vec!["orphan", "bare"]);
    assert!(matches!(g.done[0].at, DoneAt::Read(_)));
    assert!(matches!(g.done[1].at, DoneAt::Go(_)));
    assert!(g.managers.iter().any(|m| m.go.is_none()));
}

/// DONE FOR YOU HOLDS BOTH OUTCOMES, THE FAILED ONES MARKED: a worker nobody waits on that
/// could not finish, and one whose agent left before landing, list beside what finished, with
/// their words. A stall keeps its need instead, a credit claim's worker is the claim's, and a
/// landing an exit followed reads finished. This Mac counts the failed.
#[test]
fn done_for_you_holds_both_outcomes_with_the_failed_rows_marked() {
    use crate::task_record::{FROM_CTL, WHY_COULD_NOT_FINISH, WHY_EXIT, WHY_LANDED};
    let with_why = |mut row: SessionRow, why| {
        row.task.as_mut().unwrap().why = Some(why);
        row
    };
    let mut claim = worker(6, "claim", FROM_CTL, TaskState::Failed, 0);
    claim.task.as_mut().unwrap().own_report = true;
    // A worker with its own report that landed, its end unseen: still only its own report.
    let mut disk = finished(with_why(
        worker(8, "disk", FROM_CTL, TaskState::Done, 0),
        WHY_LANDED,
    ));
    disk.task.as_mut().unwrap().own_report = true;
    disk.landed = true;
    disk.end = Some(EndRow {
        serial: 1,
        since_unix_ms: 0,
        seen: false,
    });
    let g = local_glance(
        &[
            with_why(
                worker(2, "stuck", FROM_CTL, TaskState::Failed, 0),
                WHY_COULD_NOT_FINISH,
            ),
            with_why(worker(3, "left", FROM_CTL, TaskState::Exited, 0), WHY_EXIT),
            finished(with_why(
                worker(4, "landed-left", FROM_CTL, TaskState::Exited, 0),
                WHY_LANDED,
            )),
            worker(5, "frozen", FROM_CTL, TaskState::Stalled, 0),
            claim,
            worker(7, "fine", FROM_CTL, TaskState::Done, 0),
            disk,
        ],
        &[],
        0,
        None,
    );
    let rows: Vec<(&str, Option<&str>)> = g
        .done
        .iter()
        .map(|d| (d.label.as_str(), d.failed.as_deref()))
        .collect();
    for row in [
        ("stuck", Some("it said it could not finish")),
        ("left", Some("its agent exited before landing")),
        ("landed-left", None),
        ("fine", None),
    ] {
        assert!(rows.contains(&row), "{row:?}: {rows:?}");
    }
    assert_eq!(rows.len(), 4, "{rows:?}");
    let line =
        |label: &str| done_line(g.done.iter().find(|d| d.label == label).expect("listed"), 0);
    assert!(line("stuck").starts_with(FAILED_GLYPH), "{}", line("stuck"));
    assert!(
        line("stuck").contains("failed: it said"),
        "{}",
        line("stuck")
    );
    assert!(line("fine").starts_with('\u{2713}'), "{}", line("fine"));
    assert!(g.here.failed >= 1, "{:?}", g.here);
    assert!(
        here_line(&g.here, g.machine.as_deref()).contains("failed"),
        "{}",
        here_line(&g.here, g.machine.as_deref())
    );
}

/// A MANAGER'S WORKERS ROW SAYS HOW IT ENDED: its own glyph and the why, a landing an exit
/// followed keeping the finished glyph.
#[test]
fn a_workers_row_says_how_it_ended() {
    use crate::task_record::{WHY_COULD_NOT_FINISH, WHY_EXIT, WHY_LANDED};
    let manager = SessionRow {
        id: 1,
        sid: "s-manager".to_string(),
        title: "lead".to_string(),
        ..SessionRow::default()
    };
    let with_why = |mut row: SessionRow, why| {
        row.task.as_mut().unwrap().why = Some(why);
        row
    };
    let g = local_glance(
        &[
            manager,
            with_why(
                worker(2, "stuck", "s-manager", TaskState::Failed, 0),
                WHY_COULD_NOT_FINISH,
            ),
            with_why(
                worker(3, "left", "s-manager", TaskState::Exited, 0),
                WHY_EXIT,
            ),
            finished(with_why(
                worker(4, "landed", "s-manager", TaskState::Exited, 0),
                WHY_LANDED,
            )),
        ],
        &[],
        0,
        None,
    );
    let rows = &g.managers[0].workers;
    let line = |label: &str| worker_line(rows.iter().find(|w| w.label == label).expect("a row"), 0);
    assert!(line("stuck").starts_with(FAILED_GLYPH), "{}", line("stuck"));
    assert!(
        line("stuck").contains("failed: it said it could not finish"),
        "{}",
        line("stuck")
    );
    assert!(line("left").starts_with(FAILED_GLYPH), "{}", line("left"));
    assert!(line("landed").starts_with('\u{2713}'), "{}", line("landed"));
}

fn ended(id: u64, title: &str, at_unix: i64, seen: bool) -> SessionRow {
    SessionRow {
        id,
        sid: format!("s-{id:020}"),
        title: title.to_string(),
        agent: "idle",
        end: Some(EndRow {
            serial: 1,
            since_unix_ms: unix_ms(at_unix),
            seen,
        }),
        ..SessionRow::default()
    }
}

fn done_lines(g: &Glance, now_unix: i64) -> Vec<String> {
    worded(menu_lines(&compose_status_menu(g, now_unix), &[]))
        .into_iter()
        .filter(|l| l.starts_with("statusmenu \"Done for you") || l.contains("\u{2713} "))
        .collect()
}

#[test]
fn an_unseen_end_is_one_go_row_worded_landed() {
    let now = 1_000_000;
    let rows = [
        ended(1, "Calc.py small tested module", now - 41 * 60, false),
        ended(2, "proj-api refactor", now - 3 * 60, false),
        ended(3, "looked at", now - 60, true),
    ];
    let g = local_glance(&rows, &[], now, None);
    let labels: Vec<&str> = g.done.iter().map(|d| d.label.as_str()).collect();
    assert_eq!(
        labels,
        ["proj-api refactor", "Calc.py small tested module"],
        "newest first; the seen end has none"
    );
    let goes: Vec<u64> = g
        .done
        .iter()
        .map(|d| match d.at {
            DoneAt::Go(at) => g.go[usize::from(at)],
            DoneAt::Read(_) => panic!("an end has no result: {d:?}"),
        })
        .collect();
    assert_eq!(goes, [2, 1], "a click goes to the session");
    assert!(g.done.iter().all(|d| d.result_bytes == 0));
    assert_eq!(
        done_lines(&g, now),
        [
            "statusmenu \"Done for you (2)\" enabled=no",
            "statusmenu \"\u{2713} proj-api refactor \u{2014} landed 3m\" enabled=yes checked=no",
            "statusmenu \"\u{2713} Calc.py small tested module \u{2014} landed 41m\" enabled=yes checked=no",
        ]
    );
    assert_eq!(g.button_title(), "\u{276f}");
    let mut unseen = rows.clone();
    unseen[2].end.as_mut().expect("an end").seen = false;
    assert_eq!(local_glance(&unseen, &[], now, None).done.len(), 3);
}

#[test]
fn a_worker_whose_task_row_stands_gets_one_row() {
    let end = Some(EndRow {
        serial: 1,
        since_unix_ms: 2_000_000,
        seen: false,
    });
    let manager = SessionRow {
        id: 1,
        sid: "s-manager".to_string(),
        title: "lab-sweep".to_string(),
        ..SessionRow::default()
    };
    let mut read = worker(
        2,
        "fleet summary",
        crate::task_record::FROM_CTL,
        TaskState::Done,
        1434,
    );
    read.end = end;
    let mut bare = worker(3, "bare", crate::task_record::FROM_CTL, TaskState::Done, 0);
    bare.end = end;
    let mut managed = worker(4, "managed", "s-manager", TaskState::Working, 0);
    managed.end = end;
    let g = local_glance(&[manager, read, bare, managed.clone()], &[], 2_000, None);
    let rows: Vec<(&str, bool)> = g
        .done
        .iter()
        .map(|d| (d.label.as_str(), matches!(d.at, DoneAt::Read(_))))
        .collect();
    assert_eq!(rows, [("fleet summary", true), ("bare", false)], "one each");
    assert_eq!(
        done_lines(&g, 2_000)[1],
        "statusmenu \"\u{2713} fleet summary \u{2014} landed 16m \u{b7} 1.4 KB\" enabled=yes checked=no",
        "the task's own row, worded as its landing"
    );
    assert!(
        g.managers
            .iter()
            .any(|m| m.workers.iter().any(|w| w.label == "managed")),
        "the managed worker's row is under its manager"
    );
    let g = local_glance(&[managed], &[], 2_000, None);
    assert_eq!(g.done.len(), 1);
    assert!(matches!(g.done[0].at, DoneAt::Go(_)));
}

#[test]
fn a_landed_workers_one_row_says_landed() {
    let with_end = |end: Option<bool>| {
        let mut read = worker(
            2,
            "fleet summary",
            crate::task_record::FROM_CTL,
            TaskState::Done,
            1434,
        );
        read.end = end.map(|seen| EndRow {
            serial: 1,
            since_unix_ms: 2_000_000,
            seen,
        });
        local_glance(&[read], &[], 2_000, None)
    };
    for seen in [false, true] {
        let g = with_end(Some(seen));
        assert_eq!(g.done.len(), 1, "one row: {:?}", g.done);
        assert!(matches!(g.done[0].at, DoneAt::Read(_)), "the task's row");
        assert_eq!(
            done_lines(&g, 2_000)[1],
            "statusmenu \"\u{2713} fleet summary \u{2014} landed 16m \u{b7} 1.4 KB\" enabled=yes checked=no",
            "seen={seen}",
        );
    }
    let g = with_end(None);
    assert_eq!(g.done.len(), 1, "the task's row stands: {:?}", g.done);
    assert!(!g.done[0].landed, "no end: done");
}

#[test]
fn the_heading_counts_every_end_and_the_newest_are_shown() {
    let now = 1_000_000;
    let rows: Vec<SessionRow> = (1..=7)
        .map(|id| {
            ended(
                id,
                &format!("t{id}"),
                now - i64::try_from(id).unwrap() * 60,
                false,
            )
        })
        .collect();
    let g = local_glance(&rows, &[], now, None);
    let lines = done_lines(&g, now);
    assert_eq!(lines[0], "statusmenu \"Done for you (7)\" enabled=no");
    let shown: Vec<&str> = lines[1..]
        .iter()
        .map(|l| {
            l.split('\u{2713}')
                .nth(1)
                .unwrap_or_default()
                .split(' ')
                .nth(1)
                .unwrap_or_default()
        })
        .collect();
    assert_eq!(shown, ["t1", "t2", "t3", "t4", "t5"]);
    let mut fewer = rows.clone();
    fewer[0].end.as_mut().expect("an end").seen = true;
    let g = local_glance(&fewer, &[], now, None);
    assert_eq!(
        done_lines(&g, now).last().map(|l| l.contains("t6")),
        Some(true)
    );
}

#[test]
fn finished_work_keeps_the_bar_quiet() {
    let now = 1_000_000;
    let end = ended(1, "proj-api refactor", now - 60, false);
    let g = local_glance(
        &[end.clone(), agent_row(2, "busy", "busy", 1)],
        &[],
        now,
        None,
    );
    assert_eq!(g.button_title(), "\u{276f}");
    assert_eq!(g.need_count(), 0);
    let g = local_glance(
        &[end.clone(), agent_row(2, "rm", "prompt", 1)],
        &[],
        now,
        None,
    );
    assert_eq!(g.button_title(), "\u{276f}", "a need wins the title");
    let mut seen = end;
    seen.end.as_mut().expect("an end").seen = true;
    let g = local_glance(&[seen, agent_row(2, "busy", "busy", 1)], &[], now, None);
    assert_eq!(g.button_title(), "\u{276f}", "CONTROL: seen");
}

#[test]
fn an_end_rows_age_is_composed_at_paint() {
    let now = 1_000_000;
    let rows = [ended(1, "proj-api refactor", now - 3 * 60, false)];
    let g = local_glance(&rows, &[], now, None);
    assert!(done_lines(&g, now)[1].contains("landed 3m"));
    assert!(done_lines(&g, now + 2 * 3600)[1].contains("landed 2h"));
    assert_eq!(
        local_glance(&rows, &[], now + 600, None).fingerprint(),
        g.fingerprint()
    );
    let mut untimed = rows.clone();
    untimed[0].end.as_mut().expect("an end").since_unix_ms = 0;
    let g = local_glance(&untimed, &[], now, None);
    assert_eq!(
        done_lines(&g, now)[1],
        "statusmenu \"\u{2713} proj-api refactor \u{2014} landed\" enabled=yes checked=no"
    );
}

#[test]
fn waiting_is_one_row_per_cause() {
    let wait = |id: u64, word: WaitWord, wall: Option<&'static str>| SessionRow {
        id,
        title: format!("t{id}"),
        agent: "busy",
        waiting: Some(crate::needs::Waiting {
            word,
            wall,
            reset: None,
            entry: None,
        }),
        ..SessionRow::default()
    };
    let mut rows: Vec<SessionRow> = (0..8)
        .map(|id| wait(id, WaitWord::Limit, Some("usage-session")))
        .collect();
    rows.push(wait(8, WaitWord::Api, Some("api-error")));
    rows.push(wait(9, WaitWord::Api, Some("api-error")));
    let g = local_glance(&rows, &[], 0, Some(100));
    assert_eq!(g.waiting.len(), 2);
    assert_eq!(g.waiting[0].tabs.len(), 8);
    assert_eq!(g.waiting[1].since_unix, Some(100));
    assert!(g.needs.is_empty(), "a wait is never a need");
    assert_eq!(g.button_title(), "\u{276f}");
}

#[test]
fn a_writers_wait_is_shown_in_its_words() {
    let wait = |id: u64, owner: &str, text: &str| SessionRow {
        id,
        title: format!("tab {id}"),
        agent: "idle",
        waiting: Some(crate::needs::Waiting {
            word: WaitWord::Entry,
            wall: None,
            reset: None,
            entry: Some(crate::needs::WaitEntry {
                owner: owner.to_string(),
                text: text.to_string(),
            }),
        }),
        ..SessionRow::default()
    };
    let mut limited = wait(9, "-", "");
    limited.waiting = Some(crate::needs::Waiting {
        word: WaitWord::Limit,
        wall: Some("usage-session"),
        reset: Some("7pm".to_string()),
        entry: None,
    });
    let rows = [
        wait(4, "ci", "waiting for CI on PR 412"),
        wait(5, "ci", "waiting for CI on PR 413"),
        wait(6, "ci", "waiting for CI on PR 413"),
        wait(7, "-", "back after lunch"),
        limited,
    ];
    let g = local_glance(&rows, &[], 0, None);
    let lines: Vec<String> = g.waiting.iter().map(wait_line).collect();
    assert_eq!(
        lines,
        [
            "\u{29d7} tab 4 \u{2014} waiting for CI on PR 412 (ci)",
            "\u{29d7} 2 tabs \u{2014} waiting for CI on PR 413 (ci)",
            "\u{29d7} tab 7 \u{2014} back after lunch",
            "\u{29d7} 1 waiting out the session usage limit \u{2014} resets 7pm",
        ]
    );
    assert_eq!(g.here.waiting, 5, "a wait is never work");
}

#[test]
fn tags_round_trip_and_retired_tags_are_inert() {
    let p = Pick {
        generation: 513,
        index: 65_535,
    };
    let every = [
        StatusAction::DismissExceptions,
        StatusAction::Overview,
        StatusAction::ReadThisMac,
        StatusAction::HandBackAll,
        StatusAction::ShowException(41),
        StatusAction::Go(p),
        StatusAction::ReadResult(p),
        StatusAction::RaiseSibling(p),
        StatusAction::ReadFleet,
        StatusAction::ReadFleetNow,
        StatusAction::PeerRead(p),
        StatusAction::PeerScreen(p),
        StatusAction::PeerSsh(p),
        StatusAction::PeerTakeOver(p),
        StatusAction::PeerHandBack(p),
        StatusAction::MacRead(p),
        StatusAction::MacScreen(p),
        StatusAction::MacSsh(p),
        StatusAction::OpenSystemState,
        StatusAction::RotateFleetAccess { repeated: false },
        StatusAction::RotateFleetAccess { repeated: true },
        StatusAction::SetFleetAccess(FleetAccess::ReadOnly),
        StatusAction::SetFleetAccess(FleetAccess::ReadWrite),
        StatusAction::SetFleetAccess(FleetAccess::Full),
    ];
    for a in every {
        assert_ne!(a.tag(), 0, "{a:?}");
        assert_eq!(StatusAction::from_tag(a.tag()), Some(a));
    }
    let tags: std::collections::HashSet<isize> = every.iter().map(|a| a.tag()).collect();
    assert_eq!(tags.len(), every.len(), "no two actions share a tag");
    for retired in [
        1,
        2,
        3,
        4,
        TAG_BAND + 7,
        2 * TAG_BAND + 3,
        3 * TAG_BAND + 4242,
    ] {
        assert_eq!(StatusAction::from_tag(retired), None, "{retired}");
    }
    assert_eq!(StatusAction::from_tag(0), None);
    assert_eq!(StatusAction::from_tag(99), None);
    assert_eq!(StatusAction::from_tag(99 * TAG_BAND + 5), None);
    assert_eq!(StatusAction::ShowException(u64::MAX).tag(), 0);
    assert_eq!(StatusAction::from_tag(5 * TAG_BAND + (1 << 33)), None);
    assert_eq!(
        StatusAction::from_tag(16 * TAG_BAND + 3),
        None,
        "a level past Full Access is no level"
    );
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
fn fleet_access_leads_the_menu_in_a_fleet_with_the_level_in_force_checked() {
    let now = 10_000_i64;
    let slot = fleet_slot(Vec::new(), u64::try_from((now - 20) * 1000).unwrap());
    let mut g = Glance::build(&[], &[], Some(&slot), unix_ms(now), None);
    assert!(g.in_fleet());
    let levels = || -> Vec<Shape> {
        FleetAccess::ALL
            .into_iter()
            .map(|level| Shape::Act(StatusAction::SetFleetAccess(level)))
            .collect()
    };
    for level in FleetAccess::ALL {
        g.access = level;
        // A raise that has not taken effect is the icon's, never the checkmark's.
        g.access_shown = FleetAccess::Full;
        let menu = compose_status_menu(&g, now);
        let Some(StatusRow::Submenu { rows, .. }) = menu.first() else {
            panic!("Fleet Access first: {menu:?}");
        };
        let checked: Vec<(StatusAction, bool, bool)> = rows
            .iter()
            .map(|row| match row {
                StatusRow::Action {
                    action,
                    enabled: true,
                    checked,
                    tip,
                    ..
                } => (*action, *checked, tip.is_some()),
                other => panic!("a level: {other:?}"),
            })
            .collect();
        assert_eq!(
            checked,
            FleetAccess::ALL
                .map(|l| (StatusAction::SetFleetAccess(l), l == level, true))
                .to_vec(),
            "each level a pick with a hover, the one in force checked"
        );
        // The empty desk in a fleet: the access block in front of what it was.
        let mut want = vec![
            Shape::Sub(levels()),
            Shape::Sep,
            Shape::Info,
            Shape::Sep,
            Shape::Fleet,
        ];
        want.extend(acts());
        assert_eq!(shape(&menu), want);
    }
    let hovers: std::collections::BTreeSet<&str> =
        FleetAccess::ALL.into_iter().map(access_tip).collect();
    assert_eq!(hovers.len(), 3, "each level's hover is its own");

    // Out of a fleet no level is offered, and the footer is the same.
    let mut desk = vec![Shape::Info];
    desk.extend(acts());
    assert_eq!(
        shape(&compose_status_menu(&local_glance(&[], &[], 0, None), 0)),
        desk
    );
    let mut alone = slot.clone();
    alone.book.in_fleet = false;
    let g = Glance::build(&[], &[], Some(&alone), unix_ms(now), None);
    let mut want = vec![Shape::Info, Shape::Sep, Shape::Info];
    want.extend(acts());
    assert_eq!(shape(&compose_status_menu(&g, now)), want);
}

#[test]
fn the_degrade_rows_are_the_root_menus_and_never_the_fleet_submenus() {
    let now = 10_000_i64;
    let mut slot = fleet_slot(
        vec![mac("lab-mini", 0, 1, false)],
        u64::try_from((now - 20) * 1000).unwrap(),
    );
    slot.error = Some("no answer in 10 s: killed".to_string());
    slot.book.issuer_label = Some("studio".to_string());
    slot.book.unbound_issuer = Some("studio".to_string());
    let g = Glance::build(&[], &[], Some(&slot), unix_ms(now), None);
    let FleetLine::InFleet {
        member_of,
        unbound_issuer,
        failed,
        ..
    } = &g.fleet
    else {
        panic!("in a fleet: {:?}", g.fleet);
    };
    let degrade = degrade_rows(
        (member_of.as_deref(), unbound_issuer.as_deref()),
        failed.as_deref(),
    );
    assert_eq!(degrade.len(), 3, "PRECONDITION: every degrade row stands");
    let root = compose_status_menu(&g, now);
    let at = root
        .iter()
        .position(|r| matches!(r, StatusRow::FleetSubmenu { .. }))
        .expect("the Fleet row");
    assert_eq!(
        &root[at + 1..=at + degrade.len()],
        degrade.as_slice(),
        "under the Fleet row"
    );
    let fleet = compose_fleet_menu(&slot, now);
    assert!(
        degrade.iter().all(|row| !fleet.contains(row)),
        "printed once: {fleet:?}"
    );
}

#[test]
fn the_face_is_the_caret_and_in_a_fleet_the_lock_at_the_level_it_shows() {
    use FleetAccess::{Full, ReadOnly, ReadWrite};
    let caret = "\u{276f}";
    let slot = fleet_slot(Vec::new(), 0);
    let mut g = Glance::build(&[], &[], Some(&slot), 0, None);
    let rest = g.button_face();
    assert_eq!(
        rest,
        ButtonFace {
            title: caret.to_string(),
            lock: Some(ReadOnly),
        }
    );
    let (tip, label, print) = (g.tooltip(), g.accessibility_label(), g.fingerprint());
    assert_ne!(tip, "aterm", "in a fleet the hover says the level");

    // A click's raise: the lock shows it and the title names it while it is fresh.
    g.access_shown = ReadWrite;
    g.access_flash = Some(ReadWrite);
    let fresh = g.button_face();
    assert_eq!(fresh.lock, Some(ReadWrite));
    assert!(
        fresh.title.starts_with(caret) && fresh.title.len() > caret.len(),
        "{:?}",
        fresh.title
    );
    assert_eq!(g.button_title(), caret, "the stable entry stays the caret");
    assert_ne!(g.fingerprint(), print, "so the item repaints");
    assert_ne!(g.tooltip(), tip, "the hover says what is not in effect yet");
    assert_ne!(g.accessibility_label(), label);
    assert_eq!(button_face_change(Some(&rest), &g), Some(fresh.clone()));
    assert_eq!(button_face_change(Some(&fresh), &g), None);

    // The name goes and the lock stays.
    g.access = ReadWrite;
    g.access_flash = None;
    assert_eq!(
        button_face_change(Some(&fresh), &g),
        Some(ButtonFace {
            title: caret.to_string(),
            lock: Some(ReadWrite),
        })
    );
    g.access = Full;
    g.access_shown = Full;
    assert_eq!(g.button_face().lock, Some(Full));

    // Out of a fleet the level means nothing: the caret alone, whatever it is.
    let mut out = local_glance(&[], &[], 0, None);
    out.access = Full;
    out.access_shown = Full;
    out.access_flash = Some(Full);
    assert_eq!(
        out.button_face(),
        ButtonFace {
            title: caret.to_string(),
            lock: None,
        }
    );
    assert_eq!(out.tooltip(), "aterm");
    assert_eq!(out.accessibility_label(), "aterm");
}

/// A fleet Mac below Full Access says so on its row; one at Full Access, and one whose
/// level was not read, say nothing.
#[test]
fn a_fleet_mac_below_full_access_says_so_on_its_row() {
    use FleetAccess::{Full, ReadOnly, ReadWrite};
    let label = |access: Option<FleetAccess>| {
        let mac = crate::fleet_glance::FleetMac {
            access,
            ..mac("lab-mini", 0, 1, false)
        };
        match mac_row(&mac, pick(0), None, 0) {
            StatusRow::Submenu { label, .. } => label,
            other => panic!("a Mac's row: {other:?}"),
        }
    };
    let plain = label(None);
    assert_eq!(label(Some(Full)), plain);
    let (read, write) = (label(Some(ReadOnly)), label(Some(ReadWrite)));
    assert!(
        read.starts_with(&plain) && read.len() > plain.len(),
        "{read}"
    );
    assert!(
        write.starts_with(&plain) && write.len() > plain.len(),
        "{write}"
    );
    assert_ne!(read, write);
}

#[test]
fn the_fingerprint_is_unmoved_by_ages_alone() {
    let rows = |since: i64, ms: u64| {
        let mut asked = row(1, "zsh", Some("look"));
        asked.need_since = Some(since);
        let mut held = agent_row(2, "bench", "busy", 1);
        held.held = Some(HoldWord::Local);
        held.held_since = Some(since);
        let mut w = worker(3, "lint", "s-gone", TaskState::Done, 9);
        w.task.as_mut().unwrap().since_unix_ms = ms;
        vec![asked, held, w]
    };
    let a = local_glance(&rows(100, 1_000), &[], 200, None);
    let b = local_glance(&rows(150, 90_000), &[], 900, None);
    assert_ne!(a, b, "the ages differ");
    assert_eq!(a.fingerprint(), b.fingerprint());
    let mut more = rows(100, 1_000);
    more.push(row(4, "vim", Some("another")));
    assert_ne!(
        a.fingerprint(),
        local_glance(&more, &[], 200, None).fingerprint()
    );
    let mut regen = a.clone();
    regen.generation = 9;
    assert_eq!(a.fingerprint(), regen.fingerprint());
}

/// THE MACHINE'S TITLE LEADS THE TOOLTIP AND THIS MAC'S ROW (machine names §5.5), moves the
/// fingerprint so a rename reaches the item, and leaves the face alone; before a read, both
/// keep their plain words.
#[test]
fn the_machine_title_leads_the_tooltip_and_this_mac_row() {
    let plain = local_glance(&[row(1, "zsh", None)], &[], 0, None);
    assert_eq!(plain.tooltip(), "aterm");
    assert_eq!(
        here_line(&plain.here, plain.machine.as_deref()),
        "This Mac \u{2014} 1 shell"
    );
    let mut named = plain.clone();
    named.machine = Some("m8 Heron".to_owned());
    assert_eq!(named.tooltip(), "aterm \u{b7} m8 Heron");
    assert_eq!(
        here_line(&named.here, named.machine.as_deref()),
        "m8 Heron \u{2014} 1 shell"
    );
    assert_eq!(
        here_line(&Here::default(), Some("m0 Maize")),
        "m0 Maize \u{2014} no sessions"
    );
    assert!(
        compose_status_menu(&named, 0)
            .contains(&StatusRow::Info("m8 Heron \u{2014} 1 shell".to_owned())),
        "the menu's row carries the title"
    );
    assert_ne!(plain.fingerprint(), named.fingerprint());
    assert_eq!(
        named.button_title(),
        plain.button_title(),
        "the face stays ❯"
    );
}

#[test]
fn diagnostics_do_not_change_the_menu_bar_face() {
    let calm = local_glance(&[row(1, "zsh", None)], &[], 0, None);
    let face = calm.button_face();
    assert_eq!(
        face,
        ButtonFace {
            title: "\u{276f}".to_string(),
            lock: None,
        },
        "out of a fleet the face is the caret alone"
    );
    assert_eq!(button_face_change(None, &calm), Some(face.clone()));
    for rows in [
        vec![row(1, "zsh", Some("look"))],
        vec![agent_row(1, "w", "wall:auth", 1)],
    ] {
        let mut diagnostic = local_glance(&rows, &[], 0, None);
        diagnostic.exceptions.push((11, "recorded error".into()));
        assert_eq!(button_face_change(Some(&face), &diagnostic), None);
        assert_eq!(diagnostic.tooltip(), "aterm");
    }
}

/// Two Grok sessions on a sign-in wall while this machine is signed out are one need on
/// the menu (the machine's one sign-in), where two Claude sign-in walls stay two.
#[test]
fn two_grok_sign_in_walls_are_one_menu_need() {
    let grok = |id: u64, rev: u64| {
        row_of(
            id,
            "grok",
            &NeedFacts {
                grok_signin: Some(crate::needs::GrokSignin::Idle),
                ..agent_facts("wall:auth", rev)
            },
        )
    };
    let g = local_glance(&[grok(1, 3), grok(2, 4)], &[], 0, None);
    assert_eq!(g.needs.len(), 1, "{:?}", g.needs);
    assert_eq!(g.needs[0].what, crate::needs::GROK_SIGNIN_WHAT);
    assert_eq!(g.needs[0].label, grok(1, 3).named());
    // CONTROL: Claude's sign-in walls are each their session's.
    let g = local_glance(
        &[
            agent_row(1, "a", "wall:auth", 3),
            agent_row(2, "b", "wall:auth", 4),
        ],
        &[],
        0,
        None,
    );
    assert_eq!(g.needs.len(), 2, "{:?}", g.needs);
}

#[test]
fn the_tab_name_drops_status_glyphs_and_a_title_raises_nothing() {
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
    let g = local_glance(
        &[
            row(9, "\u{26a0} needs human: approval", None),
            row(8, "Operator: fleet idle", None),
        ],
        &[],
        0,
        None,
    );
    assert!(g.needs.is_empty());
    let g = local_glance(
        &[row(9, "\u{26a0} needs human", Some("approval"))],
        &[],
        0,
        None,
    );
    assert_eq!(g.local_need_sessions(), vec![9]);
}

#[test]
fn the_need_ages_remember_since_when_a_need_and_a_hold_stand() {
    let asked = |key: u64, hold: Option<HoldWord>| SessionRow {
        id: 1,
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
    assert_eq!((h.need_since(1), h.held_since(1)), (Some(100), Some(160)));
    assert!(h.observe(1, Some(&asked(8, Some(HoldWord::Local))), 200));
    assert_eq!((h.need_since(1), h.held_since(1)), (Some(200), Some(160)));
    assert!(h.observe(1, Some(&asked(8, Some(HoldWord::Person))), 300));
    assert_eq!(h.held_since(1), Some(300), "another hold is a new one");
    let mut answered = asked(8, Some(HoldWord::Person));
    answered.agent = "prompt";
    assert!(!h.observe(1, Some(&answered), 310), "one class");
    assert!(h.observe(1, None, 400));
    assert_eq!((h.need_since(1), h.held_since(1)), (None, None));
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
    let boxed = agent_row(4, "builder", "prompt", 5);
    let _ = before.observe(4, Some(&boxed), 100);
    let carry = StatusCarry::parse(&before.carry_of(4).row().expect("a row"));
    let reboxed = agent_row(9, "builder", "prompt", 1);
    let successor = |carry: StatusCarry, at: i64| {
        let mut h = NeedAges::default();
        h.adopt(9, carry, 1_000);
        let _ = h.observe(9, Some(&reboxed), at);
        h
    };
    let mut h = successor(carry.clone(), 1_010);
    assert_eq!(h.need_since(9), Some(100), "its age carried");
    let _ = h.observe(9, Some(&agent_row(9, "builder", "prompt", 2)), 1_020);
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
        let _ = h.observe(9, Some(&agent_row(9, "builder", verdict, 1)), 1_010);
        h.need_since(9)
    };
    assert_eq!(
        agent_row(9, "b", "wall:auth", 1).need.map(|n| n.word),
        Some(NeedWord::Login),
        "PRECONDITION"
    );
    assert_eq!(
        agent_row(9, "b", "wall:memory", 1).need.map(|n| n.word),
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

#[test]
fn the_menu_lists_the_digest_and_offers_to_file_it() {
    let mut g = local_glance(&[], &[], 0, None);
    g.exceptions = (0..7).map(|n| (n, format!("thing {n}"))).collect();
    let menu = compose_status_menu(&g, 0);
    let shown = menu
        .iter()
        .filter(|r| {
            matches!(
                r,
                StatusRow::Action {
                    action: StatusAction::ShowException(_),
                    ..
                }
            )
        })
        .count();
    assert_eq!(shown, EXCEPTION_LINES_SHOWN);
    assert_eq!(
        menu.iter()
            .filter(|r| matches!(
                r,
                StatusRow::Action {
                    action: StatusAction::DismissExceptions,
                    ..
                }
            ))
            .count(),
        1
    );
    assert_ne!(
        g.fingerprint(),
        local_glance(&[], &[], 0, None).fingerprint()
    );
}

fn fleet_slot(
    macs: Vec<crate::fleet_glance::FleetMac>,
    read_at_ms: u64,
) -> crate::fleet_glance::FleetSlot {
    let macs = macs
        .into_iter()
        .map(|mac| crate::fleet_glance::FleetMac { read_at_ms, ..mac })
        .collect();
    crate::fleet_glance::FleetSlot {
        generation: 3,
        snapshot: Some(std::sync::Arc::new(crate::fleet_glance::FleetSnapshot {
            read_at_ms,
            macs,
            unanswered: vec![crate::fleet_glance::Unanswered {
                name: "mac-22".to_string(),
                dial: "mac-22".to_string(),
                why: "no connection in 3 s".to_string(),
                ..crate::fleet_glance::Unanswered::default()
            }],
            seats: None,
        })),
        book: crate::fleet_glance::BookFacts {
            in_fleet: true,
            minted: true,
            issuer_label: None,
            unbound_issuer: None,
        },
        ..crate::fleet_glance::FleetSlot::default()
    }
}

fn mac(label: &str, needs: usize, working: usize, older: bool) -> crate::fleet_glance::FleetMac {
    crate::fleet_glance::FleetMac {
        name: label.to_string(),
        dial: label.to_string(),
        host: crate::fleet_glance::host_word(label),
        older,
        sessions: Some(needs + working),
        activity: (!older).then(|| crate::fleet_glance::AgentTally {
            agents: u16::try_from(needs + working).expect("a few agents"),
            busy: u16::try_from(working).expect("a few agents"),
            unknown: u16::try_from(needs).expect("a few agents"),
            ..crate::fleet_glance::AgentTally::default()
        }),
        needs: (0..needs)
            .map(|n| crate::fleet_glance::FleetNeed {
                sid: format!("s-{n}"),
                word: NeedWord::Stalled,
                who: "claude".to_string(),
                worker_of: Some("s-1e91".to_string()),
                age_ms: Some(60_000),
                what: None,
                claim: None,
                carries_on: false,
            })
            .collect(),
        ..crate::fleet_glance::FleetMac::default()
    }
}

/// A Mac whose agents only wait or sit at a limit is listed among the active ones, and its
/// label says so, from the same tally the fold reads. Negative control: an idle Mac folds
/// into the quiet Macs.
#[test]
fn a_waiting_or_walled_mac_is_labelled_where_it_is_listed() {
    let with = |label: &str, waiting: u16, walled: u16| crate::fleet_glance::FleetMac {
        activity: Some(crate::fleet_glance::AgentTally {
            agents: 1 + waiting + walled,
            idle: 1,
            waiting,
            walled,
            ..crate::fleet_glance::AgentTally::default()
        }),
        ..mac(label, 0, 0, false)
    };
    let slot = fleet_slot(
        vec![
            with("mac-idle", 0, 0),
            with("mac-walled", 0, 2),
            with("mac-waiting", 1, 0),
            // Its agents were not read: absent, never quiet.
            crate::fleet_glance::FleetMac {
                activity: None,
                ..mac("mac-unread", 0, 0, false)
            },
        ],
        10_000,
    );
    let rows = compose_fleet_menu(&slot, 10);
    let labels: Vec<&str> = rows
        .iter()
        .filter_map(|row| match row {
            StatusRow::Submenu { label, .. } => Some(label.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        labels.contains(&"mac-walled \u{2014} 2 at a limit"),
        "{labels:?}"
    );
    assert!(
        labels.contains(&"mac-waiting \u{2014} 1 waiting"),
        "{labels:?}"
    );
    assert!(labels.contains(&"1 quiet Mac"), "{labels:?}");
    assert!(
        labels.contains(&"mac-unread \u{2014} agents not read"),
        "{labels:?}"
    );
    assert!(
        !labels.iter().any(|l| l.starts_with("mac-idle")),
        "{labels:?}"
    );
}

#[test]
fn a_partly_read_fleet_mac_is_not_folded_among_quiet_macs() {
    for (instances, needs) in [(1, "-"), (2, "-"), (2, "ask")] {
        let wire = aterm_json::json!([{
            "label":"m7.local", "fleet":true,
            "instance":{
                "node":"peer-node", "host":"m7.local", "level":"facts",
                "sessions":"1", "instances":instances.to_string()
            },
            "sessions":[{
                "sid":"s-peer", "state":"alive", "program":"sh",
                "program_kind":"shell", "needs":needs
            }]
        }]);
        let snapshot = crate::fleet_glance::parse(&wire.to_string(), None, 10_000)
            .expect("a peer's reported instance coverage");
        let slot = crate::fleet_glance::FleetSlot {
            generation: 3,
            snapshot: Some(std::sync::Arc::new(snapshot)),
            book: crate::fleet_glance::BookFacts {
                in_fleet: true,
                ..crate::fleet_glance::BookFacts::default()
            },
            ..crate::fleet_glance::FleetSlot::default()
        };
        let rows = compose_fleet_menu(&slot, 10);
        // A directly listed Mac has its own read action. A quiet group holds
        // another submenu layer, so this tests coverage grouping, not prose.
        let directly_listed = rows.iter().any(|row| {
            matches!(row, StatusRow::Submenu { rows, .. }
                if rows.iter().any(|row|
                    matches!(row, StatusRow::Action { action: StatusAction::MacRead(_), .. })))
        });
        assert_eq!(directly_listed, instances > 1 || needs == "ask", "{rows:?}");
        let glance = Glance::build(&[], &[], Some(&slot), 10_000, None);
        assert_eq!(glance.need_count(), usize::from(needs == "ask"));
    }
}

#[test]
fn counted_needs_are_shown_before_an_old_reads() {
    let now = 10_000_i64;
    let read_ms = |ago: i64| u64::try_from((now - ago) * 1000).unwrap();
    let mut slot = fleet_slot(
        vec![mac("mac-old", 3, 0, false), mac("mac-17", 1, 0, false)],
        read_ms(20),
    );
    let mut snapshot = (**slot.snapshot.as_ref().expect("a read")).clone();
    snapshot.macs[0].read_at_ms = read_ms(301);
    slot.snapshot = Some(std::sync::Arc::new(snapshot));
    let rows: Vec<SessionRow> = (0..9).map(|id| row(id, "zsh", Some("look"))).collect();
    let g = Glance::build(&rows, &[], Some(&slot), unix_ms(now), None);
    assert_eq!((g.needs.len(), g.need_count()), (13, 10));
    assert!(g.needs[..NEEDS_SHOWN].iter().all(|n| n.counted));
    let menu = compose_status_menu(&g, now);
    // The Needs you section's rows, under its heading.
    let first = menu
        .iter()
        .position(|r| matches!(r, StatusRow::Info(_)))
        .expect("the section's heading")
        + 1;
    let more = more_in_overview(g.needs.len() - NEEDS_SHOWN);
    let kinds: Vec<&str> = menu[first..=first + NEEDS_SHOWN]
        .iter()
        .map(|r| match r {
            StatusRow::Submenu { .. } => "fleet",
            StatusRow::Action {
                action: StatusAction::Go(_),
                ..
            } => "go",
            row if *row == more => "more",
            _ => "other",
        })
        .collect();
    let mut expected = vec!["fleet"];
    expected.extend(["go"; 9]);
    expected.push("more");
    assert_eq!(kinds, expected);
}

#[test]
fn fleet_needs_count_only_from_a_fresh_read() {
    let now = 10_000_i64;
    let read_ms = |ago: i64| u64::try_from((now - ago) * 1000).unwrap();
    let slot = fleet_slot(
        vec![mac("mac-17", 1, 0, false), mac("mac-old", 0, 1, true)],
        read_ms(20),
    );
    let g = Glance::build(&[], &[], Some(&slot), unix_ms(now), None);
    assert_eq!(g.needs.len(), 1);
    assert_eq!((g.need_count(), g.stuck()), (1, true));
    assert_eq!(g.button_title(), "\u{276f}");
    assert!(g.in_fleet());
    let stale = fleet_slot(vec![mac("mac-17", 1, 0, false)], read_ms(301));
    let g_stale = Glance::build(&[], &[], Some(&stale), unix_ms(now), None);
    assert_eq!((g_stale.needs.len(), g_stale.need_count()), (1, 0));
    assert_eq!(g_stale.button_title(), "\u{276f}");
    let mut failed = slot.clone();
    failed.error = Some("no answer in 10 s: killed".to_string());
    let g_failed = Glance::build(&[], &[], Some(&failed), unix_ms(now), None);
    assert_eq!(
        g_failed.button_title(),
        g.button_title(),
        "a failed read never moves it"
    );
    let menu = compose_status_menu(&g, now);
    let subs: Vec<Vec<Shape>> = menu
        .iter()
        .filter_map(|r| match r {
            StatusRow::Submenu { rows, .. } => Some(shape(rows)),
            _ => None,
        })
        .collect();
    assert_eq!(
        subs,
        vec![
            FleetAccess::ALL
                .into_iter()
                .map(|level| Shape::Act(StatusAction::SetFleetAccess(level)))
                .collect(),
            vec![
                Shape::Act(StatusAction::PeerRead(pick(0))),
                Shape::Act(StatusAction::PeerScreen(pick(0))),
                Shape::Act(StatusAction::PeerSsh(pick(0))),
                Shape::Info,
            ],
        ],
        "Fleet Access, then the fleet need's submenu"
    );
    assert!(
        menu.iter()
            .any(|r| matches!(r, StatusRow::FleetSubmenu { .. }))
    );
    // The footer is the same in a fleet and out of one; reading the fleet in full is the
    // Fleet submenu's.
    assert_eq!(shape(&menu[menu.len() - 3..]), acts());
    assert!(!menu.iter().any(|r| matches!(
        r,
        StatusRow::Action {
            action: StatusAction::ReadFleet | StatusAction::ReadFleetNow,
            ..
        }
    )));
    assert_eq!(
        shape(&compose_fleet_menu(&slot, now))
            .rsplitn(2, |row| *row == Shape::Sep)
            .next(),
        Some(
            &[
                Shape::Act(StatusAction::ReadFleetNow),
                Shape::Act(StatusAction::ReadFleet)
            ][..]
        )
    );
    let mut alone = slot.clone();
    alone.book.in_fleet = false;
    let menu = compose_status_menu(
        &Glance::build(&[], &[], Some(&alone), unix_ms(now), None),
        now,
    );
    assert!(
        !menu
            .iter()
            .any(|r| matches!(r, StatusRow::FleetSubmenu { .. }))
    );
    assert_eq!(shape(&menu[menu.len() - 3..]), acts());
    assert!(
        !menu.iter().any(|r| matches!(
            r,
            StatusRow::Action {
                action: StatusAction::SetFleetAccess(_),
                ..
            } | StatusRow::Submenu { .. }
        )),
        "out of a fleet no level is offered: {menu:?}"
    );
}

#[test]
fn a_fleet_need_counts_by_its_own_read_to_the_millisecond() {
    let fresh = u64::try_from(crate::fleet_glance::COUNT_FRESH.as_millis()).unwrap();
    let read_at = 1_000_000_123;
    let slot = fleet_slot(vec![mac("mac-17", 1, 0, false)], read_at);
    let count = |slot: &crate::fleet_glance::FleetSlot, now_ms| {
        Glance::build(&[], &[], Some(slot), now_ms, None).need_count()
    };
    assert_eq!(count(&slot, read_at + fresh), 1, "the control");
    assert_eq!(count(&slot, read_at + fresh + 1), 0);
    let mut carried = fleet_slot(vec![mac("mac-busy", 0, 2, false)], read_at + 60_000);
    if let Some(snap) = carried.snapshot.as_mut() {
        std::sync::Arc::make_mut(snap)
            .macs
            .push(crate::fleet_glance::FleetMac {
                read_at_ms: read_at,
                carried: true,
                ..mac("mac-22", 1, 0, false)
            });
    }
    assert_eq!(count(&carried, read_at + fresh), 1);
    assert_eq!(
        count(&carried, read_at + fresh + 1),
        0,
        "its own read lapsed"
    );
    let g = Glance::build(&[], &[], Some(&carried), read_at + 60_000, None);
    assert!(
        matches!(
            g.fleet,
            FleetLine::InFleet {
                read: Some((1, 2, Some(2), _)),
                ..
            }
        ),
        "mac-busy answered (2 agents) of 2 asked: {:?}",
        g.fleet
    );
}

#[test]
fn the_fleet_submenu_lists_macs_needs_first_and_folds_the_rest() {
    let mut hostless = mac("x y", 0, 2, false);
    hostless.host = None;
    let unproven = |mac| crate::fleet_glance::FleetMac {
        unproven: true,
        ..mac
    };
    let slot = fleet_slot(
        vec![
            mac("mac-busy", 0, 3, false),
            mac("mac-17", 2, 0, false),
            mac("mac-q1", 0, 0, false),
            mac("mac-q2", 0, 0, false),
            unproven(mac("mac-old", 0, 1, true)),
            hostless,
            unproven(mac("mac-key", 3, 0, false)),
        ],
        1000,
    );
    let at = |index: u16| Pick {
        generation: 3,
        index,
    };
    let menu = compose_fleet_menu(&slot, 10);
    let mac_acts = |index: u16, host: bool| {
        let mut acts = vec![Shape::Act(StatusAction::MacRead(at(index)))];
        if host {
            acts.push(Shape::Act(StatusAction::MacScreen(at(index))));
            acts.push(Shape::Act(StatusAction::MacSsh(at(index))));
        }
        acts
    };
    let tail = |rows: &[StatusRow]| match rows {
        [
            ..,
            StatusRow::Separator,
            StatusRow::Action { action: now, .. },
            StatusRow::Action { action: full, .. },
        ] => Some((*now, *full)),
        _ => None,
    };
    let shapes = shape(&menu);
    let firsts: Vec<Vec<Shape>> = shapes
        .iter()
        .take(3)
        .map(|s| match s {
            Shape::Sub(rows) => rows
                .iter()
                .skip_while(|r| !matches!(r, Shape::Sep))
                .skip(1)
                .map(|r| match r {
                    Shape::Act(a) => Shape::Act(*a),
                    _ => Shape::Info,
                })
                .collect(),
            _ => Vec::new(),
        })
        .collect();
    assert_eq!(
        firsts,
        vec![mac_acts(1, true), mac_acts(0, true), mac_acts(5, false)]
    );
    assert!(
        matches!(&menu[3], StatusRow::Submenu { rows, .. } if rows.len() == 2),
        "2 quiet"
    );
    assert!(
        matches!(&menu[4], StatusRow::Submenu { rows, .. } if rows.len() == 1),
        "1 not answering"
    );
    assert!(
        matches!(&menu[5], StatusRow::Submenu { label, rows }
            if rows.len() == 1 && label.contains("key not proven")),
        "1 unproven, though it has the most needs"
    );
    assert!(
        matches!(&menu[6], StatusRow::Submenu { label, rows }
            if rows.len() == 1 && label.contains("older aterm")),
        "1 older"
    );
    assert_eq!(
        tail(&menu),
        Some((StatusAction::ReadFleetNow, StatusAction::ReadFleet))
    );
    let mut unread = slot.clone();
    unread.snapshot = None;
    assert_eq!(
        shape(&compose_fleet_menu(&unread, 10)),
        vec![
            Shape::Info,
            Shape::Sep,
            Shape::Act(StatusAction::ReadFleetNow),
            Shape::Act(StatusAction::ReadFleet),
        ]
    );
}

const NEED_A_CUT: &str = "grant Screen Recording to com.aterm.aterm in System Settings \u{25b8} \
                          Priv\u{2026}";

fn harness_row(id: u64, title: &str, first: &str, since: i64) -> SessionRow {
    report_row(aterm_phase::Program::Claude, id, title, first, since)
}

/// A `NEEDS YOU:` report of `program`'s, as its harness writes the needs entry.
fn report_row(
    program: aterm_phase::Program,
    id: u64,
    title: &str,
    first: &str,
    since: i64,
) -> SessionRow {
    let facts = NeedFacts {
        attention: vec![crate::needs::AttentionFact {
            owner: crate::harness_host::NEEDS_ATTENTION_OWNER.to_string(),
            text: format!(
                "{} needs you: {first} (+1 more) \u{2014} carrying on",
                program.name()
            ),
            kind: Some(aterm_types::needs::AttentionKind::Ask),
        }],
        ..NeedFacts::default()
    };
    SessionRow {
        need_since: Some(since),
        ..row_of(id, title, &facts)
    }
}

/// The composed menu's row whose label starts `head`: its label and its tip.
fn menu_row(rows: &[StatusRow], head: &str) -> Option<(String, Option<String>)> {
    rows.iter().find_map(|r| match r {
        StatusRow::Action { label, tip, .. } if label.starts_with(head) => {
            Some((label.clone(), tip.clone()))
        }
        _ => None,
    })
}

/// STOPPED WORK FIRST (`docs/DESIGN-harness-autopilot-2026-10-15.md` §14.7): a three-hour-old
/// credential report in tab 3 and a two-minute-old approval box in tab 5 are both decide, both
/// `◇`; the box lists first, because it has stopped its agent and the report's agent carries
/// on past it. Among the requests the oldest still leads, and the menu shows them in that order.
#[test]
fn a_stopped_need_lists_before_an_older_report() {
    let now = 100_000_i64;
    let mut rows = a_box_beside_an_older_report(now);
    assert!(
        rows[0].need.as_ref().is_some_and(|n| n.harness.is_some())
            && rows[1].need.as_ref().is_some_and(|n| n.harness.is_none()),
        "PRECONDITION: a report beside a box"
    );
    rows.push(SessionRow {
        need_since: Some(now - 600),
        ..agent_row(6, "lint", "prompt", 1)
    });
    let g = local_glance(&rows, &[], now, None);
    assert!(
        g.needs
            .iter()
            .all(|n| n.word.severity() == Severity::Decide),
        "PRECONDITION: one severity"
    );
    let labels: Vec<&str> = g.needs.iter().map(|n| n.label.as_str()).collect();
    assert_eq!(
        labels,
        ["lint", "build", "proj-api"],
        "the boxes, oldest first, then the older report"
    );
    assert_eq!(g.local_need_sessions(), vec![6, 5, 3]);
    let menu = compose_status_menu(&g, now);
    let shown: Vec<&str> = menu
        .iter()
        .filter_map(|r| match r {
            StatusRow::Action {
                label, indent: 0, ..
            } if label.starts_with('\u{25c7}') => Some(label.as_str()),
            _ => None,
        })
        .collect();
    let want: Vec<String> = g.needs.iter().map(|n| need_line(n, now, 0)).collect();
    assert_eq!(shown, want, "the menu shows them in that order");
}

/// STOPPED WORK FIRST ACROSS ATERMS AND MACS (§14.7): a sibling's or a fleet peer's report,
/// which its facts row says with `needs_by=harness`, lists after this aterm's report as it
/// did before §14.7 (local before sibling before fleet), and every box, wherever it stands,
/// lists before every report. CONTROL: with the row's word not carried (a peer row read as a
/// box), the sibling's older report would list above the sibling's box and this aterm's
/// report.
#[test]
fn a_siblings_and_a_peers_report_list_after_every_box_and_after_this_aterms_report() {
    let now = 100_000_i64;
    let rows = a_box_beside_an_older_report(now);
    let need = |sid: &str, word: NeedWord, age_s: u64, carries_on: bool| SiblingNeed {
        sid: sid.to_string(),
        word,
        label: format!("sib-{sid}"),
        age_ms: Some(age_s * 1000),
        claim: None,
        carries_on,
    };
    let sibling = SiblingInstance {
        pid: 900,
        sock: "/tmp/aterm-900.sock".to_string(),
        sessions: 2,
        needs: vec![
            need("report", NeedWord::Ask, 5 * 3600, true),
            need("box", NeedWord::Approval, 3600, false),
        ],
        scanned_ms: unix_ms(now - 10),
    };
    let peer =
        |sid: &str, word: NeedWord, age_s: u64, carries_on: bool| crate::fleet_glance::FleetNeed {
            sid: sid.to_string(),
            word,
            who: format!("peer-{sid}"),
            worker_of: None,
            age_ms: Some(age_s * 1000),
            what: None,
            claim: None,
            carries_on,
        };
    let slot = fleet_slot(
        vec![crate::fleet_glance::FleetMac {
            name: "mac-17".to_string(),
            dial: "mac-17".to_string(),
            sessions: Some(2),
            activity: Some(crate::fleet_glance::AgentTally {
                agents: 2,
                ..crate::fleet_glance::AgentTally::default()
            }),
            needs: vec![
                peer("report", NeedWord::Ask, 4 * 3600, true),
                peer("box", NeedWord::Approval, 1800, false),
            ],
            ..crate::fleet_glance::FleetMac::default()
        }],
        unix_ms(now - 20),
    );
    let order = |siblings: &[SiblingInstance], slot: &crate::fleet_glance::FleetSlot| {
        Glance::build(&rows, siblings, Some(slot), unix_ms(now), None)
            .needs
            .iter()
            .map(|n| n.label.clone())
            .collect::<Vec<_>>()
    };
    let g = Glance::build(
        &rows,
        std::slice::from_ref(&sibling),
        Some(&slot),
        unix_ms(now),
        None,
    );
    assert!(
        g.needs.iter().all(|n| n.counted),
        "PRECONDITION: every read is fresh"
    );
    assert_eq!(
        order(std::slice::from_ref(&sibling), &slot),
        [
            "build",
            "sib-box",
            "mac-17 \u{b7} peer-box",
            "proj-api",
            "sib-report",
            "mac-17 \u{b7} peer-report",
        ],
        "every box, local before sibling before fleet; then every report, the same"
    );
    let mut read_as_a_box = sibling.clone();
    for need in &mut read_as_a_box.needs {
        need.carries_on = false;
    }
    assert_eq!(
        order(std::slice::from_ref(&read_as_a_box), &slot)[1..3],
        ["sib-report", "sib-box"],
        "CONTROL: a sibling report not said as one leads its own box"
    );
}

/// A report row's tip says its program carries on (§14.7), in that program's own name —
/// never `Claude Code` for agy or Muse Code — and a box's row carries none.
#[test]
fn a_report_row_says_it_carries_on() {
    use aterm_agent::supervise::mode_words::report_words;
    use aterm_phase::Program;
    let now = 100_000_i64;
    for program in [Program::Claude, Program::Agy, Program::Muse] {
        let rows = [
            report_row(program, 3, "proj-api", "sign 0.48.0", now - 3600),
            SessionRow {
                need_since: Some(now - 60),
                ..agent_row(5, "build", "prompt", 1)
            },
        ];
        let menu = compose_status_menu(&local_glance(&rows, &[], now, None), now);
        let (_, tip) = menu_row(&menu, "\u{25c7} proj-api \u{2014} sign 0.48.0")
            .unwrap_or_else(|| panic!("{program:?}: the report's row: {menu:?}"));
        assert_eq!(
            tip.as_deref(),
            Some(
                report_words(
                    program.product(),
                    aterm_agent::harness::hold_said::NeedTail::CarryingOn,
                    false
                )
                .as_str()
            ),
            "{program:?}"
        );
        let tip = tip.unwrap_or_default();
        assert!(tip.starts_with(program.product()), "{tip}");
        assert!(
            tip.ends_with(aterm_agent::supervise::mode_words::CARRIES_ON),
            "{tip}"
        );
        if program != Program::Claude {
            assert!(!tip.contains("Claude Code"), "{tip}");
        }
        let (_, box_tip) = menu_row(&menu, "\u{25c7} build \u{2014} ").expect("the box's row");
        assert_eq!(
            box_tip, None,
            "CONTROL: a box that stopped its agent says nothing so"
        );
        let lines = menu_lines(&menu, &[]);
        assert!(
            lines.iter().any(|l| l.contains("proj-api")
                && l.contains(&format!(
                    " tip={:?}",
                    report_words(
                        program.product(),
                        aterm_agent::harness::hold_said::NeedTail::CarryingOn,
                        false
                    )
                ))),
            "chrome's statusmenu carries the tip: {lines:#?}"
        );
    }
}

/// Where its need is all that holds its Land session's landing ([`SessionRow::need_alone`]),
/// a report row's tip says the landing waits on it alone (§14.7); a landing held by the need
/// and its repository too (the control), or no landing at all, says it carries on.
#[test]
fn a_need_that_alone_holds_a_landing_says_so() {
    use aterm_agent::supervise::mode_words::report_words;
    let now = 100_000_i64;
    let tip_of = |row: SessionRow| {
        let menu = compose_status_menu(&local_glance(&[row], &[], now, None), now);
        menu_row(&menu, "\u{25c7} land-d \u{2014} rotate the deploy key").and_then(|(_, tip)| tip)
    };
    let report = harness_row(7, "land-d", "rotate the deploy key", now - 60);
    let alone = SessionRow {
        landing: true,
        need_alone: true,
        ..report.clone()
    };
    assert_eq!(
        tip_of(alone),
        Some(report_words(
            aterm_phase::Program::Claude.product(),
            aterm_agent::harness::hold_said::NeedTail::CarryingOn,
            true
        ))
    );
    assert_eq!(
        tip_of(SessionRow {
            landing: true,
            need_alone: false,
            ..report.clone()
        }),
        Some(report_words(
            aterm_phase::Program::Claude.product(),
            aterm_agent::harness::hold_said::NeedTail::CarryingOn,
            false
        )),
        "CONTROL: a hold with another leg too carries on meanwhile"
    );
    assert_eq!(
        tip_of(report),
        Some(report_words(
            aterm_phase::Program::Claude.product(),
            aterm_agent::harness::hold_said::NeedTail::CarryingOn,
            false
        ))
    );
}

/// A REPORT WHOSE ENTRY SAYS THE WORK WAITS ON THE PERSON SAYS SO ON EVERY SURFACE (the
/// post-landing review, 2026-11-02): beside the stop for replies that repeat, or a landed task,
/// the entry ends ` — waits on you`, and the menu row's tip and the tab's hover say
/// `<product> waits on you` as the entry and the chip say it, never that it carries on;
/// for Claude Code, agy and Muse Code alike. CONTROL: an entry that carries on says so on
/// both. Negative control: `report_words` reading the alone flag only (`origin/main`)
/// says `carries on meanwhile` beside `waits on you`.
#[test]
fn a_report_that_waits_on_the_person_says_so_on_the_tip_and_the_hover() {
    use aterm_agent::harness::hold_said::NeedTail;
    use aterm_agent::supervise::mode_words::CARRIES_ON;
    let now = 100_000_i64;
    for program in [
        aterm_phase::Program::Claude,
        aterm_phase::Program::Agy,
        aterm_phase::Program::Muse,
    ] {
        for tail in NeedTail::ALL {
            let facts = NeedFacts {
                attention: vec![crate::needs::AttentionFact {
                    owner: crate::harness_host::NEEDS_ATTENTION_OWNER.to_string(),
                    text: format!(
                        "{} needs you: sign 0.48.0 \u{2014} {}",
                        program.name(),
                        tail.words()
                    ),
                    kind: Some(aterm_types::needs::AttentionKind::Ask),
                }],
                ..NeedFacts::default()
            };
            let row = SessionRow {
                need_since: Some(now - 60),
                ..row_of(3, "proj-api", &facts)
            };
            let ask = row
                .need
                .as_ref()
                .and_then(|n| n.harness.clone())
                .expect("the report's ask");
            assert_eq!(ask.tail, tail, "{program:?}");
            let menu = compose_status_menu(&local_glance(&[row], &[], now, None), now);
            let (_, tip) = menu_row(&menu, "\u{25c7} proj-api \u{2014} sign 0.48.0")
                .unwrap_or_else(|| panic!("{program:?}: the report's row: {menu:?}"));
            let hover = crate::app_presence::harness_need_hover(&ask);
            let want = match tail {
                NeedTail::WaitsOnYou => {
                    format!("{} {}", program.product(), NeedTail::WaitsOnYou.words())
                }
                NeedTail::CarryingOn => format!("{} {CARRIES_ON}", program.product()),
            };
            assert_eq!(tip.as_deref(), Some(want.as_str()), "{program:?} {tail:?}");
            assert_eq!(
                hover.last(),
                Some(&want),
                "{program:?} {tail:?}: {hover:#?}"
            );
            if tail == NeedTail::WaitsOnYou {
                assert!(
                    !hover.iter().any(|l| l.contains(CARRIES_ON)),
                    "{program:?}: {hover:#?}"
                );
            }
        }
    }
}

#[test]
fn a_need_row_keeps_its_age_and_clips_the_need() {
    let now = 10_000_i64;
    let label = "proj-api refactor: the gateway, the store and the CLI (v2.1)";
    assert_eq!(label.len(), 60);
    let g = local_glance(
        &[harness_row(1, label, NEED_A_CUT, now - 180)],
        &[],
        now,
        None,
    );
    assert!(g.needs[0].harness.is_some(), "the row carries the words");
    let line = need_line(&g.needs[0], now, 0);
    assert!(line.len() <= LINE_MAX, "{} bytes: {line:?}", line.len());
    assert!(line.ends_with(" (+1 more) \u{b7} 3m"), "{line:?}");
    assert!(
        line.starts_with(&format!("\u{25c7} {label} \u{2014} grant Screen Recording")),
        "{line:?}"
    );
    assert!(!line.contains("needs you"), "{line:?}");
    let long = "x".repeat(150);
    let g = local_glance(
        &[harness_row(1, &long, NEED_A_CUT, now - 180)],
        &[],
        now,
        None,
    );
    let line = need_line(&g.needs[0], now, 0);
    assert!(
        line.len() <= LINE_MAX && line.ends_with(" \u{b7} 3m"),
        "{line:?}"
    );
    assert!(line.contains("\u{2014} grant Screen"), "{line:?}");
}

#[test]
fn the_shown_key_folds_the_harness_words() {
    let a = harness_row(1, "t", NEED_A_CUT, 0);
    let mut b = a.clone();
    if let Some(need) = b.need.as_mut()
        && let Some(ask) = need.harness.as_mut()
    {
        ask.more = 7;
    }
    assert_ne!(shown_key(&a), shown_key(&b));
}

fn told_facts(needs: &[&str]) -> NeedFacts {
    let more = needs.len() - 1;
    let tail = if more == 0 {
        String::new()
    } else {
        format!(" (+{more} more)")
    };
    let entry = format!("claude needs you: {}{tail} \u{2014} carrying on", needs[0]);
    let set: Vec<String> = needs.iter().map(|need| (*need).to_string()).collect();
    let mut told = crate::needs::ToldNeeds::default();
    told.tell(
        crate::needs::HarnessNeeds::told(&set, &entry).map(std::sync::Arc::new),
        Some(&entry),
    );
    NeedFacts {
        attention: vec![crate::needs::AttentionFact {
            owner: crate::harness_host::NEEDS_ATTENTION_OWNER.to_string(),
            text: entry,
            kind: Some(aterm_types::needs::AttentionKind::Ask),
        }],
        told,
        ..NeedFacts::default()
    }
}

fn told_row(id: u64, title: &str, needs: &[&str], since: i64) -> SessionRow {
    SessionRow {
        need_since: Some(since),
        ..row_of(id, title, &told_facts(needs))
    }
}

fn named_needs(title: &str, n: usize) -> Vec<String> {
    (1..=n).map(|i| format!("{title} {i}")).collect()
}

fn needs_section(menu: &[StatusRow]) -> &[StatusRow] {
    let end = menu
        .iter()
        .position(|r| matches!(r, StatusRow::Separator))
        .unwrap_or(menu.len());
    &menu[1..end]
}

fn further_labels(rows: &[StatusRow]) -> Vec<&str> {
    rows.iter()
        .filter_map(|r| match r {
            StatusRow::Action {
                label, indent: 1, ..
            } => Some(label.as_str()),
            _ => None,
        })
        .collect()
}

fn own_row<'a>(rows: &'a [StatusRow], title: &str) -> &'a str {
    rows.iter()
        .find_map(|r| match r {
            StatusRow::Action {
                label, indent: 0, ..
            } if label.starts_with(&format!("\u{25c7} {title} \u{2014} ")) => Some(label.as_str()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no row of {title}: {rows:?}"))
}

#[test]
fn every_further_need_reads_on_a_row_of_its_own_capped() {
    let now = 10_000_i64;
    let three = named_needs("three", 3);
    let five = named_needs("five", 5);
    fn refs(v: &[String]) -> Vec<&str> {
        v.iter().map(String::as_str).collect()
    }
    let g = local_glance(
        &[
            told_row(1, "one", &["one 1"], now - 300),
            told_row(2, "three", &refs(&three), now - 200),
            told_row(3, "five", &refs(&five), now - 100),
        ],
        &[],
        now,
        None,
    );
    let menu = compose_status_menu(&g, now);
    let section = needs_section(&menu);
    assert_eq!(
        further_labels(section),
        vec!["three 2", "three 3", "five 2", "five 3 (+2 more)"],
        "{section:#?}"
    );
    let order: Vec<&str> = section
        .iter()
        .filter_map(|r| match r {
            StatusRow::Action { label, .. } => Some(label.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(order.len(), 7, "{order:#?}");
    assert!(
        order[0].starts_with("\u{25c7} one \u{2014} one 1"),
        "{order:#?}"
    );
    assert!(
        order[1].starts_with("\u{25c7} three \u{2014} three 1"),
        "{order:#?}"
    );
    assert_eq!(&order[2..4], ["three 2", "three 3"]);
    assert!(
        order[4].starts_with("\u{25c7} five \u{2014} five 1"),
        "{order:#?}"
    );
    assert_eq!(&order[5..7], ["five 2", "five 3 (+2 more)"]);
    assert!(!own_row(section, "one").contains("more)"));
    assert!(
        !own_row(section, "three").contains("more)"),
        "every need has its row"
    );
    assert!(
        own_row(section, "five").ends_with("five 1 \u{b7} 1m"),
        "the count rides the last row under it: {}",
        own_row(section, "five")
    );
    assert!(
        !section.iter().any(|r| matches!(
            r,
            StatusRow::Action {
                action: StatusAction::Overview,
                ..
            }
        )),
        "no overflow row"
    );

    let fives: Vec<Vec<String>> = (1..=5).map(|i| named_needs(&format!("s{i}"), 5)).collect();
    let rows: Vec<SessionRow> = fives
        .iter()
        .enumerate()
        .map(|(i, needs)| {
            let id = u64::try_from(i).unwrap() + 1;
            told_row(
                id,
                &format!("s{id}"),
                &refs(needs),
                now - 600 + 10 * i64::try_from(i).unwrap(),
            )
        })
        .collect();
    let g = local_glance(&rows, &[], now, None);
    let menu = compose_status_menu(&g, now);
    let section = needs_section(&menu);
    assert_eq!(further_labels(section).len(), NEED_MORE_ALL, "{section:#?}");
    for title in ["s1", "s2", "s3", "s4"] {
        assert!(!own_row(section, title).contains("more)"), "{title}");
        assert!(
            further_labels(section).contains(&format!("{title} 3 (+2 more)").as_str()),
            "{title}: {section:#?}"
        );
    }
    assert!(
        own_row(section, "s5").contains(" (+4 more) "),
        "the menu's rows spent: {}",
        own_row(section, "s5")
    );

    let rows: Vec<SessionRow> = (1..=12_u64)
        .map(|id| {
            let first = format!("t{id} 1");
            let second = format!("t{id} 2");
            told_row(
                id,
                &format!("t{id}"),
                &[&first, &second],
                now - 1000 + i64::try_from(id).unwrap(),
            )
        })
        .collect();
    let g = local_glance(&rows, &[], now, None);
    let menu = compose_status_menu(&g, now);
    let section = needs_section(&menu);
    assert_eq!(further_labels(section).len(), NEED_MORE_ALL);
    assert_eq!(
        section.last(),
        Some(&more_in_overview(12 - NEEDS_SHOWN)),
        "the needs past the ones shown are counted on one line"
    );
    assert!(
        !section.iter().any(|r| matches!(
            r,
            StatusRow::Action {
                action: StatusAction::Overview,
                ..
            }
        )),
        "and that line is no second Overview"
    );
    assert!(
        own_row(section, "t8").contains("t8 1 \u{b7} "),
        "{}",
        own_row(section, "t8")
    );
    assert!(
        own_row(section, "t9").contains(" (+1 more) "),
        "{}",
        own_row(section, "t9")
    );
    assert!(own_row(section, "t10").contains(" (+1 more) "));
}

#[test]
fn a_further_needs_row_goes_where_its_session_goes() {
    let now = 10_000_i64;
    let g = local_glance(
        &[
            told_row(1, "alpha", &["a 1", "a 2", "a 3"], now - 200),
            told_row(2, "beta", &["b 1", "b 2"], now - 100),
        ],
        &[],
        now,
        None,
    );
    let menu = compose_status_menu(&g, now);
    let mut owner: Option<StatusAction> = None;
    let mut further = 0;
    for (i, row) in menu.iter().enumerate() {
        match row {
            StatusRow::Action {
                action, indent: 0, ..
            } if matches!(action, StatusAction::Go(_)) => owner = Some(*action),
            StatusRow::Action {
                action,
                indent: 1,
                enabled,
                ..
            } => {
                further += 1;
                assert!(*enabled);
                assert_eq!(Some(*action), owner, "row {i} goes where its session goes");
                let at = MenuAt::parse(&i.to_string()).expect("a path");
                assert_eq!(row_at(&menu, &[], &at), Some(row));
            }
            _ => {}
        }
    }
    assert_eq!(further, 3, "{menu:#?}");
    let go_of = |label: &str| {
        menu.iter().find_map(|r| match r {
            StatusRow::Action {
                label: l,
                action: StatusAction::Go(pick),
                ..
            } if l == label => Some(g.go[usize::from(pick.index)]),
            _ => None,
        })
    };
    assert_eq!(
        (go_of("a 2"), go_of("a 3"), go_of("b 2")),
        (Some(1), Some(1), Some(2))
    );
}

#[test]
fn menu_lines_marks_a_row_one_level_in() {
    let now = 10_000_i64;
    let g = local_glance(
        &[told_row(1, "beta", &["b 1", "b 2"], now - 100)],
        &[],
        now,
        None,
    );
    let lines = menu_lines(&compose_status_menu(&g, now), &[]);
    assert_eq!(
        lines.get(2).map(String::as_str),
        Some(
            "statusmenu \"b 2\" enabled=yes checked=no \
             tip=\"Claude Code in beta needs this too\" indent=1 at=2"
        ),
        "{lines:#?}"
    );
    let own = lines.get(1).expect("the session's row");
    assert!(
        own.starts_with("statusmenu \"\u{25c7} beta \u{2014} b 1 "),
        "{own}"
    );
    assert!(!own.contains("indent="), "{own}");
    assert!(
        lines
            .iter()
            .all(|l| !l.contains("indent=") || l.starts_with("statusmenu \"")),
        "{lines:#?}"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn a_further_needs_item_is_drawn_one_level_in() {
    let now = 10_000_i64;
    let g = local_glance(
        &[
            told_row(1, "alpha", &["a 1", "a 2", "a 3"], now - 200),
            told_row(2, "beta", &["b 1"], now - 100),
        ],
        &[],
        now,
        None,
    );
    let menu = compose_status_menu(&g, now);
    let mut drawn: Vec<(String, isize)> = Vec::new();
    aterm_objc::autoreleasepool(|_| {
        for row in &menu {
            let StatusRow::Action {
                label,
                action,
                enabled,
                checked,
                indent,
                ..
            } = row
            else {
                continue;
            };
            let item = super::macos::action_item(
                aterm_objc::Id::NIL,
                label,
                *action,
                (*enabled, *checked, *indent),
            )
            .expect("an item");
            // SAFETY: `-indentationLevel` is `-(NSInteger)` with no side effects, on a live NSMenuItem.
            let level =
                unsafe { crate::appkit::send_isize(item.id(), aterm_objc::sel!(indentationLevel)) };
            drawn.push((label.clone(), level));
        }
    });
    let level_of = |label: &str| {
        drawn
            .iter()
            .find(|(l, _)| l == label)
            .map(|(_, level)| *level)
    };
    assert_eq!(
        (level_of("a 2"), level_of("a 3")),
        (Some(1), Some(1)),
        "{drawn:#?}"
    );
    assert!(
        drawn
            .iter()
            .filter(|(l, _)| l != "a 2" && l != "a 3")
            .all(|(_, level)| *level == 0),
        "every other row flush: {drawn:#?}"
    );
    assert!(
        drawn.len() > 4,
        "the session rows and the menu's own: {drawn:#?}"
    );
}

#[test]
fn a_further_needs_row_says_whose_it_is_and_the_count_follows_the_rows() {
    let now = 10_000_i64;
    let alpha: Vec<String> = (1..=5).map(|i| format!("a {i}")).collect();
    let refs: Vec<&str> = alpha.iter().map(String::as_str).collect();
    let g = local_glance(&[told_row(1, "alpha", &refs, now - 60)], &[], now, None);
    let menu = compose_status_menu(&g, now);
    let section = needs_section(&menu);
    assert_eq!(further_labels(section), vec!["a 2", "a 3 (+2 more)"]);
    let own = own_row(section, "alpha");
    assert!(own.ends_with("a 1 \u{b7} 1m"), "{own}");
    let tips: Vec<Option<&str>> = section
        .iter()
        .filter_map(|r| match r {
            StatusRow::Action { tip, .. } => Some(tip.as_deref()),
            _ => None,
        })
        .collect();
    let whose = Some("Claude Code in alpha needs this too");
    let carries_on = aterm_agent::supervise::mode_words::report_words(
        aterm_phase::Program::Claude.product(),
        aterm_agent::harness::hold_said::NeedTail::CarryingOn,
        false,
    );
    assert_eq!(
        tips,
        vec![Some(carries_on.as_str()), whose, whose],
        "the report's own row says its agent carries on (§14.7): {section:#?}"
    );
    let lines = menu_lines(&menu, &[]);
    assert_eq!(
        lines.get(3).map(String::as_str),
        Some(
            "statusmenu \"a 3 (+2 more)\" enabled=yes checked=no \
             tip=\"Claude Code in alpha needs this too\" indent=1 at=3"
        ),
        "{lines:#?}"
    );
    let g = local_glance(
        &[told_row(1, "Claude Code", &["c 1", "c 2"], now - 60)],
        &[],
        now,
        None,
    );
    let menu = compose_status_menu(&g, now);
    let tips: Vec<&str> = needs_section(&menu)
        .iter()
        .filter_map(|r| match r {
            StatusRow::Action {
                tip: Some(tip),
                indent: 1,
                ..
            } => Some(tip.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(tips, ["Claude Code needs this too"]);
}

#[test]
fn a_told_need_reads_whole_on_the_menu() {
    const NEED_A: &str = "grant Screen Recording to com.aterm.aterm in System Settings \
                          \u{25b8} Privacy & Security \u{25b8} Screen Recording, so the \
                          windowed check can capture the menu";
    const NEED_B: &str = "the release credential (~/.aterm/release-credentials.toml) to \
                          sign 0.48.0";
    let now = 10_000_i64;
    let mut facts = told_facts(&[NEED_A, NEED_B]);
    let cut: String = NEED_A.chars().take(63).chain(['\u{2026}']).collect();
    let entry = format!("claude needs you: {cut} (+1 more) \u{2014} carrying on");
    facts.attention[0].text.clone_from(&entry);
    let set = vec![NEED_A.to_string(), NEED_B.to_string()];
    let mut told = crate::needs::ToldNeeds::default();
    told.tell(
        crate::needs::HarnessNeeds::told(&set, &entry).map(std::sync::Arc::new),
        Some(&entry),
    );
    facts.told = told;
    let row = SessionRow {
        need_since: Some(now - 180),
        ..row_of(1, "proj-api refactor", &facts)
    };
    let g = local_glance(&[row], &[], now, None);
    let menu = compose_status_menu(&g, now);
    let section = needs_section(&menu);
    assert_eq!(further_labels(section), [NEED_B]);
    let own = own_row(section, "proj-api refactor");
    assert!(own.len() <= LINE_MAX, "{own}");
    assert!(own.ends_with("\u{2026} \u{b7} 3m"), "{own}");
    assert!(own.contains("\u{2014} grant Screen Recording"), "{own}");
}

/// System State's one quiet signal (§3.6): one action row that opens the
/// tab, with the vault approval code under it, before This Mac; no row when
/// nothing needs a person.
#[test]
fn the_system_state_line_opens_the_tab_and_carries_the_code() {
    let mut g = Glance::default();
    let plain = compose_status_menu(&g, 0);
    assert!(
        !plain.iter().any(|row| matches!(
            row,
            StatusRow::Action {
                action: StatusAction::OpenSystemState,
                ..
            }
        )),
        "nothing needs a person: no line"
    );
    g.system_state = Some(SystemStateLine {
        label: "System State: Secrets vault needs you".to_string(),
        note: Some("Approval code 4F7-2KQ".to_string()),
    });
    let menu = compose_status_menu(&g, 0);
    assert_eq!(
        menu.first(),
        Some(&act(
            "System State: Secrets vault needs you",
            StatusAction::OpenSystemState
        ))
    );
    assert_eq!(
        menu.get(1),
        Some(&StatusRow::Info("Approval code 4F7-2KQ".to_string()))
    );
    assert_eq!(menu.get(2), Some(&StatusRow::Separator));
    assert_eq!(
        menu.get(3),
        Some(&StatusRow::Info(here_line(&g.here, g.machine.as_deref())))
    );
}

/// Every clickable row's label and tip in `menu`, a submenu's rows after its own.
fn tips_of(menu: &[StatusRow]) -> Vec<(String, Option<String>)> {
    menu.iter()
        .flat_map(|row| match row {
            StatusRow::Action { label, tip, .. } => vec![(label.clone(), tip.clone())],
            StatusRow::Submenu { rows, .. } => tips_of(rows),
            _ => Vec::new(),
        })
        .collect()
}

/// A LANDING'S ROWS SAY WHAT ATERM CHECKED (`docs/DESIGN-harness-autopilot-2026-10-15.md`
/// §14.9, slice 9c): over [`a_landing_with_its_receipt`], the Done for you row's tip says what
/// aterm checked and what the agent said, naming the head and `origin/main`; the worker's row
/// under its manager what aterm checked in a repository with no remote. A landing with no
/// receipt (codex-alab's own) says aterm checked nothing; a done its face moved off says
/// nothing. NEGATIVE CONTROL (run by hand): `DoneRow::tip` left `None` fails the first.
#[test]
fn a_landings_rows_say_what_aterm_checked() {
    use aterm_agent::supervise::mode_words::{NO_RECORD, checked_words, landed_tip};
    let now = 1_800_000_000;
    let mut rows = a_landing_with_its_receipt(unix_ms(now));
    let (on_main, no_remote) = landing_receipts();
    let tip = |rows: &[SessionRow], lead: &str| {
        let menu = compose_status_menu(&local_glance(rows, &[], now, None), now);
        let found: Vec<_> = tips_of(&menu)
            .into_iter()
            .filter(|(label, _)| label.starts_with(lead))
            .collect();
        let [(_, tip)] = found.as_slice() else {
            panic!("one row {lead:?}: {menu:#?}");
        };
        tip.clone()
    };
    let done = "\u{2713} proj-api \u{2014} landed";
    let worker = "\u{2713} parser \u{2014} landed";
    let on_tip = tip(&rows, done).expect("the Done for you row's tip");
    assert_eq!(on_tip, landed_tip(Some(&on_main)));
    assert!(on_tip.contains("3f9a1c2e4b7d on origin/main"), "{on_tip}");
    assert_eq!(tip(&rows, worker), Some(checked_words(Some(&no_remote))));
    rows[1].receipt = None;
    rows[2].receipt = None;
    assert_eq!(tip(&rows, done).as_deref(), Some(NO_RECORD));
    assert_eq!(tip(&rows, worker).as_deref(), Some(NO_RECORD));
    rows[1].landed = false;
    rows[2].landed = false;
    assert_eq!(tip(&rows, done), None, "the face moved off");
    assert_eq!(tip(&rows, worker), None, "the face moved off");
}
