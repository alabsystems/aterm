// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The ledger's open model switch: what a later loop restores of a Fable move onto Opus.

use super::*;
use crate::supervise::facts::FactKind;
use crate::supervise::policy::turn_end::{
    AccountAt, FABLE_FALLBACK, RULE_ACCOUNT, RULE_API_RETRY, RULE_CONTINUE, RULE_LAND,
    RULE_LAND_NUDGE, RULE_LIMIT_RESUME, RULE_MODEL_FALLBACK, RULE_MODEL_RESTORE, SwitchProc,
};

const SID: &str = "@s-9";

/// The move's typed command, as the loop writes it for `Restart::Model { to: FABLE_FALLBACK }`.
fn move_command() -> String {
    format!("relaunch --model {FABLE_FALLBACK}")
}

/// The return to a launch that named no `--model`, as the loop writes `Restart::ModelBack`.
const BACK_UNNAMED: &str = "relaunch with no --model, as its launch";

fn row_of(
    ts: i64,
    rule_id: &str,
    outcome: Outcome,
    command: &str,
    reason: &str,
    sid: &str,
) -> String {
    Row {
        rule_id,
        outcome,
        command,
        reason,
        box_seq: 1,
    }
    .to_json(ts, Some(sid))
}

fn row_at(ts: i64, rule_id: &str, outcome: Outcome, command: &str, reason: &str) -> String {
    row_of(ts, rule_id, outcome, command, reason, SID)
}

/// The Fable bucket's move onto Opus, from a launch that named `launch`.
fn switch_row(ts: i64, launch: Option<&str>, back_at: Option<i64>, made: i64) -> String {
    row_at(
        ts,
        RULE_MODEL_FALLBACK,
        Outcome::Typed,
        &move_command(),
        &model_switch_reason(Some("fable"), launch, back_at, made),
    )
}

fn body(rows: &[String]) -> String {
    rows.iter().map(|r| format!("{r}\n")).collect()
}

fn open(rows: &[String]) -> Option<OpenSwitch> {
    open_model_switch_in(&body(rows), Some(SID))
}

type Seed<'a> = (i64, &'a str, Outcome, &'a str, String);

fn seed_ledger(dir: &Path, name: &str, rows: &[Seed<'_>]) -> PathBuf {
    let path = dir.join(format!("{name}.jsonl"));
    let body: String = rows
        .iter()
        .map(|(ts, rule_id, outcome, command, reason)| {
            format!("{}\n", row_at(*ts, rule_id, *outcome, command, reason))
        })
        .collect();
    std::fs::write(&path, body).expect("write");
    path
}

const FILLER: &str = "cd /Users//someone/Developer/project && cargo test -p project-core --lib -- parser::tests 2>&1 | tail -40";
const FILLER_WHY: &str = "the safe rules proved the command reads and runs tests";

#[test]
fn a_fable_move_opens_the_switch_with_its_reason_s_fields() {
    let reason = model_switch_reason(Some("fable"), None, Some(5_000), 4_000);
    assert_eq!(
        reason,
        "the turn-end policy (model switch: from=fable launch=- back_at=5000 made=4000)"
    );
    assert_eq!(
        open(&[switch_row(4_000_123, None, Some(5_000), 4_000)]),
        Some(OpenSwitch {
            from: Some("fable".to_string()),
            to: FABLE_FALLBACK.to_string(),
            launch: None,
            back_at_unix: Some(5_000),
            asked: false,
            made_unix: 4_000,
            proc: None,
        })
    );
    let unreset = open(&[switch_row(4_000_123, None, None, 4_000)]).expect("the switch");
    assert_eq!(
        unreset.back_at_unix, None,
        "no named reset: no return scheduled"
    );
    // CONTROL: an ordinary act opens nothing.
    assert_eq!(
        open(&[row_at(
            1_000,
            RULE_CONTINUE,
            Outcome::Typed,
            "keep going",
            &act_reason(None)
        )]),
        None
    );
}

#[test]
fn a_launch_naming_fable_is_restored_as_the_switch_s_launch() {
    let reason = model_switch_reason(Some("fable"), Some("claude-fable-5-2"), None, 7);
    assert!(reason.contains(" launch=claude-fable-5-2 "), "{reason}");
    let named = open(&[row_at(
        7_000,
        RULE_MODEL_FALLBACK,
        Outcome::Typed,
        &move_command(),
        &reason,
    )])
    .expect("the switch");
    assert_eq!(named.launch.as_deref(), Some("claude-fable-5-2"));
    assert_eq!(named.to, FABLE_FALLBACK);
    assert_eq!(named.made_unix, 7);
    // CONTROL: a launch that named none reads back as none.
    assert_eq!(
        open(&[switch_row(7_000, None, None, 7)])
            .expect("the switch")
            .launch,
        None
    );
}

#[test]
fn an_older_build_s_switch_row_opens_nothing() {
    // The reason an older build wrote (before 2026-11-09's removal): no `launch=`.
    let older = "the turn-end policy (model switch: from=opus back_at=- made=1)";
    assert_eq!(
        open(&[row_at(
            1_000,
            RULE_MODEL_FALLBACK,
            Outcome::Typed,
            "relaunch --model sonnet",
            older
        )]),
        None
    );
    // A typed move row whose reason is no switch's at all opens nothing either.
    assert_eq!(
        open(&[row_at(
            1_000,
            RULE_MODEL_FALLBACK,
            Outcome::Typed,
            &move_command(),
            "the turn-end policy"
        )]),
        None
    );
    // An older row after an open switch leaves that switch as it was, apart from the ask.
    let standing = open(&[switch_row(1_000, None, Some(9), 1)]).expect("the switch");
    let after = open(&[
        switch_row(1_000, None, Some(9), 1),
        row_at(
            2_000,
            RULE_MODEL_FALLBACK,
            Outcome::Typed,
            "relaunch --model sonnet",
            older,
        ),
    ])
    .expect("the switch stands");
    assert_eq!(
        (
            after.to,
            after.from,
            after.launch,
            after.back_at_unix,
            after.made_unix
        ),
        (
            standing.to,
            standing.from,
            standing.launch,
            standing.back_at_unix,
            standing.made_unix
        )
    );
    // CONTROL: the same row with `launch=` opens the switch.
    let current = "the turn-end policy (model switch: from=opus launch=- back_at=- made=1)";
    assert!(
        open(&[row_at(
            1_000,
            RULE_MODEL_FALLBACK,
            Outcome::Typed,
            "relaunch --model sonnet",
            current
        )])
        .is_some()
    );
}

#[test]
fn a_model_restore_closes_the_switch() {
    let switch = switch_row(1_000, None, Some(9), 1);
    for back in [
        BACK_UNNAMED.to_string(),
        "relaunch --model claude-fable-5-2".to_string(),
    ] {
        let back = row_at(
            2_000,
            RULE_MODEL_RESTORE,
            Outcome::Typed,
            &back,
            &act_reason(None),
        );
        assert_eq!(open(&[switch.clone(), back.clone()]), None, "{back}");
        // A restore before the switch closes nothing the switch opens after it.
        assert!(open(&[back, switch.clone()]).is_some());
    }
    // The tab's process ended with another program on another conversation: the switch is over.
    let closed = row_at(
        3_000,
        RULE_MODEL_RESTORE,
        Outcome::Waiting,
        "",
        SWITCH_CLOSED,
    );
    assert_eq!(open(&[switch.clone(), closed]), None);
    // CONTROL: a waiting restore row with another reason closes nothing.
    let other = row_at(3_000, RULE_MODEL_RESTORE, Outcome::Waiting, "", "a note");
    assert!(open(&[switch, other]).is_some());
}

#[test]
fn an_account_move_leaves_the_switch_open() {
    let switch = switch_row(1_000, Some("claude-fable-5-2"), Some(9), 1);
    let was = open(std::slice::from_ref(&switch)).expect("the switch");
    let moved = row_at(
        2_000,
        RULE_ACCOUNT,
        Outcome::Typed,
        &format!("{ACCOUNT_MOVE_COMMAND}work"),
        &account_switch_reason("work", AccountAt::Wall),
    );
    let after = open(&[switch, moved]).expect("the relaunch keeps the line's --model");
    // The return is still owed: what it puts back and when are as the switch made them. The
    // account move is an act after the switch, so the switch reads as asked.
    assert_eq!(after, OpenSwitch { asked: true, ..was });
}

#[test]
fn a_turn_asked_on_the_fallback_marks_the_switch_asked() {
    let switch = switch_row(1_000, None, Some(9), 1);
    let asked = row_at(
        2_000,
        RULE_MODEL_FALLBACK,
        Outcome::Waiting,
        "",
        FALLBACK_ASKED,
    );
    assert!(open(&[switch.clone(), asked.clone()]).expect("open").asked);
    let act = row_at(
        2_000,
        RULE_CONTINUE,
        Outcome::Typed,
        "keep going",
        &act_reason(None),
    );
    assert!(open(&[switch.clone(), act]).expect("open").asked);
    // The switch's own turn, typed under the fallback's rule with no relaunch command.
    let own = row_at(
        2_000,
        RULE_MODEL_FALLBACK,
        Outcome::Typed,
        "keep going",
        "-",
    );
    assert!(open(&[switch.clone(), own]).expect("open").asked);
    // A record with no switch open opens none.
    assert_eq!(open(std::slice::from_ref(&asked)), None);
    // A new switch after the ask is not asked.
    let again = switch_row(3_000, None, Some(19), 3);
    let fresh = open(&[switch.clone(), asked, again]).expect("open");
    assert!(!fresh.asked && fresh.made_unix == 3, "{fresh:?}");
    // CONTROL: a waiting row under another rule asks nothing.
    let other = row_at(
        2_000,
        RULE_POINT,
        Outcome::Waiting,
        "",
        point_reason(Carried::streak(1), false).as_str(),
    );
    assert!(!open(&[switch, other]).expect("open").asked);
}

#[test]
fn the_switch_s_process_row_puts_it_in_that_process() {
    let switch = switch_row(1_000, None, Some(9), 1);
    let p = SwitchProc {
        pid: 4242,
        start: 1_791_000_000,
        conversation: Some("0f1e2d3c-aaaa-bbbb-cccc-000000000001".to_string()),
    };
    let reason = switch_process_reason(&p);
    assert!(reason.contains("proc=4242@1791000000"), "{reason}");
    let moved = row_at(2_000, RULE_MODEL_FALLBACK, Outcome::Waiting, "", &reason);
    assert_eq!(
        open(&[switch.clone(), moved.clone()]).expect("open").proc,
        Some(p.clone())
    );
    let unnamed = SwitchProc {
        conversation: None,
        ..p.clone()
    };
    let unnamed_row = row_at(
        2_000,
        RULE_MODEL_FALLBACK,
        Outcome::Waiting,
        "",
        &switch_process_reason(&unnamed),
    );
    assert_eq!(
        open(&[switch.clone(), unnamed_row]).expect("open").proc,
        Some(unnamed)
    );
    // The process row moves nothing else: the switch is not asked by it.
    assert!(!open(&[switch.clone(), moved.clone()]).expect("open").asked);
    // A process row before any switch opens none, and a new switch starts in no process.
    assert_eq!(open(std::slice::from_ref(&moved)), None);
    let again = switch_row(3_000, None, None, 3);
    assert_eq!(
        open(&[switch.clone(), moved.clone(), again])
            .expect("open")
            .proc,
        None
    );
    // CONTROL: the same reason under the restore's rule is no process row.
    let elsewhere = row_at(2_000, RULE_MODEL_RESTORE, Outcome::Waiting, "", &reason);
    assert_eq!(open(&[switch, elsewhere]).expect("open").proc, None);
}

#[test]
fn a_switch_is_read_for_its_own_tab() {
    let theirs = row_of(
        1_000,
        RULE_MODEL_FALLBACK,
        Outcome::Typed,
        &move_command(),
        &model_switch_reason(Some("fable"), None, None, 1),
        "@s-10",
    );
    let b = body(&[theirs]);
    assert_eq!(open_model_switch_in(&b, Some(SID)), None);
    assert!(
        open_model_switch_in(&b, Some("s-10")).is_some(),
        "the `@` is no part of the sid"
    );
    assert!(open_model_switch_in(&b, Some("@s-10")).is_some());
    assert_eq!(open_model_switch_in(&b, None), None);
    // Another tab's restore closes nothing of this tab's.
    let mine = switch_row(1_000, None, None, 1);
    let their_back = row_of(
        2_000,
        RULE_MODEL_RESTORE,
        Outcome::Typed,
        BACK_UNNAMED,
        "-",
        "@s-10",
    );
    assert!(open(&[mine, their_back]).is_some());
}

#[test]
fn the_switch_is_read_from_the_ledger_s_file() {
    let dir = crate::supervise::ScratchDir::new("apr-switch", "file");
    let path = seed_ledger(
        &dir,
        "switch",
        &[(
            1_000,
            RULE_MODEL_FALLBACK,
            Outcome::Typed,
            &move_command(),
            model_switch_reason(Some("fable"), None, Some(9), 1),
        )],
    );
    assert_eq!(
        open_model_switch(&path, Some(SID)).map(|o| o.to).as_deref(),
        Some(FABLE_FALLBACK)
    );
    // CONTROL: no file, no switch.
    assert_eq!(
        open_model_switch(&dir.join("absent.jsonl"), Some(SID)),
        None
    );
}

#[test]
fn a_standing_switch_survives_a_ledger_past_sixteen_mib_cut() {
    let dir = crate::supervise::ScratchDir::new("apr-switch", "big");
    let past = 17 << 20;
    let ledger = path_under(&dir, Some(SID)).expect("the drive dir");
    let mut body = format!("{}\n", switch_row(1_000, None, None, 1));
    let mut ts = 2_000;
    while body.len() <= past {
        body.push_str(&row_at(
            ts,
            "allow-once@v1",
            Outcome::Approved,
            "echo captured",
            "an ordinary approval",
        ));
        body.push('\n');
        ts += 1;
    }
    std::fs::write(&ledger, &body).expect("write");
    let standing = open_model_switch_in(&body, Some(SID));
    assert!(standing.is_some(), "the switch stands");
    bound(&ledger);
    let cut = std::fs::read_to_string(&ledger).expect("read");
    assert_eq!(cut.lines().count(), KEEP_ROWS + 1, "the ledger is cut");
    assert!(cut.len() as u64 <= MAX_BYTES, "{} bytes", cut.len());
    assert_eq!(
        open_model_switch_in(&cut, Some(SID)),
        standing,
        "the standing switch survives the cut"
    );
}

#[test]
fn a_cut_keeps_a_standing_switch_past_runs_of_records_and_acts() {
    let dir = crate::supervise::ScratchDir::new("apr-switch", "cut");
    let filler: Vec<Seed<'_>> = (0..3000)
        .map(|i| {
            (
                i,
                "allow-once@v1",
                Outcome::Approved,
                FILLER,
                FILLER_WHY.to_string(),
            )
        })
        .collect();
    let t = 1_000_000_i64;
    let command = move_command();
    let switch: Seed<'_> = (
        t,
        RULE_MODEL_FALLBACK,
        Outcome::Typed,
        command.as_str(),
        model_switch_reason(Some("fable"), None, Some(t / 1000 + 3 * 86_400), t / 1000),
    );
    let point = |ts: i64, n: u32| -> Seed<'static> {
        (
            ts,
            RULE_POINT,
            Outcome::Waiting,
            "",
            point_reason(Carried::streak(n), false),
        )
    };
    let read = |path: &Path| open_model_switch(path, Some(SID));
    // A run of records: points and turn-ran rows.
    let mut records = filler.clone();
    records.push(switch.clone());
    for n in 1..=1100_u32 {
        let at = t + 10_000 + 300_000 * i64::from(n);
        records.push((
            at,
            RULE_TURN_RAN,
            Outcome::Waiting,
            "",
            TURN_RAN.to_string(),
        ));
        records.push(point(at + 20_000, n));
    }
    // A run of acts.
    let mut acts = filler.clone();
    acts.push(switch.clone());
    let retry = "Claude Code reported an API error (overloaded). Carry on from where you stopped.";
    for n in 1..=1100_i64 {
        let at = t + 10_000 + 300_000 * n;
        acts.push((at, RULE_API_RETRY, Outcome::Typed, retry, act_reason(None)));
        acts.push(point(at + 3_000, 0));
    }
    for (name, rows) in [("records", &records), ("acts", &acts)] {
        let path = seed_ledger(&dir, name, rows);
        assert!(
            std::fs::metadata(&path).unwrap().len() > MAX_BYTES,
            "{name}"
        );
        let before = read(&path);
        assert!(
            before
                .as_ref()
                .is_some_and(|o| o.to == FABLE_FALLBACK && o.asked == (name == "acts")),
            "{name}: {before:?}"
        );
        bound(&path);
        let kept = std::fs::read_to_string(&path).unwrap();
        assert!(
            kept.len() as u64 <= MAX_BYTES,
            "{name}: {} bytes",
            kept.len()
        );
        assert_eq!(read(&path), before, "{name}");
        let path = seed_ledger(&dir, &format!("{name}-plain"), rows);
        crate::harness::cli::bound_ledger(&path, MAX_BYTES, KEEP_ROWS).expect("cut");
        assert_eq!(
            read(&path),
            None,
            "CONTROL: {name}: a plain cut loses the switch"
        );
    }
    // A switch made back stays closed through the cut.
    let mut over = acts.clone();
    let end = t + 10_000 + 300_000 * 1101;
    over.push((
        end,
        RULE_MODEL_RESTORE,
        Outcome::Typed,
        BACK_UNNAMED,
        act_reason(None),
    ));
    over.push((
        end + 1,
        RULE_CONTINUE,
        Outcome::Typed,
        "keep going",
        act_reason(None),
    ));
    let path = seed_ledger(&dir, "over", &over);
    assert_eq!(read(&path), None);
    bound(&path);
    let cut = std::fs::read_to_string(&path).unwrap();
    let acts_kept = cut.lines().filter(|l| record_of(l).is_none()).count();
    assert_eq!(acts_kept, KEEP_ROWS, "{} rows", cut.lines().count());
    assert_eq!(read(&path), None);
    // A switch with no named reset under 4000 asked acts: kept through the cut, asked.
    let mut held: Vec<Seed<'_>> = vec![(
        t,
        RULE_MODEL_FALLBACK,
        Outcome::Typed,
        command.as_str(),
        model_switch_reason(Some("fable"), None, None, t / 1000),
    )];
    for n in 1..=4000_i64 {
        let at = t + 600_000 * n;
        held.push((
            at,
            RULE_CONTINUE,
            Outcome::Typed,
            "keep going",
            act_reason(None),
        ));
        held.push(point(at + 300_000, 0));
    }
    let path = seed_ledger(&dir, "switch-held", &held);
    let before = read(&path).expect("the switch");
    assert!(before.asked && before.back_at_unix.is_none(), "{before:?}");
    bound(&path);
    let kept = std::fs::read_to_string(&path).unwrap();
    assert!(kept.len() as u64 <= MAX_BYTES, "{} bytes", kept.len());
    assert_eq!(read(&path), Some(before));
}

#[test]
fn a_cut_keeps_a_land_episode_and_a_switch_standing_together() {
    let dir = crate::supervise::ScratchDir::new("apr-switch", "cut-land");
    let sid = Some(SID);
    let mut rows: Vec<Seed<'_>> = (0..3000)
        .map(|i| {
            (
                i,
                "allow-once@v1",
                Outcome::Approved,
                FILLER,
                FILLER_WHY.to_string(),
            )
        })
        .collect();
    let t = 1_000_000_i64;
    let command = move_command();
    rows.push((
        t,
        RULE_MODEL_FALLBACK,
        Outcome::Typed,
        command.as_str(),
        model_switch_reason(Some("fable"), None, Some(t / 1000 + 3 * 86_400), t / 1000),
    ));
    for (k, rule) in [RULE_LAND, RULE_LAND_NUDGE, RULE_LAND_NUDGE]
        .into_iter()
        .enumerate()
    {
        let at = t + 1_000 + 1_000 * i64::try_from(k).unwrap();
        rows.push((at, rule, Outcome::Typed, "(land)", act_reason(None)));
        rows.push((
            at + 500,
            RULE_POINT,
            Outcome::Waiting,
            "",
            point_reason(Carried::streak(0), false),
        ));
    }
    let retry = "Claude Code reported an API error (overloaded). Carry on from where you stopped.";
    for n in 1..=1100_i64 {
        let at = t + 10_000 + 300_000 * n;
        rows.push((at, RULE_API_RETRY, Outcome::Typed, retry, act_reason(None)));
        rows.push((
            at + 3_000,
            RULE_POINT,
            Outcome::Waiting,
            "",
            point_reason(Carried::streak(0), false),
        ));
    }
    let read = |path: &Path| {
        (
            open_model_switch(path, sid).map(|o| o.to),
            land_episode(path, sid),
            short_streak(path, sid),
        )
    };
    let path = seed_ledger(&dir, "both", &rows);
    assert!(std::fs::metadata(&path).unwrap().len() > MAX_BYTES);
    let before = read(&path);
    assert!(before.0.is_some() && before.1.is_some(), "{before:?}");
    bound(&path);
    let kept = std::fs::read_to_string(&path).unwrap();
    assert_eq!(read(&path), before);
    let first = kept.lines().next().expect("rows");
    assert!(
        first.contains(RULE_MODEL_FALLBACK),
        "the switch row leads: {first}"
    );
    let path = seed_ledger(&dir, "both-plain", &rows);
    crate::harness::cli::bound_ledger(&path, MAX_BYTES, KEEP_ROWS).expect("cut");
    let plain = read(&path);
    assert!(plain.0.is_none() && plain.1.is_none(), "CONTROL: {plain:?}");
}

#[test]
fn a_cut_keeps_every_standing_switch_row_when_its_workspace_is_full() {
    let mut rows = vec![String::new()];
    for sid in ["@s-9", "@s-10"] {
        rows.push(row_of(
            1_000,
            RULE_MODEL_FALLBACK,
            Outcome::Typed,
            &move_command(),
            &model_switch_reason(Some("fable"), Some("claude-fable-5-2"), None, 1),
            sid,
        ));
    }
    for i in 0..KEEP_ROWS + 8 {
        rows.push(row_at(
            i64::try_from(i).unwrap() + 2_000,
            "allow-once@v1",
            Outcome::Approved,
            "echo captured",
            "a captured ordinary approval",
        ));
    }
    rows.push(String::new());
    let body = rows.join("\n") + "\n";
    let original: Vec<String> = body.lines().map(str::to_string).collect();
    let path = Path::new("/captured/self.approvals.jsonl");
    let read = |_: &Path| None;
    let cut_of = |body: &str, work: &mut CutWork| {
        cut_rows_with_work(path, &mut body.lines().map(str::to_string), &read, work).expect("held")
    };
    let held = |body: &str| -> Vec<String> {
        let rows: Vec<&str> = body.lines().filter(|l| !l.trim().is_empty()).collect();
        let from = rows.len() - KEEP_ROWS;
        rows.iter()
            .enumerate()
            .filter(|(i, l)| {
                *i >= from
                    || LandWalk::reads(l)
                    || SwitchWalk::reads(l)
                    || l.contains(RULE_EARLIER_BOOKS)
            })
            .map(|(_, l)| (*l).to_string())
            .collect()
    };
    let cut = cut_of(&body, &mut CutWork::for_cut());
    assert!(cut.len() < original.len(), "an ordinary cut still cuts");
    for sid in ["@s-9", "@s-10"] {
        let standing = open_model_switch_in(&body, Some(sid));
        assert!(
            standing
                .as_ref()
                .is_some_and(|o| o.launch.as_deref() == Some("claude-fable-5-2")),
            "{sid}: {standing:?}"
        );
        assert_eq!(
            open_model_switch_in(&cut.join("\n"), Some(sid)),
            standing,
            "the standing switch survives"
        );
    }
    for mut work in [
        CutWork {
            entries: 1,
            visits: usize::MAX,
            bytes: usize::MAX,
        },
        CutWork {
            entries: usize::MAX,
            visits: 1,
            bytes: usize::MAX,
        },
        CutWork {
            entries: usize::MAX,
            visits: usize::MAX,
            bytes: 1,
        },
    ] {
        let kept = cut_of(&body, &mut work);
        assert_eq!(
            kept,
            held(&body),
            "STANDING_WORKSPACE_EXCESS_MUST_PRESERVE_EVERY_STANDING_ROW"
        );
        for sid in ["@s-9", "@s-10"] {
            assert_eq!(
                open_model_switch_in(&kept.join("\n"), Some(sid)),
                open_model_switch_in(&body, Some(sid)),
                "the standing switch survives an unadmitted workspace"
            );
        }
    }
}

#[test]
fn a_restored_tab_s_cut_keeps_its_take_up_while_the_earlier_tab_s_switch_stands() {
    let dir = crate::supervise::ScratchDir::new("apr-switch", "restored-cut");
    let root: &Path = &dir;
    let (was, tab) = ("s-a0", "s-b1");
    std::fs::create_dir_all(root.join("drive")).expect("the drive");
    let row = |ts: i64, rule_id: &str, outcome: Outcome, command: &str, reason: &str, sid| {
        format!("{}\n", row_of(ts, rule_id, outcome, command, reason, sid))
    };
    let t = 1_000_000_i64;
    let switch = model_switch_reason(Some("fable"), None, Some(t / 1000 + 3 * 86_400), t / 1000);
    let theirs = ledger_under(root, Some(was));
    std::fs::write(
        &theirs,
        row(
            t,
            RULE_MODEL_FALLBACK,
            Outcome::Typed,
            &move_command(),
            &switch,
            was,
        ),
    )
    .expect("the earlier tab's books");
    crate::harness::upgrade_drive::note_earlier_tab_under(root, tab, was);
    let approvals = |from: i64| -> String {
        (0..3000)
            .map(|i| {
                row(
                    from + i,
                    "allow-once@v1",
                    Outcome::Approved,
                    FILLER,
                    FILLER_WHY,
                    tab,
                )
            })
            .collect()
    };
    let take_up = row(
        t + 1_000,
        RULE_EARLIER_BOOKS,
        Outcome::Waiting,
        "",
        EARLIER_BOOKS,
        tab,
    );
    let books = |path: &Path| {
        let own = std::fs::read_to_string(path).unwrap_or_default();
        let earlier = vec![(was.to_string(), std::fs::read_to_string(&theirs).unwrap())];
        let up = takes_up_earlier(&own, Some(tab));
        let books = if up {
            books_through(&own, Some(tab), &earlier)
        } else {
            own
        };
        (up, open_model_switch_in(&books, Some(tab)).map(|o| o.to))
    };
    let path = ledger_under(root, Some(tab));
    std::fs::write(&path, format!("{take_up}{}", approvals(t + 2_000))).expect("the tab's");
    assert!(std::fs::metadata(&path).unwrap().len() > MAX_BYTES);
    let before = books(&path);
    assert_eq!(before, (true, Some(FABLE_FALLBACK.to_string())));
    bound(&path);
    assert_eq!(books(&path), before, "the switch stands: the take-up kept");
    let back = row(
        t + 1_500,
        RULE_MODEL_RESTORE,
        Outcome::Typed,
        BACK_UNNAMED,
        &act_reason(None),
        tab,
    );
    std::fs::write(&path, format!("{take_up}{back}{}", approvals(t + 2_000))).expect("over");
    assert_eq!(books(&path), (true, None));
    bound(&path);
    assert_eq!(books(&path), (false, None), "the take-up cut");
    std::fs::write(&path, format!("{take_up}{}", approvals(t + 2_000))).expect("plain");
    crate::harness::cli::bound_ledger(&path, MAX_BYTES, KEEP_ROWS).expect("cut");
    assert_eq!(books(&path), (false, None), "CONTROL");
}

#[test]
fn a_second_restore_reads_no_switch_the_tab_between_ended_before_its_cut() {
    let dir = crate::supervise::ScratchDir::new("apr-switch", "restored-chain");
    let root: &Path = &dir;
    let (a, b, c) = ("s-a0", "s-b1", "s-c2");
    std::fs::create_dir_all(root.join("drive")).expect("the drive");
    let row = |ts: i64, rule_id: &str, outcome: Outcome, command: &str, reason: &str, sid| {
        format!("{}\n", row_of(ts, rule_id, outcome, command, reason, sid))
    };
    let t = 1_000_000_i64;
    let switch = model_switch_reason(Some("fable"), None, Some(t / 1000 + 3 * 86_400), t / 1000);
    std::fs::write(
        ledger_under(root, Some(a)),
        row(
            t,
            RULE_MODEL_FALLBACK,
            Outcome::Typed,
            &move_command(),
            &switch,
            a,
        ),
    )
    .expect("A's books");
    crate::harness::upgrade_drive::note_earlier_tab_under(root, b, a);
    let take_up = row(
        t + 1_000,
        RULE_EARLIER_BOOKS,
        Outcome::Waiting,
        "",
        EARLIER_BOOKS,
        b,
    );
    let back = row(
        t + 1_500,
        RULE_MODEL_RESTORE,
        Outcome::Typed,
        BACK_UNNAMED,
        &act_reason(None),
        b,
    );
    let approvals: String = (0..3000)
        .map(|i| {
            row(
                t + 2_000 + i,
                "allow-once@v1",
                Outcome::Approved,
                FILLER,
                FILLER_WHY,
                b,
            )
        })
        .collect();
    let books_of = |tab: &str| -> String {
        let path = ledger_under(root, Some(tab));
        let read = |p: &Path| std::fs::read_to_string(p).ok();
        let own = read(&path).unwrap_or_default();
        let earlier: Vec<(String, String)> =
            crate::harness::upgrade_drive::earlier_ledgers_in(&path, Some(tab), &read)
                .into_iter()
                .filter_map(|(t, p)| Some((t, read(&p)?)))
                .collect();
        if takes_up_earlier(&own, Some(tab)) && earlier.iter().any(|(t, b)| holds_rows_of(b, t)) {
            books_through(&own, Some(tab), &earlier)
        } else {
            own
        }
    };
    let switched = |tab: &str| open_model_switch_in(&books_of(tab), Some(tab)).map(|o| o.to);
    let bp = ledger_under(root, Some(b));
    std::fs::write(&bp, format!("{take_up}{back}{approvals}")).expect("B's books");
    assert_eq!(switched(b), None, "B switched back");
    bound(&bp);
    assert!(
        !took_up_earlier(&std::fs::read_to_string(&bp).unwrap(), b),
        "B's take-up cut"
    );
    assert_eq!(switched(b), None, "B after its cut");
    crate::harness::upgrade_drive::note_earlier_tab_under(root, c, b);
    assert_eq!(switched(c), None, "C reads no switch B ended");
    let cont = dir.join("continuous.jsonl");
    let a_rows = std::fs::read_to_string(ledger_under(root, Some(a)))
        .unwrap()
        .replace("\"sid\":\"s-a0\"", "\"sid\":\"s-b1\"");
    std::fs::write(&cont, format!("{a_rows}{back}{approvals}")).unwrap();
    bound(&cont);
    assert!(
        open_model_switch(&cont, Some(b)).is_none(),
        "CONTROL: continuous"
    );
    std::fs::write(&bp, format!("{take_up}{approvals}")).expect("B's books, no switch back");
    bound(&bp);
    assert!(
        took_up_earlier(&std::fs::read_to_string(&bp).unwrap(), b),
        "CONTROL: kept"
    );
    assert_eq!(
        switched(c).as_deref(),
        Some(FABLE_FALLBACK),
        "CONTROL: through B"
    );
}

#[test]
fn the_short_streak_skips_the_model_move_and_its_return() {
    let dir = crate::supervise::ScratchDir::new("apr-switch", "streak");
    let act = |ts: i64, n: u32| -> Seed<'static> {
        (
            ts,
            RULE_CONTINUE,
            Outcome::Typed,
            "keep going",
            act_reason_after(Carried::streak(n), None),
        )
    };
    let point = |ts: i64, n: u32| -> Seed<'static> {
        (
            ts,
            RULE_POINT,
            Outcome::Waiting,
            "",
            point_reason(Carried::streak(n), false),
        )
    };
    let command = move_command();
    let relaunch: Seed<'_> = (
        9_000,
        RULE_MODEL_FALLBACK,
        Outcome::Typed,
        command.as_str(),
        model_switch_reason(Some("fable"), None, None, 9),
    );
    let back: Seed<'_> = (
        9_500,
        RULE_MODEL_RESTORE,
        Outcome::Typed,
        BACK_UNNAMED,
        act_reason(None),
    );
    let sid = Some(SID);
    for (tag, moved) in [("move", relaunch.clone()), ("back", back.clone())] {
        let path = seed_ledger(
            &dir,
            &format!("streak-{tag}"),
            &[act(1_000, 2), act(2_000, 3), point(3_000, 4), moved.clone()],
        );
        let s = short_streak(&path, sid).expect("a streak");
        assert_eq!((s.at_ms, s.short, s.point), (3_000, 4, true), "{tag}");
        let path = seed_ledger(
            &dir,
            &format!("streak-{tag}-act-after"),
            &[act(1_000, 2), point(3_000, 4), moved, act(10_000, 4)],
        );
        let s = short_streak(&path, sid).expect("a streak");
        assert_eq!(
            (s.at_ms, s.short, s.point, s.recorded),
            (10_000, 4, false, true),
            "{tag}"
        );
    }
    // CONTROL: an ordinary act in the move's place is the last act.
    let path = seed_ledger(
        &dir,
        "streak-act",
        &[act(1_000, 2), act(2_000, 3), point(3_000, 4), act(9_000, 0)],
    );
    let s = short_streak(&path, sid).expect("a streak");
    assert_eq!((s.at_ms, s.point), (9_000, false));
}

#[test]
fn a_fallback_ask_between_a_try_and_its_first_read_is_no_read() {
    let dir = crate::supervise::ScratchDir::new("apr-switch", "first-read");
    let limit = LimitTry {
        bucket: false,
        in_place: false,
        attempts: 1,
        entitlement: true,
    };
    let wall = || {
        wall_wait_reason(
            None,
            None,
            aterm_phase::WallKind::Spend,
            "You've hit your monthly spend limit.",
        )
    };
    let mut rows: Vec<Seed<'_>> = vec![
        (1_000, RULE_WALL_WAIT, Outcome::Waiting, "", wall()),
        (
            10_000,
            RULE_LIMIT_RESUME,
            Outcome::Typed,
            "keep going",
            act_reason(Some(limit)),
        ),
        (
            12_000,
            RULE_MODEL_FALLBACK,
            Outcome::Waiting,
            "",
            FALLBACK_ASKED.to_string(),
        ),
        (30_000, RULE_WALL_WAIT, Outcome::Waiting, "", wall()),
    ];
    let path = seed_ledger(&dir, "first-read", &rows);
    let a = awaited_try(&path, Some(SID)).expect("the try");
    assert_eq!((a.read_ms, a.unread, a.met), (Some(30_000), false, true));
    let u = usage_attempt(&path, Some(SID)).expect("the try's episode");
    assert_eq!((u.read_ms, u.met_ms), (Some(30_000), Some(30_000)));
    rows.pop();
    let path = seed_ledger(&dir, "first-read-unread", &rows);
    let a = awaited_try(&path, Some(SID)).expect("the try");
    assert!(a.unread && a.read_ms.is_none(), "{a:?}");
}

/// A fact row's typed line (`fact-hold@v1`).
fn fact_typed(ts: i64) -> String {
    let line = crate::supervise::occupancy::templates::wait_nudge(30 * 60_000);
    row_at(
        ts,
        FactKind::Hold.rule(),
        Outcome::Typed,
        line.text(),
        "rule=wait-nudge@v1 episode=00000000000000aa held_ms=1800000",
    )
}

/// A held verdict's row (`fact-held@v1`): `waiting`, its episode and guard.
fn fact_held(ts: i64) -> String {
    use crate::supervise::occupancy::rules::{HELD_RULE, RuleId, held_reason};
    row_at(
        ts,
        HELD_RULE,
        Outcome::Waiting,
        "",
        &held_reason(
            RuleId::WaitNudge,
            0xaa,
            0,
            1_800_000,
            false,
            crate::supervise::Guard::Person,
        ),
    )
}

#[test]
fn a_fact_line_asks_nothing_on_a_model_switch() {
    let switch = switch_row(1_000, None, None, 1);
    let read = |rows: &[String]| open(rows).map(|o| o.asked);
    let plain = read(std::slice::from_ref(&switch));
    assert_eq!(plain, Some(false));
    let told = [switch.clone(), fact_typed(9_000)];
    let held = [switch, fact_held(9_000)];
    assert_eq!(read(&told), plain, "a fact line moves no reader of acts");
    assert_eq!(
        read(&held),
        plain,
        "a held verdict's row moves no reader of acts"
    );
    assert_eq!(
        with_facts_counted(|| read(&held)),
        plain,
        "read as an act, a held verdict's row is still no act: it is waiting"
    );
    assert_eq!(
        with_facts_counted(|| read(&told)),
        Some(true),
        "CONTROL: read as an act, the fact line asks the switch"
    );
}
