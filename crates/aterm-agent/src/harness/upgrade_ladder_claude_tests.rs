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
        human_seq: None,
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
        account_roster: None,
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
    look_facts(&opts(), sf, st, TAB, &read, &[], t)
        .expect("readable ledger evidence")
        .0
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

/// THE STALL'S OWN SCREEN NEVER RESTARTS OVER THE WORK IT SHOWS: Claude Code 2.1.310's turn over
/// while three dynamic workflows it started run on in its own process (captured read-only
/// 2026-10-27 from the tab whose upgrade read `overdue:background` for 19 hours). The server
/// publishes that turn's rest (`agent=idle`), and Claude Code's session file may say `idle`; no
/// shell runs under it. The look counts the published rest as no quiet and the screen's workflows
/// as the agent's own work, so the restart waits on them at every rung, Land's included, where a
/// restart would end them. With the workflows gone the same look restarts.
/// The look [`look_facts`] takes of a Claude Code 2.1.310 session `behind` seconds behind its
/// upgrade, whose server publishes the rest (`agent=idle`) over `rows`, the cursor on the
/// composer's `❯` row, its words dim, and no process under the agent.
fn rest_look(rows: &[String], behind: u64) -> Facts {
    let rest = "OK schema=1 agent=idle agent_since_ms=600000 hold=0 hand=-";
    let file = SessionFile {
        pid: PID,
        session_id: "01a0dc36-1dc1-7ee2-be91-11bc323e377c".to_string(),
        cwd: "/".to_string(),
        version: "2.1.310".to_string(),
        status: "idle".to_string(),
        status_updated_at_ms: (NOW - 600) * 1_000,
        proc_start: String::new(),
        kind: "interactive".to_string(),
        entrypoint: "cli".to_string(),
        claude_dir: None,
        profile: None,
    };
    let caret = rows
        .iter()
        .rposition(|r| r.starts_with('❯'))
        .expect("the composer");
    let scr = Screen {
        rows: rows.to_vec(),
        cursor: Some((caret, 2)),
        seq: 7,
        first: 0,
        generation: None,
        human: HumanInput::Ago(600_000),
        title: None,
        human_seq: None,
    };
    let mut st = St {
        pending_since: NOW - behind,
        ..St::default()
    };
    let _ = quiet_s(Some(rest), &mut st, 7, NOW - 2 * upgrade::QUIET_S);
    let read = TabRead {
        scr: &scr,
        status: Some(rest),
        dim: true,
    };
    look_facts(&opts(), &file, &mut st, TAB, &read, &[], NOW)
        .expect("readable ledger evidence")
        .0
}

#[test]
fn the_stall_screens_upgrade_never_restarts_over_the_work_it_shows() {
    use aterm_phase::prompt::fixtures::{WAITING_WORKFLOWS_2_1_310, screen};
    let look = rest_look;
    let captured = screen(WAITING_WORKFLOWS_2_1_310);
    for behind in [60, upgrade::RUNG_LAND_S] {
        let f = look(&captured, behind);
        assert!(f.composer_empty, "its dim `keep going` is no draft");
        assert!(!f.approval_box && !f.busy_footer, "{f:?}");
        assert_eq!(f.quiet_s, 0, "its published rest is no quiet");
        assert_eq!(
            f.background,
            ["screen:waiting-for-a-dynamic-workflow"],
            "its workflows are its own work"
        );
        assert_eq!(
            upgrade::next_step(&Phase::Pending, &f, false, NOW),
            upgrade::Step::Wait("background"),
            "behind {behind}s: {f:?}"
        );
    }
    // CONTROL: the workflows ended, the same turn's end restarts.
    let wait = captured
        .iter()
        .position(|r| r.starts_with("✻ Waiting for 3 dynamic workflows"))
        .expect("the waiting row");
    let (head, panel) = captured[wait].split_once('│').expect("the panel's rule");
    let mut ended = captured.clone();
    ended[wait] = format!(
        "{:<w$}│{panel}",
        "✻ Brewed for 6m 7s · done 10:07 AM",
        w = head.chars().count()
    );
    ended.retain(|r| !r.trim_start().starts_with('◯'));
    let f = look(&ended, 60);
    assert!(
        f.background.is_empty() && f.quiet_s >= upgrade::QUIET_S,
        "{f:?}"
    );
    assert_eq!(
        upgrade::next_step(&Phase::Pending, &f, false, NOW),
        upgrade::Step::Terminate,
        "{f:?}"
    );
    // A workflow's progress row left at its full bar after the workflow ended (measured on
    // 2.1.304) is no running work of the agent's: it holds no restart.
    let path = aterm_tempfile::manifest_dir().join(
        "../aterm-phase/src/fixtures/claude-2.1.304-agents-panel-workflow-done-3m20s-120col.txt",
    );
    let done = screen(&std::fs::read_to_string(path).expect("the fixture"));
    assert_eq!(
        aterm_phase::phase::background_wait(&done),
        Some("a workflow running"),
        "the reader still reads its row"
    );
    assert!(look(&done, 60).background.is_empty());
}

/// A Claude Code screen over its own work, CONSTRUCTED in the shape of `aterm_phase`'s
/// `a_wait_claude_code_hid_under_its_own_wake_still_stands`: `status` the row its turn stopped on,
/// `body` the rows drawn under that row above the composer (a wake and its reply carry the wait
/// up, so no status row stands over the composer), `under` the rows under the footer.
fn own_work_screen(status: &str, body: &[&str], under: &[&str]) -> Vec<String> {
    let rule = "─".repeat(100);
    let mut rows = vec![
        "⏺ The review runs as a workflow; it reports when it ends.".to_string(),
        String::new(),
        status.to_string(),
        String::new(),
    ];
    rows.extend(body.iter().map(|r| (*r).to_string()));
    rows.extend([
        rule.clone(),
        "❯ keep going".to_string(),
        rule,
        "  ⏵⏵ bypass permissions on (shift+tab to cycle) · ← for agents".to_string(),
        String::new(),
    ]);
    rows.extend(under.iter().map(|r| (*r).to_string()));
    rows
}

/// THE AGENT'S OWN WORK HOLDS THE RESTART WHATEVER RULE THE READER READS FIRST: a workflow's
/// progress row under the footer (`◯ name ▰▰▱ 1/2`) reads `a workflow running` ahead of the wait
/// Claude Code carries above its own wake, and ahead of a done row's `… still running`. Each of
/// those is work the agent runs in its own process, which no process row names, so the restart
/// waits for it at every rung, LAND's too.
/// The restart's signal, which re-reads the screen, goes only where none of it is pending
/// ([`turn_over_shown`]).
/// NEGATIVE CONTROL: the same screens once that work is over (the workflow's completion and a
/// done row with nothing left running, its progress row left at its full bar as 2.1.304 leaves
/// it) restart at LAND.
#[test]
fn the_agents_own_work_holds_the_restart_under_any_footer_rule() {
    const RUNNING: &str = "  ◯ review-pass  ▰▰▰▰▰▰▰▰▰▰▱▱▱▱▱▱▱▱▱▱  1/2 · 3m02s · ↓ 12.4k tokens";
    const FULL: &str = "  ◯ review-pass  ▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰  2/2 · 3m20s · ↓ 20.1k tokens";
    let wake = [
        "⏺ Monitor event: \"lander\"",
        "",
        "⏺ Still in its crates-tests lane. I restarted the watch.",
        "",
    ];
    let carried = own_work_screen(
        "✻ Waiting for 1 dynamic workflow to finish",
        &wake,
        &[RUNNING],
    );
    let left = own_work_screen(
        "✻ Brewed for 6m 7s · done 10:07 AM · 2 agents still running",
        &[],
        &[RUNNING],
    );
    // The same done row as 2.1.310 and later draw it beside their side panel: it and every row
    // above the composer padded out to the panel's rule
    // (`claude-2.1.316-waiting-agents-and-workflows-turn-over-panel.txt`).
    let mut beside = own_work_screen(
        "✻ Brewed for 6m 7s · done 10:07 AM · 2 agents still running",
        &[],
        &[RUNNING],
    );
    for row in &mut beside[..4] {
        *row = format!("{row:<81}│");
    }
    for (name, rows, work) in [
        ("carried", &carried, "screen:background-work-still-running"),
        (
            "left-running",
            &left,
            "screen:background-work-still-running",
        ),
        (
            "left-running beside the panel",
            &beside,
            "screen:background-work-still-running",
        ),
    ] {
        assert_eq!(
            aterm_phase::phase::background_wait(rows),
            Some("a workflow running"),
            "{name}: the reader reads the footer's rule first"
        );
        for behind in [60, upgrade::RUNG_LAND_S] {
            let f = rest_look(rows, behind);
            assert!(
                f.composer_empty && !f.approval_box && !f.busy_footer,
                "{name}: {f:?}"
            );
            assert_eq!(f.background, [work], "{name}: its own work");
            assert_eq!(
                upgrade::next_step(&Phase::Pending, &f, false, NOW),
                upgrade::Step::Wait("background"),
                "{name}, behind {behind}s: {f:?}"
            );
        }
        let signal = read(rows.clone(), 7, HumanInput::Ago(600_000));
        assert!(!turn_over_shown(&signal, false), "{name}: the signal waits");
    }
    // CONTROL: the work is over and its progress row stays drawn at its full bar.
    let completed = own_work_screen(
        "✻ Waiting for 1 dynamic workflow to finish",
        &[
            "⏺ Dynamic workflow \"review-pass\" completed · 3m 20s",
            "",
            "⏺ The review is in; nothing to fix.",
            "",
            "✻ Cooked for 9m 0s · done 10:11 AM",
            "",
        ],
        &[FULL],
    );
    let done = own_work_screen("✻ Brewed for 6m 7s · done 10:07 AM", &[], &[FULL]);
    for (name, rows) in [("completed", &completed), ("done", &done)] {
        assert_eq!(
            aterm_phase::phase::background_wait(rows),
            Some("a workflow running"),
            "{name}: the reader still reads its row"
        );
        let f = rest_look(rows, upgrade::RUNG_LAND_S);
        assert!(f.background.is_empty(), "{name}: {f:?}");
        assert_eq!(
            upgrade::next_step(&Phase::Pending, &f, false, NOW),
            upgrade::Step::Terminate,
            "{name}: {f:?}"
        );
        let signal = read(rows.clone(), 7, HumanInput::Ago(600_000));
        assert!(turn_over_shown(&signal, false), "{name}: the signal goes");
    }
}

/// THE WORK A DONE ROW NAMES ABOVE AN IDLE COMPACTION HOLDS THE RESTART: Claude Code 2.1.312
/// compacted an idle conversation under the done row of a turn that left work running (the tab of
/// 2026-10-27), and its notice, a row of the transcript, is drawn between that row and the
/// composer. A done row there that names background agents still running names work of Claude
/// Code's own process, which no process row names and a restart would end, so the restart waits
/// for it at every rung and its signal does not go.
/// NEGATIVE CONTROL: the captured screen, whose done row names a shell alone (the process table's
/// to name), holds nothing by its words.
#[test]
fn the_work_a_done_row_names_above_an_idle_compaction_holds_the_restart() {
    use aterm_phase::prompt::fixtures::{IDLE_COMPACTED_OVER_A_SHELL_2_1_312, screen};
    let captured = screen(IDLE_COMPACTED_OVER_A_SHELL_2_1_312);
    let f = rest_look(&captured, upgrade::RUNG_LAND_S);
    assert!(f.background.is_empty(), "{f:?}");
    let done = captured
        .iter()
        .position(|r| r.starts_with("✻ Worked for 23s"))
        .expect("the done row");
    let mut agents = captured.clone();
    agents[done] = "✻ Worked for 23s · done 4:57 PM · 1 shell, 2 agents still running".to_string();
    for behind in [60, upgrade::RUNG_LAND_S] {
        let f = rest_look(&agents, behind);
        assert_eq!(
            f.background,
            ["screen:background-work-still-running"],
            "behind {behind}s: {f:?}"
        );
        assert_eq!(
            upgrade::next_step(&Phase::Pending, &f, false, NOW),
            upgrade::Step::Wait("background"),
            "behind {behind}s: {f:?}"
        );
    }
    let signal = read(agents, 7, HumanInput::Ago(600_000));
    assert!(!turn_over_shown(&signal, false), "the signal waits");
}

/// WORK ONLY THE FOOTER'S PILL COUNTS HOLDS THE RESTART: Claude Code counts what still runs in its
/// footer's pill, one label for each kind (2.1.286, read from its source: `1 MCP task`, `2
/// background tasks`, `◇ 1 cloud session`, `1 Artifact comment monitor`; no capture draws one),
/// and where its done row names none of that work, the pill is the one place the screen names it.
/// No process row names it either ([`background`] reads the processes under the agent that
/// [`BACKGROUND`] names), so the restart waits for it at every rung, LAND's too, and its signal
/// does not go ([`turn_over_shown`]). The screens are the measured 2.1.312 capture with its done
/// row's ` · 1 shell still running` dropped and its pill's `1 shell` replaced by each label
/// (CONSTRUCTED).
/// NEGATIVE CONTROL: the same screen with its pill counting no work (`10 files changed`) restarts
/// at LAND, its signal going.
#[test]
fn work_only_the_footers_pill_counts_holds_the_restart() {
    use aterm_phase::prompt::fixtures::{IDLE_COMPACTED_OVER_A_SHELL_2_1_312, screen};
    let captured = screen(IDLE_COMPACTED_OVER_A_SHELL_2_1_312);
    let done = captured
        .iter()
        .position(|r| r.starts_with("✻ Worked for 23s"))
        .expect("the done row");
    let pill = captured.len() - 1;
    assert!(captured[pill].ends_with("· 1 shell"), "{captured:#?}");
    let counting = |count: &str| -> Vec<String> {
        let mut rows = captured.clone();
        rows[done] = "✻ Worked for 23s · done 4:57 PM".to_string();
        rows[pill] = captured[pill].replace("· 1 shell", &format!("· {count}"));
        rows
    };
    for label in [
        "1 MCP task",
        "2 background tasks",
        "◇ 1 cloud session",
        "1 Artifact comment monitor",
    ] {
        let rows = counting(label);
        for behind in [60, upgrade::RUNG_LAND_S] {
            let f = rest_look(&rows, behind);
            assert_eq!(
                upgrade::next_step(&Phase::Pending, &f, false, NOW),
                upgrade::Step::Wait("background"),
                "{label}, behind {behind}s: {f:?}"
            );
            assert_eq!(
                f.background,
                ["screen:background-work-running"],
                "{label}, behind {behind}s"
            );
        }
        let signal = read(rows, 7, HumanInput::Ago(600_000));
        assert!(
            !turn_over_shown(&signal, false),
            "{label}: the signal waits"
        );
    }
    // CONTROL: the pill counts no work.
    let files = counting("10 files changed");
    let f = rest_look(&files, upgrade::RUNG_LAND_S);
    assert!(f.background.is_empty(), "{f:?}");
    assert_eq!(
        upgrade::next_step(&Phase::Pending, &f, false, NOW),
        upgrade::Step::Terminate,
        "{f:?}"
    );
    let signal = read(files, 7, HumanInput::Ago(600_000));
    assert!(turn_over_shown(&signal, false), "the signal goes");
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
