// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Live upgrade ladder conformance tests for Claude Code's lane.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use aterm_models_harness::harness_upgrade_ladder_model;
use aterm_spec::derive::Model;

use super::*;
use crate::supervise::screen::HumanInput;

type State = BTreeMap<&'static str, i64>;

const GRACE: u32 = 120;

const NOW: u64 = 1_790_607_857;

const PID: u32 = 4_242;
const TAB: &str = "7";

const MUTANTS: [&str; 6] = [
    "AttendedWithoutLadder",
    "MoveMidTurn",
    "MoveOverABox",
    "MoveOverADraft",
    "NowWaivesThePerson",
    "SeqSettle",
];

fn reach(m: &Model) -> BTreeSet<State> {
    let mut seen = BTreeSet::from([m.init_state()]);
    let mut queue = VecDeque::from([m.init_state()]);
    while let Some(s) = queue.pop_front() {
        for a in &m.actions {
            for next in m.successors(a.name, &s) {
                if seen.insert(next.clone()) {
                    queue.push_back(next);
                }
            }
        }
    }
    seen
}

fn tab_rows(words: &str, busy: bool, draft: &str, tick: u32) -> Vec<String> {
    let rule = "─".repeat(40);
    let mut rows = vec![
        "❯ fix the footer".to_string(),
        String::new(),
        format!("⏺ {words}"),
        String::new(),
    ];
    if busy {
        rows.push(format!(
            "✶ Deliberating… ({tick}s · ↑ 1.2k tokens · esc to interrupt)"
        ));
        rows.push(String::new());
    }
    rows.extend([
        rule.clone(),
        format!("❯ {draft}"),
        rule,
        format!("  ⏵⏵ auto mode on (shift+tab to cycle) · {tick}s"),
    ]);
    rows
}

fn read(rows: Vec<String>, seq: u64, human: HumanInput) -> Screen {
    let cursor = aterm_phase::phase::composer_draft(&rows)
        .map(|(caret, lines)| (caret, 2 + lines.first().map_or(0, |l| l.chars().count())));
    Screen {
        rows,
        cursor,
        seq,
        first: 0,
        generation: None,
        human,
        title: None,
    }
}

fn look_rows(s: &State, tick: u32) -> Vec<String> {
    if s["dialog"] == 1 {
        aterm_phase::prompt::fixtures::screen(aterm_phase::prompt::fixtures::BOX_BASH_TOUCH)
    } else {
        let draft = if s["draft"] == 1 { "and the tests" } else { "" };
        tab_rows("Done.", s["work"] == 1, draft, tick)
    }
}

/// How a state's `quiet` reaches a look: the screen's own `seq` with no verdict behind the
/// server's `agent=` (`Seq`, a repaint moved it), the server's verdict moved a moment ago
/// (`Verdict`), or Claude Code's session file rewritten a moment ago (`File`). Each holds still
/// where the state is quiet.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum History {
    Seq,
    Verdict,
    File,
}

const HISTORIES: [History; 3] = [History::Seq, History::Verdict, History::File];

fn status_of(s: &State, h: History) -> String {
    let (agent, since_ms) = if s["work"] == 1 {
        ("busy", 4_000)
    } else {
        match h {
            History::Seq => ("unknown", 5),
            History::Verdict if s["quiet"] == 0 => ("idle", 1_000),
            History::Verdict | History::File => ("idle", 600_000),
        }
    };
    format!("OK schema=1 agent={agent} agent_since_ms={since_ms} hold=0 hand=-")
}

fn opts() -> Opts {
    let none = PathBuf::from("/nonexistent/aterm-ladder-claude");
    Opts {
        home: none.join("home"),
        state: none.join("state"),
        sock: None,
        token: None,
        only_sid: None,
        dry_run: false,
        human_grace_s: GRACE,
        hand_back: false,
        background: false,
        aterm_state: None,
        ask: false,
        several_logins: false,
        usage_credits: false,
        usage_tap: None,
        instances: Instances::Listed(Vec::new()),
        press: None,
        timings: crate::harness::Timings::FAST,
        agents_off: Vec::new(),
        land: false,
        tell: crate::harness::upgrade_drive::Tell::Off,
    }
}

fn session_file(s: &State, h: History, t: u64) -> SessionFile {
    let changed_s = if h == History::File && s["quiet"] == 0 {
        1
    } else {
        600
    };
    SessionFile {
        pid: PID,
        session_id: "01a0dc36-1dc1-7ee2-be91-11bc323e377c".to_string(),
        cwd: "/".to_string(),
        version: "1.0.0".to_string(),
        status: if s["work"] == 1 { "busy" } else { "idle" }.to_string(),
        status_updated_at_ms: (t - changed_s) * 1_000,
        proc_start: String::new(),
        kind: "interactive".to_string(),
        entrypoint: "cli".to_string(),
        claude_dir: None,
        profile: None,
    }
}

fn look_at(st: &mut St, sf: &SessionFile, scr: &Screen, status: &str, t: u64) -> Facts {
    let read = TabRead {
        scr,
        status: Some(status),
        dim: false,
    };
    look_facts(&opts(), sf, st, TAB, &read, &[], t).0
}

fn facts_of(s: &State, h: History) -> (Facts, bool) {
    const BEHIND: [u64; 4] = [
        60,
        upgrade::RUNG_SETTLED_S,
        upgrade::RUNG_KEYS_S,
        upgrade::RUNG_LAND_S,
    ];
    let human = match s["keys"] {
        0 => HumanInput::Ago(600_000),
        1 => HumanInput::Ago(60_000),
        _ => HumanInput::Ago(1_000),
    };
    let mut st = St {
        pending_since: NOW - BEHIND[usize::try_from(s["rung"]).expect("a rung")],
        ..St::default()
    };
    let status = status_of(s, h);
    let _ = quiet_s(Some(&status), &mut st, 7, NOW - 2 * upgrade::QUIET_S);
    if s["seen"] == 2 {
        let t = NOW - upgrade::QUIET_S;
        let earlier = read(look_rows(s, 50), 7, human);
        let _ = look_at(&mut st, &session_file(s, h, t), &earlier, &status, t);
    }
    let seq = if h == History::Seq && s["quiet"] == 0 {
        8
    } else {
        7
    };
    let now = read(look_rows(s, 51), seq, human);
    let idle_read = {
        let mut probe = st.clone();
        ladder_look(&mut probe, "claude", &now, PID, GRACE, NOW).idle_read
    };
    let f = look_at(&mut st, &session_file(s, h, NOW), &now, &status, NOW);
    (f, idle_read)
}

fn real_moves(f: &Facts, now: bool) -> bool {
    let f = Facts {
        owner_now: now,
        ..f.clone()
    };
    upgrade::next_step(&Phase::Pending, &f, false, NOW) == upgrade::Step::Terminate
}

#[test]
fn the_real_ladder_is_the_derived_machine_on_the_claude_lane() {
    use aterm_spec::xref;
    assert!(xref::reset_entered_anchors(), "the evidence window opens");
    let model = harness_upgrade_ladder_model();
    let states = reach(&model);
    assert!(states.len() > 200, "the lane has a space: {}", states.len());
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let fired = aterm_spec::interp::fired_actions(&model);
    let dead: BTreeSet<&str> = model
        .actions
        .iter()
        .map(|a| a.name)
        .filter(|a| !fired.contains(a))
        .collect();
    assert_eq!(dead, BTreeSet::from(MUTANTS), "the model's whole Buggy set");
    let mut caught: BTreeSet<&str> = BTreeSet::new();
    let (mut moves, mut waits, mut still_moves, mut projected) = (0, 0, 0, 0);
    let mut looked: BTreeMap<(State, History), (Facts, bool)> = BTreeMap::new();
    let vars = ["rung", "quiet", "seen", "keys", "draft", "dialog", "work"];
    for s in states.iter().filter(|s| s["behind"] == 1) {
        let moves_here = model.action_enabled("Move", s);
        let enabled: BTreeSet<&str> = [
            "MoveOverADraft",
            "MoveOverABox",
            "MoveMidTurn",
            "NowWaivesThePerson",
            "SeqSettle",
        ]
        .into_iter()
        .filter(|mutant| buggy.action_enabled(mutant, s))
        .collect();
        let refused = {
            let mut decided = s.clone();
            buggy.fire("AttendedWithoutLadder", &mut decided)
                && !buggy.action_enabled("Move", &decided)
        };
        let key: State = vars.iter().map(|&k| (k, s[k])).collect();
        for h in HISTORIES {
            let (f, idle_read) = looked
                .entry((key.clone(), h))
                .or_insert_with(|| facts_of(&key, h))
                .clone();
            assert!(!f.taskless, "nothing on disk says the session has no task");
            let real = real_moves(&f, false);
            assert_eq!(moves_here, real, "{s:?}\n{h:?}\n{f:?}");
            if real {
                moves += 1;
                if s["quiet"] == 0 && s["rung"] < 3 {
                    still_moves += 1;
                }
            } else {
                waits += 1;
            }
            if idle_read {
                let mut want: BTreeMap<&str, i64> = vars.iter().map(|&k| (k, s[k])).collect();
                want.insert("seen", if s["seen"] == 2 { 2 } else { 0 });
                assert_eq!(ladder_projection(&f), want, "{s:?}\n{h:?}");
                projected += 1;
            }
            for mutant in ["MoveOverADraft", "MoveOverABox", "MoveMidTurn"] {
                if enabled.contains(mutant) && !real {
                    caught.insert(mutant);
                }
            }
            if enabled.contains("NowWaivesThePerson") && !real_moves(&f, true) {
                caught.insert("NowWaivesThePerson");
            }
            if enabled.contains("SeqSettle") && real {
                caught.insert("SeqSettle");
            }
            if refused && real {
                caught.insert("AttendedWithoutLadder");
            }
        }
    }
    assert!(
        moves > 0 && waits > 0 && still_moves > 0 && projected > 0,
        "{moves} {waits} {still_moves} {projected}"
    );
    assert_eq!(caught, BTreeSet::from(MUTANTS), "every mutant caught alone");

    for (action, method) in [("Advance", "rung"), ("Look", "still"), ("Move", "gate")] {
        assert!(
            xref::refinements().any(|a| a.machine == "HarnessUpgradeLadder"
                && a.action == action
                && a.rust_method == method
                && a.project == "aterm_agent::harness::upgrade_drive::ladder_projection"),
            "{action} @ {method} anchored"
        );
        let id = format!("HarnessUpgradeLadder::{action} @ {method}");
        assert!(
            xref::entered_anchor_ids().contains(id.as_str()),
            "{id} entered"
        );
    }
}

#[test]
fn the_still_run_counts_claude_looks_at_the_same_words_and_not_repaints() {
    let status = "OK schema=1 agent=unknown agent_since_ms=5";
    let mut st = St::default();
    let never = HumanInput::Never;
    let at = |st: &mut St, rows: Vec<String>, t: u64| {
        ladder_look(st, "claude", &read(rows, t, never), PID, GRACE, t).still_s
    };
    assert_eq!(
        at(&mut st, tab_rows("Done.", false, "", 51), 1_000),
        0,
        "one look is no run"
    );
    assert_eq!(quiet_s(Some(status), &mut st, 1, 1_000), 0);
    assert_eq!(at(&mut st, tab_rows("Done.", false, "", 52), 1_030), 30);
    assert_eq!(
        quiet_s(Some(status), &mut st, 2, 1_030),
        0,
        "the screen's own quiet restarts at the repaint"
    );
    assert_eq!(
        at(&mut st, tab_rows("Now the tests.", false, "", 52), 1_060),
        0
    );
    let busy = ladder_look(
        &mut st,
        "claude",
        &read(tab_rows("Now the tests.", true, "", 3), 1, never),
        PID,
        GRACE,
        1_090,
    );
    assert!(!busy.idle_read, "a running turn is no idle reading");
    assert_eq!(busy.still_s, 0);
    assert_eq!(
        at(&mut st, tab_rows("Now the tests.", false, "", 52), 1_120),
        0
    );
    assert_eq!(
        at(&mut st, tab_rows("Now the tests.", false, "", 53), 1_150),
        30
    );
    assert_eq!(
        at(
            &mut st,
            tab_rows("Now the tests.", false, "", 53),
            1_150 + IDLE_RUN_GAP_S + 1
        ),
        0
    );
}

/// A Claude Code screen under a command status line (aterm's usage tap is one), CONSTRUCTED in the
/// shape of 2.1.311's tapped mid-reply capture: the reply's rows drawn so far and no spinner or
/// `esc to interrupt` anywhere, `status` the line the status line prints under the composer.
fn tapped(reply: &[&str], status: &str) -> Vec<String> {
    let rule = "─".repeat(40);
    let mut rows = vec!["❯ write the landing report".to_string(), String::new()];
    rows.extend(reply.iter().map(|r| (*r).to_string()));
    rows.extend([
        String::new(),
        rule.clone(),
        "❯ ".to_string(),
        rule,
        format!("  {status}"),
        "  ⏵⏵ auto mode on (shift+tab to cycle)".to_string(),
    ]);
    rows
}

/// The print guard in [`look_facts`]: a reply still being drawn under a status line reads idle at
/// every look, but the words above its composer grow, so no two of its looks are in a row and the
/// `busy` status left over it never lags ([`upgrade::status_lags`]) for `--now` to end the turn on.
/// NEGATIVE CONTROL: the same session at one still transcript collects two looks and lags, a
/// status line's refresh below the composer moving nothing.
#[test]
fn a_reply_still_being_drawn_never_collects_two_lag_looks() {
    let reply = [
        "⏺ Nothing left: the report is written and the branch is on main.",
        "  - Checkout: clean, nothing unpushed.",
        "  - Worktree: removed, its branch merged.",
        "  - Fixtures: three, each named for the frame it holds.",
        "  NEEDS YOU: nothing",
    ];
    let status = "OK schema=1 agent=idle agent_since_ms=600000 hold=0 hand=-";
    let t0 = NOW;
    let sf = SessionFile {
        pid: PID,
        session_id: "01a0dc36-1dc1-7ee2-be91-11bc323e377c".to_string(),
        cwd: "/".to_string(),
        version: "2.1.311".to_string(),
        status: "busy".to_string(),
        status_updated_at_ms: (t0 - 600) * 1_000,
        proc_start: String::new(),
        kind: "interactive".to_string(),
        entrypoint: "cli".to_string(),
        claude_dir: None,
        profile: None,
    };
    let mut st = St::default();
    let mut t = t0;
    for drawn in 1..=reply.len() {
        let rows = tapped(&reply[..drawn], "lab status line");
        let scr = read(
            rows,
            u64::try_from(drawn).expect("a seq"),
            HumanInput::Never,
        );
        let probe = ladder_look(&mut st.clone(), "claude", &scr, PID, GRACE, t);
        assert!(
            probe.idle_read,
            "PRECONDITION: the screen alone reads a drawing reply idle: {:?}",
            scr.rows
        );
        let f = look_at(&mut st, &sf, &scr, status, t);
        assert_eq!(
            f.idle_looks, 1,
            "{drawn} rows drawn: each look the first of a run"
        );
        assert!(!upgrade::status_lags(&f), "{drawn} rows drawn: no lag");
        t += upgrade::QUIET_S;
    }
    let done = |line: &str| read(tapped(&reply, line), 9, HumanInput::Never);
    let f = look_at(&mut st, &sf, &done("lab status line"), status, t);
    assert_eq!(f.idle_looks, 2, "the same words again: in a row");
    assert!(upgrade::status_lags(&f), "and the status lags");
    t += upgrade::QUIET_S;
    let f = look_at(&mut st, &sf, &done("lab status line 2"), status, t);
    assert_eq!(f.idle_looks, 3, "the status line's refresh is no new word");
    assert!(upgrade::status_lags(&f));
}
