// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::harness::login_latch::LoginLatches;
use crate::harness::repo_work::RepoWork;
use crate::supervise::policy::login::LoginShared;
use crate::supervise::policy::turn_end::{
    DONE_CHECK, FINISHED, LAND, LAND_NUDGE, RULE_CONTINUE, RULE_DONE_CHECK, RULE_LAND, RULE_LEFT,
    RULE_LOGIN, RULE_LOGIN_BACK, RULE_SUGGESTION, repo_work_suffix,
};
use std::sync::PoisonError;

#[path = "remembered_limit_tests.rs"]
mod remembered_limit;

#[path = "walls_tests.rs"]
mod walls;

#[path = "hold_journal_tests.rs"]
mod hold_journal;

#[path = "seed_matrix_tests.rs"]
mod seed_matrix;

#[path = "tap_clock_loop_tests.rs"]
mod tap_clock_loop;

fn hosted(max_s: u64) -> SuperviseOpts {
    SuperviseOpts {
        policy: SupervisorConfig {
            answer_questions: false,
            ..owners_policy()
        },
        ..auto(max_s)
    }
}

fn ended(said: &str) -> Vec<String> {
    let mut r = rows(&[
        &format!("⏺ {said}"),
        "",
        "✻ Worked for 12m 2s · done 4:24 PM",
        "",
    ]);
    r.extend(composer("  ⏵⏵ bypass permissions on (shift+tab to cycle)"));
    r
}

fn suggesting(said: &str) -> Vec<String> {
    let mut r = ended(said);
    let caret = r.iter().position(|row| row == "❯").expect("caret row");
    r[caret] = "❯ keep going".to_string();
    r
}

const STOP: &str = "I need your decision on the schema before I go on.";

fn ledger_at(tag: &str) -> (PathBuf, PathBuf) {
    let dir = crate::supervise::test_scratch_path("te-ledger", tag);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("tmp dir");
    let path = dir.join("s-1.jsonl");
    (dir, path)
}

#[test]
fn a_turn_end_after_work_is_continued_once_and_a_stop_phrase_escalates() {
    let (dir, ledger) = ledger_at("continue");
    let mut m = Mock::new(vec![
        busy_screen(),
        ended("Fixed the parser; the suite is green."),
        busy_screen(),
        ended(STOP),
    ]);
    m.turn_releases = Some(1);
    m.vanish_after = Some(2);
    let (lines, _) = watch_lines_with(&mut m, &hosted(30), |s| {
        s.set_approval_ledger(Some(ledger.clone()));
    });
    assert_eq!(
        lines,
        [
            "EVENT idle seq=102 ⏺ Fixed the parser; the suite is green.".to_string(),
            "CONTINUED seq=102 rule=continue@v1 keep going".to_string(),
            format!("EVENT idle seq=104 ⏺ {STOP}"),
            "EXIT session gone (await seq 104 failed: aterm-ctl: ERR exited)".to_string(),
        ],
        "{:#?}",
        m.requests
    );
    let turn = continue_request("keep going");
    assert_eq!(
        turn,
        "turn submit=guarded:^❯\\skeep\\x20going\\s*$ idle=600 timeout=2500 yield=0.2 keep going"
    );
    let at = m
        .requests
        .iter()
        .position(|r| *r == turn)
        .expect("the turn");
    assert_eq!(m.requests[at - 1], "status");
    assert_eq!(count(&m, "turn"), 1, "{:#?}", m.requests);
    assert!(
        m.attention
            .as_deref()
            .is_some_and(|a| a.starts_with("claude idle: ")),
        "{:?}",
        m.attention
    );
    let rows = std::fs::read_to_string(&ledger).unwrap_or_default();
    assert!(
        rows.lines()
            .any(|l| l.contains("\"rule_id\":\"continue@v1\"")
                && l.contains("\"decision\":\"typed\"")
                && l.contains("\"command\":\"keep going\"")),
        "{rows}"
    );
    let _ = std::fs::remove_dir_all(&dir);

    let mut m = Mock::new(vec![
        busy_screen(),
        ended("Fixed the parser; the suite is green."),
    ]);
    m.vanish_after = Some(2);
    let mut off = hosted(30);
    off.policy.continue_policy = false;
    let _ = watch_lines(&mut m, &off);
    assert_eq!(count(&m, "turn"), 0, "{:#?}", m.requests);
}

#[test]
fn a_done_report_is_checked_and_a_done_reply_is_checked_again() {
    let (dir, journal) = journal_file("done-check");
    let mut compacted = rows(&[
        "⏺ DONE",
        "",
        "✻ Worked for 12m 2s · done 4:24 PM",
        "  ⎿  Skills restored (workflow-authoring)",
        "",
        "⏺ Compacted while idle, before the prompt cache expired",
        "",
    ]);
    compacted.extend(composer("  ⏵⏵ bypass permissions on (shift+tab to cycle)"));
    let mut m = Mock::new(vec![
        busy_screen(),
        ended("Fixed the parser; all done."),
        busy_screen(),
        ended("The task is complete. The parser fix is committed and all tests passed."),
        busy_screen(),
        compacted,
    ]);
    m.turn_releases = Some(1);
    m.vanish_after = Some(4);
    m.stall_sleep = Some(Duration::from_secs(20));
    let opts = SuperviseOpts {
        policy: owners_policy(),
        journal: Some(journal.clone()),
        ..auto(30)
    };
    let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
        s.set_turn_end_timing(TurnEndTiming {
            short_backoff: Duration::from_millis(30),
            short_backoff_max: Duration::from_millis(60),
            ..TurnEndTiming::default()
        });
    });
    assert!(
        lines.contains(&format!(
            "CONTINUED seq=102 rule={RULE_DONE_CHECK} {}",
            clip(DONE_CHECK)
        )),
        "{lines:#?}"
    );
    let turns: Vec<&String> = m
        .requests
        .iter()
        .filter(|r| r.starts_with("turn "))
        .collect();
    assert!(
        turns.len() >= 2 && turns.iter().all(|r| **r == continue_request(DONE_CHECK)),
        "the check, and again: {:#?}",
        m.requests
    );
    let done: Vec<String> = journal_records(&journal)
        .0
        .into_iter()
        .map(|r| r.line)
        .filter(|l| l.starts_with("DONE "))
        .collect();
    assert!(done.is_empty(), "{done:#?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_check_answered_faster_than_any_read_is_judged_at_its_answer() {
    const IDLE: &str = "Both branches have landed and no lander is running.";
    let screen = |more: &[&str]| {
        let mut r = rows(&[
            "❯ land the two branches",
            "",
            &format!("⏺ {IDLE}"),
            "",
            "✻ Worked for 6s · done 10:54 PM",
            "",
        ]);
        r.extend(rows(more));
        r.extend(composer("  ⏵⏵ auto mode on (shift+tab to cycle)"));
        r
    };
    let other = screen(&[
        "❯ what is left?",
        "",
        &format!("⏺ {IDLE}"),
        "",
        "✻ Baked for 0s · done 11:01 PM",
        "",
    ]);
    let check_row = format!("❯ {DONE_CHECK}");
    let answer_row = format!("⏺ {IDLE}");
    let answered = screen(&[
        &check_row,
        "",
        &answer_row,
        "",
        "✻ Cogitated for 0s · done 11:01 PM",
        "",
    ]);
    for (case, after, ends) in [("fast", answered, true), ("other", other, false)] {
        let (dir, journal) = journal_file(&format!("fast-answer-{case}"));
        let mut m = Mock::new(vec![busy_screen(), screen(&[]), after]);
        m.turn_releases = Some(1);
        m.vanish_after = Some(10);
        m.stall_sleep = Some(Duration::from_millis(20));
        let opts = SuperviseOpts {
            policy: owners_policy(),
            journal: Some(journal.clone()),
            ..auto(30)
        };
        let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
            s.set_turn_end_timing(TurnEndTiming {
                short_backoff: Duration::from_millis(30),
                short_backoff_max: Duration::from_millis(60),
                take_within: Duration::from_millis(50),
                ..TurnEndTiming::default()
            });
        });
        let records: Vec<String> = journal_records(&journal)
            .0
            .into_iter()
            .map(|r| r.line)
            .collect();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            lines.contains(&format!(
                "CONTINUED seq=102 rule={RULE_DONE_CHECK} {}",
                clip(DONE_CHECK)
            )),
            "{case}: {lines:#?}"
        );
        let done: Vec<&String> = records.iter().filter(|l| l.starts_with("DONE ")).collect();
        let waited = records
            .iter()
            .any(|l| l.contains("the worker to take the continue-done-check@v1 act"));
        assert!(done.is_empty(), "{case}: {records:#?}");
        assert_eq!(waited, !ends, "{case}: {records:#?}");
    }
}

/// A host whose conversation's record ends on the turn the loop's last act
/// opened once it typed one ([`IdleHost::act_taken`]), closed in 41 ms —
/// Claude Code's record of a prompt it answered at once — and on the
/// person's `land the two branches` before: recorded at `at_ms`, the
/// loop's fixed clock.
#[derive(Debug)]
struct TypedRecordHost {
    typed: std::sync::atomic::AtomicBool,
    at_ms: u64,
}

impl IdleHost for TypedRecordHost {
    fn wants(&self) -> bool {
        false
    }
    fn at_idle(&self) -> Option<HostStep> {
        None
    }
    fn owns_turn_end(&self) -> bool {
        false
    }
    fn finished(&self) -> Option<bool> {
        Some(false)
    }
    fn act_taken(&self, _: crate::harness::acts::Act) {
        self.typed.store(true, std::sync::atomic::Ordering::SeqCst);
    }
    fn last_turn(&self) -> Option<crate::harness::upgrade::LastTurn> {
        let (said, ended) = if self.typed.load(std::sync::atomic::Ordering::SeqCst) {
            (DONE_CHECK, Duration::from_millis(41))
        } else {
            ("land the two branches", Duration::from_secs(6))
        };
        Some(crate::harness::upgrade::LastTurn {
            opened: Some("prompt:fast".to_string()),
            opened_said: Some(crate::supervise::approvals::words_hash(said)),
            ended: Some(ended),
            opened_ms: Some(self.at_ms),
            ..Default::default()
        })
    }
}

#[test]
fn the_record_tells_a_check_answered_faster_than_any_read() {
    const IDLE: &str = "Both branches have landed and no lander is running.";
    let screen = |more: &[&str]| {
        let mut r = rows(&[
            "❯ land the two branches",
            "",
            &format!("⏺ {IDLE}"),
            "",
            "✻ Worked for 6s · done 10:54 PM",
            "",
        ]);
        r.extend(rows(more));
        r.extend(composer("  ⏵⏵ auto mode on (shift+tab to cycle)"));
        r
    };
    let answer_row = format!("⏺ {IDLE}");
    let answered = screen(&[&answer_row, ""]);
    for (case, hosted, ends) in [("record", true, true), ("none", false, false)] {
        let host = Arc::new(TypedRecordHost {
            typed: std::sync::atomic::AtomicBool::new(false),
            at_ms: u64::try_from(TEST_NOW).expect("a time") * 1000,
        });
        let (dir, journal) = journal_file(&format!("fast-record-{case}"));
        let mut m = Mock::new(vec![busy_screen(), screen(&[]), answered.clone()]);
        m.turn_releases = Some(1);
        m.vanish_after = Some(10);
        m.stall_sleep = Some(Duration::from_millis(20));
        let opts = SuperviseOpts {
            policy: owners_policy(),
            journal: Some(journal.clone()),
            idle_host: hosted.then(|| Arc::clone(&host) as Arc<dyn IdleHost>),
            ..auto(30)
        };
        let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
            s.set_clock(TEST_NOW, PDT);
            s.set_turn_end_timing(TurnEndTiming {
                short_backoff: Duration::from_millis(30),
                short_backoff_max: Duration::from_millis(60),
                take_within: Duration::from_millis(50),
                ..TurnEndTiming::default()
            });
        });
        let records: Vec<String> = journal_records(&journal)
            .0
            .into_iter()
            .map(|r| r.line)
            .collect();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            lines.contains(&format!(
                "CONTINUED seq=102 rule={RULE_DONE_CHECK} {}",
                clip(DONE_CHECK)
            )),
            "{case}: {lines:#?}"
        );
        let done: Vec<&String> = records.iter().filter(|l| l.starts_with("DONE ")).collect();
        let waited = records
            .iter()
            .any(|l| l.contains("the worker to take the continue-done-check@v1 act"));
        assert!(done.is_empty(), "{case}: {records:#?}");
        assert_eq!(waited, !ends, "{case}: {records:#?}");
    }
}

fn assert_reaction_awaited(m: &Mock, write: &str) {
    let next = m
        .requests
        .iter()
        .skip_while(|r| !r.starts_with(write))
        .nth(1);
    let bound = format!(" timeout {}", turn_end_loop::REACTION_WAIT.as_millis());
    assert!(
        next.is_some_and(|r| r.starts_with("await seq ") && r.ends_with(&bound)),
        "after `{write}` the reaction is awaited{bound}: {:#?}",
        m.requests
    );
}

#[test]
fn the_workers_suggestion_is_accepted_with_the_accept_key_and_a_fenced_enter() {
    let mut m = Mock::new(vec![
        busy_screen(),
        suggesting("Stage 1 is in."),
        suggesting("Stage 1 is in."),
        busy_screen(),
        ended(STOP),
    ]);
    m.gen_fence = true;
    m.sends_gen = true;
    m.help = "key [id=<key>] [if=<re>] [if-gen=<e.s>] <name>: send a named key\n".to_string();
    m.screen_cols.insert(2, 12);
    m.vanish_after = Some(2);
    let (lines, _) = watch_lines(&mut m, &hosted(30));
    assert!(
        lines.contains(&format!(
            "CONTINUED seq=102 rule={RULE_SUGGESTION} keep going"
        )),
        "{lines:#?}\n{:#?}",
        m.requests
    );
    let keys: Vec<&String> = m.presses();
    assert_eq!(
        keys,
        [
            "key if-gen=1.102 if=^❯\\x20keep\\x20going\\s*$ right",
            "key if-gen=1.103 if=^❯\\x20keep\\x20going\\s*$ enter",
        ],
        "{:#?}",
        m.requests
    );
    assert_eq!(count(&m, "turn"), 0, "{:#?}", m.requests);
    assert_reaction_awaited(&m, "key if-gen=1.102");

    let mut m = Mock::new(vec![
        busy_screen(),
        suggesting("Stage 1 is in."),
        ended(STOP),
    ]);
    m.turn_releases = Some(1);
    m.vanish_after = Some(2);
    let (lines, _) = watch_lines(&mut m, &hosted(30));
    assert!(
        lines.contains(&format!(
            "CONTINUED seq=102 rule={RULE_SUGGESTION} keep going"
        )),
        "{lines:#?}"
    );
    assert!(m.presses().is_empty(), "{:#?}", m.requests);
    assert!(m.requests.contains(&continue_request("keep going")));
}

#[test]
fn a_torn_read_after_the_accept_key_is_read_again() {
    let torn = {
        let mut rows = suggesting("Stage 1 is in.");
        let caret = rows
            .iter()
            .position(|row| row == "❯ keep going")
            .expect("caret row");
        rows[caret] = String::new();
        rows
    };
    let accepting = |screens: Vec<Vec<String>>, filled_at: Option<usize>| {
        let mut m = Mock::new(screens);
        m.gen_fence = true;
        m.sends_gen = true;
        m.help = "key [id=<key>] [if=<re>] [if-gen=<e.s>] <name>: send a named key\n".to_string();
        m.idle_never = true;
        if let Some(at) = filled_at {
            m.screen_cols.insert(at, 12);
        }
        m.vanish_after = Some(2);
        m
    };
    let mut m = accepting(
        vec![
            busy_screen(),
            suggesting("Stage 1 is in."),
            torn.clone(),
            suggesting("Stage 1 is in."),
            busy_screen(),
            ended(STOP),
        ],
        Some(3),
    );
    let (lines, _) = watch_lines(&mut m, &hosted(30));
    assert!(
        lines.contains(&format!(
            "CONTINUED seq=102 rule={RULE_SUGGESTION} keep going"
        )),
        "{lines:#?}\n{:#?}",
        m.requests
    );
    assert_eq!(
        m.presses(),
        [
            "key if-gen=1.102 if=^❯\\x20keep\\x20going\\s*$ right",
            "key if-gen=1.104 if=^❯\\x20keep\\x20going\\s*$ enter",
        ],
        "{:#?}",
        m.requests
    );
    assert_eq!(count(&m, "turn"), 0, "{:#?}", m.requests);
    let mut between: Vec<&str> = m
        .requests
        .iter()
        .skip_while(|r| !(verb_of(r) == "key" && r.ends_with(" right")))
        .skip(1)
        .take_while(|r| !(verb_of(r) == "key" && r.ends_with(" enter")))
        .filter_map(|r| {
            if r.starts_with("await idle ") {
                Some("settle")
            } else if verb_of(r) == "text" {
                Some("read")
            } else {
                None
            }
        })
        .collect();
    between.dedup();
    assert_eq!(
        between,
        ["settle", "read", "settle", "read"],
        "{:#?}",
        m.requests
    );

    let (dir, path) = journal_file("te-accept-torn");
    let mut m = accepting(
        vec![
            busy_screen(),
            suggesting("Stage 1 is in."),
            torn.clone(),
            torn,
        ],
        None,
    );
    let opts = SuperviseOpts {
        journal: Some(path.clone()),
        ..hosted(30)
    };
    let (lines, _) = watch_lines(&mut m, &opts);
    let (records, _) = journal_records(&path);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(
        m.presses(),
        ["key if-gen=1.102 if=^❯\\x20keep\\x20going\\s*$ right"],
        "{lines:#?}\n{:#?}",
        m.requests
    );
    assert_eq!(count(&m, "turn"), 0, "{:#?}", m.requests);
    assert!(
        records
            .iter()
            .any(|r| r.kind == "skipped" && r.seq == Some(102)),
        "{records:#?}"
    );
}

#[test]
fn a_draft_over_a_selection_drawn_in_colour_only_is_sent_only_onto_its_own_entry() {
    use aterm_phase::prompt::fixtures::{
        SLASH_MENU_CONFIG_2_1_268, SLASH_MENU_CONFIG_CELLS_2_1_268,
        SLASH_MENU_CONFIG_DOWN_CELLS_2_1_268, cells, screen,
    };
    let mut at_end = screen(SLASH_MENU_CONFIG_2_1_268);
    assert!(at_end[3].is_empty() && at_end[4].is_empty());
    at_end[3] = "⏺ Stage 1 is in.".to_string();
    at_end[4] = "✻ Worked for 12m 2s · done 4:24 PM".to_string();
    let caret = at_end
        .iter()
        .position(|r| r.starts_with('❯') && r.trim_end().ends_with("/config"))
        .expect("the caret row");
    for (what, measured, sent, unanswered) in [
        (
            "`/config` selected",
            SLASH_MENU_CONFIG_CELLS_2_1_268,
            true,
            false,
        ),
        (
            "`/update-config` selected",
            SLASH_MENU_CONFIG_DOWN_CELLS_2_1_268,
            false,
            false,
        ),
        (
            "the cells unanswered",
            SLASH_MENU_CONFIG_CELLS_2_1_268,
            false,
            true,
        ),
    ] {
        let mut m = fenced(vec![busy_screen(), at_end.clone()]);
        m.cursor_row = Some(caret);
        m.cursor_col = Some(2 + "/config".chars().count());
        m.cell_unreadable = unanswered;
        m.cells = cells(measured)
            .into_iter()
            .map(|(r, c, reply)| ((r, c), reply))
            .collect();
        m.vanish_after = Some(3);
        let mut opts = hosted(3);
        opts.policy.human_grace_s = 0;
        let (lines, _) = watch_lines(&mut m, &opts);
        assert!(
            m.requests.iter().any(|r| r.starts_with("cell 8 2")),
            "{what}: the entries' cells were read: {:#?}",
            m.requests
        );
        let enters = m
            .presses()
            .iter()
            .filter(|r| r.ends_with(" enter") && r.contains("if-gen="))
            .count();
        assert_eq!(
            (enters, count(&m, "turn") + count(&m, "send")),
            (usize::from(sent), 0),
            "{what}: {lines:#?}\n{:#?}",
            m.requests
        );
        let reason = &crate::supervise::policy::turn_end::MENU_DIVERTS[..60];
        let escalated = m
            .requests
            .iter()
            .any(|r| r.starts_with("meta set attention") && r.contains(reason));
        assert_eq!(
            escalated,
            !sent && !unanswered,
            "{what}: {lines:#?}\n{:#?}",
            m.requests
        );
    }
}

#[test]
fn a_draft_over_an_at_menu_is_sent_only_with_no_entry_selected() {
    use aterm_phase::prompt::fixtures::{
        AT_MENU_NO_2_1_268, AT_MENU_NO_CELLS_2_1_268, AT_MENU_NO_DOWN_CELLS_2_1_268, cells, screen,
    };
    let mut at_end = screen(AT_MENU_NO_2_1_268);
    assert!(at_end[3].is_empty() && at_end[4].is_empty());
    at_end[3] = "⏺ Stage 1 is in.".to_string();
    at_end[4] = "✻ Worked for 12m 2s · done 4:24 PM".to_string();
    let caret = at_end
        .iter()
        .position(|r| r.starts_with('❯') && r.trim_end().ends_with("read @no"))
        .expect("the caret row");
    for (what, measured, sent, unanswered) in [
        ("none selected", AT_MENU_NO_CELLS_2_1_268, true, false),
        (
            "`novel.txt` selected",
            AT_MENU_NO_DOWN_CELLS_2_1_268,
            false,
            false,
        ),
        (
            "the cells unanswered",
            AT_MENU_NO_CELLS_2_1_268,
            false,
            true,
        ),
    ] {
        let mut m = fenced(vec![busy_screen(), at_end.clone()]);
        m.cursor_row = Some(caret);
        m.cursor_col = Some(2 + "read @no".chars().count());
        m.cell_unreadable = unanswered;
        m.cells = cells(measured)
            .into_iter()
            .map(|(r, c, reply)| ((r, c), reply))
            .collect();
        m.vanish_after = Some(3);
        let mut opts = hosted(3);
        opts.policy.human_grace_s = 0;
        let (lines, _) = watch_lines(&mut m, &opts);
        assert!(
            m.requests.iter().any(|r| r.starts_with("cell 11 2")),
            "{what}: the entries' cells were read: {:#?}",
            m.requests
        );
        let enters = m
            .presses()
            .iter()
            .filter(|r| r.ends_with(" enter") && r.contains("if-gen="))
            .count();
        assert_eq!(
            (enters, count(&m, "turn") + count(&m, "send")),
            (usize::from(sent), 0),
            "{what}: {lines:#?}\n{:#?}",
            m.requests
        );
        let reason = &crate::supervise::policy::turn_end::MENU_DIVERTS[..60];
        let escalated = m
            .requests
            .iter()
            .any(|r| r.starts_with("meta set attention") && r.contains(reason));
        assert_eq!(
            escalated,
            !sent && !unanswered,
            "{what}: {lines:#?}\n{:#?}",
            m.requests
        );
    }
}

#[test]
fn a_draft_over_an_at_menu_of_one_entry_is_told_not_pressed() {
    use aterm_phase::prompt::fixtures::{
        AT_MENU_NO_2_1_268, AT_MENU_NO_CELLS_2_1_268, AT_MENU_ONE_2_1_268, cells, screen,
    };
    let at_end = |fixture: &str, draft: &str| {
        let mut rows = screen(fixture);
        assert!(rows[3].is_empty() && rows[4].is_empty());
        rows[3] = "⏺ Stage 1 is in.".to_string();
        rows[4] = "✻ Worked for 12m 2s · done 4:24 PM".to_string();
        let caret = rows
            .iter()
            .position(|r| r.starts_with('❯') && r.trim_end().ends_with(draft))
            .expect("the caret row");
        (rows, caret)
    };
    for (what, fixture, draft, measured, sent) in [
        ("one entry", AT_MENU_ONE_2_1_268, "see @nort", "", false),
        (
            "four entries, none selected",
            AT_MENU_NO_2_1_268,
            "read @no",
            AT_MENU_NO_CELLS_2_1_268,
            true,
        ),
    ] {
        let (rows, caret) = at_end(fixture, draft);
        let mut m = fenced(vec![busy_screen(), rows]);
        m.cursor_row = Some(caret);
        m.cursor_col = Some(2 + draft.chars().count());
        if !measured.is_empty() {
            m.cells = cells(measured)
                .into_iter()
                .map(|(r, c, reply)| ((r, c), reply))
                .collect();
        }
        m.vanish_after = Some(3);
        let mut opts = hosted(3);
        opts.policy.human_grace_s = 0;
        let (lines, _) = watch_lines(&mut m, &opts);
        let enters = m
            .presses()
            .iter()
            .filter(|r| r.ends_with(" enter") && r.contains("if-gen="))
            .count();
        assert_eq!(
            (enters, count(&m, "turn") + count(&m, "send")),
            (usize::from(sent), 0),
            "{what}: {lines:#?}\n{:#?}",
            m.requests
        );
        let reason = &crate::supervise::policy::turn_end::MENU_DIVERTS[..60];
        let told = m
            .requests
            .iter()
            .filter(|r| r.starts_with("meta set attention") && r.contains(reason))
            .count();
        assert_eq!(
            told,
            usize::from(!sent),
            "{what}: {lines:#?}\n{:#?}",
            m.requests
        );
    }
}

#[test]
fn a_draft_that_held_the_point_answered_lets_the_session_go_on() {
    use aterm_phase::prompt::fixtures::{AT_MENU_ONE_2_1_268, screen};
    let mut at_end = screen(AT_MENU_ONE_2_1_268);
    assert!(at_end[3].is_empty() && at_end[4].is_empty());
    at_end[3] = "⏺ Stage 1 is in.".to_string();
    at_end[4] = "✻ Worked for 12m 2s · done 4:24 PM".to_string();
    let caret = 6;
    assert_eq!(at_end[caret], "❯\u{a0}see @nort");
    assert_eq!(at_end[8], "  + north.rs");
    let mut cleared = at_end.clone();
    cleared[caret] = "❯".to_string();
    cleared[8] = "  ⏸ manual mode on · ? for shortcuts".to_string();
    let sent = ended("Read northstar-notes.md: three goals, two done.");
    let sent_caret = sent.iter().position(|r| r == "❯").expect("the caret row");
    let mut typed_on = at_end.clone();
    typed_on[caret] = "❯\u{a0}see @north".to_string();
    for (what, after, after_caret, after_col, goes_on) in [
        ("cleared", cleared, caret, 2, true),
        ("sent", sent, sent_caret, 2, true),
        (
            "typed on",
            typed_on,
            caret,
            2 + "see @north".chars().count(),
            false,
        ),
    ] {
        let mut m = fenced(vec![busy_screen(), at_end.clone(), after]);
        m.agent_pushes = true;
        m.attention_gates = vec![(1, 0)];
        m.cursor_row = Some(caret);
        m.cursor_col = Some(2 + "see @nort".chars().count());
        m.screen_cursors.insert(2, after_caret);
        m.screen_cols.insert(2, after_col);
        m.vanish_after = Some(3);
        let mut opts = hosted(3);
        opts.policy.human_grace_s = 0;
        let (lines, _) = watch_lines(&mut m, &opts);
        let reason = &crate::supervise::policy::turn_end::MENU_DIVERTS[..60];
        let told = m
            .requests
            .iter()
            .filter(|r| r.starts_with("meta set attention") && r.contains(reason))
            .count();
        assert_eq!(told, 1, "{what}: {lines:#?}\n{:#?}", m.requests);
        let enters = m.presses().iter().filter(|r| r.ends_with(" enter")).count();
        assert_eq!(enters, 0, "{what}: {lines:#?}\n{:#?}", m.requests);
        let continued = m
            .requests
            .iter()
            .filter(|r| {
                (r.starts_with("turn ") || r.contains(" turn ")) && r.ends_with(" keep going")
            })
            .count();
        assert_eq!(
            (continued, count(&m, "turn") + count(&m, "send")),
            (usize::from(goes_on), usize::from(goes_on)),
            "{what}: {lines:#?}\n{:#?}",
            m.requests
        );
        assert_eq!(
            count(&m, "meta unset attention") > 0,
            goes_on,
            "{what}: {lines:#?}\n{:#?}",
            m.requests
        );
    }
}

fn ended_over_menu(fixture: &str, said: &str, draft: &str) -> (Vec<String>, usize) {
    let mut rows = aterm_phase::prompt::fixtures::screen(fixture);
    assert!(rows[3].is_empty() && rows[4].is_empty());
    rows[3] = format!("⏺ {said}");
    rows[4] = "✻ Worked for 12m 2s · done 4:24 PM".to_string();
    let caret = rows
        .iter()
        .position(|r| r.starts_with('❯') && r.trim_end().ends_with(draft))
        .expect("the caret row");
    (rows, caret)
}

fn ended_on_own_words(said: &str) -> (Vec<String>, usize, usize) {
    let mut rows = ended(said);
    let caret = rows.iter().position(|r| r == "❯").expect("the caret row");
    let words = "[aterm harn";
    rows[caret] = format!("❯ {words}");
    (rows, caret, 2 + words.chars().count())
}

fn badges(m: &Mock, reason: &str) -> usize {
    m.requests
        .iter()
        .filter(|r| r.starts_with("meta set attention") && r.contains(reason))
        .count()
}

fn kept_going(m: &Mock) -> usize {
    m.requests
        .iter()
        .filter(|r| (r.starts_with("turn ") || r.contains(" turn ")) && r.ends_with(" keep going"))
        .count()
}

#[test]
fn own_words_that_held_the_point_cleared_let_the_session_go_on() {
    let (at_end, caret, col) = ended_on_own_words("Stage 1 is in.");
    for (what, after, goes_on) in [
        ("cleared", ended("Stage 1 is in."), true),
        ("left", at_end.clone(), false),
    ] {
        let mut m = fenced(vec![busy_screen(), at_end.clone(), after]);
        m.agent_pushes = true;
        m.attention_gates = vec![(1, 0)];
        m.cursor_row = Some(caret);
        m.cursor_col = Some(col);
        m.screen_cursors.insert(2, caret);
        m.screen_cols.insert(2, if goes_on { 2 } else { col });
        m.vanish_after = Some(3);
        let mut opts = hosted(3);
        opts.policy.human_grace_s = 0;
        let (lines, _) = watch_lines(&mut m, &opts);
        let reason = &crate::supervise::policy::turn_end::OWN_WORDS_STAND[..40];
        assert_eq!(
            badges(&m, reason),
            1,
            "{what}: {lines:#?}\n{:#?}",
            m.requests
        );
        let enters = m.presses().iter().filter(|r| r.ends_with(" enter")).count();
        assert_eq!(enters, 0, "{what}: {lines:#?}\n{:#?}", m.requests);
        assert_eq!(
            (kept_going(&m), count(&m, "turn") + count(&m, "send")),
            (usize::from(goes_on), usize::from(goes_on)),
            "{what}: {lines:#?}\n{:#?}",
            m.requests
        );
        assert_eq!(
            count(&m, "meta unset attention") > 0,
            goes_on,
            "{what}: {lines:#?}\n{:#?}",
            m.requests
        );
    }
}

#[test]
fn a_question_told_over_a_draft_is_told_once_when_the_draft_goes() {
    use aterm_phase::prompt::fixtures::AT_MENU_ONE_2_1_268;
    let asked = "Should I push the branch to origin now?";
    let (menu, menu_caret) = ended_over_menu(AT_MENU_ONE_2_1_268, asked, "see @nort");
    let mut menu_cleared = menu.clone();
    menu_cleared[menu_caret] = "❯".to_string();
    menu_cleared[menu_caret + 2] = "  ⏸ manual mode on · ? for shortcuts".to_string();
    let (own, own_caret, own_col) = ended_on_own_words(asked);
    for (what, at_end, caret, col, cleared) in [
        (
            "a draft over the menu",
            menu,
            menu_caret,
            2 + "see @nort".chars().count(),
            menu_cleared,
        ),
        ("aterm's own words", own, own_caret, own_col, ended(asked)),
    ] {
        let mut m = fenced(vec![busy_screen(), at_end, cleared]);
        m.agent_pushes = true;
        m.attention_gates = vec![(1, 0)];
        m.cursor_row = Some(caret);
        m.cursor_col = Some(col);
        m.screen_cursors.insert(2, caret);
        m.screen_cols.insert(2, 2);
        m.vanish_after = Some(3);
        let mut opts = hosted(3);
        opts.policy.human_grace_s = 0;
        let (lines, _) = watch_lines(&mut m, &opts);
        assert_eq!(
            (badges(&m, ""), badges(&m, asked)),
            (1, 1),
            "{what}: {lines:#?}\n{:#?}",
            m.requests
        );
        assert_eq!(
            count(&m, "meta unset attention"),
            0,
            "{what}: {lines:#?}\n{:#?}",
            m.requests
        );
        let enters = m.presses().iter().filter(|r| r.ends_with(" enter")).count();
        assert_eq!(
            (
                kept_going(&m),
                count(&m, "turn") + count(&m, "send"),
                enters
            ),
            (0, 0, 0),
            "{what}: {lines:#?}\n{:#?}",
            m.requests
        );
    }
}

#[test]
fn a_draft_whose_cells_were_not_read_cleared_lets_the_session_go_on() {
    use aterm_phase::prompt::fixtures::AT_MENU_PARTIAL_2_1_268;
    let draft = "and @RE";
    let (at_end, caret) = ended_over_menu(AT_MENU_PARTIAL_2_1_268, "Stage 1 is in.", draft);
    let mut cleared = at_end.clone();
    cleared[caret] = "❯".to_string();
    cleared[caret + 2] = "  ⏸ manual mode on · ? for shortcuts".to_string();
    cleared[caret + 3] = String::new();
    let mut m = fenced(vec![busy_screen(), at_end, cleared]);
    m.agent_pushes = true;
    m.cursor_row = Some(caret);
    m.cursor_col = Some(2 + draft.chars().count());
    m.screen_cols.insert(2, 2);
    m.vanish_after = Some(3);
    let mut opts = hosted(3);
    opts.policy.human_grace_s = 0;
    let (lines, _) = watch_lines(&mut m, &opts);
    assert_eq!(badges(&m, ""), 0, "{lines:#?}\n{:#?}", m.requests);
    assert_eq!(kept_going(&m), 1, "{lines:#?}\n{:#?}", m.requests);
}

#[test]
fn a_menu_draft_whose_cells_go_unread_keeps_its_badge() {
    use aterm_phase::prompt::fixtures::AT_MENU_ONE_2_1_268;
    let draft = "see @nort";
    let (at_end, caret) = ended_over_menu(AT_MENU_ONE_2_1_268, "Stage 1 is in.", draft);
    let mut two = at_end.clone();
    two[caret + 3] = "  + northstar-notes.md".to_string();
    let mut m = fenced(vec![busy_screen(), at_end, two]);
    m.agent_pushes = true;
    m.attention_gates = vec![(1, 0)];
    m.cursor_row = Some(caret);
    m.cursor_col = Some(2 + draft.chars().count());
    m.vanish_after = Some(3);
    let mut opts = hosted(3);
    opts.policy.human_grace_s = 0;
    let (lines, _) = watch_lines(&mut m, &opts);
    let reason = &crate::supervise::policy::turn_end::MENU_DIVERTS[..60];
    assert_eq!(badges(&m, reason), 1, "{lines:#?}\n{:#?}", m.requests);
    assert_eq!(
        count(&m, "meta unset attention"),
        0,
        "{lines:#?}\n{:#?}",
        m.requests
    );
    let enters = m.presses().iter().filter(|r| r.ends_with(" enter")).count();
    assert_eq!(
        (
            kept_going(&m),
            count(&m, "turn") + count(&m, "send"),
            enters
        ),
        (0, 0, 0),
        "{lines:#?}\n{:#?}",
        m.requests
    );
}

#[test]
fn an_unknown_composer_is_never_typed_over_or_submitted() {
    let screen = suggesting("Stage 1 is in.");
    let caret = screen
        .iter()
        .position(|r| r.starts_with('❯'))
        .expect("caret row");
    let rows = [
        ("known DIM", false, false, "d0d0d0", "dim", 1, true),
        ("known DIM homed", false, false, "d0d0d0", "dim", 0, true),
        (
            "known homed draft",
            true,
            false,
            "d0d0d0",
            "none link=https%3A%2F%2Fexample.test",
            0,
            true,
        ),
        (
            "known homed draft legacy",
            false,
            false,
            "d0d0d0",
            "none",
            0,
            false,
        ),
        ("unreadable legacy", false, true, "d0d0d0", "dim", 0, false),
        ("unreadable current", true, true, "d0d0d0", "dim", 0, false),
        (
            "missing attributes current",
            true,
            false,
            "d0d0d0",
            "",
            0,
            false,
        ),
        ("missing colour current", true, false, "", "none", 0, false),
        (
            "unreadable colour current",
            true,
            false,
            "nothex",
            "none",
            0,
            false,
        ),
        (
            "unknown attributes current",
            true,
            false,
            "d0d0d0",
            "unrecognized",
            0,
            false,
        ),
        (
            "extra field current",
            true,
            false,
            "d0d0d0",
            "none unexpected",
            0,
            false,
        ),
    ];
    let row = |(label, current, unreadable, fg, attrs, off, admitted): (
        &'static str,
        bool,
        bool,
        &'static str,
        &'static str,
        usize,
        bool,
    )| {
        let frames = vec![busy_screen(), screen.clone(), ended(STOP)];
        let mut m = if current {
            fenced(frames)
        } else {
            Mock::new(frames)
        };
        m.cursor_row = Some(caret + off);
        m.cell_unreadable = unreadable;
        m.cell_attrs = attrs;
        m.cell_fg = fg;
        m.turn_releases = Some(1);
        m.vanish_after = Some(2);
        let mut opts = hosted(3);
        opts.policy.human_grace_s = 0;
        let (lines, _) = watch_lines(&mut m, &opts);
        assert!(
            m.requests.iter().any(|r| r.starts_with("cell ")),
            "{label}: {:?}",
            m.requests
        );
        let input = count(&m, "turn") + count(&m, "send") + m.presses().len();
        if admitted {
            assert!(
                input > 0,
                "admitted control did not reach input {label}: {lines:?} / {:?}",
                m.requests
            );
            if current {
                assert!(
                    m.presses()
                        .iter()
                        .any(|r| r.ends_with(" enter") && r.contains("if-gen=")),
                    "known draft must reach fenced Enter: {:?}",
                    m.requests
                );
            } else {
                assert!(
                    m.requests.contains(&continue_request("keep going")),
                    "known placeholder: {:?}",
                    m.requests
                );
            }
        } else if input != 0 {
            return Some((label, lines, m.requests));
        }
        None
    };
    let row = &row;
    let wrong: Vec<_> = std::thread::scope(|scope| {
        let runs: Vec<_> = rows
            .into_iter()
            .map(|r| scope.spawn(move || row(r)))
            .collect();
        runs.into_iter()
            .filter_map(|run| run.join().unwrap_or_else(|p| std::panic::resume_unwind(p)))
            .collect()
    });
    assert!(wrong.is_empty(), "unknown composer caused input: {wrong:?}");
}

#[test]
fn a_529_waits_its_backoff_then_continues_exactly_once() {
    let (dir, path) = journal_file("te-529");
    let end_529 = aterm_phase::prompt::fixtures::screen(aterm_phase::prompt::fixtures::END_529);
    let mut m = Mock::new(vec![
        busy_screen(),
        end_529.clone(),
        busy_screen(),
        ended(STOP),
    ]);
    m.turn_releases = Some(1);
    m.vanish_after = Some(2);
    let opts = SuperviseOpts {
        journal: Some(path.clone()),
        ..hosted(30)
    };
    let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
        s.set_turn_end_timing(TurnEndTiming {
            retry_backoff: vec![Duration::from_millis(50); 3],
            ..TurnEndTiming::default()
        });
    });
    assert_eq!(count(&m, "turn"), 1, "{lines:#?}\n{:#?}", m.requests);
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("CONTINUED seq=102 rule=api-retry@v1 ")
                && !l.ends_with(" keep going")),
        "{lines:#?}"
    );
    let (records, _) = journal_records(&path);
    let _ = std::fs::remove_dir_all(&dir);
    let waiting = records
        .iter()
        .find(|r| r.kind == "waiting")
        .expect("a WAITING row");
    assert!(
        waiting.summary.ends_with("overloaded retry 1"),
        "{}",
        waiting.summary
    );
    assert!(
        records.iter().any(|r| r.kind == "continued"),
        "the continuation: {records:#?}"
    );
    assert_eq!(count(&m, "meta set attention"), 1, "{:#?}", m.requests);
}

#[test]
fn a_fable_limit_with_no_host_to_relaunch_is_waited_out_never_by_model() {
    let (dir, path) = journal_file("te-fable");
    let mut fable = rows(&[
        "⏺ Running the suite.",
        "  ⎿  You've reached your Fable limit. Run /usage-credits to continue or switch \
         models with /model.",
        "",
        "✻ Worked for 3m 2s · done 4:51 PM",
        "",
    ]);
    fable.extend(composer("  ? for shortcuts"));
    let mut m = Mock::new(vec![busy_screen(), fable.clone()]);
    m.vanish_after = Some(2);
    let opts = SuperviseOpts {
        journal: Some(path.clone()),
        ..hosted(3)
    };
    let (lines, _) = watch_lines(&mut m, &opts);
    let (records, _) = journal_records(&path);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        !m.requests.iter().any(|r| r.contains("/model")),
        "never /model: {:#?}",
        m.requests
    );
    assert!(
        !lines
            .iter()
            .any(|l| l.starts_with("TYPED") || l.starts_with("CONTINUED")),
        "{lines:#?}"
    );
    assert!(
        records
            .iter()
            .any(|r| r.kind == "limited" && r.summary.starts_with("handled: You've reached")),
        "{records:#?}"
    );
    assert!(
        records
            .iter()
            .any(|r| r.kind == "waiting" && r.summary.contains("until=")),
        "the reset waited out: {records:#?}"
    );
    assert_eq!(count(&m, "meta set attention"), 0, "{:#?}", m.requests);
}

#[test]
fn a_context_no_compact_lowers_is_one_exception_across_the_rungs_until_it_is_lowered() {
    use aterm_phase::prompt::fixtures as fx;
    let (dir, path) = journal_file("te-context-hopeless");
    let host = Arc::new(LadderHost::default());
    let frame = |f: &str| fx::screen(f);
    let mut m = Mock::new(vec![
        busy_screen(),
        frame(fx::CONTEXT_WALL_MEASURED),
        frame(fx::COMPACT_RUNNING_MEASURED),
        frame(fx::COMPACT_FAILED_1_MEASURED),
        frame(fx::COMPACT_RUNNING_MEASURED),
        frame(fx::COMPACT_FAILED_2_MEASURED),
        frame(fx::COMPACT_RUNNING_MEASURED),
        frame(fx::COMPACT_FAILED_3_MEASURED),
        frame(fx::COMPACT_RUNNING_MEASURED),
        frame(fx::COMPACT_FAILED_3_MEASURED),
        frame(fx::COMPACT_RUNNING_MEASURED),
        frame(fx::COMPACT_FAILED_3_MEASURED),
        frame(fx::COMPACT_RUNNING_MEASURED),
        frame(fx::COMPACT_FAILED_3_MEASURED),
        frame(fx::COMPACT_RUNNING_MEASURED),
        frame(fx::COMPACT_SUCCEEDED_MEASURED),
        busy_screen(),
        ended(STOP),
    ]);
    m.turn_gates = vec![1, 3, 5, 7, 9, 11, 13, 15];
    m.vanish_after = Some(2);
    let opts = SuperviseOpts {
        journal: Some(path.clone()),
        idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
        ..hosted(30)
    };
    let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
        s.set_turn_end_timing(TurnEndTiming {
            retry_backoff: vec![Duration::from_millis(50); 7],
            ..TurnEndTiming::default()
        });
    });
    let (records, _) = journal_records(&path);
    let _ = std::fs::remove_dir_all(&dir);
    let story: Vec<&str> = records
        .iter()
        .filter_map(|r| match r.kind.as_str() {
            "exception" => Some("EXCEPTION"),
            "cleared" => Some("CLEARED"),
            "typed" | "continued" if r.line.contains("/compact") => Some("/compact"),
            "typed" | "continued" => Some("keep going"),
            _ => None,
        })
        .collect();
    let compact = "/compact";
    assert_eq!(
        story,
        [
            compact,
            compact,
            compact,
            "EXCEPTION",
            compact,
            compact,
            compact,
            compact,
            "CLEARED",
            "keep going"
        ],
        "{lines:#?}"
    );
    let said = |kind: &str| records.iter().find(|r| r.kind == kind).map(|r| &r.line);
    let (raised, ended) = (said("exception").unwrap(), said("cleared").unwrap());
    assert!(raised.contains("kind=context when=later"), "{raised}");
    assert!(ended.contains(" exception=context after="), "{ended}");
    assert_eq!(
        *host.told.lock().unwrap(),
        ["exception:context", "exception:none"],
        "the host hears it once, and its end"
    );
}

#[test]
fn a_lost_login_types_login_and_raises_no_badge() {
    let (dir, path) = journal_file("te-login");
    let mut lost = rows(&[
        "⏺ Pushing the branch.",
        "  ⎿  Not logged in · Please run /login",
        "",
        "✻ Worked for 3m 2s · done 4:51 PM",
        "",
    ]);
    lost.extend(composer("  ? for shortcuts"));
    let dialog = rows(&[
        " Browser didn't open? Use the url below to sign in (⌘/ctrl + click):",
        " https://claude.ai/oauth/authorize?code=true",
        "",
        " Paste code here if prompted >",
    ]);
    let mut m = Mock::new(vec![
        busy_screen(),
        lost,
        dialog,
        busy_screen(),
        ended("Pushed."),
    ]);
    m.turn_releases = Some(1);
    m.vanish_after = Some(2);
    let mut opts = SuperviseOpts {
        journal: Some(path.clone()),
        ..hosted(30)
    };
    opts.policy.continue_policy = false;
    let (lines, _) = watch_lines(&mut m, &opts);
    let (records, _) = journal_records(&path);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        lines.contains(&"TYPED seq=102 rule=auth-login@v1 /login".to_string()),
        "{lines:#?}"
    );
    assert_eq!(count(&m, "turn"), 1, "{:#?}", m.requests);
    assert!(
        !m.requests
            .iter()
            .any(|r| r.starts_with("meta set attention")),
        "no badge: {:#?}",
        m.requests
    );
    assert_eq!(m.attention, None);
    assert!(
        !records.iter().any(|r| r.kind == "exception"),
        "a dialog passed through between points tells nothing: {records:#?}"
    );
}

fn login_picker() -> Vec<String> {
    let mut r = login_expired();
    r.truncate(r.len() - 4);
    r.extend(rows(&[
        "❯ /login",
        "",
        &"─".repeat(100),
        " Claude Code can be used with your Claude subscription or billed based on API usage \
         through your Console account.",
        "",
        " Select login method:",
        "",
        " ❯ 1. Claude account with subscription · Pro, Max, Team, or Enterprise",
        "   2. Anthropic Console account · API usage billing",
        "   3. 3rd-party platform · Amazon Bedrock, Microsoft Foundry, or Vertex AI",
        "",
    ]));
    r
}

fn handoff() -> Vec<String> {
    aterm_phase::prompt::fixtures::screen(aterm_phase::prompt::fixtures::LOGIN_HANDOFF)
}

fn login_expired() -> Vec<String> {
    aterm_phase::prompt::fixtures::screen(aterm_phase::prompt::fixtures::LOGIN_EXPIRED)
}

fn login_back() -> Vec<String> {
    let mut r = login_expired();
    let at = r
        .iter()
        .position(|row| row.starts_with("⏺ Login expired"))
        .expect("the wall row");
    r.splice(
        at + 1..at + 1,
        rows(&["", "❯ /login", "  ⎿  Login successful"]),
    );
    r
}

#[test]
fn the_login_expired_row_gets_login_once_and_the_login_back_is_continued() {
    let (dir, path) = journal_file("te-login-expired");
    let mut m = Mock::new(vec![
        busy_screen(),
        login_expired(),
        login_picker(),
        login_back(),
        busy_screen(),
        ended(STOP),
    ]);
    m.turn_gates = vec![1, 3];
    m.vanish_after = Some(2);
    let opts = SuperviseOpts {
        journal: Some(path.clone()),
        ..hosted(30)
    };
    let (lines, _) = watch_lines(&mut m, &opts);
    let (records, _) = journal_records(&path);
    let _ = std::fs::remove_dir_all(&dir);
    let decided: Vec<&String> = lines
        .iter()
        .filter(|l| l.starts_with("TYPED") || l.starts_with("CONTINUED"))
        .collect();
    assert_eq!(
        decided,
        [
            "TYPED seq=102 rule=auth-login@v1 /login",
            "CONTINUED seq=104 rule=continue@v1 keep going",
        ],
        "{lines:#?}\n{:#?}",
        m.requests
    );
    assert!(
        !m.requests
            .iter()
            .any(|r| r.starts_with("meta set attention owner=supervisor claude wall: ")),
        "no badge for the lost login: {:#?}",
        m.requests
    );
    assert!(
        !m.requests.iter().any(|r| r.starts_with("key")),
        "no key pressed on the login picker: {:#?}",
        m.requests
    );
    assert!(
        m.requests
            .iter()
            .any(|r| r.starts_with("meta set attention owner=supervisor claude other: ")),
        "the picker is a person's: {:#?}",
        m.requests
    );
    assert!(
        records.iter().any(|r| r.kind == "escalated"),
        "{records:#?}"
    );
    assert!(
        lines.contains(&"EVENT idle seq=104 ⎿  Login successful".to_string()),
        "{lines:#?}"
    );
}

#[test]
fn a_shell_a_yield_and_a_missed_guard_type_nothing() {
    let (dir, path) = journal_file("te-shell");
    let mut m = Mock::new(vec![
        busy_screen(),
        ended("Fixed the parser; the suite is green."),
    ]);
    m.program = "zsh";
    m.vanish_after = Some(2);
    let opts = SuperviseOpts {
        journal: Some(path.clone()),
        ..hosted(30)
    };
    let _ = watch_lines(&mut m, &opts);
    let (records, _) = journal_records(&path);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(count(&m, "turn"), 0, "{:#?}", m.requests);
    assert!(
        records
            .iter()
            .any(|r| r.kind == "skipped" && r.summary.contains("zsh")),
        "{records:#?}"
    );

    let mut m = Mock::new(vec![
        busy_screen(),
        ended("Fixed the parser; the suite is green."),
    ]);
    m.verb_replies
        .insert("turn", VecDeque::from([err("yield timeout momentum=0.61")]));
    m.vanish_after = Some(2);
    let (lines, _) = watch_lines(&mut m, &hosted(30));
    assert!(
        m.requests.contains(&continue_request("keep going")),
        "{:#?}",
        m.requests
    );
    assert!(
        !lines.iter().any(|l| l.starts_with("CONTINUED")),
        "{lines:#?}"
    );
    assert_eq!(count(&m, "meta set attention"), 0, "{:#?}", m.requests);

    let (dir, ledger) = ledger_at("missed");
    let mut m = Mock::new(vec![
        busy_screen(),
        ended("Fixed the parser; the suite is green."),
    ]);
    m.verb_replies.insert(
        "turn",
        VecDeque::from([ok(
            "OK 0 turn skipped reason=guard submitted=0 seq=102 id=1\n",
        )]),
    );
    m.vanish_after = Some(2);
    let (lines, _) = watch_lines_with(&mut m, &hosted(30), |s| {
        s.set_approval_ledger(Some(ledger.clone()));
    });
    assert_eq!(count(&m, "turn"), 1, "{:#?}", m.requests);
    assert!(
        !lines.iter().any(|l| l.starts_with("CONTINUED")),
        "{lines:#?}"
    );
    assert!(
        !std::fs::read_to_string(&ledger)
            .unwrap_or_default()
            .contains("\"decision\":\"typed\""),
        "no act recorded"
    );
    assert!(
        m.attention
            .as_deref()
            .is_some_and(|a| a.contains("(typed, not submitted (")),
        "{:?}\n{:#?}",
        m.attention,
        m.requests
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_enter_written_and_not_seen_is_a_continuation_typed() {
    let timed_out = |pressed: u32| CtlReply {
        code: 124,
        stdout: format!(
            "OK 50 turn submitted=0 status=timeout seq=102 id=1 dur_ms=2501 \
             hash=0000000000000000 pressed={pressed}\n"
        ),
        stderr: String::new(),
    };
    for pressed in [1, 2, 0] {
        let (dir, ledger) = ledger_at(&format!("unverified-{pressed}"));
        let mut m = Mock::new(vec![
            busy_screen(),
            ended("Fixed the parser; the suite is green."),
        ]);
        m.verb_replies
            .insert("turn", VecDeque::from([timed_out(pressed)]));
        m.vanish_after = Some(2);
        let (lines, _) = watch_lines_with(&mut m, &hosted(30), |s| {
            s.set_approval_ledger(Some(ledger.clone()));
        });
        let typed = std::fs::read_to_string(&ledger)
            .unwrap_or_default()
            .contains("\"decision\":\"typed\"");
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(count(&m, "turn"), 1, "pressed={pressed}: {:#?}", m.requests);
        assert_eq!(typed, pressed > 0, "pressed={pressed}: {lines:#?}");
        assert_eq!(
            lines.iter().any(|l| l.starts_with("CONTINUED")),
            pressed > 0,
            "pressed={pressed}: {lines:#?}"
        );
    }
}

#[test]
fn a_question_in_prose_is_answered_by_the_sessions_priority_word() {
    use crate::supervise::policy::question::{LONG_TERM_TEXT, SHORT_TERM_TEXT};
    use crate::supervise::policy::turn_end::RULE_ANSWER;
    use crate::supervise::policy::{RULE_PROSE_LONG_TERM, RULE_PROSE_SHORT_TERM};
    let answer_text = owners_policy().answer_text;
    let full = SuperviseOpts {
        policy: owners_policy(),
        ..auto(30)
    };
    let long = SuperviseOpts {
        policy: SupervisorConfig {
            question_priority: crate::supervise::QuestionPriority::LongTerm,
            ..owners_policy()
        },
        ..auto(30)
    };
    let off = hosted(30);
    let long_text = LONG_TERM_TEXT.to_string();
    for (k, (word, opts, text, rule)) in [
        ("long-term", &off, long_text.clone(), RULE_PROSE_LONG_TERM),
        (
            "short-term",
            &off,
            SHORT_TERM_TEXT.to_string(),
            RULE_PROSE_SHORT_TERM,
        ),
        ("recommended", &off, answer_text.clone(), RULE_ANSWER),
        ("ask", &full, answer_text.clone(), RULE_ANSWER),
        ("sometimes", &full, answer_text.clone(), RULE_ANSWER),
        ("ask", &long, long_text, RULE_PROSE_LONG_TERM),
    ]
    .into_iter()
    .enumerate()
    {
        let text = format!("{text} {}", crate::supervise::policy::needs::NEEDS_HINT);
        let (dir, ledger) = ledger_at(&format!("prose-{word}-{k}"));
        let mut m = Mock::new(vec![busy_screen(), ended(PROSE_ASK)]);
        m.turn_releases = Some(1);
        m.vanish_after = Some(2);
        m.human = vec!["null"];
        m.questions = Some(word);
        let (lines, _) = watch_lines_with(&mut m, opts, |s| {
            s.set_approval_ledger(Some(ledger.clone()));
        });
        assert!(
            lines.contains(&format!("CONTINUED seq=102 rule={rule} {}", clip(&text))),
            "{word} {k}: {lines:#?}\n{:#?}",
            m.requests
        );
        assert!(
            m.requests.contains(&continue_request(&text)),
            "{word} {k}: {:#?}",
            m.requests
        );
        assert_eq!(
            m.requests.iter().filter(|r| *r == "meta").count(),
            2,
            "{word} {k}: the start-up reconcile and ONE read of the word: {:#?}",
            m.requests
        );
        assert_eq!(m.attention, None, "{word} {k}: {:#?}", m.requests);
        let rows = std::fs::read_to_string(&ledger).unwrap_or_default();
        assert!(
            rows.lines()
                .any(|l| l.contains(&format!("\"rule_id\":\"{rule}\""))
                    && l.contains("\"decision\":\"typed\"")),
            "{word} {k}: {rows}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
    let mut m = Mock::new(vec![
        busy_screen(),
        ended("Fixed the parser; the suite is green."),
    ]);
    m.turn_releases = Some(1);
    m.vanish_after = Some(2);
    m.questions = Some("long-term");
    let _ = watch_lines(&mut m, &full);
    assert_eq!(
        m.requests.iter().filter(|r| *r == "meta").count(),
        1,
        "{:#?}",
        m.requests
    );
    for word in [None, Some("ask")] {
        let mut m = Mock::new(vec![busy_screen(), ended(PROSE_ASK)]);
        m.vanish_after = Some(2);
        m.human = vec!["null"];
        m.questions = word;
        let _ = watch_lines(&mut m, &off);
        assert_eq!(count(&m, "turn"), 0, "{word:?}: {:#?}", m.requests);
        assert!(
            m.attention
                .as_deref()
                .is_some_and(|a| a.starts_with("claude question: ")),
            "{word:?}: {:?}",
            m.attention
        );
        assert_eq!(
            m.requests.iter().filter(|r| *r == "meta").count(),
            2,
            "{word:?}: {:#?}",
            m.requests
        );
    }
}

const PROSE_ASK: &str = "Should I rewrite the parser or patch the lexer?";

const FENCED_HELP: &str = "key [id=<key>] [if=<re>] [if-gen=<e.s>] <name>: send a named key\n\
                           send [id=<key>] [if=<re>] [if-gen=<e.s>] <text>: write text\n";

fn typed_in(said: &str, text: &str) -> Vec<String> {
    let mut r = ended(said);
    let caret = r.iter().position(|row| row == "❯").expect("caret row");
    r[caret] = format!("❯ {text}");
    r
}

#[test]
fn a_fenced_host_writes_the_continuation_only_on_the_judged_screen() {
    for human_seq in [Some(0), Some(3)] {
        let (dir, ledger) = ledger_at(&format!("fenced-{}", human_seq.is_some()));
        let mut m = Mock::new(vec![
            busy_screen(),
            ended("Fixed the parser; the suite is green."),
            typed_in("Fixed the parser; the suite is green.", "keep going"),
            busy_screen(),
            ended(STOP),
        ]);
        m.gen_fence = true;
        m.sends_gen = true;
        m.human_seq = human_seq;
        m.help = FENCED_HELP.to_string();
        m.screen_cols.insert(2, 12);
        m.vanish_after = Some(2);
        let (lines, _) = watch_lines_with(&mut m, &hosted(30), |s| {
            s.set_approval_ledger(Some(ledger.clone()));
        });
        assert!(
            lines.contains(&format!(
                "CONTINUED seq=102 rule={RULE_CONTINUE} keep going"
            )),
            "{human_seq:?}: {lines:#?}\n{:#?}",
            m.requests
        );
        let writes: Vec<&String> = m
            .requests
            .iter()
            .filter(|r| r.starts_with("send ") || r.starts_with("key "))
            .collect();
        let person = human_seq.expect("the fixture carries its actual person count");
        let sent = format!("send if-human={person} if-gen=1.102 if=^❯\\s*$ -- keep going");
        let entered =
            format!("key if-human={person} if-gen=1.103 if=^❯\\x20keep\\x20going\\s*$ enter");
        assert_eq!(
            writes,
            [&sent, &entered],
            "{human_seq:?}: {:#?}",
            m.requests
        );
        assert_eq!(count(&m, "turn"), 0, "{:#?}", m.requests);
        assert_reaction_awaited(&m, &format!("send if-human={person} if-gen=1.102"));
        assert!(
            std::fs::read_to_string(&ledger)
                .unwrap_or_default()
                .contains("\"decision\":\"typed\""),
            "{human_seq:?}: the act recorded"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    let mut m = Mock::new(vec![
        busy_screen(),
        ended("Fixed the parser; the suite is green."),
    ]);
    m.gen_fence = true;
    m.sends_gen = true;
    m.human_seq = Some(3);
    m.help = FENCED_HELP.to_string();
    m.verb_replies.insert(
        "send",
        VecDeque::from([ok("OK skipped reason=changed seq=103\n")]),
    );
    m.vanish_after = Some(2);
    let (lines, _) = watch_lines(&mut m, &hosted(30));
    assert!(
        !lines.iter().any(|l| l.starts_with("CONTINUED")),
        "{lines:#?}"
    );
    assert!(
        !m.requests.iter().any(|r| r.starts_with("key ")),
        "{:#?}",
        m.requests
    );
    assert_eq!(count(&m, "turn"), 0, "never an unfenced paste after a skip");

    let mut m = Mock::new(vec![
        busy_screen(),
        ended("Fixed the parser; the suite is green."),
        typed_in("Fixed the parser; the suite is green.", "wkeep going"),
    ]);
    m.gen_fence = true;
    m.sends_gen = true;
    m.human_seq = Some(3);
    m.help = FENCED_HELP.to_string();
    m.screen_cols.insert(2, 13);
    m.vanish_after = Some(2);
    let (lines, _) = watch_lines(&mut m, &hosted(30));
    assert!(
        !lines.iter().any(|l| l.starts_with("CONTINUED")),
        "{lines:#?}"
    );
    assert!(
        !m.requests.iter().any(|r| r.starts_with("key ")),
        "{:#?}",
        m.requests
    );
    assert!(
        m.attention
            .as_deref()
            .is_some_and(|a| a.contains("(typed, not submitted (")),
        "{:?}",
        m.attention
    );
}

const INPUT_PENDING: &str = "input=pending input_bytes=293 input_wait_ms=2473 fg_rss_mb=-";
const INPUT_TYPEAHEAD: &str = "input=typeahead input_bytes=293 input_wait_ms=2473 fg_rss_mb=-";

#[test]
fn nothing_is_typed_behind_input_the_worker_has_not_read() {
    let held_for = 3;
    struct Ran {
        notes: Vec<String>,
        sends: Vec<String>,
        reads_before_send: usize,
        /// The first `send`, after the last `status` that said the input was unread.
        sent_after_unread: Option<Duration>,
        requests: Vec<String>,
    }
    let run = |input: &'static str, clear_after: Option<usize>, sits: bool| {
        let (jdir, journal) = journal_file("input-unread");
        let mut m = Mock::new(vec![
            busy_screen(),
            ended("Fixed the parser; the suite is green."),
        ]);
        m.gen_fence = true;
        m.sends_gen = true;
        m.human_seq = Some(0);
        m.help = FENCED_HELP.to_string();
        m.input = input;
        if let Some(n) = clear_after {
            m.input_after = vec![(n, INPUT_CLEAR)];
        }
        if sits {
            m.key_releases = Some(1);
        }
        m.stall_sleep = Some(Duration::from_millis(20));
        m.vanish_after = Some(20_000);
        m.exit_when = Some(|m| m.requests.iter().any(|r| r.starts_with("send ")));
        let opts = SuperviseOpts {
            journal: Some(journal.clone()),
            ..hosted(600)
        };
        let _ = watch_lines(&mut m, &opts);
        let (records, _) = journal_records(&journal);
        let notes = records.into_iter().map(|r| r.line).collect();
        let _ = std::fs::remove_dir_all(&jdir);
        let reads_before_send = m
            .requests
            .iter()
            .take_while(|r| !r.starts_with("send "))
            .filter(|r| r.as_str() == "status")
            .count();
        let sends: Vec<String> = m
            .requests
            .iter()
            .filter(|r| r.starts_with("send "))
            .cloned()
            .collect();
        let sent_after_unread = m
            .first_send_at
            .zip(m.input_unread_at)
            .map(|(sent, unread)| sent.saturating_duration_since(unread));
        Ran {
            notes,
            sends,
            reads_before_send,
            sent_after_unread,
            requests: m.requests,
        }
    };
    let continued = |sends: &[String]| {
        sends.len() == 1
            && sends[0].starts_with("send if-human=0 if-gen=1.")
            && sends[0].ends_with(" if=^❯\\s*$ -- keep going")
    };
    let unread = |word: &str| {
        format!(
            "rule={RULE_CONTINUE} the worker has not read the input written to it \
             (input={word}); nothing written"
        )
    };
    let undrawn = format!(
        "rule={RULE_CONTINUE} the worker has not drawn the input it read (the screen has not \
         moved since it was unread); nothing written"
    );
    let skipped = |r: &Ran, tail: &str| {
        r.notes
            .iter()
            .any(|n| n.starts_with("SKIPPED seq=") && n.ends_with(tail))
    };
    let lines = |r: &Ran| format!("{:#?}\n{:#?}", r.notes, r.requests);
    let held = [(INPUT_PENDING, "pending"), (INPUT_TYPEAHEAD, "typeahead")];
    let controls = [INPUT_CLEAR, "input=- input_bytes=-", ""];
    let mut cases = Vec::new();
    for (input, _) in held {
        cases.extend([
            (input, Some(held_for), true),
            (input, Some(held_for), false),
        ]);
    }
    for input in controls {
        cases.extend([(input, None, true), (input, None, false)]);
    }
    let mut ran = aterm_spec::check::each(&cases, |&(input, clear_after, sits)| {
        run(input, clear_after, sits)
    })
    .into_iter();
    for (_, word) in held {
        let r = ran.next().expect("the run whose screen sits");
        assert!(skipped(&r, &unread(word)), "{word}: {}", lines(&r));
        assert!(
            skipped(&r, &undrawn),
            "{word}: read, not drawn, never held: {}",
            lines(&r)
        );
        assert!(
            continued(&r.sends),
            "{word}: one continuation: {}",
            lines(&r)
        );
        assert!(
            r.sent_after_unread
                .is_some_and(|d| d + Duration::from_secs(1) >= turn_end_loop::UNDRAWN_INPUT_HANG),
            "{word}: typed {:?} after the queue was last unread, on a screen that never moved: {}",
            r.sent_after_unread,
            lines(&r)
        );
        let r = ran.next().expect("the run whose screen moves");
        assert!(skipped(&r, &unread(word)), "{word}: {}", lines(&r));
        assert!(!skipped(&r, &undrawn), "{word}: {}", lines(&r));
        assert!(
            r.reads_before_send > held_for,
            "{word}: written behind unread input: {} status read(s) before the send\n{}",
            r.reads_before_send,
            lines(&r)
        );
        assert!(
            r.sent_after_unread
                .is_some_and(|d| d < turn_end_loop::UNDRAWN_INPUT_HANG),
            "{word}: {:?}: {}",
            r.sent_after_unread,
            lines(&r)
        );
        assert!(
            continued(&r.sends),
            "{word}: one continuation: {}",
            lines(&r)
        );
    }
    for input in controls {
        for sits in [true, false] {
            let r = ran.next().expect("a control's run");
            assert!(
                !r.notes.iter().any(|n| n.contains("has not read the input")
                    || n.contains("has not drawn the input")),
                "{input:?} sits={sits}: {:#?}",
                r.notes
            );
            assert!(
                r.reads_before_send < held_for,
                "{input:?} sits={sits}: the act waited: {:#?}",
                r.requests
            );
            assert!(
                continued(&r.sends),
                "{input:?} sits={sits}: {:#?}",
                r.requests
            );
        }
    }
}

fn agy_typed(text: &str) -> Vec<String> {
    use aterm_phase::agy::fixtures as a;
    let mut rows = aterm_phase::prompt::fixtures::screen(a::END_OF_TURN);
    let caret = aterm_phase::agy::composer(&rows).expect("the composer");
    rows[caret] = format!("> {text}");
    rows
}

struct Typed {
    m: Mock,
    lines: Vec<String>,
}

impl Typed {
    fn sends(&self) -> Vec<&String> {
        self.m
            .requests
            .iter()
            .filter(|r| r.starts_with("send "))
            .collect()
    }

    fn enters(&self) -> Vec<&String> {
        self.m
            .requests
            .iter()
            .filter(|r| r.starts_with("key ") && r.ends_with(" enter"))
            .collect()
    }

    fn after_send(&self) -> Vec<&String> {
        self.m
            .requests
            .iter()
            .skip_while(|r| !r.starts_with("send "))
            .skip(1)
            .take_while(|r| {
                !(r.starts_with("key ") && r.ends_with(" enter"))
                    && !r.starts_with("meta set attention owner=supervisor ")
            })
            .collect()
    }

    fn left_typed(&self) -> Option<&str> {
        self.m
            .requests
            .iter()
            .filter_map(|r| r.strip_prefix("meta set attention owner=supervisor "))
            .find(|a| a.contains("(typed, not submitted ("))
    }

    fn first_read_of(&self, index: usize) -> Option<Instant> {
        self.m
            .read_times
            .iter()
            .find(|(i, _)| *i == index)
            .map(|(_, at)| *at)
    }

    fn enter_at(&self) -> Option<Instant> {
        self.m
            .key_times
            .iter()
            .find(|(r, _)| r.ends_with(" enter"))
            .map(|(_, at)| *at)
    }

    fn evidence(&self) -> String {
        format!(
            "attention: {:?}\nlines: {:#?}\nrequests: {:#?}",
            self.m.attention, self.lines, self.m.requests
        )
    }
}

fn fenced_echo(
    screens: Vec<Vec<String>>,
    program: &'static str,
    hold: bool,
    unsettled: Option<&'static str>,
    person_after_send: usize,
) -> Typed {
    fenced_echo_with(screens, program, |m| {
        if hold {
            m.key_gates = vec![(2, 0)];
        }
        m.idle_never_for = unsettled;
        m.person_after_send = person_after_send;
    })
}

fn fenced_echo_with(
    screens: Vec<Vec<String>>,
    program: &'static str,
    shape: impl FnOnce(&mut Mock),
) -> Typed {
    let mut m = match program {
        "agy" => agy_mock(screens),
        "muse" => muse_mock(screens),
        other => panic!("no mock for {other}"),
    };
    m.gen_fence = true;
    m.sends_gen = true;
    m.human_seq = Some(0);
    m.help = FENCED_HELP.to_string();
    let cols: Vec<(usize, usize)> = m
        .screens
        .iter()
        .enumerate()
        .filter_map(|(i, r)| {
            let caret = match program {
                "agy" => aterm_phase::agy::composer(r),
                _ => aterm_phase::muse::composer(r),
            }?;
            let typed: String = r[caret].chars().skip(1).collect();
            let typed = typed.trim();
            (!typed.is_empty()).then(|| (i, 2 + typed.chars().count()))
        })
        .collect();
    m.screen_cols.extend(cols);
    m.vanish_after = Some(2);
    m.exit_when = Some(|m| m.attention.is_some());
    shape(&mut m);
    let (lines, _) = watch_lines_with(&mut m, &hosted(600), |s| {
        s.set_turn_end_timing(TurnEndTiming {
            min_work: Duration::ZERO,
            ..TurnEndTiming::default()
        });
    });
    Typed { m, lines }
}

#[test]
fn a_late_echo_is_waited_for_and_never_judged_by_a_budget() {
    use aterm_phase::agy::fixtures as a;
    use aterm_phase::prompt::fixtures::screen;
    assert_eq!(
        turn_end_loop::ECHO_HANG,
        aterm_spec::wait::MUST_HAPPEN,
        "the echo's wait is the one-minute hang detector"
    );
    let seq_waits = |t: &Typed| {
        t.after_send()
            .iter()
            .filter(|r| r.starts_with("await seq "))
            .count()
    };

    let t = fenced_echo(
        vec![
            screen(a::BUSY),
            screen(a::END_OF_TURN),
            agy_typed("keep"),
            agy_typed("keep going"),
            screen(a::BUSY),
        ],
        "agy",
        false,
        None,
        0,
    );
    assert_eq!(t.sends().len(), 1, "written once: {}", t.evidence());
    assert!(t.sends()[0].ends_with(" -- keep going"), "{}", t.evidence());
    assert_eq!(t.enters().len(), 1, "one Enter: {}", t.evidence());
    assert!(
        t.enters()[0].contains("if=^>\\x20keep\\x20going\\s*$"),
        "fenced on the read that showed it whole: {}",
        t.evidence()
    );
    assert!(t.left_typed().is_none(), "{}", t.evidence());
    assert!(
        seq_waits(&t) >= 2,
        "the screen waited past the read that showed a start of it: {}",
        t.evidence()
    );
    assert!(
        t.lines.iter().any(|l| l.starts_with("CONTINUED ")),
        "{}",
        t.evidence()
    );

    let t = fenced_echo(
        vec![screen(a::BUSY), screen(a::END_OF_TURN), agy_typed("keep")],
        "agy",
        true,
        None,
        0,
    );
    assert!(t.enters().is_empty(), "nothing pressed: {}", t.evidence());
    assert_eq!(t.sends().len(), 1, "{}", t.evidence());
    assert!(
        t.left_typed().is_some_and(|a| a.contains(
            "(typed, not submitted (the composer had not shown it whole a minute after it was \
             written)"
        )),
        "escalated as a hang: {}",
        t.evidence()
    );
    let waited =
        t.m.attention_at
            .zip(t.m.first_send_at)
            .map(|(told, sent)| told.saturating_duration_since(sent));
    assert!(
        waited.is_some_and(|w| w >= turn_end_loop::ECHO_HANG),
        "escalated {waited:?} after the write: {}",
        t.evidence()
    );

    for (tag, shown, person) in [("other", "wkeep going", 0), ("person", "keep going", 1)] {
        let t = fenced_echo(
            vec![
                screen(a::BUSY),
                screen(a::END_OF_TURN),
                agy_typed(shown),
                agy_typed("keep going"),
                screen(a::BUSY),
                screen(a::END_OF_TURN),
            ],
            "agy",
            false,
            None,
            person,
        );
        assert!(t.enters().is_empty(), "{tag}: {}", t.evidence());
        assert!(t.left_typed().is_some(), "{tag}: {}", t.evidence());
        assert_eq!(
            seq_waits(&t),
            1,
            "{tag}: escalated at once: {}",
            t.evidence()
        );
    }
}

#[test]
fn a_paste_guarded_enter_waits_out_the_guard_since_the_echo_was_whole() {
    use aterm_phase::muse::fixtures as u;
    use aterm_phase::prompt::fixtures::screen;
    let muse_typed = |text: &str| {
        let mut rows = screen(u::END_OF_TURN_1_4_10);
        let caret = aterm_phase::muse::composer(&rows).expect("the composer");
        rows[caret] = format!("❯ {text}");
        rows
    };
    let muse = || {
        vec![
            screen(u::BUSY_1_4_10),
            screen(u::END_OF_TURN_1_4_10),
            muse_typed("keep going"),
            screen(u::BUSY_1_4_10),
        ]
    };
    let since_whole = |t: &Typed| {
        t.enter_at()
            .zip(t.first_read_of(2))
            .map(|(enter, whole)| enter.saturating_duration_since(whole))
    };
    let settles = |t: &Typed, ms: &str| {
        t.after_send()
            .iter()
            .filter(|r| r.starts_with(&format!("await idle {ms} ")))
            .count()
    };

    let t = fenced_echo(muse(), "muse", true, Some("600"), 0);
    assert_eq!(
        (t.sends().len(), t.enters().len()),
        (1, 1),
        "one write, one Enter: {}",
        t.evidence()
    );
    assert!(t.left_typed().is_none(), "{}", t.evidence());
    let held = since_whole(&t);
    assert!(
        held.is_some_and(|h| h >= turn_end_loop::PASTE_GUARD_HOLD),
        "the Enter {held:?} after the first whole read: {}",
        t.evidence()
    );

    let t = fenced_echo(muse(), "muse", true, None, 0);
    assert_eq!(t.enters().len(), 1, "{}", t.evidence());
    assert_eq!(
        settles(&t, "600"),
        1,
        "pressed at the first whole read: {}",
        t.evidence()
    );

    use aterm_phase::agy::fixtures as a;
    let t = fenced_echo(
        vec![
            screen(a::BUSY),
            screen(a::END_OF_TURN),
            agy_typed("keep going"),
            screen(a::BUSY),
        ],
        "agy",
        true,
        Some("500"),
        0,
    );
    assert_eq!(t.enters().len(), 1, "{}", t.evidence());
    assert_eq!(
        settles(&t, "500"),
        1,
        "agy pressed at its first whole read: {}",
        t.evidence()
    );
    assert!(
        since_whole(&t).is_some_and(|h| h < turn_end_loop::PASTE_GUARD_HOLD),
        "{}",
        t.evidence()
    );
}

#[test]
fn an_accept_key_taken_late_is_waited_for_and_nothing_is_typed_behind_it() {
    let said = "Stage 1 is in.";
    let world = |screens: Vec<Vec<String>>, filled: Option<usize>, sits: bool| {
        let mut m = Mock::new(screens);
        m.gen_fence = true;
        m.sends_gen = true;
        m.human_seq = Some(0);
        m.help = FENCED_HELP.to_string();
        if let Some(at) = filled {
            m.screen_cols.insert(at, 12);
        }
        if sits {
            m.key_gates = vec![(2, 1)];
        }
        m.vanish_after = Some(2);
        m.exit_when = Some(|m| m.attention.is_some());
        let (lines, _) = watch_lines(&mut m, &hosted(600));
        let sends = m.requests.iter().filter(|r| r.starts_with("send ")).count();
        let evidence = format!("{lines:#?}\n{:#?}", m.requests);
        (m, lines, sends, evidence)
    };

    let (m, lines, sends, evidence) = world(
        vec![
            busy_screen(),
            suggesting(said),
            suggesting(said),
            suggesting(said),
            suggesting(said),
            busy_screen(),
            ended(STOP),
        ],
        Some(4),
        false,
    );
    assert_eq!(sends, 0, "nothing typed behind the key: {evidence}");
    let presses = m.presses();
    assert!(
        presses.len() == 2
            && presses[0].ends_with(" right")
            && presses[1].ends_with(" enter")
            && presses[1].contains(" if-gen=1.105 "),
        "the accept key, then Enter on the filled read: {evidence}"
    );
    assert!(
        lines.contains(&format!(
            "CONTINUED seq=102 rule={RULE_SUGGESTION} keep going"
        )),
        "{evidence}"
    );

    let (m, _, sends, evidence) = world(
        vec![busy_screen(), suggesting(said), suggesting(said)],
        None,
        true,
    );
    assert_eq!(sends, 1, "its words typed once: {evidence}");
    let pressed = m
        .key_times
        .iter()
        .find(|(r, _)| r.ends_with(" right"))
        .map(|(_, at)| *at);
    let waited = m
        .first_send_at
        .zip(pressed)
        .map(|(sent, right)| sent.saturating_duration_since(right));
    assert!(
        waited.is_some_and(|w| w >= turn_end_loop::ECHO_HANG),
        "typed {waited:?} after the accept key: {evidence}"
    );
}

#[test]
fn a_screen_unmoved_since_the_write_is_waited_past_where_it_reads_as_another_text() {
    let world = |unmoved: bool| {
        let mut shown = weekly_limited();
        let caret = shown.iter().position(|row| row == "❯").expect("caret row");
        shown[caret] = "❯ keep going, then merge when ready".to_string();
        let mut typed = shown.clone();
        typed[caret] = "❯ keep going".to_string();
        let mut screens = vec![busy_screen(), shown.clone()];
        if !unmoved {
            screens.push(shown);
        }
        screens.extend([typed, busy_screen()]);
        let mut m = Mock::new(screens);
        m.cell_attrs = "dim";
        m.cursor_row = Some(caret + 1);
        m.gen_fence = true;
        m.sends_gen = true;
        m.human_seq = Some(0);
        m.help = FENCED_HELP.to_string();
        if unmoved {
            m.unmoved_after_send = Some(Duration::from_secs(15));
        }
        m.vanish_after = Some(2);
        m.exit_when = Some(|m| m.attention.is_some());
        let opts = SuperviseOpts {
            policy: SupervisorConfig {
                human_grace_s: 0,
                ..owners_policy()
            },
            ..resuming(&auto(30), None)
        };
        let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
            s.set_clock(AFTER_RESET, PDT);
            s.set_turn_end_timing(quick_turn_end(&[Duration::from_millis(10)]));
        });
        Typed { m, lines }
    };
    let seq_waits = |t: &Typed| {
        t.after_send()
            .iter()
            .filter(|r| r.starts_with("await seq "))
            .count()
    };

    let t = world(true);
    assert_eq!(t.sends().len(), 1, "written once: {}", t.evidence());
    assert!(t.sends()[0].ends_with(" -- keep going"), "{}", t.evidence());
    assert_eq!(t.enters().len(), 1, "one Enter: {}", t.evidence());
    assert!(
        t.enters()[0].contains("if=^❯\\x20keep\\x20going\\s*$"),
        "fenced on the read that showed it whole: {}",
        t.evidence()
    );
    assert!(t.left_typed().is_none(), "{}", t.evidence());
    let waited =
        t.m.first_send_at
            .zip(t.enter_at())
            .map(|(sent, enter)| enter.saturating_duration_since(sent));
    assert!(
        waited.is_some_and(|w| w >= Duration::from_secs(15)),
        "Enter {waited:?} after the write: {}",
        t.evidence()
    );

    let t = world(false);
    assert!(t.enters().is_empty(), "{}", t.evidence());
    assert!(
        t.left_typed()
            .is_some_and(|a| a.contains("the composer does not show it as written")),
        "{}",
        t.evidence()
    );
    assert_eq!(seq_waits(&t), 1, "escalated at once: {}", t.evidence());
}

fn muse_typed(text: &str) -> Vec<String> {
    use aterm_phase::muse::fixtures as u;
    let mut rows = aterm_phase::prompt::fixtures::screen(u::END_OF_TURN_1_4_10);
    let caret = aterm_phase::muse::composer(&rows).expect("the composer");
    rows[caret] = format!("❯ {text}");
    rows
}

fn claude_echo(
    screens: Vec<Vec<String>>,
    typed: &[(usize, usize)],
    shape: impl FnOnce(&mut Mock),
) -> Typed {
    let mut m = Mock::new(screens);
    m.gen_fence = true;
    m.sends_gen = true;
    m.human_seq = Some(0);
    m.help = FENCED_HELP.to_string();
    m.screen_cols.extend(typed.iter().copied());
    m.vanish_after = Some(2);
    m.exit_when = Some(|m| m.attention.is_some());
    shape(&mut m);
    let (lines, _) = watch_lines_with(&mut m, &hosted(600), |s| {
        s.set_turn_end_timing(TurnEndTiming {
            min_work: Duration::ZERO,
            ..TurnEndTiming::default()
        });
    });
    Typed { m, lines }
}

#[test]
fn a_torn_frame_or_an_empty_composer_after_the_write_is_an_echo_still_arriving() {
    use aterm_phase::muse::fixtures as u;
    use aterm_phase::prompt::fixtures::screen;
    let entered_once = |tag: &str, t: &Typed| {
        assert_eq!(
            (t.sends().len(), t.enters().len()),
            (1, 1),
            "{tag}: one write, one Enter: {}",
            t.evidence()
        );
        assert!(t.left_typed().is_none(), "{tag}: {}", t.evidence());
        assert!(
            t.enter_at()
                .zip(t.first_read_of(3))
                .is_some_and(|(enter, whole)| enter >= whole),
            "{tag}: the Enter is fenced on the read that showed the text whole: {}",
            t.evidence()
        );
    };

    let mut torn = muse_typed("");
    let caret = aterm_phase::muse::composer(&torn).expect("the composer");
    torn.truncate(caret + 2);
    let muse = Program::Muse.reader();
    assert!(
        muse.composer(&torn).is_none() && muse.frame_torn(&torn),
        "the fixture is a torn frame no composer reads on: {torn:#?}"
    );
    let t = fenced_echo_with(
        vec![
            screen(u::BUSY_1_4_10),
            screen(u::END_OF_TURN_1_4_10),
            torn,
            muse_typed("keep going"),
            screen(u::BUSY_1_4_10),
        ],
        "muse",
        |_| {},
    );
    entered_once("torn", &t);

    let said = "Fixed the parser; the suite is green.";
    let mut placeholder = ended(said);
    let caret = placeholder
        .iter()
        .position(|row| row == "❯")
        .expect("caret row");
    placeholder[caret] = "❯ Try \"fix the flaky test\"".to_string();
    for (tag, read) in [("empty", ended(said)), ("placeholder", placeholder)] {
        let t = claude_echo(
            vec![
                busy_screen(),
                ended(said),
                read,
                typed_in(said, "keep going"),
                busy_screen(),
            ],
            &[(3, 12)],
            |_| {},
        );
        entered_once(tag, &t);
    }

    let t = claude_echo(
        vec![
            busy_screen(),
            ended(said),
            typed_in(said, "fix the flaky test"),
            typed_in(said, "keep going"),
            busy_screen(),
        ],
        &[(2, 20), (3, 12)],
        |_| {},
    );
    assert!(t.enters().is_empty(), "{}", t.evidence());
    assert!(
        t.left_typed()
            .is_some_and(|a| a.contains("the composer does not show it as written")),
        "{}",
        t.evidence()
    );
    assert_eq!(
        t.after_send()
            .iter()
            .filter(|r| r.starts_with("await seq "))
            .count(),
        1,
        "escalated at once: {}",
        t.evidence()
    );
}

#[test]
fn a_paste_guarded_enter_times_its_hold_from_the_last_whole_read_after_a_redraw() {
    use aterm_phase::muse::fixtures as u;
    use aterm_phase::prompt::fixtures::screen;
    let world = |redrawn: bool| {
        let middle = if redrawn {
            muse_typed("keep")
        } else {
            muse_typed("keep going")
        };
        fenced_echo_with(
            vec![
                screen(u::BUSY_1_4_10),
                screen(u::END_OF_TURN_1_4_10),
                muse_typed("keep going"),
                middle,
                muse_typed("keep going"),
                muse_typed("keep going"),
                screen(u::BUSY_1_4_10),
            ],
            "muse",
            |m| m.idle_never_for = Some("600"),
        )
    };
    let held_from = |t: &Typed, index: usize| {
        t.enter_at()
            .zip(t.first_read_of(index))
            .map(|(enter, whole)| enter.saturating_duration_since(whole))
    };

    let t = world(true);
    assert_eq!(
        (t.sends().len(), t.enters().len()),
        (1, 1),
        "{}",
        t.evidence()
    );
    assert!(t.left_typed().is_none(), "{}", t.evidence());
    let held = held_from(&t, 4);
    assert!(
        held.is_some_and(|h| h >= turn_end_loop::PASTE_GUARD_HOLD)
            && t.enters()[0].contains(" if-gen=1.106 "),
        "the Enter {held:?} after the whole read that followed the redraw: {}",
        t.evidence()
    );

    let t = world(false);
    assert_eq!(t.enters().len(), 1, "{}", t.evidence());
    assert!(
        held_from(&t, 2).is_some_and(|h| h >= turn_end_loop::PASTE_GUARD_HOLD)
            && t.enters()[0].contains(" if-gen=1.104 "),
        "entered the hold after the first whole read, on the next: {}",
        t.evidence()
    );
}

#[test]
fn a_settle_that_latched_before_the_echos_last_draw_holds_nothing() {
    use aterm_phase::muse::fixtures as u;
    use aterm_phase::prompt::fixtures::screen;
    let world = |behind: bool| {
        fenced_echo_with(
            vec![
                screen(u::BUSY_1_4_10),
                screen(u::END_OF_TURN_1_4_10),
                muse_typed("keep going"),
                screen(u::BUSY_1_4_10),
            ],
            "muse",
            |m| {
                m.key_gates = vec![(2, 0)];
                m.idle_latch_behind = behind;
            },
        )
    };
    let since_whole = |t: &Typed| {
        t.enter_at()
            .zip(t.first_read_of(2))
            .map(|(enter, whole)| enter.saturating_duration_since(whole))
    };
    let settles = |t: &Typed| {
        t.after_send()
            .iter()
            .filter(|r| r.starts_with("await idle 600 "))
            .count()
    };

    let t = world(true);
    assert_eq!(
        (t.sends().len(), t.enters().len()),
        (1, 1),
        "{}",
        t.evidence()
    );
    let held = since_whole(&t);
    assert!(
        held.is_some_and(|h| h >= turn_end_loop::PASTE_GUARD_HOLD) && settles(&t) >= 2,
        "the Enter {held:?} after the first whole read: {}",
        t.evidence()
    );

    let t = world(false);
    assert_eq!(t.enters().len(), 1, "{}", t.evidence());
    assert_eq!(settles(&t), 1, "entered at that read: {}", t.evidence());
}

#[test]
fn a_read_on_another_grid_is_never_the_screen_the_write_found() {
    use aterm_phase::agy::fixtures as a;
    use aterm_phase::prompt::fixtures::screen;
    let world = |grid: (u64, u64)| {
        fenced_echo_with(
            vec![
                screen(a::BUSY),
                screen(a::END_OF_TURN),
                agy_typed("wkeep going"),
            ],
            "agy",
            |m| {
                m.key_gates = vec![(2, 0)];
                m.screen_gens.insert(2, grid);
            },
        )
    };
    let seq_waits = |t: &Typed| {
        t.after_send()
            .iter()
            .filter(|r| r.starts_with("await seq "))
            .count()
    };

    let entered_first = |t: &Typed| {
        t.after_send()
            .len()
            .checked_add(1)
            .and_then(|n| {
                t.m.requests
                    .iter()
                    .skip_while(|r| !r.starts_with("send "))
                    .nth(n)
            })
            .is_some_and(|r| r.starts_with("key ") && r.ends_with(" enter"))
    };

    let t = world((2, 3));
    assert!(!entered_first(&t), "{}", t.evidence());
    assert!(
        t.left_typed()
            .is_some_and(|a| a.contains("(the composer does not show it as written)")),
        "{}",
        t.evidence()
    );
    assert_eq!(seq_waits(&t), 1, "escalated at once: {}", t.evidence());

    let t = world((1, 102));
    assert!(!entered_first(&t), "{}", t.evidence());
    assert!(
        t.left_typed()
            .is_some_and(|a| a.contains("had not shown it whole a minute after")),
        "{}",
        t.evidence()
    );
}

#[test]
fn a_hand_held_through_a_slow_echo_is_renewed_before_it_lapses() {
    use crate::harness::upgrade_drive::{HAND_RENEW, HAND_TTL_MS};
    let said = "Fixed the parser; the suite is green.";
    let acquired =
        || ok("OK lease acquired holder=aterm-harness@1 ttl_ms=60000 expires_in_ms=60000 hard=1\n");
    let run = |hand: bool| {
        let mut m = Mock::new(vec![
            busy_screen(),
            ended(said),
            typed_in(said, "keep going"),
            busy_screen(),
            ended(STOP),
        ]);
        m.gen_fence = true;
        m.sends_gen = true;
        m.human_seq = Some(0);
        m.help = FENCED_HELP.to_string();
        m.screen_cols.insert(2, 12);
        m.vanish_after = Some(2);
        m.unmoved_after_send = Some(Duration::from_secs(50));
        m.verb_replies.insert(
            "lease",
            VecDeque::from([
                acquired(),
                acquired(),
                acquired(),
                acquired(),
                acquired(),
                ok("OK lease released\n"),
            ]),
        );
        let (lines, _) = watch_lines_with(&mut m, &hosted(600), |s| {
            s.set_supervisor_name(Some(HAND_HOLDER.to_string()));
            s.type_under_hand(hand);
        });
        let evidence = format!("{lines:#?}\n{:#?}\n{:#?}", m.requests, m.lease_times);
        assert!(
            lines.contains(&format!(
                "CONTINUED seq=102 rule={RULE_CONTINUE} keep going"
            )),
            "{evidence}"
        );
        (m, evidence)
    };

    let (m, evidence) = run(true);
    let take = crate::harness::upgrade_drive::hand_take_here_request_as(HAND_HOLDER);
    let takes: Vec<Instant> = m
        .lease_times
        .iter()
        .filter(|(r, _)| *r == take)
        .map(|(_, at)| *at)
        .collect();
    let enter = m
        .key_times
        .iter()
        .find(|(r, _)| r.ends_with(" enter"))
        .map(|(_, at)| *at)
        .expect("the Enter");
    assert!(takes.len() >= 2, "taken and renewed: {evidence}");
    let longest = takes
        .iter()
        .chain(std::iter::once(&enter))
        .collect::<Vec<_>>()
        .windows(2)
        .map(|w| w[1].saturating_duration_since(*w[0]))
        .max()
        .unwrap_or_default();
    assert!(
        longest <= HAND_RENEW * 2 && longest < Duration::from_millis(HAND_TTL_MS),
        "the hand went {longest:?} without a renewal: {evidence}"
    );

    let (m, evidence) = run(false);
    assert!(m.lease_times.is_empty(), "{evidence}");
}

#[test]
fn a_text_left_typed_on_a_screen_that_never_moved_is_never_typed_again_on_it() {
    let said = "Fixed the parser; the suite is green.";
    let act = |unmoved: Duration| {
        let mut m = Mock::new(vec![
            busy_screen(),
            ended(said),
            typed_in(said, "keep going"),
            busy_screen(),
        ]);
        m.gen_fence = true;
        m.sends_gen = true;
        m.human_seq = Some(0);
        m.help = FENCED_HELP.to_string();
        m.screen_cols.insert(2, 12);
        m.unmoved_after_send = Some(unmoved);
        let mut s = virtual_session(&mut m, None);
        let point = s.await_turn(Duration::from_secs(60)).expect("the turn end");
        let mut heard = Heard::default();
        let typed = s
            .type_text(&point, "keep going", RULE_CONTINUE, &mut heard)
            .expect("the act");
        let held = s.input_unread(point.screen.seq, RULE_CONTINUE, &mut heard);
        drop(s);
        let evidence = format!("{:#?}\n{:#?}", heard.noted, m.requests);
        let attention = m
            .requests
            .iter()
            .find(|r| r.starts_with("meta set attention owner=supervisor "))
            .cloned();
        (typed, held, heard.noted, attention, evidence)
    };

    let (typed, held, noted, attention, evidence) = act(Duration::from_secs(120));
    assert!(!typed, "left typed: {evidence}");
    assert!(
        attention.is_some_and(|a| a.contains(
            "(typed, not submitted (the composer had not shown it whole a minute after it was \
             written)"
        )),
        "escalated as a hang: {evidence}"
    );
    assert!(held, "the next act on that screen is held: {evidence}");
    assert!(
        noted.iter().any(|n| n.starts_with("SKIPPED seq=102 ")
            && n.contains("the worker has not drawn the input it read")),
        "{evidence}"
    );

    let (typed, held, _, attention, evidence) = act(Duration::from_secs(5));
    assert!(typed, "entered: {evidence}");
    assert!(attention.is_none(), "{evidence}");
    assert!(!held, "nothing held back: {evidence}");
}

const HAND_HOLDER: &str = "aterm-harness@1";

#[test]
fn the_windows_host_types_the_fenced_continuation_under_its_hand() {
    let take = crate::harness::upgrade_drive::hand_take_here_request_as(HAND_HOLDER);
    let back = crate::harness::upgrade_drive::hand_give_back_here_request_as(HAND_HOLDER);
    assert_ne!(
        take,
        crate::harness::upgrade_drive::hand_take_here_request(),
        "the holder is no pid of this process's"
    );
    assert!(
        take.ends_with(" hard here") && back.ends_with(" here"),
        "{take} / {back}"
    );
    let fenced = [
        "send if-human=0 if-gen=1.102 if=^❯\\s*$ -- keep going",
        "key if-human=0 if-gen=1.103 if=^❯\\x20keep\\x20going\\s*$ enter",
    ];
    let run = |hand: bool, acquired: CtlReply| {
        let mut m = Mock::new(vec![
            busy_screen(),
            ended("Fixed the parser; the suite is green."),
            typed_in("Fixed the parser; the suite is green.", "keep going"),
            busy_screen(),
            ended(STOP),
        ]);
        m.gen_fence = true;
        m.sends_gen = true;
        m.human_seq = Some(0);
        m.help = FENCED_HELP.to_string();
        m.screen_cols.insert(2, 12);
        m.vanish_after = Some(2);
        m.verb_replies.insert(
            "lease",
            VecDeque::from([acquired, ok("OK lease released\n")]),
        );
        let (lines, _) = watch_lines_with(&mut m, &hosted(30), |s| {
            s.set_supervisor_name(Some(HAND_HOLDER.to_string()));
            s.type_under_hand(hand);
        });
        assert!(
            lines.contains(&format!(
                "CONTINUED seq=102 rule={RULE_CONTINUE} keep going"
            )),
            "{hand}: {lines:#?}\n{:#?}",
            m.requests
        );
        m.requests
            .iter()
            .filter(|r| ["lease ", "send ", "key "].iter().any(|v| r.starts_with(v)))
            .cloned()
            .collect::<Vec<String>>()
    };
    let acquired =
        || ok("OK lease acquired holder=aterm-harness@1 ttl_ms=60000 expires_in_ms=60000 hard=1\n");
    assert_eq!(
        run(true, acquired()),
        [take.as_str(), fenced[0], fenced[1], back.as_str()]
    );
    for refused in [
        "lease held holder=manager expires_in_ms=9000",
        "lease held holder=aterm-harness@1 expires_in_ms=9000",
        "busy turn=4",
    ] {
        assert_eq!(
            run(true, err(refused)),
            [take.as_str(), fenced[0], fenced[1]],
            "a tab held elsewhere is not taken: {refused}"
        );
    }
    assert_eq!(run(false, acquired()), fenced, "no hand asked for");
}

#[test]
fn the_windows_host_types_the_workers_suggestion_under_its_hand() {
    let run = |hand: bool| {
        let mut m = Mock::new(vec![
            busy_screen(),
            suggesting("Stage 1 is in."),
            typed_in("Stage 1 is in.", "keep going"),
            busy_screen(),
            ended(STOP),
        ]);
        m.gen_fence = true;
        m.sends_gen = true;
        m.human_seq = Some(0);
        m.help = FENCED_HELP.to_string();
        m.screen_cols.insert(2, 12);
        m.vanish_after = Some(2);
        m.verb_replies.insert(
            "lease",
            VecDeque::from([
                ok("OK lease acquired holder=aterm-harness@1 ttl_ms=60000 \
                    expires_in_ms=60000 hard=1\n"),
                ok("OK lease released\n"),
            ]),
        );
        let (lines, _) = watch_lines_with(&mut m, &hosted(30), |s| {
            s.set_supervisor_name(Some(HAND_HOLDER.to_string()));
            s.type_under_hand(hand);
        });
        assert!(
            lines.contains(&format!(
                "CONTINUED seq=102 rule={RULE_SUGGESTION} keep going"
            )),
            "{hand}: {lines:#?}\n{:#?}",
            m.requests
        );
        assert_eq!(count(&m, "turn"), 0, "{:#?}", m.requests);
        m.requests
            .iter()
            .filter(|r| ["lease ", "send ", "key "].iter().any(|v| r.starts_with(v)))
            .cloned()
            .collect::<Vec<String>>()
    };
    assert_eq!(
        run(true),
        [
            crate::harness::upgrade_drive::hand_take_here_request_as(HAND_HOLDER).as_str(),
            "send if-human=0 if-gen=1.102 if=^❯\\x20keep\\x20going\\s*$ -- keep going",
            "key if-human=0 if-gen=1.103 if=^❯\\x20keep\\x20going\\s*$ enter",
            crate::harness::upgrade_drive::hand_give_back_here_request_as(HAND_HOLDER).as_str(),
        ]
    );
    let accepted = run(false);
    assert_eq!(
        accepted.first().map(String::as_str),
        Some("key if-gen=1.102 if=^❯\\x20keep\\x20going\\s*$ right"),
        "the accept key: {accepted:#?}"
    );
    assert!(
        !accepted
            .iter()
            .any(|r| r.starts_with("lease ") || r.starts_with("send ")),
        "{accepted:#?}"
    );
}

#[test]
fn a_continuation_a_refusal_turned_away_is_typed_once_it_can_be() {
    for refusal in ["halted reason=owner origin=local", "busy lease=s-9"] {
        let (jdir, journal) = journal_file("refused-continue");
        let mut m = Mock::new(vec![
            busy_screen(),
            ended("Fixed the parser; the suite is green."),
        ]);
        m.verb_replies
            .insert("turn", VecDeque::from([err(refusal)]));
        if refusal.starts_with("halted") {
            m.hold = "1";
            m.hold_lifts_after = Some(2);
        }
        m.stall_sleep = Some(Duration::from_millis(20));
        m.vanish_after = Some(1500);
        m.exit_when = Some(|m| m.turns >= 2);
        let opts = SuperviseOpts {
            journal: Some(journal.clone()),
            ..hosted(60)
        };
        let (lines, _) = watch_lines(&mut m, &opts);
        assert_eq!(count(&m, "turn"), 2, "{refusal}: {:#?}", m.requests);
        assert!(
            lines.iter().any(|l| l.starts_with("CONTINUED seq=")
                && l.ends_with(&format!("rule={RULE_CONTINUE} keep going"))),
            "{refusal}: {lines:#?}"
        );
        let (records, _) = journal_records(&journal);
        let notes: Vec<&str> = records.iter().map(|r| r.line.as_str()).collect();
        let lifted = if refusal.starts_with("halted") {
            "UNHALTED seq=102"
        } else {
            "REFUSED seq=102"
        };
        assert!(
            notes.iter().any(|n| n.starts_with(lifted)),
            "{refusal}: {notes:#?}"
        );
        let _ = std::fs::remove_dir_all(&jdir);
    }

    let mut m = Mock::new(vec![
        busy_screen(),
        ended("Fixed the parser; the suite is green."),
    ]);
    m.gen_fence = true;
    m.sends_gen = true;
    m.human_seq = Some(0);
    m.help = FENCED_HELP.to_string();
    m.send_replies
        .push_back(err("halted reason=owner origin=local"));
    m.hold = "1";
    m.hold_lifts_after = Some(2);
    m.stall_sleep = Some(Duration::from_millis(20));
    m.vanish_after = Some(1500);
    m.exit_when = Some(|m| m.requests.iter().filter(|r| r.starts_with("send ")).count() >= 2);
    let _ = watch_lines(&mut m, &hosted(60));
    let sends: Vec<&String> = m
        .requests
        .iter()
        .filter(|r| r.starts_with("send "))
        .collect();
    assert_eq!(sends.len(), 2, "{:#?}", m.requests);
    assert_eq!(
        sends[0],
        "send if-human=0 if-gen=1.102 if=^❯\\s*$ -- keep going"
    );
    assert!(
        sends[1].starts_with("send if-human=0 if-gen=1.")
            && sends[1].ends_with(" if=^❯\\s*$ -- keep going"),
        "{sends:#?}"
    );
}

#[test]
fn a_keep_going_continuation_waits_out_an_edit_hold() {
    let editing = "busy editing ticket=41 view=7";
    let (jdir, journal) = journal_file("edit-hold");
    let mut m = Mock::new(vec![
        busy_screen(),
        ended("Fixed the parser; the suite is green."),
    ]);
    m.verb_replies.insert(
        "turn",
        VecDeque::from([err(editing), err(editing), err(editing)]),
    );
    m.stall_sleep = Some(Duration::from_millis(20));
    m.vanish_after = Some(1500);
    m.exit_when = Some(|m| m.turns >= 4);
    let opts = SuperviseOpts {
        journal: Some(journal.clone()),
        ..hosted(60)
    };
    let (lines, _) = watch_lines(&mut m, &opts);
    assert_eq!(
        count(&m, "turn"),
        4,
        "three refused while editing, then typed: {:#?}",
        m.requests
    );
    let continued: Vec<&String> = lines
        .iter()
        .filter(|l| {
            l.starts_with("CONTINUED seq=")
                && l.ends_with(&format!("rule={RULE_CONTINUE} keep going"))
        })
        .collect();
    assert_eq!(continued.len(), 1, "typed once, after: {lines:#?}");
    let (records, _) = journal_records(&journal);
    let refused: Vec<&str> = records
        .iter()
        .map(|r| r.line.as_str())
        .filter(|n| n.starts_with("REFUSED "))
        .collect();
    assert_eq!(
        refused.len(),
        1,
        "said once, not at every retry: {refused:#?}"
    );
    assert!(
        refused[0].ends_with("ERR busy editing ticket=41 view=7"),
        "{refused:#?}"
    );
    let _ = std::fs::remove_dir_all(&jdir);

    let mut m = Mock::new(vec![
        busy_screen(),
        ended("Fixed the parser; the suite is green."),
    ]);
    m.gen_fence = true;
    m.sends_gen = true;
    m.human_seq = Some(0);
    m.help = FENCED_HELP.to_string();
    m.send_replies.push_back(err(editing));
    m.send_replies.push_back(err(editing));
    m.stall_sleep = Some(Duration::from_millis(20));
    m.vanish_after = Some(1500);
    m.exit_when = Some(|m| m.requests.iter().filter(|r| r.starts_with("send ")).count() >= 3);
    let _ = watch_lines(&mut m, &hosted(60));
    let sends: Vec<&String> = m
        .requests
        .iter()
        .filter(|r| r.starts_with("send "))
        .collect();
    assert_eq!(
        sends.len(),
        3,
        "two refused, then written: {:#?}",
        m.requests
    );
    for send in &sends {
        assert!(send.ends_with(" -- keep going"), "{sends:#?}");
    }
}

#[test]
fn a_refusals_episode_is_its_holder_and_ends_after_a_quiet_spell() {
    assert_eq!(
        super::super::lifecycle::refusal_key(
            "REFUSED ERR busy input-unread bytes=4096 wait_ms=1200"
        ),
        "REFUSED ERR busy input-unread"
    );
    assert_eq!(
        super::super::lifecycle::refusal_key("REFUSED ERR busy turn=17"),
        "REFUSED ERR busy turn=17"
    );
    let mut m = Mock::new(vec![busy_screen()]);
    let mut s = session(&mut m, None);
    let t = Instant::now();
    let person = |ms: u32| format!("REFUSED ERR busy person human_ms={ms}");
    assert!(s.refusal_news(&person(1200), t), "first met");
    let mut at = t;
    for i in 0..400u32 {
        at += Duration::from_secs(9);
        assert!(!s.refusal_news(&person(1200 + i), at), "retry {i}");
    }
    at += REFUSAL_QUIET;
    assert!(s.refusal_news(&person(5), at), "after the quiet spell");
    for said in [
        "REFUSED ERR busy lease=s-9",
        "REFUSED ERR busy lease=s-7",
        "REFUSED ERR busy turn=17",
        "REFUSED ERR busy turn=18",
        "HALTED ERR halted reason=owner origin=local",
        "HALTED ERR halted reason=fabric-lost origin=fleet",
    ] {
        at += Duration::from_secs(1);
        assert!(s.refusal_news(said, at), "{said}");
    }
}

#[test]
fn a_refusals_episode_ends_at_a_taken_write_and_at_a_lifted_hold() {
    let refused_lines = |heard: &Heard| -> Vec<String> {
        heard
            .noted
            .iter()
            .filter(|l| l.starts_with("REFUSED ") || l.starts_with("HALTED "))
            .map(|l| {
                l.split(' ')
                    .filter(|w| !w.starts_with("seq=") && !w.starts_with("backoff="))
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect()
    };
    let mut m = Mock::new(vec![ended("Fixed the parser; the suite is green.")]);
    m.verb_replies.insert(
        "turn",
        VecDeque::from([
            err("busy lease=s-9"),
            ok("OK id=1 submitted=1 status=settled seq=100 dur_ms=600 hash=0\n"),
        ]),
    );
    m.stall_sleep = Some(Duration::from_millis(5));
    let mut s = session(&mut m, Some("@s-1".to_string()));
    let mut heard = Heard::default();
    let lease = "ERR busy lease=s-9";
    s.back_off(lease, 101, None, &mut heard)
        .unwrap_or_else(|e| panic!("{e:?}"));
    s.back_off(lease, 102, None, &mut heard)
        .unwrap_or_else(|e| panic!("{e:?}"));
    let r = s
        .call(&["turn", "keep going"])
        .unwrap_or_else(|e| panic!("{e:?}"));
    assert!(r.is_err("busy"), "{r:?}");
    s.back_off(lease, 103, None, &mut heard)
        .unwrap_or_else(|e| panic!("{e:?}"));
    assert_eq!(refused_lines(&heard), vec![format!("REFUSED {lease}")]);
    let r = s
        .call(&["turn", "keep going"])
        .unwrap_or_else(|e| panic!("{e:?}"));
    assert!(r.ok(), "{r:?}");
    s.back_off(lease, 104, None, &mut heard)
        .unwrap_or_else(|e| panic!("{e:?}"));
    assert_eq!(
        refused_lines(&heard),
        vec![format!("REFUSED {lease}"), format!("REFUSED {lease}")]
    );

    let mut m = Mock::new(vec![ended("Fixed the parser; the suite is green.")]);
    m.hold = "1";
    m.hold_lifts_after = Some(1);
    m.stall_sleep = Some(Duration::from_millis(5));
    let mut s = session(&mut m, Some("@s-1".to_string()));
    let mut heard = Heard::default();
    let hold = "ERR halted reason=owner origin=local";
    s.park_on_hold(hold, 101, &mut heard)
        .unwrap_or_else(|e| panic!("{e:?}"));
    assert!(
        heard.noted.iter().any(|l| l.starts_with("UNHALTED ")),
        "{:#?}",
        heard.noted
    );
    s.park_on_hold(hold, 102, &mut heard)
        .unwrap_or_else(|e| panic!("{e:?}"));
    assert_eq!(
        refused_lines(&heard),
        vec![format!("HALTED {hold}"), format!("HALTED {hold}")]
    );
}

#[test]
fn a_fenced_press_a_hold_turned_away_is_pressed_once_it_lifts() {
    let held = |screen: Vec<String>, key: &'static str| {
        let mut m = Mock::new(vec![busy_screen(), screen]);
        m.gen_fence = true;
        m.sends_gen = true;
        m.help = FENCED_HELP.to_string();
        m.key_replies
            .push_back(err("halted reason=owner origin=local"));
        m.hold = "1";
        m.hold_lifts_after = Some(2);
        m.stall_sleep = Some(Duration::from_millis(20));
        m.vanish_after = Some(1500);
        m.exit_when = Some(match key {
            "right" => |m: &Mock| m.presses().iter().filter(|p| p.ends_with(" right")).count() >= 2,
            _ => |m: &Mock| m.presses().iter().filter(|p| p.ends_with(" enter")).count() >= 2,
        });
        m
    };
    let opts = SuperviseOpts {
        policy: SupervisorConfig {
            answer_questions: false,
            human_grace_s: 1,
            ..owners_policy()
        },
        ..auto(60)
    };
    let mut m = held(suggesting("Stage 1 is in."), "right");
    let _ = watch_lines(&mut m, &opts);
    let rights: Vec<&String> = m
        .presses()
        .into_iter()
        .filter(|p| p.ends_with(" right"))
        .collect();
    assert_eq!(rights.len(), 2, "{:#?}", m.requests);
    assert_eq!(
        rights[0],
        "key if-gen=1.102 if=^❯\\x20keep\\x20going\\s*$ right"
    );
    let mut m = held(typed_in("Stage 1 is in.", "also check the lexer"), "enter");
    m.cursor_col = Some(24);
    let _ = watch_lines(&mut m, &opts);
    let enters: Vec<&String> = m
        .presses()
        .into_iter()
        .filter(|p| p.ends_with(" enter"))
        .collect();
    assert_eq!(enters.len(), 2, "{:#?}", m.requests);
    assert!(
        enters
            .iter()
            .all(|p| p.starts_with("key if-gen=1.") && p.contains("lexer")),
        "{enters:#?}"
    );
}

#[test]
fn a_long_texts_guard_never_matches_a_shells_row() {
    let answer = owners_policy().answer_text;
    let rules = format!(
        "keep going (standing rules: {})",
        "stay on the branch, never push to main, run the lane's tests before every commit, \
         and keep the ledger rows exact"
    );
    for text in [&answer, &rules] {
        for caret in ['❯', '›'] {
            let guard = aterm_observe::row_matcher(
                &crate::supervise::run::turn_end_loop::composer_guard(caret, text),
            )
            .expect("a pattern");
            for shell in [
                "user@host ~ % Nobody is here to answer. Decide for yourself with your best \
                 judgment: take the option you would recommend, prefer reversible steps, and \
                 keep going.",
                "ould recommend, prefer reversible steps, and keep going.",
                "$ keep going.",
                "$ keep the ledger rows exact)",
            ] {
                assert!(!guard.matches(shell), "{caret}: {shell}");
            }
            for width in 24..=200 {
                let last = word_wrapped_last_row(&format!("{caret} {text}"), width);
                assert!(guard.matches(&last), "{caret} width {width}: {last:?}");
            }
            let nbsp = format!("{caret}\u{a0}{text}");
            assert!(guard.matches(&nbsp), "{nbsp}");
        }
    }
    let long = "word ".repeat(200);
    let capped = crate::supervise::run::turn_end_loop::capped_rules(long.trim_end());
    assert!(capped.chars().count() <= crate::supervise::run::turn_end_loop::RULES_CAP + 1);
    assert!(capped.ends_with("word…"), "{capped}");
    assert_eq!(
        crate::supervise::run::turn_end_loop::capped_rules("short rules"),
        "short rules"
    );
}

#[test]
fn gemini_clis_guard_is_anchored_one_column_in() {
    use crate::supervise::run::turn_end_loop::{composer_guard, composer_guard_at};
    let reader = &aterm_phase::GeminiReader;
    let (caret, column) = (
        aterm_phase::ScreenReader::caret(reader).expect("a caret"),
        aterm_phase::ScreenReader::caret_column(reader),
    );
    assert_eq!((caret, column), ('>', 1));
    let rows = aterm_phase::prompt::fixtures::screen(aterm_phase::gemini::fixtures::DRAFT);
    let measured = rows
        .iter()
        .find(|r| r.starts_with(" > "))
        .expect("the caret row");
    let text = "a draft the lab never sends";
    let g = aterm_observe::row_matcher(&composer_guard_at(caret, column, text)).expect("compiles");
    assert!(g.matches(measured), "{measured}");
    assert!(g.matches(&format!(" > {text} {:60}", "")));
    for other in [
        format!("> {text}"),
        format!("  > {text}"),
        " >   Type your message or @path/to/file".to_string(),
        format!("$ {text}"),
    ] {
        assert!(!g.matches(&other), "{other}");
    }
    let old = aterm_observe::row_matcher(&composer_guard(caret, text)).expect("compiles");
    assert!(!old.matches(measured), "the control");

    let long = owners_policy().answer_text;
    let g = aterm_observe::row_matcher(&composer_guard_at(caret, column, &long)).expect("compiles");
    let old = aterm_observe::row_matcher(&composer_guard(caret, &long)).expect("compiles");
    let mut old_missed = 0;
    for width in 30..=160 {
        let last = gemini_wrapped_last_row(&long, width);
        assert!(g.matches(&last), "width {width}: {last:?}");
        old_missed += usize::from(!old.matches(&last));
    }
    assert!(old_missed > 0, "the column-0 guard misses some width");
    for shell in [
        format!("user@host ~ % {long}"),
        "ould recommend, prefer reversible steps, and keep going.".to_string(),
        "> keep going.".to_string(),
        "$ keep going.".to_string(),
    ] {
        assert!(!g.matches(&shell), "{shell}");
    }
}

#[test]
fn a_yolo_caret_is_escaped_in_its_guard() {
    use crate::supervise::run::turn_end_loop::{composer_guard, composer_guard_at};
    let rows = aterm_phase::prompt::fixtures::screen(aterm_phase::gemini::fixtures::YOLO_DRAFT);
    let caret = aterm_phase::ScreenReader::caret_on(&aterm_phase::GeminiReader, &rows)
        .expect("the composer's caret");
    assert_eq!(caret, '*');
    let measured = rows
        .iter()
        .find(|r| r.starts_with(" * "))
        .expect("the caret row");
    let text = "hello in yolo";
    let guard = composer_guard_at(caret, 1, text);
    assert!(guard.starts_with("^\\s\\*\\s"), "{guard}");
    let g = aterm_observe::row_matcher(&guard).expect("compiles");
    assert!(g.matches(measured), "{measured}");
    for other in [
        format!(" > {text}"),
        format!("* {text}"),
        format!("  * {text}"),
    ] {
        assert!(!g.matches(&other), "{other}");
    }
    let long = owners_policy().answer_text;
    let g = aterm_observe::row_matcher(&composer_guard_at(caret, 1, &long)).expect("compiles");
    for width in 30..=160 {
        let last = gemini_wrapped_last_row(&long, width).replacen(" > ", " * ", 1);
        assert!(g.matches(&last), "width {width}: {last:?}");
    }
    assert_eq!(
        composer_guard('❯', "keep going"),
        "^❯\\skeep\\x20going\\s*$"
    );
    assert_eq!(
        composer_guard('›', "keep going"),
        "^›\\skeep\\x20going\\s*$"
    );
    assert_eq!(
        composer_guard_at('>', 1, "keep going"),
        "^\\s>\\skeep\\x20going\\s*$"
    );
}

#[test]
fn muse_codes_measured_drafts_take_the_composer_guard() {
    use crate::supervise::run::turn_end_loop::composer_guard;
    use aterm_phase::muse::fixtures as u;
    use aterm_phase::prompt::fixtures::screen;
    let matcher = |caret: char, text: &str| {
        aterm_observe::row_matcher(&composer_guard(caret, text)).expect("a pattern")
    };
    let mut short = screen(u::IDLE_1_4_10);
    let caret = aterm_phase::muse::composer(&short).expect("the composer");
    short[caret] = "❯ keep going".to_string();
    assert!(matcher('❯', "keep going").matches("❯ keep going"));
    let codex = matcher('›', "keep going");
    assert!(!short.iter().any(|r| codex.matches(r)), "the control");

    let long = "keep going. Standing rules: never push to main without the merge contract green, \
                run the tests for what you touched, keep the shared tree clean, commit each \
                finished slice with its own message, and say what you measured and what you did \
                not, in plain words without hedging, before ending the turn here";
    let wrapped = screen(u::DRAFT_WRAPPED);
    let (caret, lines) = aterm_phase::muse::composer_draft(&wrapped).expect("the draft");
    assert_eq!(lines.len(), 3);
    let guard = matcher('❯', long);
    assert_eq!(wrapped[caret + 2], "  before ending the turn here");
    assert!(guard.matches(&wrapped[caret + 2]));
    for row in &wrapped[caret..caret + 2] {
        assert!(!guard.matches(row), "the control: {row}");
    }
}

fn gemini_wrapped_last_row(text: &str, width: usize) -> String {
    let room = width - 4;
    let chars: Vec<char> = text.chars().collect();
    let (mut at, mut rows) = (0, Vec::new());
    while at < chars.len() {
        let end = (at + room).min(chars.len());
        let cut = if end == chars.len() {
            end
        } else {
            (at + 1..=end)
                .rev()
                .find(|&i| chars[i] == ' ')
                .unwrap_or(end)
        };
        rows.push(chars[at..cut].iter().collect::<String>());
        at = if cut < chars.len() && chars[cut] == ' ' {
            cut + 1
        } else {
            cut
        };
    }
    let last = rows.len() - 1;
    format!("{}{}", if last == 0 { " > " } else { "   " }, rows[last])
}

fn word_wrapped_last_row(text: &str, width: usize) -> String {
    const INDENT: &str = "  ";
    let len = |s: &str| s.chars().count();
    let mut row = String::new();
    for word in text.split(' ') {
        let mut word = word.to_string();
        loop {
            let fresh = row.is_empty() || row == INDENT;
            let need = len(&word) + usize::from(!fresh);
            if len(&row) + need <= width {
                if !fresh {
                    row.push(' ');
                }
                row.push_str(&word);
                break;
            }
            if fresh {
                let room = width - len(&row);
                row.extend(word.chars().take(room));
                word = word.chars().skip(room).collect();
            }
            row = INDENT.to_string();
        }
    }
    row
}

fn seed_ledger(path: &std::path::Path, age_min: i64, sid: Option<&str>) {
    let now = crate::supervise::journal::unix_ms();
    let rows: Vec<String> = (0..6)
        .map(|k| {
            crate::supervise::approvals::Row {
                rule_id: RULE_CONTINUE,
                outcome: crate::supervise::approvals::Outcome::Typed,
                command: "keep going",
                reason: "the turn-end policy",
                box_seq: 100 + u64::try_from(k).unwrap(),
            }
            .to_json(now - (age_min + k) * 60_000, sid)
        })
        .collect();
    std::fs::write(path, rows.join("\n") + "\n").expect("the ledger");
}

#[test]
fn the_typing_budget_counts_an_earlier_loops_acts_from_the_ledger() {
    for (tag, age, sid, cap, spent) in [
        ("recent", 1, None, 6, true),
        ("old", 120, None, 6, false),
        ("other", 1, Some("@s-9"), 6, false),
        ("uncapped", 1, None, 0, false),
    ] {
        let (dir, ledger) = ledger_at(&format!("budget-{tag}"));
        seed_ledger(&ledger, age, sid);
        let mut m = Mock::new(vec![
            busy_screen(),
            ended("Fixed the parser; the suite is green."),
        ]);
        m.vanish_after = Some(2);
        let mut opts = hosted(30);
        opts.policy.continue_per_hour = cap;
        let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
            s.set_approval_ledger(Some(ledger.clone()));
        });
        let _ = std::fs::remove_dir_all(&dir);
        if spent {
            assert_eq!(count(&m, "turn"), 0, "{tag}: {lines:#?}");
            assert!(
                m.attention
                    .as_deref()
                    .is_some_and(|a| a.contains("continue_per_hour")),
                "{tag}: {:?}\n{lines:#?}\n{:#?}",
                m.attention,
                m.requests
            );
        } else {
            assert_eq!(count(&m, "turn"), 1, "{tag}: {lines:#?}");
        }
    }
}

#[test]
fn a_spent_budget_escalates_and_types_once_the_hour_frees() {
    for (tag, age, turns) in [("frees", 54, 1), ("held", 1, 0)] {
        let (dir, ledger) = ledger_at(&format!("budget-frees-{tag}"));
        seed_ledger(&ledger, age, None);
        let mut m = Mock::new(vec![
            busy_screen(),
            ended("Fixed the parser; the suite is green."),
        ]);
        m.stall_sleep = Some(Duration::from_secs(3600));
        let mut opts = hosted(240);
        opts.policy.continue_per_hour = 6;
        let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
            s.set_approval_ledger(Some(ledger.clone()));
        });
        let _ = std::fs::remove_dir_all(&dir);
        let told = m
            .requests
            .iter()
            .any(|r| r.starts_with("meta set attention") && r.contains("continue_per_hour"));
        assert!(
            told,
            "{tag}: the cap is told\n{lines:#?}\n{:#?}",
            m.requests
        );
        assert_eq!(count(&m, "turn"), turns, "{tag}: {lines:#?}");
    }
}

fn drawn_nbsp(mut r: Vec<String>) -> Vec<String> {
    for row in &mut r {
        if row == "❯" {
            *row = "❯\u{a0}".to_string();
        } else if let Some(rest) = row.strip_prefix("❯ ") {
            *row = format!("❯\u{a0}{rest}");
        }
    }
    r
}

#[test]
fn the_no_break_space_composer_takes_the_fenced_continuation() {
    let mut m = Mock::new(vec![
        busy_screen(),
        drawn_nbsp(ended("chunk 1 done")),
        drawn_nbsp(typed_in("chunk 1 done", "keep going")),
        busy_screen(),
        ended(STOP),
    ]);
    m.gen_fence = true;
    m.sends_gen = true;
    m.human_seq = Some(0);
    m.help = FENCED_HELP.to_string();
    m.screen_cols.insert(2, 12);
    m.vanish_after = Some(2);
    let (lines, _) = watch_lines(&mut m, &hosted(30));
    assert!(
        lines.contains(&format!(
            "CONTINUED seq=102 rule={RULE_CONTINUE} keep going"
        )),
        "{lines:#?}\n{:#?}",
        m.requests
    );
    let writes: Vec<&String> = m
        .requests
        .iter()
        .filter(|r| r.starts_with("send ") || r.starts_with("key "))
        .collect();
    assert_eq!(
        writes,
        [
            "send if-human=0 if-gen=1.102 if=^❯\\s*$ -- keep going",
            "key if-human=0 if-gen=1.103 if=^❯\\x{A0}keep\\x20going\\s*$ enter",
        ],
        "{:#?}",
        m.requests
    );
    let mut m = Mock::new(vec![busy_screen(), drawn_nbsp(ended("chunk 1 done"))]);
    m.gen_fence = true;
    m.sends_gen = true;
    m.human_seq = Some(0);
    m.help = FENCED_HELP.to_string();
    m.verb_replies
        .insert("send", VecDeque::from([ok("OK skipped seq=102\n")]));
    m.vanish_after = Some(2);
    let (lines, _) = watch_lines(&mut m, &hosted(30));
    assert!(
        !lines.iter().any(|l| l.starts_with("CONTINUED")),
        "{lines:#?}"
    );
    assert_eq!(count(&m, "send"), 1, "never retried: {:#?}", m.requests);
    assert!(
        m.attention
            .as_deref()
            .is_some_and(|a| a.starts_with("claude ")),
        "{:?}",
        m.attention
    );
}

fn hosted_until_stopped(m: &mut Mock, ledger: &Path, hand_over: bool) -> Vec<String> {
    let (r, lines) = host_stops(
        m,
        Some("@s-1"),
        ledger.to_path_buf(),
        &hosted(30),
        hand_over,
    );
    assert_eq!(r, Ok(()));
    lines
}

#[test]
fn a_handed_over_idle_escalation_survives_the_restart() {
    let (dir, ledger) = ledger_at("hand-over");
    let mut m = Mock::new(vec![busy_screen(), ended(STOP)]);
    let first = hosted_until_stopped(&mut m, &ledger, true);
    assert_eq!(
        first.last().map(String::as_str),
        Some("EXIT stopped"),
        "{first:?}"
    );
    let raised = m
        .attention
        .clone()
        .unwrap_or_else(|| panic!("the idle point escalated: {first:?}\n{:#?}", m.requests));
    assert!(
        raised.starts_with("claude idle: I need your decision"),
        "{raised}"
    );
    assert_eq!(count(&m, "@s-1 meta unset attention owner=supervisor"), 0);
    let _ = hosted_until_stopped(&mut m, &ledger, true);
    assert_eq!(
        m.attention.as_deref(),
        Some(raised.as_str()),
        "{:#?}",
        m.requests
    );
    assert_eq!(count(&m, "@s-1 meta set attention owner=supervisor"), 1);
    assert_eq!(count(&m, "@s-1 meta unset attention owner=supervisor"), 0);
    let _ = hosted_until_stopped(&mut m, &ledger, false);
    assert_eq!(m.attention, None, "{:#?}", m.requests);
    let _ = std::fs::remove_dir_all(&dir);

    let (dir, ledger) = ledger_at("hand-over-ctl");
    let mut m = Mock::new(vec![busy_screen(), ended(STOP)]);
    let _ = hosted_until_stopped(&mut m, &ledger, false);
    assert_eq!(m.attention, None);
    let _ = hosted_until_stopped(&mut m, &ledger, false);
    assert_eq!(m.attention, None);
    assert_eq!(count(&m, "@s-1 meta set attention owner=supervisor"), 2);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Seeds a model-switch ledger entry at `path` and returns its recorded unix timestamp.
pub(super) fn seed_switch(
    path: &std::path::Path,
    sid: Option<&str>,
    back_at_unix: i64,
    restored: bool,
    legacy: bool,
) -> i64 {
    use crate::supervise::approvals::{Outcome, Row, model_switch_reason};
    use crate::supervise::policy::turn_end::{RULE_MODEL_FALLBACK, RULE_MODEL_RESTORE};
    let now = crate::supervise::journal::unix_ms();
    let (written, made) = (now - 30 * 60_000, (now - 31 * 60_000).div_euclid(1000));
    let mut reason = model_switch_reason(Some("fable"), Some(back_at_unix), made);
    if legacy {
        reason = reason.replace(&format!(" made={made}"), "");
    }
    let mut rows = vec![
        Row {
            rule_id: RULE_MODEL_FALLBACK,
            outcome: Outcome::Typed,
            command: if legacy {
                "/model opus"
            } else {
                "relaunch --model opus"
            },
            reason: &reason,
            box_seq: 90,
        }
        .to_json(written, sid),
        Row {
            rule_id: RULE_MODEL_FALLBACK,
            outcome: Outcome::Typed,
            command: "keep going",
            reason: "the turn-end policy",
            box_seq: 91,
        }
        .to_json(now - 29 * 60_000, sid),
    ];
    if restored {
        rows.push(
            Row {
                rule_id: RULE_MODEL_RESTORE,
                outcome: Outcome::Typed,
                command: if legacy {
                    "/model fable"
                } else {
                    "relaunch --model fable"
                },
                reason: "the turn-end policy",
                box_seq: 95,
            }
            .to_json(now - 10 * 60_000, sid),
        );
    }
    std::fs::write(path, rows.join("\n") + "\n").expect("the ledger");
    if legacy {
        written.div_euclid(1000)
    } else {
        made
    }
}

#[test]
fn a_model_switch_is_read_back_by_a_loop_that_starts_after_it() {
    use crate::supervise::approvals::{OpenSwitch, open_model_switch};
    for legacy in [false, true] {
        for (tag, back_at, restored) in [
            ("due", TEST_NOW - 60, false),
            ("undone", TEST_NOW - 60, true),
        ] {
            let (dir, ledger) = ledger_at(&format!("switch-{tag}-{legacy}"));
            let made = seed_switch(&ledger, None, back_at, restored, legacy);
            let open = open_model_switch(&ledger, None);
            let _ = std::fs::remove_dir_all(&dir);
            assert_eq!(
                open,
                (!restored).then(|| OpenSwitch {
                    from: Some("fable".to_string()),
                    to: "opus".to_string(),
                    back_at_unix: Some(back_at),
                    asked: true,
                    made_unix: made,
                }),
                "{tag} {legacy}"
            );
        }
    }
}

#[test]
fn a_persons_keystroke_holds_the_loop_for_the_grace() {
    let full = SuperviseOpts {
        policy: owners_policy(),
        ..auto(1)
    };
    for (human_ms, holds) in [(Some(1_000), true), (Some(200_000), false), (None, false)] {
        let mut m = Mock::new(vec![busy_screen(), ended("Fixed the parser.")]);
        m.turn_releases = Some(1);
        m.vanish_after = Some(2);
        m.human_ms = human_ms;
        let (lines, _) = watch_lines(&mut m, &full);
        assert_eq!(
            count(&m, "turn"),
            usize::from(!holds),
            "{human_ms:?}: {lines:#?}"
        );
        assert_eq!(m.attention, None, "{human_ms:?}");

        let mut m = fenced(vec![bash_box(&["⏺ Cleaning."], "rm -rf build")]);
        m.vanish_after = Some(0);
        m.human_ms = human_ms;
        let (lines, _) = watch_lines(&mut m, &full);
        assert_eq!(
            m.presses().len(),
            usize::from(!holds),
            "{human_ms:?}: {lines:#?}"
        );
        assert_eq!(
            m.attention, None,
            "{human_ms:?}: a held box is nobody's to be told of"
        );
    }
}

#[test]
fn another_drivers_hand_holds_the_loop() {
    let opts = || SuperviseOpts {
        policy: SupervisorConfig {
            human_grace_s: 60,
            ..owners_policy()
        },
        ..auto(1)
    };
    for hand in ["lease:mgr", "turn:7:mgr"] {
        let mut m = fenced(vec![bash_box(&["⏺ Cleaning."], "rm -rf build")]);
        m.hand = hand;
        m.stall_sleep = Some(Duration::from_millis(100));
        let (lines, _) = watch_lines(&mut m, &opts());
        assert!(
            m.presses().is_empty(),
            "{hand}: {lines:#?}\n{:#?}",
            m.requests
        );
        assert_eq!(
            count(&m, "meta set attention"),
            0,
            "{hand}: {:#?}",
            m.requests
        );
    }
    let mut m = fenced(vec![bash_box(&["⏺ Cleaning."], "rm -rf build")]);
    m.hand = "turn:7";
    m.stall_sleep = Some(Duration::from_millis(100));
    let _ = watch_lines(&mut m, &opts());
    assert!(!m.presses().is_empty(), "{:#?}", m.requests);
}

#[test]
fn a_held_box_is_answered_once_the_persons_grace_has_passed() {
    let opts = |budget: u64| SuperviseOpts {
        policy: SupervisorConfig {
            human_grace_s: 1,
            ..owners_policy()
        },
        ..auto(budget)
    };
    let mut m = fenced(vec![bash_box(&["⏺ Cleaning."], "rm -rf build")]);
    m.human_ms_reads = VecDeque::from([Some(500)]);
    m.stall_sleep = Some(Duration::from_millis(300));
    m.vanish_after = Some(4);
    let (lines, _) = watch_lines(&mut m, &opts(10));
    assert!(!m.presses().is_empty(), "{lines:#?}\n{:#?}", m.requests);
    let held = m
        .requests
        .iter()
        .position(|r| r == "status")
        .expect("the status the box was held on");
    let press = m
        .requests
        .iter()
        .position(|r| r.starts_with("key "))
        .unwrap();
    assert!(
        m.requests[held + 1..press].iter().any(|r| r == "status"),
        "judged again on a fresh status: {:#?}",
        m.requests
    );
    assert!(
        !m.requests[..press]
            .iter()
            .any(|r| r.starts_with("meta set attention")),
        "{:#?}",
        m.requests
    );
}

#[test]
fn a_draft_still_changing_is_never_submitted() {
    let drafted = |text: &str| {
        let mut r = ended("Fixed the parser; the suite is green.");
        let caret = r.iter().position(|row| row == "❯").expect("caret");
        r[caret] = format!("❯ {text}");
        r
    };
    let opts = SuperviseOpts {
        policy: SupervisorConfig {
            human_grace_s: 1,
            ..owners_policy()
        },
        ..auto(4)
    };
    let mut screens = vec![busy_screen()];
    let words = [
        "also",
        "also check",
        "also check the",
        "also check the lexer",
    ];
    for w in words.iter().cycle().take(200) {
        screens.push(drafted(w));
    }
    let mut m = Mock::new(screens);
    m.gen_fence = true;
    m.sends_gen = true;
    m.help = "key [id=<key>] [if=<re>] [if-gen=<e.s>] <name>: send a named key\n".to_string();
    m.cursor_col = Some(24);
    m.stall_sleep = Some(Duration::from_millis(400));
    m.delay = (0..2000).map(|i| (i, Duration::from_millis(100))).collect();
    m.vanish_after = Some(0);
    let opts = SuperviseOpts {
        policy: opts.policy.clone(),
        ..auto(2)
    };
    let _ = watch_lines(&mut m, &opts);
    assert!(
        !m.presses().iter().any(|p| p.ends_with(" enter")),
        "{:#?}",
        m.requests
    );
}

fn agy_mock(screens: Vec<Vec<String>>) -> Mock {
    let carets: Vec<Option<usize>> = screens
        .iter()
        .map(|s| aterm_phase::agy::composer(s))
        .collect();
    let mut m = fenced(screens);
    m.program = "agy";
    for (i, caret) in carets.into_iter().enumerate() {
        if let Some(row) = caret {
            m.screen_cursors.insert(i, row);
        }
    }
    m
}

#[test]
fn an_agy_session_is_supervised_end_to_end() {
    use aterm_phase::agy::fixtures as a;
    use aterm_phase::prompt::fixtures::screen;
    let screens = || {
        vec![
            screen(a::BUSY),
            screen(a::BOX_EXEC),
            screen(a::BUSY),
            screen(a::END_OF_TURN),
        ]
    };
    let mut m = agy_mock(screens());
    m.gen_fence = true;
    m.sends_gen = true;
    m.turn_releases = Some(3);
    m.vanish_after = Some(2);
    let opts = SuperviseOpts {
        policy: owners_policy(),
        ..auto(30)
    };
    let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
        s.set_turn_end_timing(TurnEndTiming {
            min_work: Duration::ZERO,
            ..TurnEndTiming::default()
        });
    });
    let command_row = screen(a::BOX_EXEC)
        .into_iter()
        .find(|r| r.trim() == "touch x.txt")
        .expect("the command row");
    let press = m
        .presses()
        .into_iter()
        .find(|r| r.ends_with(" 1"))
        .cloned()
        .unwrap_or_else(|| panic!("the box answered: {lines:#?} {:#?}", m.requests));
    assert!(
        press.contains(&crate::supervise::policy::row_guard(&command_row)),
        "guarded on the command's row: {press}"
    );
    let turn = m
        .requests
        .iter()
        .find(|r| r.starts_with("turn "))
        .unwrap_or_else(|| panic!("the continuation: {lines:#?} {:#?}", m.requests));
    assert!(
        turn.starts_with("turn submit=guarded:^>\\skeep\\x20going\\s*$ "),
        "{turn}"
    );
    assert!(
        lines.iter().any(|l| l.starts_with("CONTINUED ")),
        "{lines:#?}"
    );
    assert_eq!(count(&m, "meta set attention"), 0, "nothing handed over");

    let mut m = agy_mock(screens());
    m.program = "zsh";
    m.gen_fence = true;
    m.sends_gen = true;
    m.vanish_after = Some(2);
    let _ = watch_lines(&mut m, &opts);
    assert!(m.presses().is_empty(), "{:#?}", m.requests);
    assert_eq!(count(&m, "turn"), 0, "{:#?}", m.requests);
}

#[test]
fn a_muses_prompt_hint_is_no_draft_and_a_homed_draft_is_never_typed_over() {
    use aterm_phase::muse::fixtures as u;
    use aterm_phase::prompt::fixtures::screen;
    let ended = screen(u::END_OF_TURN_1_4_10);
    let caret = aterm_phase::muse::composer(&ended).expect("the composer");
    let mut drafted = ended.clone();
    drafted[caret] = "❯ hello there".to_string();
    let mut hinted = ended.clone();
    hinted[caret] = "❯ /export saves the conversation to a text file".to_string();
    for (end, fg, typed, asked) in [
        (drafted, "ccd3db", false, true),
        (hinted, "676c74", true, true),
        (ended, "ccd3db", true, false),
    ] {
        let mut m = muse_mock(vec![screen(u::BUSY_1_4_10), end]);
        m.cell_fg = fg;
        m.cell_attrs = "none";
        m.turn_releases = Some(1);
        m.vanish_after = Some(2);
        let mut opts = hosted(3);
        opts.policy.human_grace_s = 0;
        let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
            s.set_turn_end_timing(TurnEndTiming {
                min_work: Duration::ZERO,
                ..TurnEndTiming::default()
            });
        });
        assert_eq!(
            m.requests.iter().any(|r| r.starts_with("cell ")),
            asked,
            "{fg} typed {typed}: {:#?}",
            m.requests
        );
        assert_eq!(
            m.requests
                .iter()
                .any(|r| r.starts_with("turn ") || r.starts_with("send ")),
            typed,
            "{fg} typed {typed}: {lines:#?}\n{:#?}",
            m.requests
        );
    }
}

#[test]
fn an_agy_draft_homed_to_its_caret_is_never_typed_over() {
    use aterm_phase::agy::fixtures as a;
    use aterm_phase::prompt::fixtures::screen;
    let ended = screen(a::END_OF_TURN);
    let caret = aterm_phase::agy::composer(&ended).expect("the composer");
    let mut drafted = ended.clone();
    drafted[caret] = "> hello there".to_string();
    for (end, typed) in [(drafted, false), (ended, true)] {
        let mut m = agy_mock(vec![screen(a::BUSY), end]);
        m.cell_unreadable = true;
        m.turn_releases = Some(1);
        m.vanish_after = Some(2);
        let mut opts = hosted(3);
        opts.policy.human_grace_s = 0;
        let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
            s.set_turn_end_timing(TurnEndTiming {
                min_work: Duration::ZERO,
                ..TurnEndTiming::default()
            });
        });
        assert!(
            !m.requests
                .iter()
                .any(|r| r.starts_with("cell ") && !r.ends_with(" 0")),
            "typed {typed}: {:#?}",
            m.requests
        );
        assert_eq!(
            m.requests.iter().any(|r| r.starts_with("turn ")),
            typed,
            "typed {typed}: {lines:#?}\n{:#?}",
            m.requests
        );
    }
}

#[test]
fn an_owners_act_in_two_paragraphs_is_typed_as_one_line() {
    use aterm_phase::agy::fixtures as a;
    use aterm_phase::prompt::fixtures::screen;
    const OWNERS: &str =
        "Keep going.\n\nWhen it is all done, reply with a line starting Nothing left:";
    const ONE: &str = "Keep going. When it is all done, reply with a line starting Nothing left:";
    let refused = |line: &str| {
        let mut client = crate::RelayClient::new(std::io::Cursor::new(Vec::new()));
        client
            .request_line(line)
            .err()
            .is_some_and(|e| e.kind() == std::io::ErrorKind::InvalidInput)
    };
    assert!(
        refused(&format!("send -- {OWNERS}")),
        "CONTROL: the act as written"
    );
    let typed = |m: &Mock, lines: &[String]| {
        let typing: Vec<&String> = m
            .requests
            .iter()
            .filter(|r| r.starts_with("turn ") || r.starts_with("send "))
            .collect();
        assert!(
            !typing.is_empty(),
            "the continuation: {lines:#?}\n{:#?}",
            m.requests
        );
        for r in &typing {
            assert!(!refused(r), "a request no client sends: {r:?}");
        }
        typing[0].clone()
    };
    let run = |m: &mut Mock| {
        let mut opts = hosted(3);
        opts.policy.human_grace_s = 0;
        opts.policy.continue_text = OWNERS.to_string();
        let (lines, _) = watch_lines_with(m, &opts, |s| {
            s.set_turn_end_timing(TurnEndTiming {
                min_work: Duration::ZERO,
                ..TurnEndTiming::default()
            });
        });
        lines
    };
    let mut m = agy_mock(vec![screen(a::BUSY), screen(a::END_OF_TURN)]);
    m.cell_unreadable = true;
    m.turn_releases = Some(1);
    m.vanish_after = Some(2);
    let lines = run(&mut m);
    let turn = typed(&m, &lines);
    assert!(turn.ends_with(&format!(" {ONE}")), "agy: {turn:?}");

    let mut m = Mock::new(vec![
        busy_screen(),
        ended("Fixed the parser; the suite is green."),
    ]);
    m.vanish_after = Some(2);
    let lines = run(&mut m);
    let turn = typed(&m, &lines);
    assert!(turn.ends_with(&format!(" {ONE}")), "Claude Code: {turn:?}");
}

#[test]
fn agys_command_menu_is_never_typed_into() {
    use aterm_phase::agy::fixtures as a;
    use aterm_phase::prompt::fixtures::screen;
    for (end, typed) in [(a::SLASH_MENU, false), (a::END_OF_TURN, true)] {
        let mut m = agy_mock(vec![screen(a::BUSY), screen(end)]);
        m.turn_releases = Some(1);
        m.vanish_after = Some(2);
        let (lines, _) = watch_lines_with(&mut m, &hosted(3), |s| {
            s.set_turn_end_timing(TurnEndTiming {
                min_work: Duration::ZERO,
                ..TurnEndTiming::default()
            });
        });
        assert_eq!(
            m.requests
                .iter()
                .any(|r| r.starts_with("turn ") || r.starts_with("send ")),
            typed,
            "{}: {lines:#?}\n{:#?}",
            end.lines().next().unwrap_or_default(),
            m.requests
        );
    }
}

#[test]
fn agys_sign_in_menu_is_handed_to_its_owner() {
    use aterm_phase::agy::fixtures as a;
    use aterm_phase::prompt::fixtures::screen;
    let signed_out = screen(a::SIGNED_OUT);
    let unnamed: Vec<String> = signed_out
        .iter()
        .map(|r| {
            if r.trim() == "Select login method:" {
                String::new()
            } else {
                r.clone()
            }
        })
        .collect();
    assert_ne!(unnamed, signed_out);
    for (end, told) in [(signed_out, true), (unnamed, false)] {
        let mut m = agy_mock(vec![screen(a::BUSY), end]);
        m.turn_releases = Some(1);
        m.vanish_after = Some(2);
        let (lines, _) = watch_lines(&mut m, &hosted(3));
        assert!(m.presses().is_empty(), "told {told}: {:#?}", m.requests);
        assert_eq!(count(&m, "turn"), 0, "told {told}: {:#?}", m.requests);
        let raised = m
            .requests
            .iter()
            .find_map(|r| r.strip_prefix("meta set attention owner=supervisor "));
        assert_eq!(
            raised.is_some_and(|a| a.contains("quit agy and run it again to sign in")),
            told,
            "{raised:?}\n{lines:#?}"
        );
    }
}

fn muse_mock(screens: Vec<Vec<String>>) -> Mock {
    let carets: Vec<Option<usize>> = screens
        .iter()
        .map(|s| aterm_phase::muse::composer(s))
        .collect();
    let mut m = Mock::new(screens);
    m.program = "muse";
    for (i, caret) in carets.into_iter().enumerate() {
        if let Some(row) = caret {
            m.screen_cursors.insert(i, row);
        }
    }
    m
}

#[test]
fn a_muse_code_session_is_supervised_end_to_end() {
    use aterm_phase::muse::fixtures as u;
    use aterm_phase::prompt::fixtures::screen;
    let screens = || {
        vec![
            screen(u::BUSY_1_4_10),
            screen(u::BOX_SHELL),
            screen(u::BUSY_1_4_10),
            screen(u::END_OF_TURN_1_4_10),
        ]
    };
    let mut m = muse_mock(screens());
    m.human_seq = Some(0);
    m.gen_fence = true;
    m.sends_gen = true;
    m.help = "key [id=<key>] [if=<re>] [if-gen=<e.s>] <name>: send a named key\n".to_string();
    m.help_send = Some("send [id=<key>] [if=<re>] <text>: write text\n".to_string());
    m.turn_releases = Some(3);
    m.vanish_after = Some(2);
    let opts = SuperviseOpts {
        policy: owners_policy(),
        ..auto(30)
    };
    let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
        s.set_turn_end_timing(TurnEndTiming {
            min_work: Duration::ZERO,
            ..TurnEndTiming::default()
        });
    });
    let once_row = screen(u::BOX_SHELL)
        .into_iter()
        .find(|r| r.starts_with("⟩ 1. Yes, proceed"))
        .expect("the one-shot allow");
    let press = m
        .presses()
        .into_iter()
        .find(|r| r.ends_with(" enter"))
        .cloned()
        .unwrap_or_else(|| panic!("the box answered: {lines:#?} {:#?}", m.requests));
    assert!(
        press.contains(" if-gen=")
            && press.contains(&crate::supervise::policy::row_guard(&once_row)),
        "Enter on the focused one-shot allow: {press}"
    );
    assert!(
        !m.presses()
            .iter()
            .any(|r| r.ends_with(" 1") || r.ends_with(" 2")),
        "no digit: {:#?}",
        m.requests
    );
    let turn = m
        .requests
        .iter()
        .find(|r| r.starts_with("turn "))
        .unwrap_or_else(|| panic!("the continuation: {lines:#?} {:#?}", m.requests));
    assert!(
        turn.starts_with("turn submit=guarded:^❯\\skeep\\x20going\\s*$ idle=600 "),
        "{turn}"
    );
    assert!(
        lines.iter().any(|l| l.starts_with("CONTINUED ")),
        "{lines:#?}"
    );
    assert_eq!(count(&m, "meta set attention"), 0, "nothing handed over");

    let typed = |text: &str| {
        let mut rows = screen(u::END_OF_TURN_1_4_10);
        let caret = aterm_phase::muse::composer(&rows).expect("the composer");
        rows[caret] = format!("❯ {text}");
        rows
    };
    let fenced = |program: &'static str, rows: Vec<Vec<String>>| {
        let cols: Vec<(usize, usize)> = rows
            .iter()
            .enumerate()
            .filter_map(|(i, r)| {
                let c = aterm_phase::muse::composer(r)?;
                let t = r[c].strip_prefix('❯')?.trim();
                (!t.is_empty()).then(|| (i, 2 + t.chars().count()))
            })
            .collect();
        let mut m = muse_mock(rows);
        m.screen_cols.extend(cols);
        m.program = program;
        m.gen_fence = true;
        m.sends_gen = true;
        m.human_seq = Some(0);
        m.help = FENCED_HELP.to_string();
        m.vanish_after = Some(2);
        let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
            s.set_turn_end_timing(TurnEndTiming {
                min_work: Duration::ZERO,
                ..TurnEndTiming::default()
            });
        });
        (m, lines)
    };
    let (m, lines) = fenced(
        "muse",
        vec![
            screen(u::BUSY_1_4_10),
            screen(u::END_OF_TURN_1_4_10),
            typed("keep going"),
            screen(u::BUSY_1_4_10),
            screen(u::END_OF_TURN_1_4_10),
        ],
    );
    let writes: Vec<(usize, &String)> = m
        .requests
        .iter()
        .enumerate()
        .filter(|(_, r)| r.starts_with("send ") || r.starts_with("key "))
        .collect();
    assert!(
        writes.len() >= 2,
        "fenced write and fenced Enter: {lines:#?}\n{:#?}",
        m.requests
    );
    let (sent, send) = writes[0];
    let (entered, enter) = writes[1];
    assert!(
        send.starts_with("send if-human=0 if-gen=") && send.ends_with(" -- keep going"),
        "the text alone, no Enter in its write: {send}"
    );
    assert!(
        enter.starts_with("key if-human=0 if-gen=") && enter.ends_with(" enter"),
        "{enter}"
    );
    let settled = m.requests[sent..entered]
        .iter()
        .any(|r| r.starts_with("await idle 600 "));
    assert!(
        settled,
        "the paste guard's settle between them: {:#?}",
        m.requests
    );
    assert_eq!(count(&m, "turn"), 0, "{:#?}", m.requests);

    use aterm_phase::agy::fixtures as a;
    let mut agy_typed = screen(a::END_OF_TURN);
    let caret = aterm_phase::agy::composer(&agy_typed).expect("the composer");
    agy_typed[caret] = "> keep going".to_string();
    let mut m = agy_mock(vec![
        screen(a::BUSY),
        screen(a::END_OF_TURN),
        agy_typed,
        screen(a::BUSY),
        screen(a::END_OF_TURN),
    ]);
    m.gen_fence = true;
    m.sends_gen = true;
    m.help = FENCED_HELP.to_string();
    m.vanish_after = Some(2);
    let _ = watch_lines_with(&mut m, &opts, |s| {
        s.set_turn_end_timing(TurnEndTiming {
            min_work: Duration::ZERO,
            ..TurnEndTiming::default()
        });
    });
    assert!(
        m.requests
            .iter()
            .any(|r| r.starts_with("send if-human=7 if-gen=")),
        "{:#?}",
        m.requests
    );
    assert!(
        !m.requests.iter().any(|r| r.starts_with("await idle 600 ")),
        "no paste guard's settle for agy: {:#?}",
        m.requests
    );

    let mut m = muse_mock(screens());
    m.program = "zsh";
    m.gen_fence = true;
    m.sends_gen = true;
    m.vanish_after = Some(2);
    let _ = watch_lines(&mut m, &opts);
    assert!(m.presses().is_empty(), "{:#?}", m.requests);
    assert_eq!(count(&m, "turn"), 0, "{:#?}", m.requests);
}

#[test]
fn a_muse_code_question_is_answered_by_the_cursor_and_enter() {
    use aterm_phase::muse::fixtures as u;
    use aterm_phase::prompt::fixtures::screen;
    let red = screen(u::QUESTION)
        .into_iter()
        .find(|r| r.starts_with("  ⟩ 1. Red (Recommended)"))
        .expect("the focused answer");
    for answers in [true, false] {
        let mut m = muse_mock(vec![
            screen(u::BUSY_1_4_10),
            screen(u::QUESTION),
            screen(u::QUESTION_ANSWERED),
        ]);
        m.human_seq = Some(0);
        m.gen_fence = true;
        m.sends_gen = true;
        m.help = "key [id=<key>] [if=<re>] [if-gen=<e.s>] <name>: send a named key\n".to_string();
        m.key_releases = Some(1);
        m.vanish_after = Some(2);
        let mut opts = SuperviseOpts {
            policy: owners_policy(),
            ..auto(10)
        };
        opts.policy.answer_questions = answers;
        let (lines, _) = watch_lines(&mut m, &opts);
        let presses: Vec<&String> = m.presses();
        if answers {
            assert_eq!(presses.len(), 1, "{lines:#?}\n{:#?}", m.requests);
            assert!(
                presses[0].contains(" if-gen=")
                    && presses[0].ends_with(&format!(
                        " if={} enter",
                        crate::supervise::policy::row_guard(&red)
                    )),
                "{}",
                presses[0]
            );
            assert!(
                lines.iter().any(|l| l.starts_with("APPROVED ")
                    && l.ends_with("Request user input Color → Red (Recommended)")),
                "{lines:#?}"
            );
        } else {
            assert!(presses.is_empty(), "{:#?}", m.requests);
            assert!(count(&m, "meta set attention") > 0, "{:#?}", m.requests);
        }
    }
}

#[test]
fn a_question_read_by_its_roles_is_told_once_as_the_policys_choice() {
    use aterm_phase::muse::fixtures as u;
    use aterm_phase::prompt::fixtures::screen;
    use aterm_phase::{agy::fixtures::lab as a, gemini::fixtures as g};
    type Case = (&'static str, fn() -> Mock, &'static str);
    let cases: [Case; 3] = [
        (
            "agy",
            || agy_mock(vec![screen(a::QUESTION), screen(a::QUESTION_ANSWERED)]),
            "Which colour should the lab probe use? → Red",
        ),
        (
            "gemini",
            || gemini_mock(vec![screen(g::QUESTION), screen(g::QUESTION_ANSWERED)]),
            "Which colour should the lab probe use? → Red",
        ),
        (
            "muse",
            || {
                muse_mock(vec![
                    screen(u::BUSY_1_4_10),
                    screen(u::QUESTION),
                    screen(u::QUESTION_ANSWERED),
                ])
            },
            "Which color should the probe use? → Red (Recommended)",
        ),
    ];
    for (agent, mock, told) in cases {
        for answers in [true, false] {
            let mut m = mock();
            m.human = vec!["null"];
            m.human_seq = Some(0);
            m.gen_fence = true;
            m.sends_gen = true;
            m.help = FENCED_HELP.to_string();
            m.key_releases = Some(usize::from(agent == "muse"));
            m.vanish_after = Some(2);
            let mut opts = SuperviseOpts {
                policy: owners_policy(),
                ..auto(10)
            };
            opts.policy.answer_questions = answers;
            let (lines, _) = watch_lines(&mut m, &opts);
            let chose: Vec<&String> = lines.iter().filter(|l| l.starts_with("CHOSE ")).collect();
            if answers {
                assert!(!m.presses().is_empty(), "{agent}: {:#?}", m.requests);
                assert_eq!(chose.len(), 1, "{agent}: {lines:#?}\n{:#?}", m.requests);
                assert!(
                    chose[0].starts_with("CHOSE seq=")
                        && chose[0].contains(" rule=answer-recommended@v1 policy=recommended ")
                        && chose[0].ends_with(told),
                    "{agent}: {}",
                    chose[0]
                );
            } else {
                assert!(m.presses().is_empty(), "{agent}: {:#?}", m.requests);
                assert!(chose.is_empty(), "{agent}: {lines:#?}");
            }
        }
    }
}

#[test]
fn a_muse_code_report_ending_with_the_token_gets_the_done_check() {
    use aterm_phase::muse::fixtures as u;
    use aterm_phase::prompt::fixtures::screen;
    for (tag, end, rule) in [
        ("muse-finished", u::END_OF_TURN_FINISHED, RULE_DONE_CHECK),
        ("muse-plain", u::END_OF_TURN_1_4_10, RULE_CONTINUE),
    ] {
        let (dir, journal) = journal_file(tag);
        let mut m = muse_mock(vec![screen(u::BUSY_1_4_10), screen(end)]);
        m.turn_releases = Some(1);
        m.vanish_after = Some(2);
        let opts = SuperviseOpts {
            policy: owners_policy(),
            journal: Some(journal.clone()),
            ..auto(3)
        };
        let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
            s.set_turn_end_timing(TurnEndTiming {
                min_work: Duration::ZERO,
                ..TurnEndTiming::default()
            });
        });
        let done: Vec<String> = journal_records(&journal)
            .0
            .into_iter()
            .map(|r| r.line)
            .filter(|l| l.starts_with("DONE "))
            .collect();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with("CONTINUED ") && l.contains(&format!(" rule={rule} "))),
            "{tag}: {lines:#?}\n{:#?}",
            m.requests
        );
        assert!(done.is_empty(), "{tag}: {done:#?}");
    }
}

fn gemini_mock(screens: Vec<Vec<String>>) -> Mock {
    let carets: Vec<Option<usize>> = screens
        .iter()
        .map(|s| aterm_phase::gemini::composer_draft(s).map(|(caret, _)| caret))
        .collect();
    let mut m = fenced(screens);
    m.program = "node";
    for (i, caret) in carets.into_iter().enumerate() {
        if let Some(row) = caret {
            m.screen_cursors.insert(i, row);
        }
    }
    m
}

fn gemini_ended(words: &str) -> Vec<String> {
    let mut rows =
        aterm_phase::prompt::fixtures::screen(aterm_phase::gemini::fixtures::END_OF_TURN);
    let at = rows
        .iter()
        .rposition(|r| r.starts_with("✦ "))
        .expect("the reply");
    rows[at] = format!("✦ {words}");
    rows
}

#[test]
fn a_gemini_session_is_supervised_end_to_end() {
    use aterm_phase::gemini::fixtures as g;
    use aterm_phase::prompt::fixtures::screen;
    let screens = || {
        vec![
            screen(g::BUSY_THINKING),
            screen(g::BOX_SHELL),
            screen(g::BUSY_THINKING),
            screen(g::END_OF_TURN),
        ]
    };
    let mut m = gemini_mock(screens());
    m.gen_fence = true;
    m.sends_gen = true;
    m.turn_releases = Some(3);
    m.vanish_after = Some(2);
    let opts = SuperviseOpts {
        policy: owners_policy(),
        ..auto(30)
    };
    let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
        s.set_turn_end_timing(TurnEndTiming {
            min_work: Duration::ZERO,
            ..TurnEndTiming::default()
        });
    });
    let command_row = screen(g::BOX_SHELL)
        .into_iter()
        .find(|r| r.starts_with("│ │ touch lab-probe.txt"))
        .expect("the command row");
    let press = m
        .presses()
        .into_iter()
        .find(|r| r.ends_with(" 1"))
        .cloned()
        .unwrap_or_else(|| panic!("the box answered: {lines:#?} {:#?}", m.requests));
    assert!(
        press.contains(&crate::supervise::policy::row_guard(&command_row)),
        "guarded on the command's row: {press}"
    );
    let turn = m
        .requests
        .iter()
        .find(|r| r.starts_with("turn "))
        .unwrap_or_else(|| panic!("the continuation: {lines:#?} {:#?}", m.requests));
    assert!(
        turn.starts_with("turn submit=guarded:^\\s>\\skeep\\x20going\\s*$ "),
        "{turn}"
    );
    assert!(
        lines.iter().any(|l| l.starts_with("CONTINUED ")),
        "{lines:#?}"
    );
    assert_eq!(count(&m, "meta set attention"), 0, "nothing handed over");

    let mut m = gemini_mock(screens());
    m.program = "zsh";
    m.gen_fence = true;
    m.sends_gen = true;
    m.vanish_after = Some(2);
    let _ = watch_lines(&mut m, &opts);
    assert!(m.presses().is_empty(), "{:#?}", m.requests);
    assert_eq!(count(&m, "turn"), 0, "{:#?}", m.requests);
}

#[test]
fn a_gemini_continuation_past_the_paste_fold_is_guarded_on_its_chip() {
    use crate::supervise::run::turn_end_loop::composer_guard_at;
    use aterm_phase::gemini::fixtures as g;
    use aterm_phase::prompt::fixtures::screen;
    let long = format!(
        "keep going on the owner's terms: {}finish it.",
        "verify each step, commit it and push it, then read what came back. ".repeat(8)
    );
    assert!(long.len() > aterm_phase::gemini::PASTE_FOLD_UNITS);
    let mut m = gemini_mock(vec![screen(g::BUSY_THINKING), screen(g::END_OF_TURN)]);
    m.turn_releases = Some(1);
    m.vanish_after = Some(2);
    let opts = SuperviseOpts {
        policy: SupervisorConfig {
            continue_text: long.clone(),
            ..owners_policy()
        },
        ..auto(30)
    };
    let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
        s.set_turn_end_timing(TurnEndTiming {
            min_work: Duration::ZERO,
            ..TurnEndTiming::default()
        });
    });
    let turn = m
        .requests
        .iter()
        .find(|r| r.starts_with("turn "))
        .unwrap_or_else(|| panic!("the continuation: {lines:#?} {:#?}", m.requests));
    assert!(
        turn.ends_with(&long),
        "the owner's words typed whole: {turn}"
    );
    let chip = aterm_phase::gemini::folded_paste(&long).expect("past the fold");
    let guard = composer_guard_at('>', 1, &chip);
    assert!(
        turn.starts_with(&format!("turn submit=guarded:{guard} ")),
        "the chip's guard: {turn}"
    );

    let rows = screen(g::FOLDED_PASTE);
    let measured = aterm_phase::gemini::folded_paste(&"x".repeat(538)).expect("folds");
    let at_chip =
        aterm_observe::row_matcher(&composer_guard_at('>', 1, &measured)).expect("compiles");
    assert!(
        rows.iter().any(|r| at_chip.matches(r)),
        "the measured chip row"
    );
    let words = aterm_observe::row_matcher(&composer_guard_at('>', 1, &long)).expect("compiles");
    assert!(
        !rows.iter().any(|r| words.matches(r)),
        "the control: the words' guard matches no row of the folded screen"
    );
    for reader in [
        &aterm_phase::ClaudeReader as &dyn aterm_phase::ScreenReader,
        &aterm_phase::CodexReader,
        &aterm_phase::AgyReader,
        &aterm_phase::MuseReader,
        &aterm_phase::GenericReader,
    ] {
        assert_eq!(reader.folded_paste(&long), None, "{:?}", reader.program());
    }
}

#[test]
fn a_gemini_done_report_is_checked_once_and_a_done_reply_ends_the_task() {
    use aterm_phase::gemini::fixtures as g;
    use aterm_phase::prompt::fixtures::screen;
    let mut m = gemini_mock(vec![
        screen(g::BUSY_THINKING),
        gemini_ended("Fixed the parser; all done."),
        screen(g::BUSY_THINKING),
        gemini_ended("DONE"),
    ]);
    m.turn_releases = Some(1);
    m.vanish_after = Some(4);
    let opts = SuperviseOpts {
        policy: owners_policy(),
        ..auto(30)
    };
    let quick = |s: &mut Watched<'_, '_>| {
        s.set_turn_end_timing(TurnEndTiming {
            min_work: Duration::ZERO,
            ..TurnEndTiming::default()
        });
    };
    let (lines, _) = watch_lines_with(&mut m, &opts, quick);
    assert!(
        lines.iter().any(|l| l.starts_with("CONTINUED ")
            && l.contains(&format!("rule={RULE_DONE_CHECK} "))),
        "{lines:#?}"
    );
    let turns: Vec<&String> = m
        .requests
        .iter()
        .filter(|r| r.starts_with("turn "))
        .collect();
    assert_eq!(turns.len(), 1, "the check alone: {:#?}", m.requests);
    assert!(
        turns[0].starts_with("turn submit=guarded:^\\s[>\\s]\\s.*"),
        "its long form, one column in: {}",
        turns[0]
    );
    assert!(turns[0].ends_with(DONE_CHECK), "{}", turns[0]);

    let mut m = gemini_mock(vec![
        screen(g::BUSY_THINKING),
        gemini_ended("Fixed the parser; the suite is green."),
    ]);
    m.turn_releases = Some(1);
    m.vanish_after = Some(2);
    let _ = watch_lines_with(&mut m, &opts, quick);
    assert!(
        m.requests
            .iter()
            .any(|r| r.starts_with("turn ") && r.ends_with(" keep going")),
        "the control: {:#?}",
        m.requests
    );
}

#[test]
fn a_gemini_draft_is_never_typed_over_and_its_placeholder_is_continued() {
    use aterm_phase::gemini::fixtures as g;
    use aterm_phase::prompt::fixtures::screen;
    let ended = screen(g::END_OF_TURN);
    let drafted = {
        let held = screen(g::DRAFT);
        let caret = aterm_phase::gemini::composer_draft(&held)
            .expect("the composer")
            .0;
        let at = aterm_phase::gemini::composer_draft(&ended)
            .expect("the composer")
            .0;
        let mut rows = ended.clone();
        rows[at] = held[caret].clone();
        let status = rows
            .iter()
            .position(|r| r.trim() == "? for shortcuts")
            .expect("the status row");
        rows[status] = String::new();
        rows
    };
    assert_eq!(
        aterm_phase::gemini::composer_draft(&drafted).map(|(_, l)| l),
        Some(vec!["a draft the lab never sends".to_string()]),
        "the draft is read"
    );
    for (end, typed) in [(drafted, false), (ended, true)] {
        let mut m = gemini_mock(vec![screen(g::BUSY_THINKING), end]);
        m.cell_unreadable = true;
        m.turn_releases = Some(1);
        m.vanish_after = Some(2);
        let mut opts = hosted(3);
        opts.policy.human_grace_s = 0;
        let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
            s.set_turn_end_timing(TurnEndTiming {
                min_work: Duration::ZERO,
                ..TurnEndTiming::default()
            });
        });
        assert!(
            !m.requests.iter().any(|r| r.starts_with("cell ")),
            "typed {typed}: {:#?}",
            m.requests
        );
        assert_eq!(
            m.requests.iter().any(|r| r.starts_with("turn ")),
            typed,
            "typed {typed}: {lines:#?}\n{:#?}",
            m.requests
        );
    }
}

#[test]
fn gemini_clis_command_menu_is_never_typed_into() {
    use aterm_phase::gemini::fixtures as g;
    use aterm_phase::prompt::fixtures::screen;
    for (end, typed) in [(g::SLASH_MENU, false), (g::END_OF_TURN, true)] {
        let mut m = gemini_mock(vec![screen(g::BUSY_THINKING), screen(end)]);
        m.turn_releases = Some(1);
        m.vanish_after = Some(2);
        let (lines, _) = watch_lines_with(&mut m, &hosted(3), |s| {
            s.set_turn_end_timing(TurnEndTiming {
                min_work: Duration::ZERO,
                ..TurnEndTiming::default()
            });
        });
        assert_eq!(
            m.requests
                .iter()
                .any(|r| r.starts_with("turn ") || r.starts_with("send ")),
            typed,
            "{}: {lines:#?}\n{:#?}",
            end.lines().next().unwrap_or_default(),
            m.requests
        );
    }
}

#[test]
fn gemini_clis_per_day_usage_limit_answered_stop_is_kept_trying_while_a_person_is_told() {
    use aterm_phase::gemini::fixtures as g;
    use aterm_phase::prompt::fixtures::screen;
    let dialog = screen(g::LIMIT_DIALOG);
    assert_eq!(
        aterm_phase::gemini::limit_reset(&dialog),
        None,
        "PRECONDITION"
    );
    let (dir, journal) = journal_file("te-gemini-day-stopped");
    let mut m = gemini_mock(vec![
        screen(g::BUSY_THINKING),
        dialog.clone(),
        screen(g::LIMIT_STOPPED),
        screen(g::BUSY_THINKING),
        dialog.clone(),
        screen(g::BUSY_THINKING),
        gemini_ended("Carried on."),
    ]);
    m.attention_gates = vec![(1, 0)];
    m.turn_gates = vec![2];
    m.key_gates = vec![(4, 0)];
    m.stall_sleep = Some(Duration::from_secs(3600));
    m.exit_when = Some(|m| {
        let unset = m
            .requests
            .iter()
            .rposition(|r| r.starts_with("meta unset attention owner=supervisor"));
        let key = m.requests.iter().rposition(|r| verb_of(r) == "key");
        key.is_some() && unset > key
    });
    let opts = SuperviseOpts {
        policy: owners_policy(),
        journal: Some(journal.clone()),
        ..auto(6 * 3600)
    };
    let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
        s.set_turn_end_timing(TurnEndTiming {
            min_work: Duration::ZERO,
            ..TurnEndTiming::default()
        });
        s.set_clock_running();
    });
    let (records, _) = journal_records(&journal);
    let _ = std::fs::remove_dir_all(&dir);
    let all = || records.iter().map(|r| r.line.as_str()).collect::<Vec<_>>();
    let at = |needle: &str| -> Vec<usize> {
        m.requests
            .iter()
            .enumerate()
            .filter(|(_, r)| r.starts_with(needle))
            .map(|(i, _)| i)
            .collect()
    };
    let verb_at = |verb: &str| -> Vec<usize> {
        m.requests
            .iter()
            .enumerate()
            .filter(|(_, r)| verb_of(r) == verb)
            .map(|(i, _)| i)
            .collect()
    };
    let turns = verb_at("turn");
    let keys = verb_at("key");
    assert_eq!(turns.len(), 1, "{lines:#?}\n{:#?}", all());
    assert_eq!(keys.len(), 1, "{:#?}", m.requests);
    assert!(turns[0] < keys[0], "{:#?}", m.requests);
    assert!(m.presses()[0].ends_with(" 1"), "{:?}", m.presses());
    let typed: Vec<_> = records
        .iter()
        .filter(|r| r.line.starts_with("CONTINUED "))
        .collect();
    assert_eq!(typed.len(), 1, "{:#?}", all());
    assert!(
        typed[0].line.contains("rule=usage-resume@v1"),
        "{:#?}",
        all()
    );
    let raised = at("meta set attention owner=supervisor");
    let lowered = at("meta unset attention owner=supervisor");
    assert_eq!(raised.len(), 1, "raised once: {:#?}", m.requests);
    assert!(
        m.requests[raised[0]].contains("names no reset"),
        "{}",
        m.requests[raised[0]]
    );
    assert!(raised[0] < turns[0], "{:#?}", m.requests);
    assert_eq!(lowered.len(), 1, "taken down once: {:#?}", m.requests);
    assert!(lowered[0] > keys[0], "{:#?}", m.requests);
    assert!(
        !m.requests
            .iter()
            .any(|r| r.contains("An unknown error occurred.")),
        "the error is no person's: {:#?}",
        m.requests
    );
    let until = |l: &str| -> i64 {
        let stamp = l
            .split_whitespace()
            .find_map(|w| w.strip_prefix("until="))
            .expect("an until");
        aterm_types::rfc3339::parse_utc(stamp).expect("a stamp")
    };
    let near = |got: i64, want: i64| (want..want + 30).contains(&got);
    let error_wait = records
        .iter()
        .find(|r| r.line.starts_with("WAITING ") && r.line.contains("usage-session resets"))
        .unwrap_or_else(|| panic!("the error waited on: {:#?}", all()));
    assert!(
        near(until(&error_wait.line), TEST_NOW + 1800),
        "the first rung: {:#?}",
        all()
    );
    let waited_ms: u64 = m.requests[..turns[0]]
        .iter()
        .filter(|r| r.starts_with("await seq "))
        .filter_map(|r| r.rsplit(' ').next().and_then(|ms| ms.parse::<u64>().ok()))
        .sum();
    assert!(
        waited_ms >= 1_700_000,
        "typed at the rung, not into the quota: {waited_ms} ms {:#?}",
        all()
    );
    let again = records
        .iter()
        .find(|r| {
            r.line.starts_with("WAITING ")
                && r.line.contains("rule=limit-retry@v1")
                && r.line.contains("try 1)")
        })
        .unwrap_or_else(|| panic!("the dialog's return waited on: {:#?}", all()));
    assert!(
        again
            .line
            .contains("(drawn again after its typed resume, try 1)"),
        "{}",
        again.line
    );
    assert!(
        near(until(&again.line), until(&error_wait.line) + 2 * 3600),
        "the second rung, from the resume: {:#?}",
        all()
    );
    assert!(
        records
            .iter()
            .any(|r| r.line.starts_with("CLEARED ") && r.line.contains(" box ")),
        "{:#?}",
        all()
    );

    for (tag, screens, gates) in [
        (
            "the error alone",
            vec![screen(g::BUSY_THINKING), screen(g::LIMIT_STOPPED)],
            vec![(1, 0)],
        ),
        (
            "after a point that was neither",
            vec![
                screen(g::BUSY_THINKING),
                dialog.clone(),
                gemini_ended("Answered."),
                screen(g::LIMIT_STOPPED),
            ],
            vec![(1, 0), (3, 1)],
        ),
    ] {
        let mut m = gemini_mock(screens);
        m.attention_gates = gates;
        m.vanish_after = Some(6);
        let opts = SuperviseOpts {
            policy: owners_policy(),
            ..auto(30)
        };
        let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
            s.set_turn_end_timing(TurnEndTiming {
                min_work: Duration::ZERO,
                ..TurnEndTiming::default()
            });
        });
        assert!(
            m.presses().is_empty(),
            "{tag}: {lines:#?}\n{:#?}",
            m.requests
        );
        assert!(
            !m.requests
                .iter()
                .any(|r| r.starts_with("turn ") && r.contains("usage-resume")),
            "{tag}: {:#?}",
            m.requests
        );
        assert!(
            m.requests
                .iter()
                .any(|r| r.starts_with("meta set attention owner=supervisor")
                    && r.contains("An unknown error occurred.")),
            "{tag}: the error handed over: {lines:#?}\n{:#?}",
            m.requests
        );
    }
    let mut m = gemini_mock(vec![screen(g::BUSY_THINKING), screen(g::BOX_SHELL)]);
    m.key_releases = Some(1);
    m.vanish_after = Some(2);
    let _ = watch_lines(&mut m, &opts);
    assert!(
        m.presses().iter().any(|r| r.ends_with(" 1")),
        "the control: {:#?}",
        m.requests
    );
}

fn gemini_reset_redrawn(rows: &[String], reset: &str) -> Vec<String> {
    rows.iter()
        .map(|r| {
            if !r.starts_with("│ Access resets at") {
                return r.clone();
            }
            let width = r.chars().count() - 1;
            let with = r.replacen("5:06 AM", reset, 1);
            let body = with.trim_end_matches('│').trim_end();
            format!("{body:<width$}│")
        })
        .collect()
}

#[test]
fn gemini_clis_bucket_dialog_is_waited_on_then_tried_by_its_stop_and_a_resume() {
    use aterm_phase::gemini::fixtures as g;
    use aterm_phase::prompt::fixtures::screen;
    for (tag, fixture, digit) in [
        ("bucket", g::LIMIT_DIALOG_BUCKET, " 2"),
        (
            "bucket under the Google login",
            g::LIMIT_DIALOG_BUCKET_UPGRADE,
            " 3",
        ),
        (
            "NEGATIVE CONTROL: the account's",
            g::LIMIT_DIALOG_ACCOUNT,
            " 1",
        ),
    ] {
        let bucket = digit != " 1";
        let dialog = gemini_reset_redrawn(&screen(fixture), "9:05 AM");
        assert_eq!(
            aterm_phase::gemini::limit_reset(&dialog)
                .map(|r| r.to_string())
                .as_deref(),
            Some("9:05 AM PDT"),
            "PRECONDITION {tag}"
        );
        let (dir, journal) = journal_file("te-gemini-bucket");
        let after = if bucket {
            screen(g::LIMIT_STOPPED)
        } else {
            screen(g::BUSY_THINKING)
        };
        let mut m = gemini_mock(vec![
            screen(g::BUSY_THINKING),
            dialog,
            after,
            screen(g::BUSY_THINKING),
            gemini_ended("Carried on."),
        ]);
        m.key_gates = vec![(1, 0)];
        if bucket {
            m.turn_gates = vec![2];
        }
        m.stall_sleep = Some(Duration::from_secs(3600));
        m.exit_when = Some(|m| m.requests.iter().any(|r| verb_of(r) == "turn"));
        let opts = SuperviseOpts {
            policy: owners_policy(),
            journal: Some(journal.clone()),
            ..auto(2 * 3600)
        };
        let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
            s.set_turn_end_timing(TurnEndTiming {
                min_work: Duration::ZERO,
                ..TurnEndTiming::default()
            });
            s.set_clock_running();
        });
        let (records, _) = journal_records(&journal);
        let _ = std::fs::remove_dir_all(&dir);
        let all = || records.iter().map(|r| r.line.as_str()).collect::<Vec<_>>();
        let presses = m.presses();
        assert_eq!(presses.len(), 1, "{tag}: {lines:#?}\n{:#?}", m.requests);
        assert!(presses[0].ends_with(digit), "{tag}: {presses:?}");
        assert!(
            !m.requests
                .iter()
                .any(|r| r.starts_with("meta set attention owner=supervisor")),
            "{tag}: never handed over: {:#?}",
            m.requests
        );
        let due_s = TEST_NOW + 10 * 60 + 60 + 60;
        let wait = records
            .iter()
            .map(|r| r.line.as_str())
            .find(|l| l.starts_with("WAITING ") && l.contains("rule=limit-retry@v1"))
            .unwrap_or_else(|| panic!("{tag}: the dialog waited on: {:#?}", all()));
        assert!(
            wait.contains(&format!(
                "until={}",
                utc_stamp(u64::try_from(due_s).expect("a time"))
            )),
            "{tag}: {wait}"
        );
        let said = if bucket {
            "resets at 9:05 AM PDT: Stop is pressed and the turn resumed past it"
        } else {
            "resets at 9:05 AM PDT: Keep trying is pressed past it"
        };
        assert!(wait.contains(said), "{tag}: {wait}");
        let key = m
            .requests
            .iter()
            .position(|r| verb_of(r) == "key")
            .expect("the press");
        let waited: u64 = m.requests[..key]
            .iter()
            .filter(|r| r.starts_with("await gone ^│") && r.contains("Access"))
            .filter_map(|r| r.rsplit(' ').next().and_then(|ms| ms.parse::<u64>().ok()))
            .sum();
        assert!(waited >= 710_000, "{tag}: the reset waited out: {waited}");
        let subject = if bucket {
            "Stop (reset 9:05 AM PDT)"
        } else {
            "Keep trying (reset 9:05 AM PDT)"
        };
        assert!(
            records
                .iter()
                .any(|r| r.line.starts_with("APPROVED ") && r.line.ends_with(subject)),
            "{tag}: {:#?}",
            all()
        );
        let typed: Vec<&str> = records
            .iter()
            .map(|r| r.line.as_str())
            .filter(|l| l.starts_with("CONTINUED "))
            .collect();
        if bucket {
            assert_eq!(typed.len(), 1, "{tag}: {:#?}", all());
            assert!(
                typed[0].contains("rule=usage-resume@v1"),
                "{tag}: {typed:?}"
            );
            let turn = m
                .requests
                .iter()
                .position(|r| verb_of(r) == "turn")
                .expect("the resume");
            assert!(turn > key, "{tag}: {:#?}", m.requests);
        } else {
            assert!(
                !typed.iter().any(|l| l.contains("rule=usage-resume@v1")),
                "{tag}: {typed:?}"
            );
        }
    }
}

#[test]
fn the_error_a_stop_leaves_is_the_limit_a_read_drew_with_no_point_deciding_it() {
    use aterm_phase::gemini::fixtures as g;
    use aterm_phase::prompt::fixtures::screen;
    let opts = SuperviseOpts {
        policy: owners_policy(),
        ..auto(60)
    };
    let off = SuperviseOpts {
        policy: SupervisorConfig {
            limit_wait: false,
            ..owners_policy()
        },
        ..auto(60)
    };
    let stopped = screen(g::LIMIT_STOPPED);
    let reading = aterm_phase::Program::Gemini.reader().read(&stopped, None);
    assert!(
        reading
            .wall
            .as_ref()
            .is_some_and(aterm_phase::gemini::stopped_error),
        "PRECONDITION: {reading:?}"
    );
    let grace = i64::try_from(
        TurnEndTiming::default()
            .grace_for(aterm_phase::Program::Gemini)
            .as_secs(),
    )
    .expect("a grace");
    let gemini = aterm_phase::Program::Gemini;
    let adopted_as = |reads: &[(Vec<String>, i64)], pressed: bool, opts: &SuperviseOpts| {
        let mut m = gemini_mock(vec![stopped.clone()]);
        let mut s = session(&mut m, None);
        s.program = Some("node".to_owned());
        let mut last = 0;
        for (rows, at) in reads {
            s.set_clock(TEST_NOW + at, PDT);
            let busy = gemini.reader().phase(rows) == aterm_phase::Phase::Busy;
            s.note_limit_dialog(gemini, rows, busy);
            last = *at;
        }
        if pressed {
            s.limit_retry_pressed();
        }
        s.set_clock(TEST_NOW + last + 2, PDT);
        s.limit_stopped_adopt(&reading, opts);
        s.limit_stopped(&reading, &stopped)
            .map(|l| (l.wall.kind, l.wall.reset, l.reset_unix))
    };
    let adopted = |reads: &[Vec<String>], opts: &SuperviseOpts| {
        let timed: Vec<(Vec<String>, i64)> = (0..)
            .zip(reads)
            .map(|(i, rows)| (rows.clone(), 20 * i))
            .collect();
        adopted_as(&timed, false, opts)
    };
    let day = screen(g::LIMIT_DIALOG);
    assert_eq!(
        adopted(std::slice::from_ref(&day), &opts),
        Some((
            aterm_phase::WallKind::UsageSession,
            None,
            TEST_NOW + 1800 - grace
        )),
        "the per-day dialog: its ladder's first rung from its first read"
    );
    assert_eq!(
        adopted(&[day.clone(), day.clone()], &opts),
        Some((
            aterm_phase::WallKind::UsageSession,
            None,
            TEST_NOW + 1800 - grace
        ))
    );
    assert_eq!(
        adopted(&[gemini_reset_dialog("9:05 AM PDT")], &opts),
        Some((
            aterm_phase::WallKind::UsageSession,
            Some("9:05 AM PDT".to_owned()),
            TEST_NOW + 10 * 60 + 60
        )),
        "the account's dialog: its reset placed"
    );
    assert!(
        adopted_as(
            &[(day.clone(), 0), (screen(g::BUSY_THINKING), 3)],
            false,
            &opts
        )
        .is_some(),
        "a frame between"
    );
    assert_eq!(adopted(&[], &opts), None, "no dialog read");
    assert_eq!(
        adopted_as(
            &[
                (day.clone(), 0),
                (screen(g::BUSY_THINKING), 60),
                (screen(g::BUSY_THINKING), 3600)
            ],
            false,
            &opts
        ),
        None,
        "the worker working past the dialog"
    );
    assert_eq!(
        adopted_as(&[(day.clone(), 0)], true, &opts),
        None,
        "the loop's own press"
    );
    assert_eq!(
        adopted(&[day.clone(), screen(g::BOX_SHELL)], &opts),
        None,
        "another box read after it"
    );
    assert_eq!(
        adopted(std::slice::from_ref(&day), &off),
        None,
        "limit_wait off"
    );
    let head_end = day
        .iter()
        .position(|r| r.contains("/model to switch models."))
        .expect("PRECONDITION: the dialog's head");
    let headless: Vec<String> = day[head_end + 1..].to_vec();
    let read = aterm_phase::gemini::prompt(&headless).expect("PRECONDITION: a box read");
    assert!(read.head_off_screen, "PRECONDITION: {read:?}");
    assert_ne!(
        read.kind,
        aterm_phase::PromptKind::UsageLimit,
        "PRECONDITION"
    );
    assert_eq!(
        adopted(std::slice::from_ref(&headless), &opts),
        None,
        "its head off the rows"
    );
    assert_eq!(
        adopted(&[day.clone(), headless], &opts),
        None,
        "its head off the rows after its read"
    );
}

#[test]
fn gemini_clis_usage_limit_with_its_reset_named_is_kept_trying_past_it() {
    use aterm_phase::gemini::fixtures as g;
    use aterm_phase::prompt::fixtures::screen;
    let at_reset = |reset: &str| -> Vec<String> {
        screen(g::LIMIT_DIALOG_ACCOUNT)
            .into_iter()
            .map(|r| {
                if !r.starts_with("│ Access resets at") {
                    return r;
                }
                let width = r.chars().count() - 1;
                let with = r.replacen("5:06 AM", reset, 1);
                let body = with.trim_end_matches('│').trim_end();
                format!("{body:<width$}│")
            })
            .collect()
    };
    let dialog = at_reset("9:05 AM");
    assert_eq!(
        aterm_phase::gemini::limit_reset(&dialog)
            .map(|r| r.to_string())
            .as_deref(),
        Some("9:05 AM PDT"),
        "PRECONDITION"
    );
    let (dir, journal) = journal_file("te-gemini-keep-trying");
    const RETRIES: usize = 7;
    let mut screens = vec![screen(g::BUSY_THINKING)];
    for _ in 0..RETRIES {
        screens.extend([dialog.clone(), screen(g::BUSY_THINKING)]);
    }
    let changed = at_reset("10:35 AM");
    assert_eq!(
        aterm_phase::gemini::limit_reset(&changed)
            .map(|r| r.to_string())
            .as_deref(),
        Some("10:35 AM PDT")
    );
    screens.extend([changed, screen(g::BUSY_THINKING)]);
    let mut m = gemini_mock(screens);
    m.key_gates = (0..=RETRIES).map(|i| (2 * i + 1, i)).collect();
    m.stall_sleep = Some(Duration::from_secs(3600));
    m.exit_when = Some(|m| m.presses().len() > RETRIES);
    let opts = SuperviseOpts {
        policy: owners_policy(),
        journal: Some(journal.clone()),
        ..auto(4 * 3600)
    };
    let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
        s.set_turn_end_timing(TurnEndTiming {
            min_work: Duration::ZERO,
            ..TurnEndTiming::default()
        });
        s.set_clock_running();
    });
    let presses = m.presses();
    assert_eq!(presses.len(), RETRIES + 1, "{lines:#?}\n{:#?}", m.requests);
    assert!(
        presses.iter().all(|p| p.ends_with(" 1")),
        "{presses:?}\n{lines:#?}"
    );
    assert!(
        !m.requests
            .iter()
            .any(|r| r.starts_with("meta set attention owner=supervisor")),
        "never handed over: {:#?}",
        m.requests
    );
    let (records, _) = journal_records(&journal);
    let due_s = TEST_NOW + 10 * 60 + 60 + 60;
    let waiting: Vec<&String> = records
        .iter()
        .map(|r| &r.line)
        .filter(|l| l.starts_with("WAITING ") && l.contains("rule=limit-retry@v1"))
        .collect();
    let until = |s: i64| format!("until={}", utc_stamp(u64::try_from(s).expect("a time")));
    assert_eq!(
        waiting.len(),
        RETRIES + 1,
        "said once for each due: {waiting:#?}"
    );
    assert!(
        waiting[0].contains(&until(due_s))
            && waiting[0].contains("resets at 9:05 AM PDT")
            && !waiting[0].contains("drawn again"),
        "{waiting:#?}"
    );
    let pauses = [120, 240, 480, 960, 1800, 1800, 1800];
    let mut next_due = due_s;
    for (n, (wait, pause)) in waiting.iter().skip(1).zip(pauses).enumerate() {
        next_due += pause;
        assert!(wait.contains(&until(next_due)), "{waiting:#?}");
        assert!(
            wait.contains(&format!(
                "(drawn again after its Keep trying, try {})",
                n + 1
            )),
            "each return says which try it answered: {waiting:#?}"
        );
    }
    assert!(
        waiting[RETRIES].contains("resets at 10:35 AM PDT"),
        "{waiting:#?}"
    );
    let mut waited = [0_u64; RETRIES + 1];
    let mut keys = 0;
    for r in &m.requests {
        if verb_of(r) == "key" {
            keys += 1;
        } else if keys <= RETRIES && r.starts_with("await gone ^│") && r.contains("Access") {
            waited[keys] += r
                .rsplit(' ')
                .next()
                .and_then(|ms| ms.parse::<u64>().ok())
                .expect("a timeout");
        }
    }
    assert!(waited[0] >= 710_000, "the reset waited out: {waited:?}");
    assert!(
        (110_000..710_000).contains(&waited[1]),
        "the back-off, not the reset again: {waited:?}"
    );
    assert!(
        waited[RETRIES] >= 1_700_000,
        "the changed reset's press waits the climb: {waited:?}"
    );
    assert!(
        records
            .iter()
            .filter(|r| r.line.starts_with("APPROVED ") && r.line.contains("Keep trying"))
            .count()
            == RETRIES + 1,
        "{records:#?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
    let opts = SuperviseOpts {
        journal: None,
        ..opts
    };

    use super::super::approval_loop::{LIMIT_RETRY_RELOOK_S, RetryDue};
    let mut m = gemini_mock(vec![dialog.clone()]);
    let mut s = session(&mut m, None);
    s.program = Some("node".to_owned());
    s.set_clock(TEST_NOW, PDT);
    s.set_mono_standing();
    assert_eq!(
        s.limit_retry_due(&dialog, &opts),
        RetryDue::Hold {
            until_unix: due_s,
            reset: Some("9:05 AM PDT".to_owned()),
        }
    );
    let mono = s.fx.mono();
    s.clock_jump_handle().slept(2 * 3600);
    assert_eq!(s.fx.mono(), mono, "the sleep does not advance Instant");
    assert_eq!(s.limit_retry_due(&dialog, &opts), RetryDue::Due);
    let later = at_reset("11:05 AM");
    assert_eq!(
        s.limit_retry_due(&later, &opts),
        RetryDue::Hold {
            until_unix: due_s + 2 * 3600,
            reset: Some("11:05 AM PDT".to_owned()),
        }
    );
    let nine_oh_five = due_s - 2 * 60;
    let tomorrow = |s: &mut Session<'_, Mock>| {
        let now = s.now_unix();
        let held = s.limit_retry_due(&dialog, &opts);
        held == RetryDue::Hold {
            until_unix: now + LIMIT_RETRY_RELOOK_S,
            reset: Some("9:05 AM PDT".to_owned()),
        } && s.limit_retry.as_ref().and_then(|r| r.placed_unix()) == Some(nine_oh_five + 86_400)
    };
    assert!(tomorrow(&mut s), "{:?}", s.limit_retry);
    drop(s);
    for program_changed in [true, false] {
        let mut m = gemini_mock(vec![dialog.clone()]);
        let mut s = session(&mut m, None);
        s.program = Some("node".to_owned());
        s.set_clock(TEST_NOW, PDT);
        assert!(
            matches!(s.limit_retry_due(&dialog, &opts), RetryDue::Hold { until_unix, .. } if until_unix == due_s)
        );
        if program_changed {
            s.program = Some("claude".to_owned());
            assert_eq!(s.limit_retry_due(&dialog, &opts), RetryDue::Not);
            s.program = Some("node".to_owned());
        } else {
            let point = point_at(&s, screen(g::END_OF_TURN), 103);
            s.turn_end_at_point(&point, None, &opts)
                .expect("the answered point");
        }
        s.clock_jump_handle().slept(2 * 3600);
        assert!(tomorrow(&mut s), "{:?}", s.limit_retry);
    }
}

#[test]
fn gemini_clis_usage_limit_answered_stop_is_waited_out_and_resumed() {
    use aterm_phase::gemini::fixtures as g;
    use aterm_phase::prompt::fixtures::screen;
    let dialog = gemini_reset_dialog("9:05 AM PDT");
    assert_eq!(
        aterm_phase::gemini::limit_reset(&dialog)
            .map(|r| r.to_string())
            .as_deref(),
        Some("9:05 AM PDT"),
        "PRECONDITION"
    );
    let (dir, journal) = journal_file("te-gemini-stopped");
    let mut m = gemini_mock(vec![
        screen(g::BUSY_THINKING),
        dialog.clone(),
        screen(g::LIMIT_STOPPED),
        screen(g::BUSY_THINKING),
        dialog.clone(),
        screen(g::BUSY_THINKING),
        gemini_ended("Carried on."),
    ]);
    m.turn_gates = vec![2];
    m.key_gates = vec![(4, 0)];
    m.stall_sleep = Some(Duration::from_secs(3600));
    m.exit_when = Some(|m| !m.presses().is_empty());
    let opts = SuperviseOpts {
        policy: owners_policy(),
        journal: Some(journal.clone()),
        ..auto(2 * 3600)
    };
    let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
        s.set_turn_end_timing(TurnEndTiming {
            min_work: Duration::ZERO,
            ..TurnEndTiming::default()
        });
        s.set_clock_running();
    });
    let (records, _) = journal_records(&journal);
    let _ = std::fs::remove_dir_all(&dir);
    let all = || records.iter().map(|r| r.line.as_str()).collect::<Vec<_>>();
    assert!(
        !m.requests
            .iter()
            .any(|r| r.starts_with("meta set attention owner=supervisor")),
        "never handed over: {:#?}\n{lines:#?}",
        all()
    );
    let typed: Vec<_> = records
        .iter()
        .filter(|r| r.line.starts_with("CONTINUED "))
        .collect();
    assert_eq!(typed.len(), 1, "{:#?}", all());
    assert!(
        typed[0].line.contains("rule=usage-resume@v1"),
        "{:#?}",
        all()
    );
    let due_s = TEST_NOW + 10 * 60 + 60 + 60;
    let mut waited = [0_u64; 2];
    let mut leg = 0;
    for r in &m.requests {
        let on_its_screen = match leg {
            0 => r.starts_with("await seq "),
            _ => r.starts_with("await gone ^│") && r.contains("Access"),
        };
        match verb_of(r) {
            "turn" => leg = 1,
            "key" => break,
            _ if on_its_screen => {
                waited[leg] += r
                    .rsplit(' ')
                    .next()
                    .and_then(|ms| ms.parse::<u64>().ok())
                    .expect("a timeout");
            }
            _ => {}
        }
    }
    assert!(
        waited[0] >= 710_000,
        "typed past the reset, not into the quota: {waited:?}\n{:#?}",
        all()
    );
    let until = |s: i64| format!("until={}", utc_stamp(u64::try_from(s).expect("a time")));
    assert!(
        records.iter().any(|r| r.line.starts_with("WAITING ")
            && r.line.contains(&until(due_s))
            && r.line.contains("usage-session resets")),
        "the error waited on to the dialog's reset: {:#?}",
        all()
    );
    let presses = m.presses();
    assert_eq!(presses.len(), 1, "{presses:?}\n{:#?}", all());
    assert!(presses[0].ends_with(" 1"), "{presses:?}");
    assert!(
        records.iter().any(|r| r.line.starts_with("WAITING ")
            && r.line.contains("rule=limit-retry@v1")
            && r.line.contains("try 1)")
            && r.line.contains(&until(due_s + 120))),
        "{:#?}",
        all()
    );
    assert!(
        (110_000..710_000).contains(&waited[1]),
        "the back-off from the resume: {waited:?}"
    );
    let pressed: Vec<_> = records
        .iter()
        .filter(|r| r.line.starts_with("APPROVED ") && r.line.contains("Keep trying"))
        .collect();
    assert_eq!(pressed.len(), 1, "{:#?}", all());

    let mut m = gemini_mock(vec![screen(g::BUSY_THINKING), screen(g::LIMIT_STOPPED)]);
    m.attention_gates = vec![(1, 0)];
    m.vanish_after = Some(3);
    let opts = SuperviseOpts {
        policy: owners_policy(),
        ..auto(30)
    };
    let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
        s.set_turn_end_timing(TurnEndTiming {
            min_work: Duration::ZERO,
            ..TurnEndTiming::default()
        });
    });
    assert_eq!(count(&m, "turn"), 0, "{lines:#?}\n{:#?}", m.requests);
    assert!(
        m.requests
            .iter()
            .any(|r| r.starts_with("meta set attention owner=supervisor")),
        "the control: {lines:#?}\n{:#?}",
        m.requests
    );
}

fn gemini_reset_dialog(reset: &str) -> Vec<String> {
    aterm_phase::prompt::fixtures::screen(aterm_phase::gemini::fixtures::LIMIT_DIALOG_ACCOUNT)
        .into_iter()
        .map(|r| {
            if !r.starts_with("│ Access resets at") {
                return r;
            }
            let width = r.chars().count() - 1;
            let with = r.replacen("5:06 AM PDT", reset, 1);
            let body = with.trim_end_matches('│').trim_end();
            format!("{body:<width$}│")
        })
        .collect()
}

#[test]
fn gemini_clis_usage_limit_reset_is_placed_in_the_zone_it_names() {
    use super::super::approval_loop::{LIMIT_RETRY_RELOOK_S, RetryDue};
    let opts = SuperviseOpts {
        policy: owners_policy(),
        ..auto(60)
    };
    let mut m = gemini_mock(vec![gemini_reset_dialog("9:05 AM PDT")]);
    let mut s = session(&mut m, None);
    s.program = Some("node".to_owned());
    s.set_clock(TEST_NOW, PDT);
    let due_s = TEST_NOW + 10 * 60 + 60 + 60;
    for reset in [
        "9:05 AM PDT",
        "6:05 PM GMT+2",
        "12:05 PM EDT",
        "4:05 PM UTC",
        "4:05 PM GMT",
        "6:05 AM HST",
        "9:35 PM GMT+5:30",
        "1:35 PM GMT-2:30",
        "5:50 AM GMT+13:45",
    ] {
        let dialog = gemini_reset_dialog(reset);
        assert_eq!(
            aterm_phase::gemini::limit_reset(&dialog)
                .map(|r| r.to_string())
                .as_deref(),
            Some(reset),
            "PRECONDITION"
        );
        assert_eq!(
            s.limit_retry_due(&dialog, &opts),
            RetryDue::Hold {
                until_unix: due_s,
                reset: Some(reset.to_owned()),
            },
            "{reset}"
        );
    }
    let bare = gemini_reset_dialog("9:05 AM");
    assert_eq!(
        s.limit_retry_due(&bare, &opts),
        RetryDue::Hold {
            until_unix: due_s,
            reset: Some("9:05 AM".to_owned()),
        }
    );
    let foreign = gemini_reset_dialog("11:05 PM CEST");
    let relook = TEST_NOW + LIMIT_RETRY_RELOOK_S;
    assert_eq!(
        s.limit_retry_due(&foreign, &opts),
        RetryDue::Hold {
            until_unix: relook,
            reset: Some("11:05 PM CEST".to_owned()),
        }
    );
    s.set_clock(relook, PDT);
    assert_eq!(s.limit_retry_due(&foreign, &opts), RetryDue::Due);
    s.limit_retry_pressed();
    s.set_clock(relook + 5, PDT);
    assert_eq!(
        s.limit_retry_due(&foreign, &opts),
        RetryDue::Hold {
            until_unix: relook + LIMIT_RETRY_RELOOK_S,
            reset: Some("11:05 PM CEST".to_owned()),
        },
        "drawn again on that look, it is looked at again a bound on, never the back-off's burst"
    );
}

fn presses_till(reset: &str, real: i64) -> Vec<i64> {
    use super::super::approval_loop::RetryDue;
    let opts = SuperviseOpts {
        policy: owners_policy(),
        ..auto(60)
    };
    let dialog = gemini_reset_dialog(reset);
    let mut m = gemini_mock(vec![dialog.clone()]);
    let mut s = session(&mut m, None);
    s.program = Some("node".to_owned());
    s.set_clock(TEST_NOW, PDT);
    let mut now = TEST_NOW;
    let mut presses = Vec::new();
    for _ in 0..400 {
        match s.limit_retry_due(&dialog, &opts) {
            RetryDue::Hold { until_unix, .. } => {
                assert!(until_unix > now, "{reset}: a hold ahead");
                now = until_unix;
            }
            RetryDue::Due => {
                presses.push(now);
                if now >= real {
                    break;
                }
                s.limit_retry_pressed();
                now += 5;
            }
            RetryDue::Not => panic!("{reset}: the dialog is the rule's"),
        }
        s.set_clock(now, PDT);
    }
    presses
}

#[test]
fn a_reset_whose_zone_is_unread_is_looked_at_a_bound_apart_and_tried_at_its_reset() {
    use super::super::approval_loop::LIMIT_RETRY_RELOOK_S;
    let grace = 60;
    let right = TEST_NOW + 14 * 3600 + 10 * 60;
    let presses = presses_till("11:05 PM", right + 60 + grace);
    let (last, looks) = presses.split_last().expect("presses");
    assert_eq!(*last, right + 60 + grace, "tried at its reset: {presses:?}");
    assert!(
        !looks.is_empty()
            && looks[0] == TEST_NOW + LIMIT_RETRY_RELOOK_S
            && looks
                .windows(2)
                .all(|w| w[1] - w[0] == LIMIT_RETRY_RELOOK_S)
            && last - looks[looks.len() - 1] <= LIMIT_RETRY_RELOOK_S,
        "looked at a bound apart: {presses:?}"
    );
    let real = TEST_NOW + 8 * 3600 + 10 * 60 + 60;
    let presses = presses_till("8:05 PM", real);
    let first_past = presses
        .iter()
        .find(|&&p| p >= real)
        .expect("a press past the real reset");
    assert!(
        first_past - real <= LIMIT_RETRY_RELOOK_S,
        "tried within a bound of the real reset: {presses:?}"
    );
}

#[test]
fn a_reset_first_read_long_past_is_tried_within_the_bound() {
    use super::super::approval_loop::{LIMIT_RETRY_RELOOK_S, RetryDue};
    let opts = SuperviseOpts {
        policy: owners_policy(),
        ..auto(60)
    };
    let dialog = gemini_reset_dialog("1:30 AM PDT");
    let mut m = gemini_mock(vec![dialog.clone()]);
    let mut s = session(&mut m, None);
    s.program = Some("node".to_owned());
    s.set_clock(TEST_NOW, PDT);
    let look = TEST_NOW + LIMIT_RETRY_RELOOK_S;
    assert_eq!(
        s.limit_retry_due(&dialog, &opts),
        RetryDue::Hold {
            until_unix: look,
            reset: Some("1:30 AM PDT".to_owned()),
        }
    );
    let tomorrows = TEST_NOW - (7 * 3600 + 25 * 60) + 86_400 + 60 + 60;
    s.set_clock(look, PDT);
    assert_eq!(s.limit_retry_due(&dialog, &opts), RetryDue::Due);
    s.limit_retry_pressed();
    s.set_clock(look + 5, PDT);
    assert_eq!(
        s.limit_retry_due(&dialog, &opts),
        RetryDue::Hold {
            until_unix: tomorrows,
            reset: Some("1:30 AM PDT".to_owned()),
        },
        "drawn again on the look: its reset waited for"
    );
}

#[test]
fn a_reset_the_loop_cannot_place_is_kept_trying_on_the_ladder() {
    use super::super::approval_loop::RetryDue;
    use crate::supervise::policy::approval::{LimitTry, limit_no_reset_reason};
    let opts = SuperviseOpts {
        policy: owners_policy(),
        ..auto(60)
    };
    let dialog = gemini_reset_dialog("soon");
    assert_eq!(
        aterm_phase::gemini::limit_reset(&dialog).map(|r| r.clock),
        Some("soon".to_owned()),
        "PRECONDITION: a reset drawn"
    );
    let mut m = gemini_mock(vec![dialog.clone()]);
    let mut s = session(&mut m, None);
    s.program = Some("node".to_owned());
    s.set_clock(TEST_NOW, PDT);
    let rung = TEST_NOW + 1800;
    assert_eq!(
        s.limit_retry_due(&dialog, &opts),
        RetryDue::Hold {
            until_unix: rung,
            reset: None,
        }
    );
    s.set_clock(rung, PDT);
    assert_eq!(s.limit_retry_due(&dialog, &opts), RetryDue::Due);
    s.limit_retry_pressed();
    s.set_clock(rung + 5, PDT);
    assert_eq!(
        s.limit_retry_due(&dialog, &opts),
        RetryDue::Hold {
            until_unix: rung + 2 * 3600,
            reset: None,
        },
        "the ladder's second rung from the press"
    );
    let said = limit_no_reset_reason(Some("soon"), LimitTry::KeepTrying);
    assert!(
        said.contains("cannot place (soon)") && !said.contains("names no reset"),
        "{said}"
    );
    assert!(said.chars().count() <= 120, "{said}");
}

#[test]
fn a_try_that_got_through_ends_the_usage_limit_episode() {
    use super::super::approval_loop::{LIMIT_RETRY_ANSWER_S, RetryDue};
    use aterm_phase::gemini::fixtures as g;
    use aterm_phase::prompt::fixtures::screen;
    let opts = SuperviseOpts {
        policy: owners_policy(),
        ..auto(60)
    };
    let named = gemini_reset_dialog("9:05 AM PDT");
    let none = screen(g::LIMIT_DIALOG);
    for (busy_at, in_flight) in [(10, true), (3 * 3600, false)] {
        assert_eq!(busy_at < LIMIT_RETRY_ANSWER_S, in_flight, "PRECONDITION");
        let mut m = gemini_mock(vec![named.clone()]);
        let mut s = session(&mut m, None);
        s.program = Some("node".to_owned());
        s.set_clock(TEST_NOW, PDT);
        assert!(matches!(
            s.limit_retry_due(&named, &opts),
            RetryDue::Hold { .. }
        ));
        let pressed = TEST_NOW + 12 * 60;
        s.set_clock(pressed, PDT);
        assert_eq!(s.limit_retry_due(&named, &opts), RetryDue::Due);
        s.limit_retry_pressed();
        s.set_clock(pressed + busy_at, PDT);
        assert!(!s.limit_retry_busy(), "no badge stands for a named reset");
        let met = pressed + busy_at + 5;
        s.set_clock(met, PDT);
        let want = if in_flight {
            pressed + 2 * 3600
        } else {
            met + 1800
        };
        assert_eq!(
            s.limit_retry_due(&none, &opts),
            RetryDue::Hold {
                until_unix: want,
                reset: None,
            },
            "busy {busy_at} s after the press"
        );
    }
}

#[test]
fn gemini_clis_usage_limit_with_no_reset_is_kept_trying_while_a_person_is_told() {
    use aterm_phase::gemini::fixtures as g;
    use aterm_phase::prompt::fixtures::screen;
    let dialog = screen(g::LIMIT_DIALOG);
    assert_eq!(
        aterm_phase::gemini::limit_reset(&dialog),
        None,
        "PRECONDITION"
    );
    const TRIES: usize = 4;
    let (dir, journal) = journal_file("te-gemini-no-reset");
    let (ledger_dir, ledger) = ledger_at("gemini-no-reset");
    let mut screens = vec![screen(g::BUSY_THINKING)];
    for _ in 0..TRIES {
        screens.extend([
            dialog.clone(),
            screen(g::BUSY_THINKING),
            screen(g::BUSY_STREAMING),
        ]);
    }
    screens.push(gemini_ended("Back past the quota."));
    let mut m = gemini_mock(screens);
    m.key_gates = (0..TRIES).map(|i| (3 * i + 1, i)).collect();
    m.stall_sleep = Some(Duration::from_secs(3600));
    m.exit_when = Some(|m| {
        let unset = m
            .requests
            .iter()
            .rposition(|r| r.starts_with("meta unset attention owner=supervisor"));
        let key = m.requests.iter().rposition(|r| verb_of(r) == "key");
        m.presses().len() >= TRIES && unset > key
    });
    let opts = SuperviseOpts {
        policy: owners_policy(),
        journal: Some(journal.clone()),
        ..auto(20 * 3600)
    };
    let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
        s.set_turn_end_timing(TurnEndTiming {
            min_work: Duration::ZERO,
            ..TurnEndTiming::default()
        });
        s.set_clock_running();
        s.set_approval_ledger(Some(ledger.clone()));
    });
    let presses = m.presses();
    assert_eq!(presses.len(), TRIES, "{lines:#?}\n{:#?}", m.requests);
    assert!(
        presses.iter().all(|p| p.ends_with(" 1")),
        "{presses:?}\n{lines:#?}"
    );
    let at = |needle: &str| -> Vec<usize> {
        m.requests
            .iter()
            .enumerate()
            .filter(|(_, r)| r.starts_with(needle))
            .map(|(i, _)| i)
            .collect()
    };
    let raised = at("meta set attention owner=supervisor");
    let lowered = at("meta unset attention owner=supervisor");
    let keys: Vec<usize> = m
        .requests
        .iter()
        .enumerate()
        .filter(|(_, r)| verb_of(r) == "key")
        .map(|(i, _)| i)
        .collect();
    assert_eq!(raised.len(), 1, "raised once: {:#?}", m.requests);
    assert!(
        m.requests[raised[0]].contains("names no reset"),
        "{}",
        m.requests[raised[0]]
    );
    assert_eq!(lowered.len(), 1, "taken down once: {:#?}", m.requests);
    assert!(raised[0] < keys[0], "{:#?}", m.requests);
    assert!(lowered[0] > keys[TRIES - 1], "{:#?}", m.requests);
    let (records, _) = journal_records(&journal);
    assert!(
        records
            .iter()
            .any(|r| r.line.starts_with("CLEARED ") && r.line.contains(" box ")),
        "{records:#?}"
    );
    let waiting: Vec<&String> = records
        .iter()
        .map(|r| &r.line)
        .filter(|l| l.starts_with("WAITING ") && l.contains("rule=limit-retry@v1"))
        .collect();
    assert_eq!(waiting.len(), TRIES, "said once for each due: {waiting:#?}");
    let untils: Vec<i64> = waiting
        .iter()
        .map(|l| {
            let stamp = l
                .split_whitespace()
                .find_map(|w| w.strip_prefix("until="))
                .expect("an until");
            aterm_types::rfc3339::parse_utc(stamp).expect("a stamp")
        })
        .collect();
    let rungs = [1800, 2 * 3600, 6 * 3600, 6 * 3600];
    let near = |got: i64, want: i64| (want..want + 30).contains(&got);
    assert!(near(untils[0], TEST_NOW + rungs[0]), "{waiting:#?}");
    for k in 1..TRIES {
        assert!(
            near(untils[k] - untils[k - 1], rungs[k]),
            "rung {k}: {waiting:#?}"
        );
        assert!(
            waiting[k].contains(&format!("(drawn again after its Keep trying, try {k})")),
            "{waiting:#?}"
        );
    }
    assert!(
        waiting
            .iter()
            .all(|l| l.contains("names no reset") && l.contains("back-off ladder")),
        "{waiting:#?}"
    );
    let ledgered = std::fs::read_to_string(&ledger).unwrap_or_default();
    let rows = |decision: &str| {
        ledgered
            .lines()
            .filter(|l| {
                l.contains("\"rule_id\":\"limit-retry@v1\"")
                    && l.contains(&format!("\"decision\":\"{decision}\""))
            })
            .count()
    };
    assert_eq!(rows("waiting"), TRIES, "{ledgered}");
    assert_eq!(rows("approved"), TRIES, "{ledgered}");
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&ledger_dir);
}

/// A host whose measure of the API's reach is scripted ([`IdleHost::reach`]):
/// each ask answers the next entry, the last for ever after; every ask is
/// counted.
#[derive(Debug, Default)]
struct ReachHost {
    script: std::sync::Mutex<VecDeque<Reach>>,
    last: std::sync::Mutex<Reach>,
    asks: std::sync::atomic::AtomicUsize,
    /// Every plan the loop told the host, in order ([`IdleHost::waiting`]).
    told: std::sync::Mutex<Vec<Option<crate::supervise::WaitPlan>>>,
}

impl ReachHost {
    fn scripted(script: &[Reach]) -> Arc<Self> {
        Arc::new(Self {
            script: std::sync::Mutex::new(script.iter().copied().collect()),
            ..Self::default()
        })
    }

    fn asks(&self) -> usize {
        self.asks.load(Ordering::SeqCst)
    }

    fn told(&self) -> Vec<Option<crate::supervise::WaitPlan>> {
        self.told.lock().unwrap().clone()
    }
}

impl IdleHost for ReachHost {
    fn waiting(&self, plan: Option<crate::supervise::WaitPlan>) {
        self.told.lock().unwrap().push(plan);
    }
    fn wants(&self) -> bool {
        false
    }
    fn at_idle(&self) -> Option<HostStep> {
        None
    }
    fn owns_turn_end(&self) -> bool {
        false
    }
    fn reach(&self) -> Reach {
        self.asks.fetch_add(1, Ordering::SeqCst);
        let mut last = self.last.lock().unwrap();
        if let Some(next) = self.script.lock().unwrap().pop_front() {
            *last = next;
        }
        *last
    }
}

fn enotfound() -> Vec<String> {
    aterm_phase::prompt::fixtures::screen(aterm_phase::prompt::fixtures::API_ERROR_ENOTFOUND)
}

fn far_rungs() -> TurnEndTiming {
    let hour = Duration::from_secs(3600);
    TurnEndTiming {
        net_backoff: vec![hour],
        down_hold: hour,
        ..TurnEndTiming::default()
    }
}

fn outage_under(script: &[Reach], max_s: u64) -> (Vec<String>, Arc<ReachHost>, Mock, Vec<String>) {
    outage_timed(script, max_s, far_rungs())
}

fn outage_timed(
    script: &[Reach],
    max_s: u64,
    timing: TurnEndTiming,
) -> (Vec<String>, Arc<ReachHost>, Mock, Vec<String>) {
    let host = ReachHost::scripted(script);
    let tag = timing.net_backoff.first().map_or(0, Duration::as_secs);
    let (dir, path) = journal_file(&format!("te-outage-{}-{max_s}-{tag}", script.len()));
    let mut m = Mock::new(vec![busy_screen(), enotfound(), busy_screen(), ended(STOP)]);
    m.turn_releases = Some(1);
    m.vanish_after = Some(2);
    m.stall_sleep = Some(Duration::from_millis(5));
    let opts = SuperviseOpts {
        idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
        journal: Some(path.clone()),
        ..hosted(max_s)
    };
    let (lines, _) = watch_lines_with(&mut m, &opts, |s| s.set_turn_end_timing(timing));
    let (records, _) = journal_records(&path);
    let _ = std::fs::remove_dir_all(&dir);
    let waiting = records
        .into_iter()
        .filter(|r| r.kind == "waiting")
        .map(|r| r.summary)
        .collect();
    (lines, host, m, waiting)
}

fn short_outage(
    tag: &str,
    screens: Vec<Vec<String>>,
    vanish_after: u32,
    max_s: u64,
) -> (Vec<String>, Arc<ReachHost>, Mock, Vec<String>) {
    let host = ReachHost::scripted(&[]);
    let (dir, path) = journal_file(tag);
    let mut m = Mock::new(screens);
    m.turn_releases = Some(1);
    m.vanish_after = Some(vanish_after);
    m.cursor_on_caret = true;
    m.stall_sleep = Some(Duration::from_millis(5));
    let opts = SuperviseOpts {
        idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
        journal: Some(path.clone()),
        ..hosted(max_s)
    };
    let timing = TurnEndTiming {
        net_backoff: vec![Duration::from_millis(50)],
        down_hold: Duration::from_millis(50),
        take_within: Duration::from_millis(300),
        ..TurnEndTiming::default()
    };
    let (lines, _) = watch_lines_with(&mut m, &opts, |s| s.set_turn_end_timing(timing));
    let (records, _) = journal_records(&path);
    let _ = std::fs::remove_dir_all(&dir);
    let waiting = records
        .into_iter()
        .filter(|r| r.kind == "waiting")
        .map(|r| r.summary)
        .collect();
    (lines, host, m, waiting)
}

#[test]
fn the_plan_is_cleared_when_the_try_is_typed_not_at_the_next_point() {
    let (lines, host, m, _) = short_outage(
        "te-plan-cleared-at-try",
        vec![busy_screen(), enotfound(), busy_screen()],
        3,
        3,
    );
    let told = host.told();
    assert!(
        lines.iter().any(|l| l.starts_with("CONTINUED")),
        "the try was made: {lines:#?}\n{:#?}",
        m.requests
    );
    assert!(
        matches!(told.as_slice(), [Some(_), None]),
        "a plan, then none once the try was typed — the run ends in the busy turn after it: \
         {told:#?}"
    );
}

#[test]
fn the_act_check_is_told_to_the_host_as_no_plan() {
    let tick = |n: u32| -> Vec<String> {
        enotfound()
            .into_iter()
            .map(|r| r.replace("Worked for 3m 0s", &format!("Worked for 3m {n}s")))
            .collect()
    };
    let screens = vec![busy_screen(), tick(0), tick(1), tick(2), tick(3), tick(4)];
    let (lines, host, _, waiting) = short_outage("te-plan-act-check", screens, 150, 30);
    let told = host.told();
    assert!(
        waiting.iter().any(|w| w.contains("the worker to take")),
        "control: the act check's wait stood: {waiting:#?}\n{lines:#?}"
    );
    assert!(
        told.iter().flatten().count() >= 1,
        "control: the rungs were told: {told:#?}"
    );
    assert!(
        told.iter()
            .flatten()
            .all(|p| !p.why.contains("the worker to take")),
        "the act check was told as a next try: {told:#?}"
    );
}

fn backed_off(tag: &str, every_step: bool) -> (Vec<Option<WaitPlan>>, Vec<String>, Vec<String>) {
    let mut quick = rows(&["⏺ Renamed the helper.", ""]);
    quick.extend(composer("  ⏵⏵ bypass permissions on (shift+tab to cycle)"));
    let host = ReachHost::scripted(&[]);
    let mut m = Mock::new(vec![busy_screen(), quick]);
    m.turn_releases = Some(1);
    m.vanish_after = Some(120);
    m.stall_sleep = Some(Duration::from_millis(5));
    let (dir, journal) = journal_file(tag);
    let opts = SuperviseOpts {
        idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
        policy: SupervisorConfig {
            human_grace_s: 0,
            ..owners_policy()
        },
        journal: Some(journal.clone()),
        ..auto(30)
    };
    super::TELL_EVERY_STEP.with(|c| c.set(every_step));
    let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
        s.set_turn_end_timing(TurnEndTiming {
            short_backoff: Duration::from_millis(200),
            short_backoff_max: Duration::from_millis(400),
            ..TurnEndTiming::default()
        });
    });
    super::TELL_EVERY_STEP.with(|c| c.set(false));
    let waiting = journal_records(&journal)
        .0
        .into_iter()
        .filter(|r| r.kind == "waiting")
        .map(|r| r.summary)
        .collect();
    let _ = std::fs::remove_dir_all(&dir);
    (host.told(), lines, waiting)
}

fn told_once(told: &[Option<WaitPlan>]) -> bool {
    let backoff = |p: &Option<WaitPlan>| {
        p.as_ref()
            .is_some_and(|p| matches!(p.kind, WaitKind::BackOff { .. }))
    };
    let starts: Vec<usize> = (0..told.len()).filter(|&i| backoff(&told[i])).collect();
    matches!(starts.as_slice(), [i] if told.get(i + 1) == Some(&None))
}

#[test]
fn a_back_off_is_told_once_at_its_start_and_cleared_once_at_its_end() {
    let (told, lines, waiting) = backed_off("te-backoff-told", false);
    assert!(
        waiting
            .iter()
            .any(|w| w.contains("1 short turn(s) in a row")),
        "control: the policy backed off: {waiting:#?}\n{lines:#?}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.contains(&format!("rule={RULE_CONTINUE} "))),
        "control: the act followed the back-off: {lines:#?}"
    );
    assert!(told_once(&told), "{told:#?}\n{lines:#?}");
    let plan = told.iter().flatten().next().expect("the back-off");
    assert_eq!(plan.kind, WaitKind::BackOff { short: 1 }, "{plan:?}");
    assert!(plan.why.contains("short turn(s) in a row"), "{plan:?}");
    let (every, lines, _) = backed_off("te-backoff-every", true);
    assert!(
        every
            .iter()
            .flatten()
            .filter(|p| matches!(p.kind, WaitKind::BackOff { .. }))
            .count()
            > 1,
        "control: the back-off took more than one step: {every:#?}\n{lines:#?}"
    );
    assert!(!told_once(&every), "{every:#?}");
}

#[test]
fn a_retry_is_told_as_a_retry_and_a_back_off_as_a_back_off() {
    let host = ReachHost::scripted(&[]);
    let mut m = Mock::new(vec![busy_screen()]);
    let opts = SuperviseOpts {
        idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
        ..hosted(2)
    };
    let mut s = session(&mut m, None);
    s.set_clock(TEST_NOW, PDT);
    let at = Instant::now() + Duration::from_secs(600);
    s.set_wait(at, "the worker to take the act", WaitKind::Other);
    s.tell_wait(&opts);
    s.set_wait(at, "api-error retry 1", WaitKind::Retry);
    s.tell_wait(&opts);
    s.set_wait(
        at,
        "3 short turn(s) in a row: 8 min back-off",
        WaitKind::BackOff { short: 3 },
    );
    s.tell_wait(&opts);
    s.tell_wait(&opts);
    s.set_due(at, "a person typing");
    s.tell_wait(&opts);
    let kinds: Vec<Option<WaitKind>> = host.told().into_iter().map(|p| p.map(|p| p.kind)).collect();
    assert_eq!(
        kinds,
        [
            None,
            Some(WaitKind::Retry),
            Some(WaitKind::BackOff { short: 3 }),
            None
        ],
        "an untold wait as none, the retry, the back-off once, then none"
    );
}

#[test]
fn a_wait_is_told_at_its_wall_second_and_again_only_when_that_second_moves() {
    let host = ReachHost::scripted(&[]);
    let mut m = Mock::new(vec![busy_screen()]);
    let opts = SuperviseOpts {
        idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
        ..hosted(2)
    };
    let mut s = session(&mut m, None);
    s.set_clock(TEST_NOW, PDT);
    let at = Instant::now() + Duration::from_secs(3600);
    s.set_wait(at, "api-error retry 1", WaitKind::Retry);
    s.tell_wait(&opts);
    s.tell_wait(&opts);
    let told = host.told();
    assert_eq!(told.len(), 1, "told once, however many steps: {told:#?}");
    let first = told[0].as_ref().expect("a plan").at_unix;
    assert!(
        (TEST_NOW + 3599..=TEST_NOW + 3600).contains(&first),
        "the deadline's wall second: {first}"
    );
    s.set_clock(TEST_NOW + 600, PDT);
    s.tell_wait(&opts);
    assert_eq!(host.told().len(), 1, "{:#?}", host.told());
    s.set_wait(at, "api-error retry 1", WaitKind::Retry);
    s.tell_wait(&opts);
    let told = host.told();
    assert_eq!(told.len(), 2, "told again: {told:#?}");
    let again = told[1].as_ref().expect("a plan again").at_unix;
    assert!(
        (first + 599..=first + 601).contains(&again),
        "{first} then {again}"
    );
    s.set_clock(TEST_NOW + 602, PDT);
    s.set_wait(at, "api-error retry 1", WaitKind::Retry);
    s.tell_wait(&opts);
    assert_eq!(host.told().len(), 2, "{:#?}", host.told());
}

#[test]
fn the_loop_reads_the_wall_of_a_tall_panes_short_transcript() {
    use aterm_phase::prompt::fixtures::{API_ERROR_TALL_PANE_MEASURED, screen};
    let whole_reads = |m: &Mock| {
        m.requests
            .iter()
            .filter(|r| r.trim_start_matches("@s-1 ") == "text --json")
            .count()
    };
    let run = |tag: &str, rows: Vec<String>| {
        let (lines, host, m, waiting) = short_outage(tag, vec![busy_screen(), rows], 4, 3);
        (lines, host.told(), whole_reads(&m), waiting, m.requests)
    };
    let tall = screen(API_ERROR_TALL_PANE_MEASURED);
    assert_eq!(tall.len(), 50, "the measured pane");
    let (lines, told, whole, waiting, requests) = run("te-tall-pane", tall.clone());
    assert!(
        waiting.iter().any(|w| w.contains("api-error retry 1")),
        "the wall is read: {waiting:#?}\n{lines:#?}\n{requests:#?}"
    );
    assert!(
        told.iter().flatten().any(|p| p.why.contains("api-error")),
        "a plan told: {told:#?}"
    );
    assert!(
        lines.iter().any(|l| l.starts_with("CONTINUED")),
        "the try typed: {lines:#?}"
    );
    assert!(whole > 0, "the tail was read again whole: {requests:#?}");

    let mut full = tall.clone();
    for row in &mut full[11..46] {
        *row = "· earlier".to_string();
    }
    let (lines, told, whole, waiting, requests) = run("te-tall-pane-full", full);
    assert!(
        !waiting.iter().any(|w| w.contains("api-error")),
        "the error is outside the zone: {waiting:#?}\n{lines:#?}"
    );
    assert!(
        told.iter().flatten().all(|p| p.kind != WaitKind::Retry),
        "{told:#?}"
    );
    assert_eq!(whole, 0, "the tail alone: {requests:#?}");

    let mut short = tall.clone();
    short.drain(11..31);
    let (lines, told, whole, waiting, requests) = run("te-tall-pane-short", short);
    assert!(
        waiting.iter().any(|w| w.contains("api-error retry 1")),
        "{waiting:#?}\n{lines:#?}"
    );
    assert!(told.iter().flatten().count() >= 1, "{told:#?}");
    assert_eq!(whole, 0, "the whole pane fits the tail: {requests:#?}");
}

fn awaits_before_turn(m: &Mock) -> usize {
    let end = m
        .requests
        .iter()
        .position(|r| r.starts_with("turn "))
        .unwrap_or(m.requests.len());
    m.requests[..end]
        .iter()
        .filter(|r| r.starts_with("await "))
        .count()
}

#[test]
fn an_up_measure_mid_wait_continues_the_outage_within_one_step() {
    let t = Instant::now();
    let down = Reach::Down { since: t };
    let up = Reach::Up {
        since: t + Duration::from_secs(1),
    };
    let (lines, host, m, waiting) = outage_under(&[down, down, down, up], 30);
    let continued: Vec<&String> = lines
        .iter()
        .filter(|l| l.starts_with("CONTINUED"))
        .collect();
    assert_eq!(continued.len(), 1, "{lines:#?}");
    assert!(
        continued[0].starts_with("CONTINUED seq=102 rule=api-back@v1 "),
        "{lines:#?}"
    );
    assert_eq!(count(&m, "turn"), 1, "{:#?}", m.requests);
    assert_eq!(waiting.len(), 1, "the hold, said once: {waiting:#?}");
    assert!(waiting[0].contains("the API is unreachable"), "{waiting:?}");
    assert_eq!(host.asks(), 6, "{lines:#?}");

    let (_, _, longer, _) = outage_under(&[down, down, down, down, up], 30);
    assert_eq!(
        awaits_before_turn(&longer),
        awaits_before_turn(&m) + 1,
        "{:#?}\n{:#?}",
        m.requests,
        longer.requests
    );

    let (lines, host, m, waiting) = outage_under(&[down], 1);
    assert_eq!(count(&m, "turn"), 0, "{lines:#?}\n{:#?}", m.requests);
    assert!(
        !lines.iter().any(|l| l.starts_with("CONTINUED")),
        "{lines:#?}"
    );
    assert!(
        waiting.iter().any(|w| w.contains("the API is unreachable")),
        "{waiting:#?}"
    );
    assert!(host.asks() > 2, "asked at each step while it waited");
}

#[test]
fn an_ordinary_or_escalated_point_never_asks_for_the_reach_again() {
    let host = ReachHost::scripted(&[Reach::Up {
        since: Instant::now(),
    }]);
    let mut m = Mock::new(vec![
        busy_screen(),
        ended("Fixed the parser; the suite is green."),
        busy_screen(),
        ended(STOP),
    ]);
    m.turn_releases = Some(1);
    m.vanish_after = Some(2);
    let opts = SuperviseOpts {
        idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
        ..hosted(30)
    };
    let (lines, _) = watch_lines(&mut m, &opts);
    assert!(
        lines.contains(&"CONTINUED seq=102 rule=continue@v1 keep going".to_string()),
        "{lines:#?}"
    );
    assert_eq!(host.asks(), 0, "an ordinary point asks nothing: {lines:#?}");

    let host = ReachHost::scripted(&[
        Reach::Down {
            since: Instant::now(),
        },
        Reach::Up {
            since: Instant::now(),
        },
    ]);
    let mut m = Mock::new(vec![busy_screen(), enotfound()]);
    m.vanish_after = Some(4);
    let mut off = hosted(30);
    off.policy.retry_api_errors = false;
    off.idle_host = Some(Arc::clone(&host) as Arc<dyn IdleHost>);
    let (lines, _) = watch_lines(&mut m, &off);
    assert_eq!(count(&m, "turn"), 0, "{lines:#?}");
    assert_eq!(count(&m, "meta set attention"), 1, "{:#?}", m.requests);
    assert_eq!(host.asks(), 1, "only at the point: {lines:#?}");
}

/// A host that wants every point, counts the background breaks offered, and
/// keeps the guards the loop said withheld one ([`IdleHost::withheld`]).
#[derive(Debug, Default)]
struct BackgroundHost {
    backgrounds: std::sync::atomic::AtomicUsize,
    informed: std::sync::Mutex<Vec<String>>,
    withheld: std::sync::Mutex<Vec<crate::supervise::Guard>>,
}

impl IdleHost for BackgroundHost {
    fn wants(&self) -> bool {
        true
    }
    fn at_idle(&self) -> Option<HostStep> {
        None
    }
    fn owns_turn_end(&self) -> bool {
        false
    }
    fn at_background(&self) -> Option<(String, crate::supervise::policy::turn_end::HostTurn)> {
        self.backgrounds.fetch_add(1, Ordering::SeqCst);
        Some((
            "upgrade step=announced:1".to_string(),
            crate::supervise::policy::turn_end::HostTurn::Notice,
        ))
    }
    fn inform(&self, text: &str) {
        self.informed.lock().unwrap().push(text.to_string());
    }
    fn withheld(&self, guard: crate::supervise::Guard) {
        self.withheld.lock().unwrap().push(guard);
    }
}

fn waiting_on(rows_above: &[&str]) -> Vec<String> {
    let mut r = rows(rows_above);
    r.extend(composer(
        "  ⏵⏵ bypass permissions on (shift+tab to cycle) · ← for agents",
    ));
    r
}

fn woken(end: &[&str]) -> Vec<String> {
    let mut above = vec![
        "⏺ Second-pass reviewers are out and the build is running.",
        "",
        "✻ Waiting for 1 dynamic workflow to finish",
        "",
        "⏺ Monitor event: \"lander\"",
        "",
        "⏺ Still in its crates-tests lane. I restarted the watch.",
        "",
    ];
    above.extend_from_slice(end);
    waiting_on(&above)
}

/// A host that hands the loop the agent's transcript `tail` and the launch of
/// the process that writes it (`launched`, unix seconds; `None`: none), wants
/// ONE point — the first offered, at a break of background work or idle,
/// where it types nothing — and counts the background breaks offered.
#[derive(Debug, Default)]
struct CarriedHost {
    tail: Option<String>,
    launched: Option<i64>,
    offered: std::sync::atomic::AtomicUsize,
    backgrounds: std::sync::atomic::AtomicUsize,
}

impl IdleHost for CarriedHost {
    fn wants(&self) -> bool {
        self.offered.load(Ordering::SeqCst) == 0
    }
    fn at_idle(&self) -> Option<HostStep> {
        self.offered.fetch_add(1, Ordering::SeqCst);
        None
    }
    fn owns_turn_end(&self) -> bool {
        false
    }
    fn at_background(&self) -> Option<(String, crate::supervise::policy::turn_end::HostTurn)> {
        self.offered.fetch_add(1, Ordering::SeqCst);
        self.backgrounds.fetch_add(1, Ordering::SeqCst);
        None
    }
    fn transcript_tail(&self) -> Option<String> {
        self.tail.clone()
    }
    fn launched_at(&self) -> Option<i64> {
        self.launched
    }
}

#[test]
fn a_turn_its_own_wake_ended_still_waiting_is_not_continued() {
    for done in [false, true] {
        let spinner = waiting_on(&[
            "⏺ Second-pass reviewers are out and the build is running.",
            "",
            "✻ Waiting for 1 dynamic workflow to finish",
            "",
            "⏺ Monitor event: \"lander\"",
            "",
            "✶ Deliberating… (2s)",
            "",
        ]);
        let end = if done {
            woken(&["✻ Worked for 3m 2s · done 4:24 PM", ""])
        } else {
            woken(&[])
        };
        let drawn = waiting_on(&[
            "⏺ Second-pass reviewers are out and the build is running.",
            "",
            "✻ Waiting for 1 dynamic workflow to finish",
            "",
        ]);
        let host = Arc::new(CarriedHost::default());
        let mut m = Mock::new(vec![drawn, spinner, end]);
        m.vanish_after = Some(2);
        let opts = SuperviseOpts {
            idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
            ..hosted(30)
        };
        let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
            s.set_background_settle(Duration::ZERO);
        });
        let why = format!("done={done}: {lines:#?}\n{:#?}", m.requests);
        let continued = lines
            .iter()
            .any(|l| l.starts_with("CONTINUED") && l.ends_with("keep going"));
        assert_eq!(continued, done, "{why}");
        assert_eq!(count(&m, "turn") > 0, done, "{why}");
        if !done {
            assert!(host.backgrounds.load(Ordering::SeqCst) > 0, "{why}");
            assert!(!lines.iter().any(|l| l.starts_with("EVENT idle")), "{why}");
        }
    }
}

#[test]
fn an_agy_or_muse_turn_ended_over_its_own_background_work_is_not_continued() {
    use aterm_phase::agy::fixtures::lab as a;
    use aterm_phase::muse::fixtures as u;
    use aterm_phase::prompt::fixtures::screen;
    for (program, busy, running, over) in [
        ("agy", a::BUSY_THINKING, a::BG_TASK, a::BG_TASK_ENDED),
        (
            "muse",
            u::BUSY_1_4_10,
            u::END_OF_TURN_BG_RUNNING,
            u::END_OF_TURN_BG_FINISHED,
        ),
    ] {
        for done in [false, true] {
            let screens = vec![screen(busy), screen(if done { over } else { running })];
            let mut m = if program == "agy" {
                agy_mock(screens)
            } else {
                muse_mock(screens)
            };
            m.turn_releases = Some(1);
            m.vanish_after = Some(2);
            let host = Arc::new(CarriedHost::default());
            let opts = SuperviseOpts {
                idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
                ..hosted(3)
            };
            let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
                s.set_background_settle(Duration::ZERO);
                s.set_turn_end_timing(TurnEndTiming {
                    min_work: Duration::ZERO,
                    ..TurnEndTiming::default()
                });
            });
            let why = format!("{program} done={done}: {lines:#?}\n{:#?}", m.requests);
            let typed = m
                .requests
                .iter()
                .any(|r| r.starts_with("turn ") || r.starts_with("send "));
            assert_eq!(typed, done, "{why}");
            let continued = lines
                .iter()
                .any(|l| l.starts_with("CONTINUED") && l.ends_with("keep going"));
            assert_eq!(continued, done, "{why}");
            if !done {
                assert!(host.backgrounds.load(Ordering::SeqCst) > 0, "{why}");
                assert!(!lines.iter().any(|l| l.starts_with("EVENT idle")), "{why}");
            }
        }
    }
}

#[test]
fn a_question_over_agy_or_muse_background_work_is_held_as_over_claude_codes_shell() {
    use crate::supervise::policy::turn_end::RULE_ANSWER;
    use aterm_phase::agy::fixtures::lab as a;
    use aterm_phase::muse::fixtures as u;
    use aterm_phase::prompt::fixtures::screen;
    let edited = |rows: Vec<String>, head: &str, with: &str, next: Option<&str>| {
        let mut rows = rows;
        let at = rows
            .iter()
            .rposition(|r| r.starts_with(head))
            .unwrap_or_else(|| panic!("a row starting {head:?}"));
        rows[at] = with.to_string();
        if let Some(next) = next {
            rows[at + 1] = next.to_string();
        }
        rows
    };
    let agy_asks = |text: &str| {
        edited(
            screen(text),
            "  Tool call done.",
            "  Should I wait for it to finish?",
            None,
        )
    };
    let muse_asks = |branch: Option<&str>| {
        let rows = edited(
            screen(u::END_OF_TURN_BG_RUNNING),
            "◆ Tool call done.",
            "◆ Should I wait for it to finish?",
            Some(""),
        );
        match branch {
            Some(branch) => {
                let at = rows
                    .iter()
                    .rposition(|r| r.starts_with("└ "))
                    .expect("the branch");
                let row = rows[at].replace("Q3 background wait  running ", branch);
                assert_ne!(row, rows[at], "the branch rewritten");
                edited(rows, "└ ", &row, None)
            }
            None => rows,
        }
    };
    let finished = format!(
        "└ ◆ Q3 background wait  ran 1s, exit 0{}1m 40s",
        " ".repeat(100)
    );
    let cases = [
        (
            "agy",
            "task",
            screen(a::BUSY_THINKING),
            agy_asks(a::BG_TASK),
            false,
        ),
        (
            "agy",
            "over",
            screen(a::BUSY_THINKING),
            agy_asks(a::BG_TASK_ENDED),
            true,
        ),
        (
            "muse",
            "shell",
            screen(u::BUSY_1_4_10),
            muse_asks(None),
            false,
        ),
        (
            "muse",
            "monitor",
            screen(u::BUSY_1_4_10),
            muse_asks(Some("Watch the lab socket  watching")),
            true,
        ),
        (
            "muse",
            "over",
            screen(u::BUSY_1_4_10),
            edited(muse_asks(None), "└ ", &finished, None),
            true,
        ),
    ];
    for (program, work, busy, asks, answered) in cases {
        let screens = vec![busy, asks];
        let mut m = if program == "agy" {
            agy_mock(screens)
        } else {
            muse_mock(screens)
        };
        m.turn_releases = Some(1);
        m.vanish_after = Some(2);
        let host = Arc::new(CarriedHost::default());
        let opts = SuperviseOpts {
            idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
            policy: owners_policy(),
            ..auto(3)
        };
        let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
            s.set_background_settle(Duration::ZERO);
            s.set_turn_end_timing(TurnEndTiming {
                min_work: Duration::ZERO,
                ..TurnEndTiming::default()
            });
        });
        let why = format!("{program} {work}: {lines:#?}\n{:#?}", m.requests);
        let typed = m
            .requests
            .iter()
            .any(|r| r.starts_with("turn ") || r.starts_with("send "));
        assert_eq!(typed, answered, "{why}");
        let answer = lines
            .iter()
            .any(|l| l.starts_with("CONTINUED") && l.contains(&format!("rule={RULE_ANSWER}")));
        assert_eq!(answer, answered, "{why}");
        if !answered {
            assert!(host.backgrounds.load(Ordering::SeqCst) > 0, "{why}");
        }
    }
}

#[test]
fn a_turn_its_transcript_says_ended_waiting_is_not_continued() {
    let end = waiting_on(&[
        "⏺ Monitor event: \"lander\"",
        "",
        "⏺ Still in its crates-tests lane. I restarted the watch.",
        "",
    ]);
    let drawn = waiting_on(&[
        "⏺ Monitor event: \"lander\"",
        "",
        "⏺ Still in its crates-tests lane. I restarted the watch.",
        "",
        "✻ Worked for 3m 2s · done 4:24 PM",
        "",
    ]);
    let submitted = waiting_on(&[
        "⏺ Still in its crates-tests lane. I restarted the watch.",
        "",
        "❯ keep going",
        "",
    ]);
    let earlier = Some(transcript_at("20:00:00"));
    let later = Some(transcript_at("22:54:00"));
    for (screen, pending, launched, waits, continues, why) in [
        (&end, 1, earlier, true, false, "pending"),
        (&end, 0, earlier, false, true, "nothing pending"),
        (&end, 1, later, false, true, "a row older than the launch"),
        (&end, 1, None, false, true, "no launch record"),
        (&drawn, 1, earlier, false, true, "a done row drawn"),
        (
            &submitted,
            1,
            earlier,
            false,
            false,
            "a prompt just submitted",
        ),
    ] {
        let host = Arc::new(CarriedHost {
            tail: Some(transcript_closed(pending)),
            launched,
            ..CarriedHost::default()
        });
        let mut m = Mock::new(vec![busy_screen(), screen.clone()]);
        m.vanish_after = Some(2);
        let opts = SuperviseOpts {
            idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
            ..hosted(30)
        };
        let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
            s.set_background_settle(Duration::ZERO);
            s.set_turn_end_timing(TurnEndTiming {
                short_backoff: Duration::ZERO,
                ..TurnEndTiming::default()
            });
        });
        let why = format!(
            "{why}: pending={pending} launched={launched:?}: {lines:#?}\n{:#?}",
            m.requests
        );
        let continued = lines
            .iter()
            .any(|l| l.starts_with("CONTINUED") && l.ends_with("keep going"));
        assert_eq!(continued, continues, "{why}");
        assert_eq!(count(&m, "turn") > 0, continues, "{why}");
        assert_eq!(host.backgrounds.load(Ordering::SeqCst) > 0, waits, "{why}");
    }
}

fn transcript_at(t: &str) -> i64 {
    crate::harness::upgrade_models::parse_utc(&format!("2026-10-07T{t}Z"))
        .and_then(|s| i64::try_from(s).ok())
        .expect("a time")
}

fn transcript_closed(pending: u32) -> String {
    let notification = r#"{"parentUuid":"p0","isSidechain":false,"type":"user","message":{"role":"user","content":"<task-notification>\n<status>completed</status>\n</task-notification>"},"uuid":"u1","timestamp":"2026-10-07T22:53:28.100Z","origin":{"kind":"task-notification"},"turnOrigin":"task_notification"}"#;
    let reply = r#"{"parentUuid":"u1","isSidechain":false,"type":"assistant","message":{"model":"claude-opus-5-5","content":[{"type":"text","text":"I restarted the lander watch."}]},"uuid":"a1","timestamp":"2026-10-07T22:53:30.500Z"}"#;
    let closed = format!(
        r#"{{"parentUuid":"a1","isSidechain":false,"type":"system","subtype":"turn_duration","durationMs":4094,"messageCount":1087,"pendingWorkflowCount":{pending},"pendingBackgroundAgentCount":0,"uuid":"d1","isMeta":false,"timestamp":"2026-10-07T22:53:32.994Z"}}"#
    );
    format!("{notification}\n{reply}\n{closed}")
}

#[test]
fn a_carried_wait_its_transcript_says_is_over_is_continued() {
    let lagging = format!(
        "{}\n{}",
        transcript_closed(0),
        r#"{"parentUuid":"d1","isSidechain":false,"type":"assistant","message":{"model":"claude-opus-5-5","content":[{"type":"text","text":"Still waiting."}]},"uuid":"a2","timestamp":"2026-10-07T22:54:10.000Z"}"#
    );
    let earlier = Some(transcript_at("20:00:00"));
    for (tail, launched, over, why) in [
        (Some(transcript_closed(0)), earlier, true, "nothing pending"),
        (Some(transcript_closed(1)), earlier, false, "pending"),
        (
            Some(transcript_closed(0)),
            Some(transcript_at("22:54:00")),
            false,
            "a row older than the launch",
        ),
        (Some(transcript_closed(0)), None, false, "no launch record"),
        (None, earlier, false, "no transcript"),
        (Some(lagging.clone()), earlier, false, "a later entry"),
    ] {
        let host = Arc::new(CarriedHost {
            tail,
            launched,
            ..CarriedHost::default()
        });
        let mut m = Mock::new(vec![busy_screen(), woken(&[])]);
        m.vanish_after = Some(2);
        let opts = SuperviseOpts {
            idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
            ..hosted(30)
        };
        let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
            s.set_background_settle(Duration::ZERO);
            s.set_turn_end_timing(TurnEndTiming {
                short_backoff: Duration::ZERO,
                ..TurnEndTiming::default()
            });
        });
        let why = format!("{why}: {lines:#?}\n{:#?}", m.requests);
        let continued = lines
            .iter()
            .any(|l| l.starts_with("CONTINUED") && l.ends_with("keep going"));
        assert_eq!(continued, over, "{why}");
        assert_eq!(count(&m, "turn") > 0, over, "{why}");
        assert_eq!(host.backgrounds.load(Ordering::SeqCst) > 0, !over, "{why}");
    }
}

/// A host for BG-1's points read again: the agent's transcript `tail` (which
/// a test may change between reads) and its process's launch; when it
/// `wants` a point it wants ONE, counting the idle and the background breaks
/// offered apart, and types nothing at either.
#[derive(Debug, Default)]
struct PointHost {
    tail: std::sync::Mutex<Option<String>>,
    launched: Option<i64>,
    wants: bool,
    idle: std::sync::atomic::AtomicUsize,
    backgrounds: std::sync::atomic::AtomicUsize,
    /// What withheld the host's step ([`IdleHost::withheld`]).
    withheld: std::sync::Mutex<Vec<crate::supervise::Guard>>,
}

impl PointHost {
    fn closed(pending: u32, wants: bool) -> Arc<Self> {
        Arc::new(Self {
            tail: std::sync::Mutex::new(Some(transcript_closed(pending))),
            launched: Some(transcript_at("20:00:00")),
            wants,
            ..Self::default()
        })
    }
}

impl IdleHost for PointHost {
    fn wants(&self) -> bool {
        self.wants
            && self.idle.load(Ordering::SeqCst) == 0
            && self.backgrounds.load(Ordering::SeqCst) == 0
    }
    fn at_idle(&self) -> Option<HostStep> {
        self.idle.fetch_add(1, Ordering::SeqCst);
        None
    }
    fn owns_turn_end(&self) -> bool {
        false
    }
    fn at_background(&self) -> Option<(String, crate::supervise::policy::turn_end::HostTurn)> {
        self.backgrounds.fetch_add(1, Ordering::SeqCst);
        None
    }
    fn withheld(&self, guard: crate::supervise::Guard) {
        self.withheld.lock().unwrap().push(guard);
    }
    fn transcript_tail(&self) -> Option<String> {
        self.tail.lock().unwrap().clone()
    }
    fn launched_at(&self) -> Option<i64> {
        self.launched
    }
}

fn point_run(
    host: &Arc<PointHost>,
    end: Vec<String>,
    vanish: u32,
    cursor: Option<usize>,
) -> (Vec<String>, String) {
    let mut m = Mock::new(vec![busy_screen(), end]);
    m.exit_when = Some(|m| m.turns >= 1);
    m.vanish_after = Some(vanish);
    m.stall_sleep = Some(Duration::from_millis(100));
    m.cursor_row = cursor;
    let opts = SuperviseOpts {
        idle_host: Some(Arc::clone(host) as Arc<dyn IdleHost>),
        ..hosted(120)
    };
    let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
        s.set_background_settle(Duration::ZERO);
        s.set_turn_end_timing(TurnEndTiming {
            short_backoff: Duration::from_millis(50),
            ..TurnEndTiming::default()
        });
    });
    let why = format!("{lines:#?}\n{:#?}", m.requests);
    (lines, why)
}

#[test]
fn a_carried_wait_over_is_one_point_at_every_read() {
    let drawn = || woken(&["✻ Worked for 3m 2s · done 4:24 PM", ""]);
    for wants in [false, true] {
        for (end, pending, over, label) in [
            (woken(&[]), 0, true, "carried, nothing pending"),
            (drawn(), 0, true, "a done row drawn"),
            (woken(&[]), 1, false, "carried, still pending"),
        ] {
            let host = PointHost::closed(pending, wants);
            let (lines, why) = point_run(&host, end, if over { 600 } else { 4 }, None);
            let why = format!("wants={wants} {label}: {why}");
            let continued = lines
                .iter()
                .any(|l| l.starts_with("CONTINUED") && l.ends_with("keep going"));
            assert_eq!(continued, over, "{why}");
            if wants {
                assert_eq!(host.idle.load(Ordering::SeqCst), usize::from(over), "{why}");
                assert_eq!(host.backgrounds.load(Ordering::SeqCst) > 0, !over, "{why}");
            }
        }
    }
}

#[test]
fn a_carried_wait_over_is_vouched_for_by_the_cursor() {
    let drawn = || woken(&["✻ Worked for 3m 2s · done 4:24 PM", ""]);
    for (end, label) in [(woken(&[]), "carried"), (drawn(), "a done row drawn")] {
        let host = PointHost::closed(0, true);
        let (_, why) = point_run(&host, end, 600, Some(2));
        assert_eq!(host.idle.load(Ordering::SeqCst), 0, "{label}: {why}");
    }
}

#[test]
fn a_point_decided_again_asks_the_transcript_again() {
    let end = waiting_on(&[
        "⏺ Monitor event: \"lander\"",
        "",
        "⏺ Still in its crates-tests lane. I restarted the watch.",
        "",
    ]);
    for pending_by_due in [true, false] {
        let host = PointHost::closed(0, false);
        let opts = SuperviseOpts {
            idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
            ..hosted(30)
        };
        let mut m = Mock::new(vec![end.clone()]);
        m.turn_releases = Some(0);
        let mut heard = Heard::default();
        {
            let mut s = session(&mut m, None);
            s.stall_host = opts.idle_host.clone();
            s.set_turn_end_timing(TurnEndTiming {
                short_backoff: Duration::ZERO,
                ..TurnEndTiming::default()
            });
            let screen = s.screen().expect("the point");
            let point = s.point_of(screen);
            assert_eq!(point.phase, Phase::Idle, "nothing pending yet");
            if pending_by_due {
                *host.tail.lock().unwrap() = Some(transcript_closed(1));
            }
            s.turn_end_now(&point, &opts, &[], &mut heard)
                .unwrap_or_else(|e| panic!("{e:?}"));
        }
        let why = format!(
            "pending_by_due={pending_by_due}: {:#?}\n{:#?}",
            heard_lines(&heard),
            m.requests
        );
        assert_eq!(count(&m, "turn") == 0, pending_by_due, "{why}");
    }
}

#[test]
fn a_bare_clock_reset_is_placed_for_the_wall_it_belongs_to() {
    let at = |h: i64, m: i64| TEST_NOW + (h - 8) * 3600 + (m - 55) * 60;
    let done = |clock: &str| {
        let mut r = rows(&[
            "⏺ Done.",
            "",
            &format!("✻ Worked for 3m 2s · done {clock}"),
            "",
        ]);
        r.extend(composer("  ⏵⏵ bypass permissions on"));
        r
    };
    let nine = "9am (America/Los_Angeles)";
    let four = "4pm (America/Los_Angeles)";
    let nine_fifty = "9:50pm (America/Los_Angeles)";
    type Case<'a> = (
        &'a str,
        (i64, i64),
        &'a str,
        bool,
        bool,
        Option<&'a str>,
        i64,
    );
    let cases: [Case; 11] = [
        ("weekly arriving", (20, 0), nine, false, true, None, 13 * 60),
        (
            "session arriving",
            (20, 0),
            nine,
            true,
            true,
            None,
            -11 * 60,
        ),
        (
            "weekly by its done row",
            (20, 0),
            nine,
            false,
            false,
            Some("7:58 PM"),
            13 * 60,
        ),
        (
            "weekly sitting, passed",
            (20, 0),
            nine,
            false,
            false,
            None,
            -11 * 60,
        ),
        (
            "weekly sitting, near",
            (14, 0),
            four,
            false,
            false,
            None,
            2 * 60,
        ),
        ("passed, arriving", (16, 5), four, false, true, None, -5),
        (
            "passed, done row",
            (16, 5),
            four,
            false,
            false,
            Some("4:05 PM"),
            -5,
        ),
        (
            "after the slack",
            (16, 9),
            four,
            false,
            true,
            None,
            24 * 60 - 9,
        ),
        ("a span", (20, 0), "in 3h", false, true, None, 180),
        (
            "session after a night",
            (8, 0),
            nine_fifty,
            true,
            false,
            Some("9:22 PM"),
            -(10 * 60 + 10),
        ),
        (
            "session sitting",
            (8, 0),
            nine_fifty,
            true,
            false,
            None,
            13 * 60 + 50,
        ),
    ];
    let mut m = Mock::new(vec![busy_screen()]);
    for (why, now, reset, window, live, done_at, want) in cases {
        let rows = done_at.map(done).unwrap_or_default();
        let mut s = session(&mut m, None);
        s.set_clock(at(now.0, now.1), PDT);
        s.set_zone_lookup(test_zones);
        let n = s.now_unix();
        let got = s
            .reset_unix_for(Some(reset), window, live, false, &rows)
            .map(|u| (u - n) / 60);
        assert_eq!(got, Some(want), "{why}");
    }
}

#[test]
fn a_weekly_wall_hit_in_the_evening_that_resets_in_the_morning_is_waited_for() {
    let evening = TEST_NOW + 11 * 3600 + 5 * 60;
    let wall = |banner: Option<&str>| {
        let mut r = rows(&[
            "⏺ Running the suite.",
            "  ⎿  You've hit your weekly limit · resets 9am (America/Los_Angeles)",
            "",
            "✻ Worked for 3m 2s · done 8:00 PM",
        ]);
        if let Some(b) = banner {
            r.push(format!("{}{b}", " ".repeat(90)));
        }
        r.extend(composer("  ⏵⏵ bypass permissions on (shift+tab to cycle)"));
        r
    };
    let run = |tag: &str, screens: Vec<Vec<String>>| {
        let (dir, path) = journal_file(tag);
        let mut m = Mock::new(screens);
        m.vanish_after = Some(6);
        let opts = SuperviseOpts {
            journal: Some(path.clone()),
            ..hosted(30)
        };
        let (lines, _) = watch_lines_with(&mut m, &opts, |s| s.set_clock(evening, PDT));
        let (records, _) = journal_records(&path);
        let _ = std::fs::remove_dir_all(&dir);
        (m, lines, records)
    };
    let waits = |records: &[crate::supervise::journal::JournalRecord]| {
        records
            .iter()
            .filter(|r| r.kind == "waiting")
            .map(|r| r.line.clone())
            .collect::<Vec<_>>()
    };
    let (m, lines, records) = run("te-evening", vec![busy_screen(), wall(None)]);
    assert_eq!(count(&m, "turn"), 0, "{lines:#?}\n{:#?}", m.requests);
    let w = waits(&records);
    assert!(
        w.iter()
            .any(|l| l.contains("until=2026-09-18T16:02:30Z") && l.contains("usage-weekly resets")),
        "{w:#?}"
    );
    let armed = "⚠ Usage limit reached · continuing automatically at 9am · esc to cancel";
    let (m, lines, records) = run("te-evening-armed", vec![busy_screen(), wall(Some(armed))]);
    assert_eq!(count(&m, "turn"), 0, "{lines:#?}");
    let w = waits(&records);
    assert!(
        w.iter()
            .any(|l| l.contains("until=2026-09-18T16:10:00Z") && l.contains("goes on by itself")),
        "{w:#?}"
    );
    let mut r = wall(None);
    r[1] = "  ⎿  You've hit your session limit · resets 9am (America/Los_Angeles)".to_string();
    let (m, lines, _) = run("te-evening-session", vec![busy_screen(), r]);
    assert_eq!(count(&m, "turn"), 1, "{lines:#?}");
}

const TAB_EVENING: i64 = TEST_NOW + 8 * 3600 + 31 * 60;

fn limit_resets_5am(typed: bool) -> Vec<String> {
    let mut r = rows(&["⏺ Still done, and I'm not starting new work.", ""]);
    if typed {
        r.extend(rows(&["❯ keep going", ""]));
    }
    r.extend(composer("  ⚠ Usage limit reached · limit resets 5am"));
    r.push("  ⏵⏵ bypass permissions on (shift+tab to cycle)".to_string());
    r
}

fn tab_run(
    tag: &str,
    screens: Vec<Vec<String>>,
    sits: Option<usize>,
    clock: i64,
) -> (Mock, Vec<String>, Vec<String>) {
    let (dir, path) = journal_file(tag);
    let mut m = Mock::new(screens);
    m.turn_releases = sits;
    m.vanish_after = Some(40);
    m.stall_sleep = Some(Duration::from_millis(5));
    let opts = SuperviseOpts {
        journal: Some(path.clone()),
        ..hosted(30)
    };
    let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
        s.set_clock(clock, PDT);
        s.set_turn_end_timing(TurnEndTiming {
            take_within: Duration::from_millis(50),
            ..TurnEndTiming::default()
        });
    });
    let (records, _) = journal_records(&path);
    let _ = std::fs::remove_dir_all(&dir);
    let waits = records
        .iter()
        .filter(|r| r.kind == "waiting")
        .map(|r| r.summary.clone())
        .collect();
    (m, lines, waits)
}

#[test]
fn a_weekly_wall_still_standing_after_a_typed_line_is_never_typed_into_again_before_its_reset() {
    for (case, screens, sits, typed) in [
        (
            "te-tab-answered",
            vec![
                busy_screen(),
                ended("Still done, and I'm not starting new work."),
                limit_resets_5am(true),
            ],
            1,
            "continue@v1",
        ),
        (
            "te-tab-sitting",
            vec![limit_resets_5am(false), limit_resets_5am(true)],
            0,
            "usage-resume@v1",
        ),
    ] {
        let (m, lines, waits) = tab_run(case, screens, Some(sits), TAB_EVENING);
        let acts: Vec<&String> = lines
            .iter()
            .filter(|l| l.starts_with("CONTINUED"))
            .collect();
        assert!(
            acts.len() == 1 && acts[0].ends_with(&format!(" rule={typed} keep going")),
            "{case}: one act, nothing typed into the wall: {lines:#?}\n{waits:#?}"
        );
        assert_eq!(count(&m, "turn"), 1, "{case}: {:#?}", m.requests);
        assert!(
            waits
                .iter()
                .any(|w| w.starts_with("until=2026-09-18T12:02:30Z ") && w.contains("resets")),
            "{case}: tomorrow's 05:00 and its grace: {waits:#?}\n{lines:#?}"
        );
        assert!(
            !waits
                .iter()
                .any(|w| w.ends_with(" resets") && !w.starts_with("until=2026-09-18T12:02:30Z ")),
            "{case}: no rung of the ladder tonight (00:36Z, 17:36 PDT, before the fix): {waits:#?}"
        );
    }
}

#[test]
fn a_session_limit_whose_reset_has_passed_is_continued() {
    let session_wall = |typed: bool| {
        let mut r = rows(&[
            "⏺ Suites are running; I'll report the number when it lands.",
            "  ⎿  You've hit your session limit · resets 3pm (America/Los_Angeles)",
            "",
        ]);
        if typed {
            r.extend(rows(&[
                "❯ keep going",
                "  ⎿  You've hit your session limit · resets 3pm (America/Los_Angeles)",
                "",
            ]));
        }
        r.extend(composer("  ⏵⏵ bypass permissions on (shift+tab to cycle)"));
        r
    };
    let (m, lines, waits) = tab_run(
        "te-tab-session-passed",
        vec![session_wall(false), session_wall(true)],
        Some(0),
        TAB_EVENING,
    );
    let typed: Vec<&String> = lines
        .iter()
        .filter(|l| l.starts_with("CONTINUED"))
        .collect();
    assert_eq!(
        typed,
        ["CONTINUED seq=101 rule=usage-resume@v1 keep going"],
        "{lines:#?}\n{waits:#?}"
    );
    assert_eq!(count(&m, "turn"), 1, "{:#?}", m.requests);
    assert!(
        waits
            .iter()
            .any(|w| w == "until=2026-09-18T00:36:00Z usage-session resets"),
        "{waits:#?}"
    );
    assert!(
        !waits.iter().any(|w| w.starts_with("until=2026-09-18T22:")),
        "a session window's passed clock is never moved to tomorrow: {waits:#?}"
    );
}

#[test]
fn a_suggestion_on_a_grid_drawn_one_row_high_is_not_a_draft_at_a_wall() {
    let suggesting = || {
        let mut r = weekly_limited();
        let caret = r.iter().position(|row| row == "❯").expect("caret row");
        r[caret] = "❯ keep going, then merge when ready".to_string();
        (r, caret)
    };
    let run = |cursor_row_from_caret: Option<usize>,
               cursor_col: Option<usize>,
               attrs: &'static str| {
        let (screen, caret) = suggesting();
        let mut m = Mock::new(vec![busy_screen(), screen]);
        m.vanish_after = Some(3);
        m.cell_attrs = attrs;
        m.cursor_col = cursor_col;
        m.gen_fence = true;
        m.sends_gen = true;
        m.help = "key [id=<key>] [if=<re>] [if-gen=<e.s>] <name>: send a named key\n".to_string();
        match cursor_row_from_caret {
            Some(k) => m.cursor_row = Some(caret + k),
            None => m.cursor_on_caret = true,
        }
        let opts = SuperviseOpts {
            policy: SupervisorConfig {
                human_grace_s: 0,
                ..owners_policy()
            },
            ..resuming(&auto(30), None)
        };
        let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
            s.set_clock(AFTER_RESET, PDT);
            s.set_turn_end_timing(quick_turn_end(&[Duration::from_millis(10)]));
        });
        let enters = m.presses().iter().filter(|p| p.ends_with(" enter")).count();
        (count(&m, "turn"), enters, lines, m.requests)
    };
    for k in [None, Some(1)] {
        let (turns, enters, lines, requests) = run(k, None, "dim");
        assert_eq!(
            (turns, enters),
            (1, 0),
            "cursor row {k:?}: {lines:#?}\n{requests:#?}"
        );
    }
    let (turns, enters, lines, requests) = run(Some(1), None, "none");
    assert_eq!((turns, enters), (0, 1), "{lines:#?}\n{requests:#?}");
    for k in [None, Some(1)] {
        let (turns, enters, lines, requests) = run(k, Some(30), "dim");
        assert_eq!(
            (turns, enters),
            (0, 1),
            "cursor row {k:?}: {lines:#?}\n{requests:#?}"
        );
    }
}

fn memory_wall() -> Vec<String> {
    memory_wall_in("2s")
}

fn memory_wall_in(span: &str) -> Vec<String> {
    const WALL: &str = include_str!(
        "../../../aterm-phase/src/fixtures/claude-2.1.283-wall-weekly-goal-paused-shell-measured.txt"
    );
    aterm_phase::prompt::fixtures::screen(WALL)
        .into_iter()
        .map(|r| {
            r.replace(
                "resets 4pm (America/Los_Angeles)",
                &format!("resets in {span}"),
            )
        })
        .collect()
}

#[test]
fn a_turn_served_under_the_remembered_walls_notice_is_no_background_work() {
    let walled = memory_wall_in("2s")
        .into_iter()
        .map(|r| r.replace("resets in 2s", "resets 4pm (America/Los_Angeles)"))
        .collect::<Vec<_>>();
    let with_turn = |rows: &[String]| {
        let mut rows = rows.to_vec();
        let at = rows
            .iter()
            .position(|r| r.starts_with("✻ Sautéed"))
            .expect("the done row");
        for (i, row) in [
            "❯ what is left to do?",
            "",
            "⏺ Nothing is left: the suite is green.",
            "",
        ]
        .iter()
        .enumerate()
        {
            rows.insert(at + i, (*row).to_string());
        }
        rows
    };
    let turned = with_turn(&walled);
    let elsewhere: Vec<String> = turned.iter().map(|r| r.replace("4pm", "9pm")).collect();
    for rows in [&walled, &turned, &elsewhere] {
        assert!(aterm_phase::background_only(rows), "{rows:#?}");
    }
    let mut m = Mock::new(vec![walled.clone()]);
    let mut s = session(&mut m, Some("@s-1".to_string()));
    let reader = s.reader(&walled);
    let mut reading = reader.read(&walled, Some(2));
    assert!(reading.wall.is_some(), "{reading:#?}");
    s.wall_memory_step(reader, &walled, &mut reading, 101);
    assert!(s.remembered_wall(reader, &turned).is_none());
    assert!(s.remembered_wall(reader, &walled).is_some());
    assert!(s.remembered_wall(reader, &elsewhere).is_some());
}

fn memory_scrolled() -> Vec<String> {
    memory_scrolled_in("2s")
}

fn memory_scrolled_in(span: &str) -> Vec<String> {
    let mut rows = memory_wall_in(span);
    let from = rows
        .iter()
        .position(|r| r.starts_with("⏺ Dynamic workflow"))
        .unwrap();
    let to = rows
        .iter()
        .position(|r| r.starts_with("⏺ Goal paused"))
        .unwrap();
    rows.drain(from..=to);
    rows
}

fn memory_with_a_live_turn() -> Vec<String> {
    memory_with_a_live_turn_in("2s")
}

fn memory_with_a_live_turn_in(span: &str) -> Vec<String> {
    let mut rows = memory_wall_in(span);
    let foot = rows
        .iter()
        .position(|r| r.contains("bypass permissions"))
        .unwrap();
    rows[foot] = "  ⏵⏵ bypass permissions on · esc to interrupt · 1 shell".to_string();
    rows
}

const HANG_S: u64 = 60;

fn memory_run_for(
    tag: &str,
    screens: Vec<Vec<String>>,
    stop_s: u64,
    sid: Option<&str>,
    setup: impl FnOnce(&mut Session<'_, Mock>),
) -> (Mock, Vec<String>, Vec<String>, PathBuf) {
    memory_run_on(tag, screens, stop_s, sid, |_| {}, setup)
}

fn memory_run_on(
    tag: &str,
    screens: Vec<Vec<String>>,
    stop_s: u64,
    sid: Option<&str>,
    server: impl FnOnce(&mut Mock),
    setup: impl FnOnce(&mut Session<'_, Mock>),
) -> (Mock, Vec<String>, Vec<String>, PathBuf) {
    memory_run_at(
        tag,
        screens,
        stop_s,
        sid,
        &memory_run_clock(),
        server,
        setup,
    )
}

fn memory_run_clock() -> Arc<crate::supervise::fx::ScriptedClock> {
    let clock = Arc::new(crate::supervise::fx::ScriptedClock::default());
    clock.run_virtual();
    let now_ms = crate::supervise::journal::unix_ms();
    clock.set_at_ms(now_ms - now_ms.rem_euclid(1000));
    clock.set_running();
    clock
}

fn memory_run_at(
    tag: &str,
    screens: Vec<Vec<String>>,
    stop_s: u64,
    sid: Option<&str>,
    clock: &Arc<crate::supervise::fx::ScriptedClock>,
    server: impl FnOnce(&mut Mock),
    setup: impl FnOnce(&mut Session<'_, Mock>),
) -> (Mock, Vec<String>, Vec<String>, PathBuf) {
    let sits = screens.len() - 1;
    let (dir, path) = journal_file(tag);
    let mut m = Mock::new(screens);
    m.turn_gates = vec![sits];
    m.cell_attrs = "dim";
    m.cursor_on_caret = true;
    m.dead_after_turns = Some(0);
    m.stall_sleep = Some(Duration::from_millis(250));
    m.waits = Some(Arc::clone(clock));
    m.end_when_parked = true;
    server(&mut m);
    let opts = SuperviseOpts {
        journal: Some(path.clone()),
        ..hosted(HANG_S)
    };
    let mut out: Vec<u8> = Vec::new();
    let stop = Arc::new(AtomicBool::new(false));
    let (run, halt) = (Arc::clone(clock), Arc::clone(&stop));
    let until = run.mono() + Duration::from_secs(stop_s);
    let hang = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    m.after_call = Some(Box::new(move || {
        let due = run.mono() >= until;
        assert!(
            due || Instant::now() < hang,
            "timed out after {:?} waiting for the run's clock to reach its stop",
            aterm_spec::wait::MUST_HAPPEN
        );
        if due {
            halt.store(true, Ordering::SeqCst);
        }
    }));
    {
        let mut s = session(&mut m, sid.map(str::to_string));
        s.set_fx(Fx::over(ClockSource::Scripted(Arc::clone(clock))));
        s.stop = Some(Arc::clone(&stop));
        s.set_zone_lookup(test_zones);
        s.set_turn_end_timing(TurnEndTiming {
            reset_grace: Duration::from_secs(1),
            reset_jitter: Duration::ZERO,
            ..TurnEndTiming::default()
        });
        setup(&mut s);
        let _ = s.watch(&opts, &mut out);
    }
    m.after_call = None;
    let lines: Vec<String> = String::from_utf8(out)
        .expect("utf-8")
        .lines()
        .map(str::to_string)
        .collect();
    let (records, _) = journal_records(&path);
    let waits = records
        .iter()
        .filter(|r| r.kind == "waiting")
        .map(|r| r.line.clone())
        .collect();
    (m, lines, waits, dir)
}

#[test]
fn a_hidden_wall_its_predecessor_waited_on_is_seeded_only_while_it_stands() {
    use crate::supervise::approvals::{self, Outcome, Row};
    let notice = memory_wall_in("3s")
        .iter()
        .find(|r| r.contains("You've hit your weekly limit"))
        .expect("the notice row")
        .trim()
        .trim_start_matches('⎿')
        .trim()
        .replace('\u{a0}', " ");
    let row = |rule: &str, outcome: Outcome, reason: &str, ts_back_ms: i64| {
        Row {
            rule_id: rule,
            outcome,
            command: "",
            reason,
            box_seq: 102,
        }
        .to_json(
            crate::supervise::journal::unix_ms() - ts_back_ms,
            Some("@s-1"),
        )
    };
    let reset = crate::supervise::limit::unix_now() + 3;
    let waits = |reset: i64, ts_back_ms: i64| {
        row(
            approvals::RULE_WALL_WAIT,
            Outcome::Waiting,
            &approvals::wall_wait_reason(
                Some(reset),
                Some("in 3s"),
                aterm_phase::WallKind::UsageWeekly,
                &notice,
            ),
            ts_back_ms,
        )
    };
    for (name, rows, seeded) in [
        ("waited on", vec![waits(reset, 500)], true),
        (
            "acted on since",
            vec![
                waits(reset, 500),
                row("continue@v1", Outcome::Typed, "keep going", 300),
            ],
            false,
        ),
        (
            "past its horizon",
            vec![waits(reset - 2 * 86_400, 2 * 86_400_000)],
            false,
        ),
    ] {
        let (dir, ledger) = ledger_at(&format!("hidden-{}", name.replace(' ', "-")));
        std::fs::write(&ledger, rows.join("\n") + "\n").expect("the ledger");
        let mut m = Mock::new(vec![memory_scrolled_in("3s")]);
        let mut s = session(&mut m, Some("@s-1".to_string()));
        s.seed_wall_memory(approvals::open_wall(&ledger, Some("@s-1")));
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(s.wall_memory.is_some(), seeded, "{name}");
        if seeded {
            assert_eq!(
                s.wall_reset.as_ref().and_then(|(_, u)| *u),
                Some(reset),
                "{name}: the reset the predecessor placed"
            );
        }
    }
}

fn auto_continue_wall_in(span: &str) -> Vec<String> {
    aterm_phase::prompt::fixtures::screen(
        aterm_phase::prompt::fixtures::WALL_SESSION_AUTO_CONTINUE_ARMED_MEASURED,
    )
    .into_iter()
    .map(|r| {
        r.replace(
            "resets 3:15pm (America/Los_Angeles)",
            &format!("resets in {span}"),
        )
        .replace("limit resets 3:15pm", &format!("limit resets in {span}"))
    })
    .collect()
}

fn auto_continue_cancelled() -> Vec<String> {
    aterm_phase::prompt::fixtures::screen(
        aterm_phase::prompt::fixtures::WALL_SESSION_AUTO_CONTINUE_CANCELLED_MEASURED,
    )
}

#[test]
fn a_limit_whose_notice_left_is_waited_on_across_a_loop_restart() {
    let dir = crate::supervise::test_scratch_path("wm-ledger", "left-restart");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("dir");
    let ledger = dir.join("s-1.jsonl");
    let clock = memory_run_clock();
    let reset = clock.mono() + Duration::from_secs(7);
    let (m, lines, _, jdir) = memory_run_at(
        "left-restart-1",
        vec![
            busy_screen(),
            auto_continue_wall_in("7s"),
            auto_continue_cancelled(),
        ],
        2,
        Some("@s-1"),
        &clock,
        |_| {},
        |s| s.set_approval_ledger(Some(ledger.clone())),
    );
    let _ = std::fs::remove_dir_all(&jdir);
    let rows = std::fs::read_to_string(&ledger).unwrap_or_default();
    assert_eq!(m.turns, 0, "{lines:#?}\n{rows}");
    assert!(
        crate::supervise::approvals::open_wall(&ledger, Some("@s-1")).is_some(),
        "the wall is open for a loop that follows: {rows}"
    );
    let (m, lines, waits, jdir) = memory_run_at(
        "left-restart-2",
        vec![auto_continue_cancelled()],
        HANG_S,
        Some("@s-1"),
        &clock,
        |_| {},
        |s| {
            s.set_approval_ledger(Some(ledger.clone()));
            s.set_turn_end_timing(TurnEndTiming {
                reset_grace: Duration::from_secs(1),
                reset_jitter: Duration::ZERO,
                short_backoff: Duration::from_secs(1),
                ..TurnEndTiming::default()
            });
        },
    );
    let _ = std::fs::remove_dir_all(&jdir);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(m.turns, 1, "{lines:#?}\n{waits:#?}\n{:#?}", m.requests);
    assert!(clock.mono() >= reset, "not before the reset: {waits:#?}");
    assert!(
        !waits.is_empty() && waits.iter().all(|w| w.ends_with("usage-session resets")),
        "{waits:#?}"
    );
    assert!(
        m.requests
            .iter()
            .any(|r| r.contains(" turn submit=guarded:") && r.ends_with(" keep going")),
        "{:#?}",
        m.requests
    );
}

fn auto_continue_wall_at(clock: &str) -> Vec<String> {
    aterm_phase::prompt::fixtures::screen(
        aterm_phase::prompt::fixtures::WALL_SESSION_AUTO_CONTINUE_ARMED_MEASURED,
    )
    .into_iter()
    .map(|r| r.replace("3:15pm", clock))
    .collect()
}

fn cancelled_then_a_persons_turn() -> Vec<String> {
    aterm_phase::prompt::fixtures::screen(
        aterm_phase::prompt::fixtures::WALL_SESSION_CANCELLED_THEN_PERSON_TURN_MEASURED,
    )
}

#[test]
fn the_wait_on_a_left_limit_wakes_on_a_persons_short_turn() {
    let (m, lines, waits, jdir) = memory_run_on(
        "left-person-turn",
        vec![
            busy_screen(),
            auto_continue_wall_at("10:38am"),
            auto_continue_cancelled(),
            cancelled_then_a_persons_turn(),
        ],
        HANG_S,
        Some("@s-1"),
        |m| {
            m.sends_gen = true;
            m.agent_pushes = true;
        },
        |s| {
            s.set_clock(TEST_NOW, PDT);
            s.set_turn_end_timing(TurnEndTiming {
                reset_grace: Duration::from_secs(1),
                reset_jitter: Duration::ZERO,
                short_backoff: Duration::from_secs(1),
                ..TurnEndTiming::default()
            });
        },
    );
    let _ = std::fs::remove_dir_all(&jdir);
    assert!(
        m.requests
            .iter()
            .any(|r| r.contains(" await agent busy,prompt")),
        "the idle points are waited on by the verdict: {:#?}",
        m.requests
    );
    assert!(
        waits.iter().any(|w| w.contains("usage-session resets")),
        "the left limit's wait: {waits:#?}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("EVENT idle seq=") && l.contains("(reply 3)")),
        "the person's turn is a point: {lines:#?}"
    );
    assert!(
        waits.last().is_some_and(|w| w.contains("short turn")),
        "the continue policy's back-off: {waits:#?}"
    );
    assert_eq!(m.turns, 1, "{lines:#?}\n{waits:#?}\n{:#?}", m.requests);
    assert!(
        m.requests
            .iter()
            .any(|r| r.contains(" turn ") && r.ends_with(" keep going")),
        "{:#?}",
        m.requests
    );
}

fn armed_conversation_b() -> Vec<String> {
    aterm_phase::prompt::fixtures::screen(
        aterm_phase::prompt::fixtures::WALL_SESSION_ARMED_CONVERSATION_B_2_1_300,
    )
}

fn resumed_older_limit_continued() -> Vec<String> {
    aterm_phase::prompt::fixtures::screen(
        aterm_phase::prompt::fixtures::RESUMED_OLDER_LIMIT_CONTINUED_AT_ARMED_LIMIT_2_1_300,
    )
}

fn cleared_at_an_armed_limit() -> Vec<String> {
    aterm_phase::prompt::fixtures::screen(
        aterm_phase::prompt::fixtures::CLEARED_AT_ARMED_LIMIT_2_1_300,
    )
}

#[test]
fn a_resume_at_an_armed_limit_types_nothing_into_it() {
    let (m, lines, waits, jdir) = memory_run_for(
        "w1-resume-armed",
        vec![
            busy_screen(),
            armed_conversation_b(),
            resumed_older_limit_continued(),
        ],
        8,
        Some("@s-1"),
        |s| {
            s.set_clock(TEST_NOW, PDT);
            s.set_turn_end_timing(TurnEndTiming {
                reset_grace: Duration::from_secs(1),
                reset_jitter: Duration::ZERO,
                short_backoff: Duration::from_secs(1),
                ..TurnEndTiming::default()
            });
        },
    );
    let _ = std::fs::remove_dir_all(&jdir);
    assert_eq!(
        m.turns, 0,
        "nothing typed into the standing limit: {lines:#?}\n{waits:#?}\n{:#?}",
        m.requests
    );
    assert!(
        waits
            .last()
            .is_some_and(|w| w.contains("until=2026-09-17T16:") && w.contains("usage-session")),
        "B's limit's own wait: {waits:#?}"
    );
}

#[test]
fn a_resume_at_an_armed_limit_waits_on_it_as_a_cancelled_one() {
    let resumed = aterm_phase::prompt::fixtures::screen(
        aterm_phase::prompt::fixtures::RESUMED_NO_LIMIT_AT_ARMED_LIMIT_2_1_300,
    );
    let (m, lines, waits, jdir) = memory_run_for(
        "w4-resume-armed",
        vec![busy_screen(), armed_conversation_b(), resumed],
        5,
        Some("@s-1"),
        |s| s.set_clock(TEST_NOW, PDT),
    );
    let _ = std::fs::remove_dir_all(&jdir);
    assert_eq!(m.turns, 0, "{lines:#?}\n{waits:#?}\n{:#?}", m.requests);
    assert!(
        waits.last().is_some_and(
            |w| w.contains("until=2026-09-17T16:19:01Z") && w.ends_with("usage-session resets")
        ),
        "the reset and its grace, as a cancelled countdown: {waits:#?}"
    );
}

#[test]
fn a_resume_at_an_armed_bucket_types_nothing_into_it() {
    let armed = aterm_phase::prompt::fixtures::screen(
        aterm_phase::prompt::fixtures::WALL_SONNET_BUCKET_AUTO_CONTINUE_ARMED_2_1_300,
    );
    let resumed = aterm_phase::prompt::fixtures::screen(
        aterm_phase::prompt::fixtures::RESUMED_NO_LIMIT_AT_ARMED_LIMIT_2_1_300,
    );
    let (m, lines, waits, jdir) = memory_run_for(
        "bucket-resume-armed",
        vec![busy_screen(), armed, resumed],
        5,
        Some("@s-1"),
        |s| s.set_clock(TEST_NOW, PDT),
    );
    let _ = std::fs::remove_dir_all(&jdir);
    assert_eq!(m.turns, 0, "{lines:#?}\n{waits:#?}\n{:#?}", m.requests);
    assert!(
        waits.last().is_some_and(
            |w| w.contains("until=2026-10-17T23:20:01Z") && w.ends_with("model-bucket resets")
        ),
        "the bucket's reset and its grace, as a cancelled countdown: {waits:#?}"
    );
}

#[test]
fn a_bucket_whose_countdown_ended_is_waited_on_across_a_loop_restart() {
    let (ldir, ledger) = ledger_at("bucket-restart");
    let armed = aterm_phase::prompt::fixtures::screen(
        aterm_phase::prompt::fixtures::WALL_SONNET_BUCKET_AUTO_CONTINUE_ARMED_2_1_300,
    );
    let resumed = || {
        aterm_phase::prompt::fixtures::screen(
            aterm_phase::prompt::fixtures::RESUMED_NO_LIMIT_AT_ARMED_LIMIT_2_1_300,
        )
    };
    let (m, lines, waits, jdir) = memory_run_for(
        "bucket-restart-1",
        vec![busy_screen(), armed, resumed()],
        4,
        Some("@s-1"),
        |s| {
            s.set_clock(TEST_NOW, PDT);
            s.set_approval_ledger(Some(ledger.clone()));
        },
    );
    let _ = std::fs::remove_dir_all(&jdir);
    let rows = std::fs::read_to_string(&ledger).unwrap_or_default();
    assert_eq!(m.turns, 0, "{lines:#?}\n{waits:#?}\n{rows}");
    assert!(
        crate::supervise::approvals::open_wall(&ledger, Some("@s-1")).is_some(),
        "the bucket's wall is open for a loop that follows: {rows}"
    );
    let (m, lines, waits, jdir) =
        memory_run_for("bucket-restart-2", vec![resumed()], 4, Some("@s-1"), |s| {
            s.set_clock(TEST_NOW, PDT);
            s.set_approval_ledger(Some(ledger.clone()));
            s.set_turn_end_timing(TurnEndTiming {
                reset_grace: Duration::from_secs(1),
                reset_jitter: Duration::ZERO,
                short_backoff: Duration::from_secs(1),
                ..TurnEndTiming::default()
            });
        });
    let _ = std::fs::remove_dir_all(&jdir);
    let _ = std::fs::remove_dir_all(&ldir);
    assert_eq!(m.turns, 0, "{lines:#?}\n{waits:#?}\n{:#?}", m.requests);
    assert!(
        waits.last().is_some_and(
            |w| w.contains("until=2026-10-17T23:20:01Z") && w.ends_with("model-bucket resets")
        ),
        "the bucket's own wait, taken up: {waits:#?}"
    );
}

#[test]
fn a_clear_at_an_armed_limit_closes_the_ledgers_wall_and_says_why() {
    let (ldir, ledger) = ledger_at("w4-clear");
    let (m, lines, waits, jdir) = memory_run_for(
        "w4-clear-armed",
        vec![
            busy_screen(),
            armed_conversation_b(),
            cleared_at_an_armed_limit(),
        ],
        6,
        Some("@s-1"),
        |s| {
            s.set_clock(TEST_NOW, PDT);
            s.set_approval_ledger(Some(ledger.clone()));
        },
    );
    let _ = std::fs::remove_dir_all(&jdir);
    let rows = std::fs::read_to_string(&ledger).unwrap_or_default();
    let _ = std::fs::remove_dir_all(&ldir);
    assert_eq!(m.turns, 0, "{lines:#?}\n{waits:#?}\n{:#?}", m.requests);
    assert!(
        rows.contains("\"rule_id\":\"wall-wait@v1\""),
        "the wall was waited on: {rows}"
    );
    let end = rows
        .lines()
        .filter(|r| r.contains("\"rule_id\":\"wall-wait-end@v1\""))
        .collect::<Vec<_>>();
    assert!(
        end.len() == 1 && end[0].contains("no task"),
        "the wall closed once, saying why: {rows}"
    );
    assert!(
        crate::supervise::approvals::open_wall(&ledger, Some("@s-1")).is_none(),
        "no wall open for a loop that follows: {rows}"
    );
}

fn own_act_met_a_limit(clock: &str) -> Vec<String> {
    auto_continue_wall_at(clock)
        .into_iter()
        .map(|r| r.replace("❯ carry on with the task", "❯ keep going"))
        .collect()
}

fn own_act_limit_cancelled(clock: &str) -> Vec<String> {
    auto_continue_cancelled()
        .into_iter()
        .map(|r| {
            r.replace("3:50pm", clock)
                .replace("❯ carry on with the task", "❯ keep going")
        })
        .collect()
}

#[test]
fn a_limit_whose_notice_left_before_it_was_judged_is_waited_on_to_its_reset() {
    let act = || {
        vec![
            busy_screen(),
            ended("Fixed the parser; the suite is green."),
        ]
    };
    let with = |mut screens: Vec<Vec<String>>, more: &[Vec<String>]| {
        screens.extend(more.iter().cloned());
        screens
    };
    for (case, screens, held, acted, event) in [
        (
            "takewin",
            with(
                act(),
                &[
                    own_act_met_a_limit("10:38am"),
                    own_act_limit_cancelled("10:38am"),
                ],
            ),
            Some(1),
            true,
            None,
        ),
        (
            "leftesc",
            with(act(), &[own_act_limit_cancelled("10:38am")]),
            Some(1),
            true,
            Some("EVENT limited seq=103 "),
        ),
        (
            "personcancel",
            vec![
                ended("Fixed the parser; the suite is green."),
                person_limit_cancelled("10:38am"),
            ],
            None,
            false,
            Some("EVENT limited "),
        ),
    ] {
        let (lines, waits, typed, rows, m) = left_run(case, screens, held);
        let want: &[&str] = if acted {
            &["CONTINUED seq=102 rule=continue@v1 keep going"]
        } else {
            &[]
        };
        assert_eq!(
            typed, want,
            "{case}: the act, and nothing typed into the limit: {lines:#?}\n{waits:#?}"
        );
        assert_eq!(
            count(&m, "turn"),
            usize::from(acted),
            "{case}: {:#?}",
            m.requests
        );
        if acted {
            assert!(
                waits
                    .iter()
                    .any(|w| w.contains("the worker to take the continue@v1 act")),
                "{case}: the act's own window still stands: {waits:#?}"
            );
        }
        if let Some(event) = event {
            assert!(
                lines.iter().any(|l| l.starts_with(event)
                    && l.contains("Usage limit reached")
                    && l.ends_with(" reset=10:38am")),
                "{case}: {lines:#?}"
            );
        }
        assert!(
            waits
                .last()
                .is_some_and(|w| w.starts_with("until=2026-09-17T17:38:0")
                    && w.ends_with("usage-session resets")),
            "{case}: the reset the notice named and its grace: {waits:#?}\n{lines:#?}"
        );
        assert!(
            rows.contains("\"rule_id\":\"wall-wait@v1\"")
                && !rows.contains("\"rule_id\":\"wall-wait-end@v1\""),
            "{case}: the wall is open in the ledger: {rows}"
        );
    }
}

#[test]
fn a_resume_the_limit_answers_inside_its_window_is_waited_on_to_the_new_reset() {
    let (dir, journal) = journal_file("te-takewin-retry");
    let (ldir, ledger) = ledger_at("takewin-retry");
    let kept = |r: String| r.replace("❯ carry on with the task", "❯ keep going");
    let armed =
        |span: &str| -> Vec<String> { auto_continue_wall_in(span).into_iter().map(kept).collect() };
    let cancelled = |span: &str| -> Vec<String> {
        auto_continue_cancelled()
            .into_iter()
            .map(|r| {
                kept(r)
                    .replace(
                        "resets 3:50pm (America/Los_Angeles)",
                        &format!("resets in {span}"),
                    )
                    .replace(
                        "resets 3:35pm (America/Los_Angeles)",
                        &format!("resets in {span}"),
                    )
            })
            .collect()
    };
    let mut m = Mock::new(vec![
        busy_screen(),
        ended("Fixed the parser; the suite is green."),
        armed("2s"),
        cancelled("2s"),
        armed("1h"),
        cancelled("1h"),
    ]);
    m.turn_gates = vec![1, 3];
    m.vanish_after = Some(100);
    m.stall_sleep = Some(Duration::from_millis(100));
    m.end_when_parked = true;
    let opts = SuperviseOpts {
        journal: Some(journal.clone()),
        ..hosted(30)
    };
    let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
        s.set_approval_ledger(Some(ledger.clone()));
        s.set_turn_end_timing(TurnEndTiming {
            take_within: Duration::from_secs(1),
            short_backoff: Duration::from_millis(100),
            reset_grace: Duration::from_secs(1),
            reset_jitter: Duration::ZERO,
            ..TurnEndTiming::default()
        });
    });
    let (records, _) = journal_records(&journal);
    let rows = std::fs::read_to_string(&ledger).unwrap_or_default();
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&ldir);
    let waits: Vec<&str> = records
        .iter()
        .filter(|r| r.kind == "waiting")
        .map(|r| r.summary.as_str())
        .collect();
    let typed: Vec<&String> = lines
        .iter()
        .filter(|l| l.starts_with("CONTINUED"))
        .collect();
    assert_eq!(
        typed,
        [
            "CONTINUED seq=102 rule=continue@v1 keep going",
            "CONTINUED seq=104 rule=usage-resume@v1 keep going",
        ],
        "nothing typed into the limit that answered the resume: {lines:#?}\n{waits:#?}"
    );
    assert!(
        waits
            .iter()
            .any(|w| w.contains("the worker to take the usage-resume@v1 act")),
        "the resume's own window: {waits:#?}"
    );
    assert!(
        waits
            .last()
            .is_some_and(|w| w.starts_with("until=2026-09-17T16:55:0")
                && w.ends_with("usage-session resets")),
        "the reset the answer named and its grace: {waits:#?}\n{lines:#?}"
    );
    assert!(
        !rows.contains("\"rule_id\":\"wall-wait-end@v1\""),
        "the wall is open in the ledger: {rows}"
    );
}

fn person_limit_cancelled(clock: &str) -> Vec<String> {
    auto_continue_cancelled()
        .into_iter()
        .map(|r| r.replace("3:50pm", clock))
        .collect()
}

fn left_run(
    tag: &str,
    screens: Vec<Vec<String>>,
    held: Option<usize>,
) -> (Vec<String>, Vec<String>, Vec<String>, String, Mock) {
    let (dir, journal) = journal_file(&format!("te-{tag}"));
    let (ldir, ledger) = ledger_at(tag);
    let mut m = Mock::new(screens);
    m.turn_releases = held;
    m.vanish_after = Some(60);
    m.stall_sleep = Some(Duration::from_millis(100));
    m.end_when_parked = true;
    let opts = SuperviseOpts {
        journal: Some(journal.clone()),
        ..hosted(30)
    };
    let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
        s.set_approval_ledger(Some(ledger.clone()));
        s.set_turn_end_timing(TurnEndTiming {
            take_within: Duration::from_secs(1),
            short_backoff: Duration::from_millis(100),
            reset_grace: Duration::from_secs(1),
            reset_jitter: Duration::ZERO,
            ..TurnEndTiming::default()
        });
    });
    let (records, _) = journal_records(&journal);
    let rows = std::fs::read_to_string(&ledger).unwrap_or_default();
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&ldir);
    let waits = records
        .iter()
        .filter(|r| r.kind == "waiting")
        .map(|r| r.summary.clone())
        .collect();
    let typed = lines
        .iter()
        .filter(|l| l.starts_with("CONTINUED"))
        .cloned()
        .collect();
    (lines, waits, typed, rows, m)
}

#[test]
fn an_older_cancelled_limit_resumed_inside_the_acts_window_is_no_days_wait() {
    let (lines, waits, typed, _, _) = left_run(
        "resumedold",
        vec![
            busy_screen(),
            ended("Fixed the parser; the suite is green."),
            person_limit_cancelled("7:30am"),
        ],
        Some(1),
    );
    assert!(
        !waits.iter().any(|w| w.starts_with("until=2026-09-18")),
        "no wait for tomorrow's 7:30: {waits:#?}\n{lines:#?}"
    );
    assert!(
        typed.len() >= 2
            && typed[0].ends_with(" rule=continue@v1 keep going")
            && typed[1].ends_with(" rule=usage-resume@v1 keep going"),
        "the act, then the continuation at the passed reset's grace: {lines:#?}\n{waits:#?}"
    );
}

fn weekly_met_by_own_act(clock: &str) -> Vec<String> {
    auto_continue_wall_at(clock)
        .into_iter()
        .map(|r| {
            r.replace("❯ carry on with the task", "❯ keep going")
                .replace("session limit", "weekly limit")
        })
        .collect()
}

fn weekly_met_by_own_act_cancelled(clock: &str) -> Vec<String> {
    auto_continue_cancelled()
        .into_iter()
        .map(|r| {
            r.replace("3:50pm", clock)
                .replace("3:35pm", clock)
                .replace("❯ carry on with the task", "❯ keep going")
                .replace("session limit", "weekly limit")
        })
        .collect()
}

#[test]
fn a_wall_read_inside_the_own_acts_window_is_placed_on_its_print() {
    let (dir, journal) = journal_file("te-takewin-placed");
    let mut m = Mock::new(vec![
        busy_screen(),
        ended("Fixed the parser; the suite is green."),
        weekly_met_by_own_act("6am"),
        weekly_met_by_own_act_cancelled("6am"),
    ]);
    m.turn_gates = vec![1];
    m.vanish_after = Some(60);
    m.stall_sleep = Some(Duration::from_millis(100));
    m.end_when_parked = true;
    let opts = SuperviseOpts {
        journal: Some(journal.clone()),
        ..hosted(30)
    };
    let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
        s.set_turn_end_timing(TurnEndTiming {
            take_within: Duration::from_secs(1),
            short_backoff: Duration::from_millis(100),
            reset_grace: Duration::from_secs(1),
            reset_jitter: Duration::ZERO,
            ..TurnEndTiming::default()
        });
    });
    let (records, _) = journal_records(&journal);
    let _ = std::fs::remove_dir_all(&dir);
    let waits: Vec<&str> = records
        .iter()
        .filter(|r| r.kind == "waiting")
        .map(|r| r.summary.as_str())
        .collect();
    let typed: Vec<&String> = lines
        .iter()
        .filter(|l| l.starts_with("CONTINUED"))
        .collect();
    assert_eq!(
        typed,
        ["CONTINUED seq=102 rule=continue@v1 keep going"],
        "the act the limit answered, and nothing typed into it: {lines:#?}\n{waits:#?}"
    );
    assert!(
        waits
            .last()
            .is_some_and(|w| w.starts_with("until=2026-09-18T13:00:0") && w.ends_with(" resets")),
        "tomorrow's 06:00 and its grace: {waits:#?}\n{lines:#?}"
    );
}

fn login_left() -> Vec<String> {
    let mut r = login_expired();
    let at = r
        .iter()
        .position(|row| row.starts_with("⏺ Login expired"))
        .expect("the wall row");
    r.splice(
        at + 1..at + 1,
        rows(&["", "❯ /model", "  ⎿  Kept model as Opus 4.7"]),
    );
    r
}

#[test]
fn a_lost_login_left_inside_the_own_acts_window_types_login() {
    let (dir, journal) = journal_file("te-takewin-login");
    let mut m = Mock::new(vec![
        busy_screen(),
        ended("Fixed the parser; the suite is green."),
        login_expired(),
        login_left(),
    ]);
    m.turn_gates = vec![1];
    m.vanish_after = Some(40);
    m.stall_sleep = Some(Duration::from_millis(100));
    m.end_when_parked = true;
    let opts = SuperviseOpts {
        journal: Some(journal.clone()),
        ..hosted(30)
    };
    let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
        s.set_turn_end_timing(TurnEndTiming {
            take_within: Duration::from_secs(1),
            short_backoff: Duration::from_millis(100),
            ..TurnEndTiming::default()
        });
    });
    let (records, _) = journal_records(&journal);
    let _ = std::fs::remove_dir_all(&dir);
    let decided: Vec<&String> = lines
        .iter()
        .filter(|l| l.starts_with("TYPED") || l.starts_with("CONTINUED"))
        .collect();
    assert_eq!(
        decided.len(),
        2,
        "the continuation, then `/login` once: {lines:#?}\n{:#?}",
        m.requests
    );
    assert_eq!(decided[0], "CONTINUED seq=102 rule=continue@v1 keep going");
    assert!(
        decided[1].starts_with("TYPED seq=") && decided[1].ends_with("rule=auth-login@v1 /login"),
        "{lines:#?}"
    );
    assert!(
        !m.requests
            .iter()
            .any(|r| r.starts_with("meta set attention")),
        "no badge beside the lost login's notice: {:#?}",
        m.requests
    );
    assert!(
        !records.iter().any(|r| r.kind == "escalated"),
        "{records:#?}"
    );
}

#[test]
fn a_reprinted_wall_is_not_sent_to_tomorrow_at_once() {
    let at = |h: i64, m: i64| TEST_NOW + (h - 8) * 3600 + (m - 55) * 60;
    let four = "4pm (America/Los_Angeles)";
    let frame = |done: &str| {
        let mut r = rows(&[
            "⏺ Working.",
            "",
            &format!("✻ Worked for 3m 2s · done {done}"),
            "",
        ]);
        r.extend(composer("  ⏵⏵ bypass permissions on"));
        r
    };
    let mut m = Mock::new(vec![busy_screen()]);
    let mut s = session(&mut m, None);
    s.set_zone_lookup(test_zones);
    let reset_after = |s: &mut Session<'_, Mock>, h: i64, mi: i64, clock: &str, text: &str| {
        s.set_clock(at(h, mi), PDT);
        let _ = s.wall_reset_at(
            Some(text),
            aterm_phase::WallKind::UsageWeekly,
            false,
            true,
            false,
            false,
            &frame(clock),
        );
        let now = s.now_unix();
        s.wall_reset
            .as_ref()
            .and_then(|(_, u)| *u)
            .map(|u| (u - now) / 60)
    };
    assert_eq!(reset_after(&mut s, 16, 3, "4:02 PM", four), Some(-3));
    for (h, mi, done) in [
        (16, 13, "4:13 PM"),
        (16, 43, "4:43 PM"),
        (17, 13, "5:13 PM"),
        (17, 43, "5:43 PM"),
    ] {
        let got = reset_after(&mut s, h, mi, done, four).expect("a placement");
        assert!(
            got < 0,
            "{h}:{mi:02}: held at the first placement, {got} min from now"
        );
    }
    assert_eq!(s.reprint_jumps, super::turn_end_loop::REPRINT_JUMPS_MAX);
    let got = reset_after(&mut s, 18, 13, "6:13 PM", four).expect("a placement");
    assert!(got > 21 * 60, "believed: {got} min");
    let mut s = session(&mut m, None);
    s.set_zone_lookup(test_zones);
    assert_eq!(reset_after(&mut s, 16, 3, "4:02 PM", four), Some(-3));
    let got =
        reset_after(&mut s, 16, 13, "4:13 PM", "5pm (America/Los_Angeles)").expect("a placement");
    assert!(
        (45..=47).contains(&got),
        "a different clock is placed on its own print: {got}"
    );
}

#[test]
fn the_wall_cache_is_dropped_when_the_worker_has_worked() {
    let mut m = Mock::new(vec![busy_screen()]);
    let mut s = session(&mut m, None);
    s.set_zone_lookup(test_zones);
    let opts = hosted(30);
    let of = |rows: Vec<String>| Screen {
        rows,
        cursor_row: 0,
        cursor_col: 2,
        seq: 1,
        first: 0,
        generation: None,
        human: crate::supervise::screen::HumanInput::Unknown,
        human_seq: None,
        cols: None,
        pane_rows: None,
        launch: None,
        submits: None,
        frame_open: false,
        inputs_before: None,
        inputs_after: None,
    };
    let wall = of(memory_wall_in("3h"));
    let r = s.turn_end_reading(&wall, None, &opts);
    assert!(r.wall.is_some() && r.reset_at.is_some(), "the wall is read");
    assert!(s.wall_reset.is_some(), "and its reset is cached");
    let mut idle = rows(&["⏺ All done.", "", "✻ Worked for 3m 2s · done 8:00 PM", ""]);
    idle.extend(composer("  ⏵⏵ bypass permissions on (shift+tab to cycle)"));
    let gone = of(idle);
    let _ = s.turn_end_reading(&gone, None, &opts);
    assert!(
        s.wall_reset.is_some(),
        "no wall on the screen but no work either: the cache stays"
    );
    let _ = s.turn_end_reading(&gone, Some(Duration::from_secs(600)), &opts);
    assert!(s.wall_reset.is_none(), "the worker worked: dropped");
    assert_eq!(s.reprint_jumps, 0);
}

#[test]
fn the_wakes_settle_is_spread_by_the_sessions_share() {
    let past = TEST_NOW + 8 * 3600;
    let mut seconds = std::collections::BTreeSet::new();
    for k in 0..20 {
        let mut m = Mock::new(vec![busy_screen()]);
        let mut s = session(&mut m, Some(format!("s-{k}")));
        s.set_clock(TEST_NOW, PDT);
        s.set_due(
            Instant::now() + Duration::from_secs(3 * 3600),
            "usage-weekly resets",
        );
        let before = Instant::now();
        s.settle_wake(past, Instant::now());
        s.woke(102, past, &mut super::super::Alone);
        let due = s.turn_end_due.expect("the settle's wait");
        assert_eq!(due.why, "the Mac woke");
        seconds.insert(due.at.saturating_duration_since(before).as_secs());
    }
    assert!(seconds.len() >= 8, "{seconds:?}");
}

fn heard_lines(heard: &Heard) -> Vec<String> {
    heard.said.iter().chain(&heard.noted).cloned().collect()
}

#[test]
fn a_sleep_between_two_steps_of_the_wait_is_seen_by_the_wait_itself() {
    use super::super::turn_end_loop::{WAKE_SETTLE, WAKE_SPREAD};
    let run = |step: i64, standing: bool| {
        let mut m = Mock::new(vec![busy_screen()]);
        let mut s = session(&mut m, Some("@s-1".to_string()));
        s.set_clock(TEST_NOW, PDT);
        if standing {
            s.set_due(
                Instant::now() + Duration::from_secs(3 * 3600),
                "usage-weekly resets",
            );
        }
        s.set_clock_jump(step);
        let seen = Turn {
            phase: Phase::Idle,
            screen: Screen {
                rows: ended("Stage done."),
                seq: 102,
                ..Screen::default()
            },
            timed_out: false,
        };
        let began = Instant::now();
        let mut deadline = began + Duration::from_millis(600);
        let mut review = Heard::default();
        let end = s.wait_for_next(seen, &hosted(30), &[], &mut deadline, &mut review);
        let took = began.elapsed();
        let lines = heard_lines(&review);
        assert!(end.is_ok(), "step {step}: {lines:#?}");
        let due = s
            .turn_end_due
            .clone()
            .map(|d| (d.at.saturating_duration_since(began), d.why));
        (lines, due, took)
    };
    let (lines, due, took) = run(4 * 3600, true);
    assert!(
        lines.iter().any(|l| l.starts_with("WAITING seq=102 until=")
            && l.ends_with("the Mac woke: deciding again")),
        "{lines:#?}"
    );
    let (after, why) = due.expect("a wait stands");
    assert_eq!(why, "the Mac woke");
    assert!(
        after >= WAKE_SETTLE && after < WAKE_SETTLE + WAKE_SPREAD + took,
        "the far deadline is replaced by a settle: {after:?} (the wait took {took:?})"
    );
    for step in [0, 20, -30] {
        let (lines, due, _) = run(step, true);
        assert!(
            !lines.iter().any(|l| l.contains("the Mac woke")),
            "step {step}: {lines:#?}"
        );
        assert!(due.expect("still standing").0 > Duration::from_secs(3 * 3600 - 5));
    }
    let (lines, due, _) = run(4 * 3600, false);
    assert!(lines.is_empty() && due.is_none(), "{lines:#?} {due:?}");
}

#[test]
fn a_clock_stepped_forward_is_no_sleep_where_the_machine_counts() {
    let run = |counts: bool, slept: bool| {
        let mut m = Mock::new(vec![busy_screen()]);
        let mut s = session(&mut m, Some("@s-1".to_string()));
        s.set_clock(TEST_NOW, PDT);
        if counts {
            s.count_sleeps(Duration::from_secs(600));
        }
        s.set_due(
            Instant::now() + Duration::from_secs(3 * 3600),
            "usage-weekly resets",
        );
        if slept {
            s.set_clock_jump(4 * 3600);
        } else {
            s.set_clock_step(4 * 3600);
        }
        let seen = Turn {
            phase: Phase::Idle,
            screen: Screen {
                rows: ended("Stage done."),
                seq: 102,
                ..Screen::default()
            },
            timed_out: false,
        };
        let mut deadline = Instant::now() + Duration::from_millis(600);
        let mut review = Heard::default();
        let end = s.wait_for_next(seen, &hosted(30), &[], &mut deadline, &mut review);
        assert!(end.is_ok(), "{:#?}", heard_lines(&review));
        let woke = heard_lines(&review)
            .iter()
            .any(|l| l.contains("the Mac woke"));
        let book: Vec<Duration> = s.sleep_book().sleeps().map(|z| z.len).collect();
        (woke, book)
    };
    let four = Duration::from_secs(4 * 3600);
    assert_eq!(run(true, true), (true, vec![four]), "a sleep, counted");
    assert_eq!(run(true, false), (false, Vec::new()), "a step, counted");
    assert_eq!(run(false, false), (true, vec![four]), "CONTROL: no count");
}

#[test]
fn a_recorded_wait_sees_the_sleep_an_unrecorded_one_sees() {
    let run = |step: i64, recorded: bool| {
        let mut m = Mock::new(vec![busy_screen()]);
        let tape = memory_tape_clock();
        let door = if recorded { tape.fx.clone() } else { Fx::os() };
        let mut taped = TapedCtl::new(&mut m, door, crate::supervise::tape::doors::CTL);
        let mut s = session(&mut taped, Some("@s-1".to_string()));
        if recorded {
            s.set_fx(tape.fx.clone());
        }
        s.set_clock(TEST_NOW, PDT);
        s.set_due(
            Instant::now() + Duration::from_secs(3 * 3600),
            "usage-weekly resets",
        );
        s.set_clock_jump(step);
        let seen = Turn {
            phase: Phase::Idle,
            screen: Screen {
                rows: ended("Stage done."),
                seq: 102,
                ..Screen::default()
            },
            timed_out: false,
        };
        let mut deadline = Instant::now() + Duration::from_millis(600);
        let mut review = Heard::default();
        let end = s.wait_for_next(seen, &hosted(30), &[], &mut deadline, &mut review);
        assert!(end.is_ok(), "step {step}");
        let due = s.turn_end_due.clone().map(|d| d.why);
        let lines: Vec<String> = heard_lines(&review)
            .into_iter()
            .map(|l| {
                l.split(' ')
                    .map(|w| {
                        if w.starts_with("until=") {
                            "until=…"
                        } else {
                            w
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect();
        drop(s);
        (lines, due)
    };
    for step in [4 * 3600, 0, 20, -30] {
        let (off_lines, off_due) = run(step, false);
        let (on_lines, on_due) = run(step, true);
        assert_eq!(on_lines, off_lines, "step {step}");
        assert_eq!(on_due, off_due, "step {step}");
        assert_eq!(
            on_due.as_deref() == Some("the Mac woke"),
            step == 4 * 3600,
            "step {step}: {on_lines:#?}"
        );
    }
}

fn memory_tape_clock() -> MemoryTape {
    MemoryTape::over(&Fx::over(ClockSource::Scripted(Arc::default())))
}

#[test]
fn a_due_the_sleep_fell_short_of_falls_at_its_wall_time() {
    let seen = |seq: u64| Turn {
        phase: Phase::Idle,
        screen: Screen {
            rows: ended("Stage done."),
            seq,
            ..Screen::default()
        },
        timed_out: false,
    };
    let run = |slept: i64| {
        let mut m = Mock::new(vec![busy_screen()]);
        let mut s = session(&mut m, Some("@s-1".to_string()));
        s.set_clock(TEST_NOW, PDT);
        s.set_due(
            Instant::now() + Duration::from_secs(3 * 3600),
            "usage-weekly resets",
        );
        let mut review = Heard::default();
        let mut deadline = Instant::now() + Duration::from_millis(600);
        let before = s.wait_for_next(seen(102), &hosted(30), &[], &mut deadline, &mut review);
        assert!(before.is_ok(), "{:#?}", heard_lines(&review));
        s.set_clock(TEST_NOW + slept, PDT);
        let mut deadline = Instant::now() + Duration::from_millis(600);
        let after = s.wait_for_next(seen(103), &hosted(30), &[], &mut deadline, &mut review);
        assert!(after.is_ok(), "{:#?}", heard_lines(&review));
        let left = s
            .due_at()
            .map(|t| t.saturating_duration_since(Instant::now()));
        (s.turn_end_due.clone().map(|d| d.why), left)
    };
    let (why, left) = run(2 * 3600);
    assert_eq!(why.as_deref(), Some("usage-weekly resets"));
    let left = left.expect("a due");
    assert!(
        left > Duration::from_secs(3600 - 60) && left <= Duration::from_secs(3600),
        "an hour from the wake: {left:?}"
    );
    let (why, left) = run(0);
    assert_eq!(why.as_deref(), Some("usage-weekly resets"));
    let left = left.expect("a due");
    assert!(left > Duration::from_secs(3 * 3600 - 60), "{left:?}");
}

struct Waited {
    lines: Vec<String>,
    due: Option<String>,
}

fn wait_once(
    s: &mut Session<'_, Mock>,
    point: &Turn,
    opts: &SuperviseOpts,
    heard: &mut Heard,
) -> Waited {
    let (said, noted) = (heard.said.len(), heard.noted.len());
    let mut deadline = s.fx.mono() + Duration::from_millis(600);
    let end = s.wait_for_next(point.clone(), opts, &[], &mut deadline, heard);
    assert!(end.is_ok(), "{:#?}", heard_lines(heard));
    Waited {
        lines: heard.said[said..]
            .iter()
            .chain(&heard.noted[noted..])
            .cloned()
            .collect(),
        due: s.turn_end_due.as_ref().map(|d| d.why.clone()),
    }
}

fn point_at(s: &Session<'_, Mock>, rows: Vec<String>, seq: u64) -> Turn {
    s.turn_of(Screen {
        rows,
        seq,
        ..Screen::default()
    })
}

fn continued(lines: &[String], rule: &str) -> bool {
    lines
        .iter()
        .any(|l| l.starts_with("CONTINUED seq=") && l.contains(&format!("rule={rule}")))
}

#[test]
fn a_reset_the_sleep_carried_past_waits_for_the_settle_though_a_repaint_wakes_the_loop() {
    use crate::supervise::policy::turn_end::RULE_LIMIT_RESUME;
    let run = |slept: bool| {
        let mut m = Mock::new(vec![weekly_limited()]);
        let opts = hosted(30);
        let mut heard = Heard::default();
        let mut s = virtual_session(&mut m, Some("@s-1".to_string()));
        s.set_clock(TEST_NOW, PDT);
        s.set_zone_lookup(test_zones);
        let at = |s: &Session<'_, Mock>, seq| point_at(s, weekly_limited(), seq);
        let first = at(&s, 102);
        let action = s
            .turn_end_at_point(&first, Some(Duration::from_secs(180)), &opts)
            .expect("a decision");
        s.turn_end_execute(&first, action, &opts, &[], &mut heard)
            .expect("the wait is set");
        let before = wait_once(&mut s, &first, &opts, &mut heard);
        assert!(
            !continued(&before.lines, RULE_LIMIT_RESUME),
            "{:#?}",
            before.lines
        );
        if slept {
            s.set_clock(AFTER_RESET + 11 * 3600, PDT);
        }
        let repainted = at(&s, 103);
        let woke = wait_once(&mut s, &repainted, &opts, &mut heard);
        s.pass_wake_settle();
        let again = at(&s, 104);
        let settled = wait_once(&mut s, &again, &opts, &mut heard);
        drop(s);
        let typed = count(&m, "@s-1 turn") + count(&m, "@s-1 send");
        (woke, settled, typed)
    };
    let (woke, settled, typed) = run(true);
    assert!(
        !continued(&woke.lines, RULE_LIMIT_RESUME),
        "nothing is typed at the wake's first wait: {:#?}",
        woke.lines
    );
    assert!(
        woke.lines
            .iter()
            .any(|l| l.starts_with("WAITING seq=103 until=")
                && l.ends_with("the Mac woke: deciding again")),
        "{:#?}",
        woke.lines
    );
    assert_eq!(woke.due.as_deref(), Some("the Mac woke"));
    assert!(
        continued(&settled.lines, RULE_LIMIT_RESUME),
        "continued once the settle has passed: {:#?}",
        settled.lines
    );
    assert_eq!(typed, 1);
    let (woke, settled, typed) = run(false);
    let lines: Vec<&String> = woke.lines.iter().chain(&settled.lines).collect();
    assert!(
        !lines
            .iter()
            .any(|l| l.contains("the Mac woke") || l.starts_with("CONTINUED")),
        "{lines:#?}"
    );
    assert_eq!(settled.due.as_deref(), Some("usage-weekly resets"));
    assert_eq!(typed, 0);
}

#[test]
fn a_back_off_the_mac_slept_through_is_over_once_the_wake_settles() {
    use crate::supervise::policy::turn_end::RULE_API_RETRY;
    let end_529 = || aterm_phase::prompt::fixtures::screen(aterm_phase::prompt::fixtures::END_529);
    let run = |slept: bool| {
        let mut m = Mock::new(vec![end_529()]);
        let opts = hosted(30);
        let mut heard = Heard::default();
        let mut s = virtual_session(&mut m, Some("@s-1".to_string()));
        s.set_clock(TEST_NOW, PDT);
        s.set_turn_end_timing(TurnEndTiming {
            retry_backoff: vec![Duration::from_secs(3600)],
            ..TurnEndTiming::default()
        });
        let point = point_at(&s, end_529(), 102);
        let action = s
            .turn_end_at_point(&point, Some(Duration::from_secs(180)), &opts)
            .expect("a decision");
        assert!(
            matches!(&action, TurnEndAction::WaitUntil { why, .. } if why == "overloaded retry 1"),
            "{action:?}"
        );
        s.turn_end_execute(&point, action, &opts, &[], &mut heard)
            .expect("the wait is set");
        let _ = wait_once(&mut s, &point, &opts, &mut heard);
        if slept {
            s.set_clock(TEST_NOW + 8 * 3600, PDT);
        }
        let woke = wait_once(&mut s, &point, &opts, &mut heard);
        s.pass_wake_settle();
        let settled = wait_once(&mut s, &point, &opts, &mut heard);
        drop(s);
        let typed = count(&m, "@s-1 turn") + count(&m, "@s-1 send");
        (woke, settled, typed)
    };
    let (woke, settled, typed) = run(true);
    assert_eq!(
        woke.due.as_deref(),
        Some("the Mac woke"),
        "{:#?}",
        woke.lines
    );
    assert!(
        continued(&settled.lines, RULE_API_RETRY),
        "the rung slept through is over: {:#?}",
        settled.lines
    );
    assert_eq!(typed, 1, "{:#?}", settled.lines);
    let (_, settled, typed) = run(false);
    assert_eq!(settled.due.as_deref(), Some("overloaded retry 1"));
    assert_eq!(typed, 0, "{:#?}", settled.lines);
}

#[test]
fn an_edge_at_the_wake_types_nothing_before_the_settle() {
    use crate::supervise::policy::turn_end::RULE_API_BACK;
    let run = |slept: bool| {
        let host = ReachHost::scripted(&[Reach::Unknown]);
        let mut m = Mock::new(vec![enotfound()]);
        m.stall_sleep = Some(Duration::from_millis(5));
        let opts = SuperviseOpts {
            idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
            ..hosted(30)
        };
        let mut heard = Heard::default();
        let mut s = virtual_session(&mut m, Some("@s-1".to_string()));
        s.set_clock(TEST_NOW, PDT);
        s.set_turn_end_timing(far_rungs());
        let point = point_at(&s, enotfound(), 102);
        let action = s
            .turn_end_at_point(&point, Some(Duration::from_secs(180)), &opts)
            .expect("a decision");
        assert!(
            matches!(&action, TurnEndAction::WaitUntil { .. }),
            "{action:?}"
        );
        s.turn_end_execute(&point, action, &opts, &[], &mut heard)
            .expect("the rung's wait");
        let _ = wait_once(&mut s, &point, &opts, &mut heard);
        host.script
            .lock()
            .unwrap()
            .push_back(Reach::Up { since: s.fx.mono() });
        if slept {
            s.set_clock(TEST_NOW + 8 * 3600, PDT);
        }
        let woke = wait_once(&mut s, &point, &opts, &mut heard);
        s.pass_wake_settle();
        let settled = wait_once(&mut s, &point, &opts, &mut heard);
        drop(s);
        let typed = count(&m, "@s-1 turn") + count(&m, "@s-1 send");
        (woke, settled, typed)
    };
    let (woke, settled, typed) = run(true);
    assert!(
        !continued(&woke.lines, RULE_API_BACK),
        "the edge types nothing inside the settle: {:#?}",
        woke.lines
    );
    assert_eq!(
        woke.due.as_deref(),
        Some("the Mac woke"),
        "{:#?}",
        woke.lines
    );
    assert!(
        continued(&settled.lines, RULE_API_BACK),
        "the try goes once the settle has passed: {:#?}",
        settled.lines
    );
    assert_eq!(typed, 1);
    let (woke, _, typed) = run(false);
    assert!(continued(&woke.lines, RULE_API_BACK), "{:#?}", woke.lines);
    assert_eq!(typed, 1);
}

/// A host that asks for a point once told to (`wanting`) and takes it with a step that types
/// into the agent (the live upgrade's carry-on), counted; it keeps the words of the guards the
/// loop said withheld a point ([`IdleHost::withheld`]).
#[derive(Debug, Default)]
struct WakeHost {
    wanting: AtomicBool,
    steps: std::sync::atomic::AtomicUsize,
    withheld: std::sync::Mutex<Vec<&'static str>>,
}

impl IdleHost for WakeHost {
    fn wants(&self) -> bool {
        self.wanting.load(Ordering::SeqCst)
    }
    fn at_idle(&self) -> Option<HostStep> {
        self.steps.fetch_add(1, Ordering::SeqCst);
        self.wanting.store(false, Ordering::SeqCst);
        Some(HostStep {
            line: "upgrade step=carry-on".to_string(),
            moved: true,
            typed: Some(crate::supervise::policy::turn_end::HostTurn::CarryOn),
            repeat: false,
        })
    }
    fn owns_turn_end(&self) -> bool {
        false
    }
    fn withheld(&self, guard: crate::supervise::Guard) {
        self.withheld.lock().unwrap().push(guard.word());
    }
}

#[test]
fn the_hosts_step_at_the_wake_waits_for_the_settle() {
    let run = |slept: bool| {
        let host = Arc::new(WakeHost::default());
        let mut m = Mock::new(vec![idle_screen()]);
        m.cursor_on_caret = true;
        let opts = SuperviseOpts {
            idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
            ..hosted(30)
        };
        let mut heard = Heard::default();
        let mut s = virtual_session(&mut m, Some("@s-1".to_string()));
        s.set_clock(TEST_NOW, PDT);
        let screen = s.screen().expect("the point");
        let point = s.turn_of(screen);
        let _ = wait_once(&mut s, &point, &opts, &mut heard);
        if slept {
            s.set_clock(TEST_NOW + 8 * 3600, PDT);
        }
        host.wanting.store(true, Ordering::SeqCst);
        let woke = wait_once(&mut s, &point, &opts, &mut heard);
        let at_wake = host.steps.load(Ordering::SeqCst);
        s.pass_wake_settle();
        let _ = wait_once(&mut s, &point, &opts, &mut heard);
        let after = host.steps.load(Ordering::SeqCst);
        let withheld = host.withheld.lock().unwrap().clone();
        (woke.lines, at_wake, after, withheld)
    };
    let (lines, at_wake, after, withheld) = run(true);
    assert_eq!(at_wake, 0, "no step inside the settle: {lines:#?}");
    assert!(
        !lines.iter().any(|l| l.starts_with("HOST seq=")),
        "{lines:#?}"
    );
    assert_eq!(withheld, ["wake-settle"], "{lines:#?}");
    assert_eq!(after, 1, "the step goes once the settle has passed");
    let (lines, at_wake, _, withheld) = run(false);
    assert_eq!(at_wake, 1, "{lines:#?}");
    assert!(withheld.is_empty(), "{withheld:?}");
}

#[test]
fn a_wait_inside_the_wakes_settle_steps_to_its_fall() {
    use super::super::turn_end_loop::WAKE_SETTLE;
    let run = |settling: bool| {
        let host = Arc::new(WakeHost::default());
        let mut m = Mock::new(vec![idle_screen()]);
        m.cursor_on_caret = true;
        m.vanish_after = Some(0);
        let opts = SuperviseOpts {
            idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
            ..hosted(30)
        };
        let mut s = session(&mut m, None);
        s.set_clock(TEST_NOW, PDT);
        let screen = s.screen().expect("the point");
        let point = s.turn_of(screen);
        if settling {
            let woke = Instant::now() + Duration::from_millis(300) - WAKE_SETTLE;
            s.settle_wake(TEST_NOW, woke);
        }
        let mut deadline = Instant::now() + Duration::from_secs(5);
        let _ = s.wait_for_next(point, &opts, &[], &mut deadline, &mut Heard::default());
        drop(s);
        m.requests
            .iter()
            .filter(|r| r.starts_with("await "))
            .find_map(|r| {
                let mut words = r.split(' ');
                words.find(|w| *w == "timeout")?;
                words.next()?.parse::<u64>().ok()
            })
            .expect("the wait's request")
    };
    let asked = run(true);
    assert!(asked <= 300, "stepped to the settle's fall: {asked} ms");
    let asked = run(false);
    assert!(asked > 300, "the budget's step: {asked} ms");
}

#[test]
fn a_point_withheld_on_its_caret_alone_is_offered_once_the_caret_lands() {
    let run = |lands: bool| {
        let host = Arc::new(WakeHost::default());
        host.wanting.store(true, Ordering::SeqCst);
        let mut m = Mock::new(vec![idle_screen()]);
        m.turn_releases = Some(0);
        if lands {
            m.caret_after_reads = Some(1);
        }
        let opts = SuperviseOpts {
            idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
            ..hosted(30)
        };
        let mut heard = Heard::default();
        let mut s = virtual_session(&mut m, Some("@s-1".to_string()));
        s.set_clock(TEST_NOW, PDT);
        let screen = s.screen().expect("the point");
        let point = s.turn_of(screen);
        let seq = point.screen.seq;
        let mut deadline = s.fx.mono() + Duration::from_secs(60);
        let end = s.wait_for_next(point, &opts, &[], &mut deadline, &mut heard);
        assert!(end.is_ok(), "{:#?}", heard_lines(&heard));
        drop(s);
        let at_seq = m.seq == seq;
        let steps = host.steps.load(Ordering::SeqCst);
        let withheld = host.withheld.lock().unwrap().clone();
        (steps, withheld, at_seq, heard_lines(&heard))
    };
    let (steps, withheld, at_seq, lines) = run(true);
    assert!(at_seq, "the content never moved: {lines:#?}");
    assert_eq!(withheld, ["not-authoritative"], "{lines:#?}");
    assert_eq!(steps, 1, "the host steps once the caret lands: {lines:#?}");
    assert!(
        lines.iter().any(|l| l.contains("upgrade step=carry-on")),
        "{lines:#?}"
    );
    let (steps, withheld, at_seq, lines) = run(false);
    assert!(at_seq, "{lines:#?}");
    assert_eq!(withheld, ["not-authoritative"], "{lines:#?}");
    assert_eq!(steps, 0, "{lines:#?}");
}

#[test]
fn a_caret_read_again_over_content_that_moved_offers_the_old_point_nothing() {
    let host = Arc::new(WakeHost::default());
    host.wanting.store(true, Ordering::SeqCst);
    let mut moved = idle_screen();
    moved[0] = "⏺ Done, and the docs are updated too.".to_string();
    let mut m = Mock::new(vec![idle_screen(), moved]);
    m.turn_releases = Some(0);
    m.caret_after_reads = Some(1);
    m.redrawn_after_reads = Some(1);
    let opts = SuperviseOpts {
        idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
        ..hosted(30)
    };
    let mut heard = Heard::default();
    let mut s = virtual_session(&mut m, Some("@s-1".to_string()));
    s.set_clock(TEST_NOW, PDT);
    let screen = s.screen().expect("the point");
    let point = s.turn_of(screen);
    let seq = point.screen.seq;
    let mut deadline = s.fx.mono() + Duration::from_secs(60);
    let end = s.wait_for_next(point, &opts, &[], &mut deadline, &mut heard);
    assert!(end.is_ok(), "{:#?}", heard_lines(&heard));
    drop(s);
    let lines = heard_lines(&heard);
    assert_ne!(m.seq, seq, "the re-read found the next frame: {lines:#?}");
    assert_eq!(
        host.withheld.lock().unwrap().clone(),
        ["not-authoritative"],
        "{lines:#?}"
    );
    assert_eq!(
        host.steps.load(Ordering::SeqCst),
        0,
        "the old point was offered on the new frame's caret: {lines:#?}"
    );
}

#[test]
fn the_hosts_background_step_at_the_wake_waits_for_the_settle() {
    let bg = || {
        let mut r = rows(&[
            "⏺ Waiting for the build.",
            "",
            "✻ Crunched for 9m 27s · done 11:07 AM · 1 shell still running",
            "",
        ]);
        r.extend(composer("  ⏵⏵ auto mode on · 1 shell · ← for agents"));
        r
    };
    let run = |slept: bool| {
        let host = Arc::new(BackgroundHost::default());
        let mut m = Mock::new(vec![bg()]);
        let mut s = session(&mut m, Some("@s-1".to_string()));
        s.set_clock(TEST_NOW, PDT);
        s.stall_host = Some(Arc::clone(&host) as Arc<dyn IdleHost>);
        s.setup.background_settle = Duration::ZERO;
        let _ = s.screen().expect("the busy read before the night");
        if slept {
            s.set_clock(TEST_NOW + 8 * 3600, PDT);
        }
        let screen = s.screen().expect("the busy read at the wake");
        let mut since = Some(Instant::now());
        s.host_steps_in_background(&screen, &mut since, None, &mut Alone)
            .expect("the break's step");
        let at_wake = host.backgrounds.load(Ordering::SeqCst);
        s.pass_wake_settle();
        s.host_steps_in_background(&screen, &mut since, None, &mut Alone)
            .expect("the break's step");
        let after = host.backgrounds.load(Ordering::SeqCst);
        let withheld: Vec<&str> = host
            .withheld
            .lock()
            .unwrap()
            .iter()
            .map(|g| g.word())
            .collect();
        (at_wake, after, withheld)
    };
    let (at_wake, after, withheld) = run(true);
    assert_eq!(at_wake, 0, "no notice inside the settle");
    assert_eq!(withheld, ["wake-settle"]);
    assert!(after >= 1, "offered once the settle has passed");
    let (at_wake, _, withheld) = run(false);
    assert_eq!(at_wake, 1);
    assert!(withheld.is_empty(), "{withheld:?}");
}

#[test]
fn a_point_after_a_sleep_a_request_saw_is_held_only_until_the_wakes_settle() {
    use crate::supervise::policy::turn_end::RULE_CONTINUE;
    let run = |slept: bool, outlasted: bool| {
        let mut m = Mock::new(vec![ended("Stage done.")]);
        let opts = hosted(30);
        let mut heard = Heard::default();
        let mut s = session(&mut m, Some("@s-1".to_string()));
        s.set_clock(TEST_NOW, PDT);
        let _ = s.call(&["status"]).expect("a read before the night");
        if slept {
            s.set_clock(TEST_NOW + 8 * 3600, PDT);
        }
        let _ = s.call(&["status"]).expect("a read of the busy turn");
        if outlasted {
            s.pass_wake_settle();
        }
        let point = point_at(&s, ended("Stage done."), 102);
        let action = s
            .turn_end_at_point(&point, Some(Duration::from_secs(180)), &opts)
            .expect("a decision");
        s.turn_end_execute(&point, action, &opts, &[], &mut heard)
            .expect("carried out");
        let due = s.turn_end_due.as_ref().map(|d| d.why.clone());
        drop(s);
        let typed = count(&m, "@s-1 turn") + count(&m, "@s-1 send");
        (heard_lines(&heard), due, typed)
    };
    let (lines, due, typed) = run(true, false);
    assert_eq!(typed, 0, "held inside the settle: {lines:#?}");
    assert_eq!(due.as_deref(), Some("the Mac woke"), "{lines:#?}");
    assert!(
        lines.iter().any(|l| l.starts_with("WAITING seq=102 until=")
            && l.ends_with("the Mac woke: deciding again")),
        "{lines:#?}"
    );
    let (lines, _, typed) = run(true, true);
    assert!(continued(&lines, RULE_CONTINUE), "{lines:#?}");
    assert_eq!(typed, 1, "{lines:#?}");
    assert!(
        !lines.iter().any(|l| l.contains("the Mac woke")),
        "a settle long passed says nothing: {lines:#?}"
    );
    let (lines, _, typed) = run(false, false);
    assert!(continued(&lines, RULE_CONTINUE), "{lines:#?}");
    assert_eq!(typed, 1, "{lines:#?}");
}

fn read_screen(rows: Vec<String>) -> Screen {
    Screen {
        rows,
        cursor_row: 0,
        cursor_col: 2,
        seq: 1,
        first: 0,
        generation: None,
        human: crate::supervise::screen::HumanInput::Unknown,
        human_seq: None,
        cols: None,
        pane_rows: None,
        launch: None,
        submits: None,
        frame_open: false,
        inputs_before: None,
        inputs_after: None,
    }
}

#[test]
fn a_sessions_jitter_is_stable_bounded_and_spread() {
    use super::super::turn_end_loop::resume_jitter;
    let span = Duration::from_secs(60);
    let read = |sid: Option<&str>, rows: Vec<String>| {
        let mut m = Mock::new(vec![busy_screen()]);
        let mut s = session(&mut m, sid.map(str::to_string));
        s.set_clock(TEST_NOW, PDT);
        s.set_zone_lookup(test_zones);
        let opts = hosted(30);
        let first = s.turn_end_reading(&read_screen(rows.clone()), None, &opts);
        let again = s.turn_end_reading(&read_screen(rows), None, &opts);
        assert_eq!(
            first.jitter, again.jitter,
            "the same session, the same share"
        );
        let unix = s.wall_reset.as_ref().and_then(|(_, u)| *u);
        (first.jitter, unix)
    };
    let mut shares = std::collections::BTreeSet::new();
    for k in 0..20 {
        let sid = format!("@s-{k}");
        let (jitter, unix) = read(Some(&sid), memory_wall_in("3h"));
        let unix = unix.expect("the placed reset");
        assert_eq!(jitter, resume_jitter(&sid, unix, span), "{sid}");
        assert!(jitter < span);
        shares.insert(jitter.as_secs());
    }
    assert!(
        shares.len() >= 8,
        "twenty sessions, several seconds: {shares:?}"
    );
    assert_eq!(
        read(None, memory_wall_in("3h")).0,
        Duration::ZERO,
        "no session"
    );
    let end_529 = aterm_phase::prompt::fixtures::screen(aterm_phase::prompt::fixtures::END_529);
    assert_eq!(
        read(Some("@s-3"), end_529).0,
        Duration::ZERO,
        "a 529 is no reset"
    );
}

#[test]
fn an_undated_cancelled_limit_is_placed_within_a_session_window() {
    let mut m = Mock::new(vec![busy_screen()]);
    let mut s = session(&mut m, None);
    s.set_zone_lookup(test_zones);
    let kind = aterm_phase::WallKind::UsageSession;
    let rows = auto_continue_cancelled();
    let mut placed = |text: &str, undated: bool| -> i64 {
        s.set_clock(TEST_NOW, PDT);
        s.wall_reset = None;
        let _ = s.wall_reset_at(Some(text), kind, false, false, true, undated, &rows);
        s.wall_reset
            .as_ref()
            .and_then(|(_, u)| *u)
            .expect("a placement")
            - TEST_NOW
    };
    let min = 60;
    let hour = 3600;
    assert_eq!(placed("7:30am", true), -(hour + 25 * min), "passed");
    assert_eq!(
        placed("11:50pm", true),
        -(9 * hour + 5 * min),
        "yesterday's, passed"
    );
    assert_eq!(
        placed("10:38am", true),
        hour + 43 * min,
        "within the window"
    );
    assert_eq!(
        placed("Oct 18 at 7:30am", true),
        30 * 86_400 + 22 * hour + 35 * min,
        "a dated reset"
    );
    assert_eq!(placed("in 7h", true), 7 * hour, "a span");
    assert_eq!(
        placed("7:30am", false),
        22 * hour + 35 * min,
        "tomorrow's on the print"
    );
    assert_eq!(placed("11:50pm", false), 14 * hour + 55 * min);
}

#[test]
fn a_unix_second_reads_back_and_stamps_as_itself_at_any_phase_of_the_clock() {
    let mut m = Mock::new(vec![busy_screen()]);
    let mut s = session(&mut m, None);
    let mut bad = Vec::new();
    for phase in [0_i64, 1, 250, 499, 500, 501, 600, 750, 999] {
        s.set_clock_at_ms(TEST_NOW * 1000 + phase, PDT);
        for ahead in [-60_i64, -1, 0, 1, 2, 59, 600, 7200] {
            let u = TEST_NOW + ahead;
            let t = s.instant_of(u);
            let back = s.unix_of(t);
            if back != u {
                bad.push(format!(
                    "phase={phase} ahead={ahead}: unix_of={back}, want {u}"
                ));
            }
            if ahead >= 1 {
                let stamp = s.stamp_in(t);
                let want = utc_stamp(u64::try_from(u).expect("after 1970"));
                if stamp != want {
                    bad.push(format!(
                        "phase={phase} ahead={ahead}: stamp_in={stamp}, want {want}"
                    ));
                }
            }
        }
    }
    assert!(
        bad.is_empty(),
        "{} mismatches:\n{}",
        bad.len(),
        bad.join("\n")
    );
}

#[test]
fn the_wall_waited_on_is_written_to_the_ledger_and_its_end_too() {
    use crate::supervise::approvals;
    let run = |tag: &str, screens: Vec<Vec<String>>| {
        let dir = crate::supervise::test_scratch_path("wm-writes", tag);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        let ledger = dir.join("s-1.jsonl");
        let before = crate::supervise::limit::unix_now();
        let (m, lines, _, jdir) = memory_run_for(tag, screens, 3, Some("@s-1"), |s| {
            s.set_approval_ledger(Some(ledger.clone()))
        });
        let after = crate::supervise::limit::unix_now();
        let _ = std::fs::remove_dir_all(&jdir);
        let rows = std::fs::read_to_string(&ledger).unwrap_or_default();
        let open = approvals::open_wall(&ledger, Some("@s-1"));
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(m.turns, 0, "{lines:#?}\n{:#?}", m.requests);
        (rows, open, before, after)
    };
    let count_rule = |rows: &str, rule: &str| {
        rows.lines()
            .filter(|l| l.contains(&format!("\"rule_id\":\"{rule}\"")))
            .count()
    };
    let (rows, open, before, after) = run(
        "wm-write",
        vec![
            busy_screen(),
            memory_wall_in("60s"),
            memory_scrolled_in("60s"),
        ],
    );
    assert_eq!(count_rule(&rows, approvals::RULE_WALL_WAIT), 1, "{rows}");
    assert_eq!(
        count_rule(&rows, approvals::RULE_WALL_WAIT_END),
        0,
        "{rows}"
    );
    let open = open.expect("an open wall");
    let reset = open.reset_unix.expect("the placed reset");
    assert!(
        reset >= before + 58 && reset <= after + 62,
        "reset {reset} not within [{before}, {after}] + a minute"
    );
    assert!(open.message.contains("weekly limit"), "{}", open.message);
    let (rows, open, _, _) = run(
        "wm-end",
        vec![
            busy_screen(),
            memory_wall_in("60s"),
            memory_with_a_live_turn_in("60s"),
        ],
    );
    assert_eq!(count_rule(&rows, approvals::RULE_WALL_WAIT), 1, "{rows}");
    assert_eq!(
        count_rule(&rows, approvals::RULE_WALL_WAIT_END),
        1,
        "{rows}"
    );
    assert!(open.is_none(), "{open:?}");
    let (rows, open, _, _) = run("wm-none", vec![busy_screen(), memory_scrolled_in("60s")]);
    assert_eq!(count_rule(&rows, approvals::RULE_WALL_WAIT), 0, "{rows}");
    assert_eq!(
        count_rule(&rows, approvals::RULE_WALL_WAIT_END),
        0,
        "{rows}"
    );
    assert!(open.is_none(), "{rows}");
}

#[test]
fn an_idle_screen_or_a_question_ends_the_wall_memory() {
    let mut m = Mock::new(vec![busy_screen()]);
    let mut s = session(&mut m, None);
    s.set_clock(TEST_NOW, PDT);
    s.set_zone_lookup(test_zones);
    let opts = hosted(30);
    let wall = read_screen(memory_wall_in("3h"));
    let scrolled = read_screen(memory_scrolled_in("3h"));
    let quiet = |said: &str| {
        let mut idle = rows(&[
            &format!("⏺ {said}"),
            "",
            "✻ Worked for 3m 2s · done 8:00 PM",
            "",
        ]);
        idle.extend(composer("  ⏵⏵ bypass permissions on (shift+tab to cycle)"));
        read_screen(idle)
    };
    let fold = |s: &mut Session<'_, Mock>, screen: &Screen, worked: Option<Duration>| {
        let r = s.turn_end_reading(screen, worked, &opts);
        s.observe_point(&r, Instant::now(), screen.seq);
        r
    };
    let _ = fold(&mut s, &wall, None);
    assert!(s.wall_memory.is_some());
    let r = s.turn_end_reading(&scrolled, None, &opts);
    assert!(
        r.wall.is_some(),
        "the scrolled screen is the remembered wall"
    );
    for said in ["All done.", "Should I rename the module?"] {
        let _ = fold(&mut s, &wall, None);
        assert!(s.wall_memory.is_some(), "{said}");
        let _ = fold(&mut s, &quiet(said), Some(Duration::from_secs(182)));
        assert!(s.wall_memory.is_none(), "{said:?} ends the memory");
        let r = s.turn_end_reading(&scrolled, None, &opts);
        assert!(
            r.wall.is_none(),
            "{said:?}: a stale memory would be read here"
        );
    }
    let _ = fold(&mut s, &wall, None);
    let _ = fold(&mut s, &quiet("All done."), None);
    assert!(s.turn_end.waits_on_named_reset());
    assert!(s.wall_memory.is_some(), "the wall stands: its memory too");
    let r = s.turn_end_reading(&scrolled, None, &opts);
    assert!(r.wall.is_some(), "the scrolled screen is the wall still");
}

#[test]
fn agy_and_muse_background_work_keeps_the_remembered_wall() {
    use aterm_phase::agy::fixtures::lab as a;
    use aterm_phase::muse::fixtures as u;
    use aterm_phase::prompt::fixtures::screen;
    for (name, wall, work, live) in [
        (
            "muse",
            u::USAGE_WALL,
            u::END_OF_TURN_BG_RUNNING,
            u::BUSY_WORK_RUNNING,
        ),
        ("agy", a::ERROR_429_LONG, a::BG_TASK, a::BG_BUSY),
    ] {
        let mut m = Mock::new(vec![busy_screen()]);
        let mut s = session(&mut m, None);
        s.set_clock(TEST_NOW, PDT);
        s.set_zone_lookup(test_zones);
        let opts = hosted(30);
        let (wall, work, live) = (
            read_screen(screen(wall)),
            read_screen(screen(work)),
            read_screen(screen(live)),
        );
        let fold = |s: &mut Session<'_, Mock>, screen: &Screen| {
            let r = s.turn_end_reading(screen, None, &opts);
            s.observe_point(&r, Instant::now(), screen.seq);
            r
        };
        let r = s.turn_end_reading(&work, None, &opts);
        assert_eq!((r.phase.clone(), r.wall), (Phase::Busy, None), "{name}");
        let r = fold(&mut s, &wall);
        assert!(matches!(r.phase, Phase::Limited { .. }), "{name}: {r:#?}");
        assert!(s.wall_memory.is_some(), "{name}");
        let kind = r.wall;
        let r = s.turn_end_reading(&work, None, &opts);
        assert!(matches!(r.phase, Phase::Limited { .. }), "{name}: {r:#?}");
        assert_eq!(r.wall, kind, "{name}: the remembered wall");
        assert!(s.wall_memory.is_some(), "{name}: the memory stands");
        let reader = s.reader(&work.rows);
        s.end_memory_on_live_turn(reader, &work.rows, 103);
        assert!(s.wall_memory.is_some(), "{name}: no live turn");
        let r = s.turn_end_reading(&live, None, &opts);
        assert_eq!((r.phase.clone(), r.wall), (Phase::Busy, None), "{name}");
        assert!(s.wall_memory.is_none(), "{name}: the live turn ended it");
        let r = s.turn_end_reading(&work, None, &opts);
        assert_eq!((r.phase.clone(), r.wall), (Phase::Busy, None), "{name}");
    }
}

#[test]
fn tier1_the_real_loop_remembers_a_hidden_wall_exactly_where_the_model_does() {
    use aterm_models_harness::supervisor_usage_wall_model;
    use std::collections::{BTreeMap, BTreeSet, VecDeque};
    type State = BTreeMap<&'static str, i64>;
    let hidden_model = aterm_spec::interp::with_consts(
        &supervisor_usage_wall_model(),
        &[
            ("Hidden", 1),
            ("Leaves", 0),
            ("OwnAct", 0),
            ("Tapped", 0),
            ("Cap", 5),
            ("Skew", 3),
        ],
    );
    let idle = || {
        let mut idle = rows(&["⏺ All done.", "", "✻ Worked for 3m 2s · done 8:00 PM", ""]);
        idle.extend(composer("  ⏵⏵ bypass permissions on (shift+tab to cycle)"));
        idle
    };
    let real_wall = |path: &[&'static str], st: &State| -> bool {
        let mut m = Mock::new(vec![busy_screen()]);
        let mut s = session(&mut m, None);
        s.set_clock(TEST_NOW, PDT);
        s.set_zone_lookup(test_zones);
        let opts = hosted(30);
        let mut fold = |rows: Vec<String>, worked: Option<Duration>| {
            let screen = read_screen(rows);
            let r = s.turn_end_reading(&screen, worked, &opts);
            s.observe_point(&r, Instant::now(), screen.seq);
        };
        for action in path {
            match *action {
                "WallAppears" | "WallAppearsLate" | "WallShows" | "Met" => {
                    fold(memory_wall_in("3h"), Some(Duration::from_secs(600)));
                }
                "WallFoundSitting" => fold(memory_wall_in("3h"), None),
                "WallHides" => fold(memory_scrolled_in("3h"), None),
                "Taken" => fold(idle(), Some(Duration::from_secs(600))),
                _ => {}
            }
        }
        let screen = if st["wall"] == 5 {
            memory_scrolled_in("3h")
        } else {
            memory_wall_in("3h")
        };
        s.turn_end_reading(&read_screen(screen), None, &opts)
            .wall
            .is_some()
    };
    let walk = |m: &aterm_spec::derive::Model| {
        let mut seen: BTreeSet<State> = BTreeSet::new();
        let mut queue = VecDeque::from([(m.init_state(), Vec::<&'static str>::new())]);
        let (mut checked, mut hidden_checked, mut disagree) = (0, 0, Vec::new());
        while let Some((st, path)) = queue.pop_front() {
            if !seen.insert(st.clone()) {
                continue;
            }
            if st["wall"] == 1 || st["wall"] == 5 {
                let want = st["wall"] == 1 || st["mem"] == 1;
                let got = real_wall(&path, &st);
                checked += 1;
                hidden_checked += usize::from(st["wall"] == 5);
                if got != want {
                    disagree.push((st.clone(), got, want));
                }
            }
            for action in &m.actions {
                let mut next = st.clone();
                if m.fire(action.name, &mut next) {
                    let mut p = path.clone();
                    p.push(action.name);
                    queue.push_back((next, p));
                }
            }
        }
        (checked, hidden_checked, disagree)
    };
    let (checked, hidden_checked, disagree) = walk(&hidden_model);
    assert!(disagree.is_empty(), "{disagree:#?}");
    assert!(
        checked > 40 && hidden_checked > 10,
        "{checked} {hidden_checked}"
    );
    let buggy = aterm_spec::interp::with_buggy(&hidden_model, 1);
    let (_, _, off) = walk(&buggy);
    assert!(
        off.iter()
            .any(|(st, got, want)| st["wall"] == 5 && st["mem"] == 0 && *got && !*want),
        "the model that forgets the wall agrees with the real loop: {off:?}"
    );
}

#[test]
fn the_budget_is_stretched_to_the_reset_the_wait_holds_for() {
    let mut m = Mock::new(vec![busy_screen()]);
    let mut s = session(&mut m, None);
    let now = 1_790_650_800;
    s.set_clock(now, PDT);
    s.set_zone_lookup(test_zones);
    let rows: Vec<String> = memory_wall()
        .into_iter()
        .map(|r| r.replace("resets in 2s", "resets 9am (America/Los_Angeles)"))
        .collect();
    let text = "9am (America/Los_Angeles)";
    assert!(s.reset_unix(Some(text)).is_some_and(|at| at < now));
    let r = s.turn_end_reading(&read_screen(rows), None, &hosted(30));
    let waited = r.reset_at.expect("the wait");
    let unix = s.wall_reset.as_ref().and_then(|(_, u)| *u).expect("placed");
    assert!(
        unix > now + 12 * 3600 && unix < now + 14 * 3600,
        "{}",
        unix - now
    );
    assert!(waited > Instant::now() + Duration::from_secs(12 * 3600));
    assert_eq!(
        s.reset_unix(Some(text)),
        Some(unix),
        "the stretch reads it too"
    );
    assert!(
        s.reset_unix(Some("10am (America/Los_Angeles)"))
            .is_some_and(|at| at < now)
    );
}

#[test]
fn the_wait_places_a_reset_across_a_dst_change_with_the_offset_there() {
    fn la(zone: &str, unix: i64) -> Option<i64> {
        let pst = unix >= 1_793_523_600;
        (zone == "America/Los_Angeles").then_some(if pst { -8 * 3600 } else { PDT })
    }
    let mut m = Mock::new(vec![busy_screen()]);
    let mut s = session(&mut m, None);
    s.set_zone_lookup(la);
    s.set_clock(1_793_514_600, PDT);
    let three = Some("3am (America/Los_Angeles)");
    assert_eq!(
        s.reset_unix_for(three, false, true, false, &[]),
        Some(1_793_530_800)
    );
    let mut printed = rows(&["⏺ Working.", "", "✻ Worked for 3m 2s · done 11:25 PM", ""]);
    printed.extend(composer("  ⏵⏵ bypass permissions on"));
    assert_eq!(
        s.reset_unix_for(three, false, true, false, &printed),
        Some(1_793_530_800)
    );
    s.set_clock(1_793_386_800, PDT);
    let nov_2 = Some("Nov 2 at 9am (America/Los_Angeles)");
    assert_eq!(
        s.reset_unix_for(nov_2, false, true, false, &[]),
        Some(1_793_638_800)
    );
}

/// A host that only listens for the canary's ladder, and answers its jobs
/// reading with `evidence` (`Unknown` by default: no reading), or with `then`
/// once it was told a call that made no progress.
#[derive(Debug, Default)]
struct LadderHost {
    told: std::sync::Mutex<Vec<String>>,
    evidence: crate::supervise::background::BackgroundEvidence,
    then: Option<crate::supervise::background::BackgroundEvidence>,
}

impl IdleHost for LadderHost {
    fn wants(&self) -> bool {
        false
    }
    fn at_idle(&self) -> Option<HostStep> {
        None
    }
    fn owns_turn_end(&self) -> bool {
        false
    }
    fn exception(&self, standing: Option<&crate::supervise::policy::exception::Exception>) {
        self.told.lock().unwrap().push(format!(
            "exception:{}",
            standing.map_or("none", |e| e.kind.word())
        ));
    }
    fn stall_unread(&self, stall: Option<StallSeen>) {
        self.told.lock().unwrap().push(match stall {
            None => "unread:none".to_string(),
            Some(StallSeen::Unread { .. }) => "unread:some".to_string(),
            Some(StallSeen::OwnWork { .. }) => "own-work:some".to_string(),
            Some(StallSeen::NoProgress { minutes, pid }) => {
                format!("no-progress:{pid}:{minutes}")
            }
        });
    }
    fn background_jobs(&self) -> crate::supervise::background::BackgroundEvidence {
        let told = self.told.lock().unwrap();
        match &self.then {
            Some(then) if told.iter().any(|t| t.starts_with("no-progress:")) => then.clone(),
            _ => self.evidence.clone(),
        }
    }
}

#[test]
fn a_stall_that_ends_tells_the_host_the_point_that_stood_under_it() {
    use crate::supervise::policy::outlook::ExceptionKind;
    let end = |standing: bool| {
        let host = Arc::new(LadderHost::default());
        let mut m = Mock::new(vec![busy_screen()]);
        let mut s = session(&mut m, None);
        s.stall_host = Some(Arc::clone(&host) as Arc<dyn IdleHost>);
        s.stuck_rung = super::super::wall_memory::StuckRung::Raised;
        s.stuck_raised = Some((Instant::now(), ExceptionKind::Stalled));
        if standing {
            let mut point = crate::supervise::policy::exception::Exception::stalled();
            point.kind = ExceptionKind::Futile;
            s.exception_open = Some((point, Instant::now()));
        }
        s.stuck_over(1, "the screen moved on", &mut Alone);
        host.told.lock().unwrap().clone()
    };
    assert_eq!(
        end(true),
        ["unread:none", "exception:futile"],
        "the point that stood under the stall is told again"
    );
    assert_eq!(end(false), ["unread:none", "exception:none"]);
}

#[test]
fn a_stall_kept_up_climbs_to_a_record_and_then_an_exception_and_its_end_withdraws_both() {
    let scrolled = memory_scrolled();
    let run = |title: Option<&str>, screen: Vec<String>| {
        let (dir, path) = journal_file("stuck-ladder");
        let host = Arc::new(LadderHost::default());
        let mut m = Mock::new(vec![busy_screen(), screen]);
        m.title = title.map(str::to_string);
        m.vanish_after = Some(14);
        m.stall_sleep = Some(Duration::from_millis(25));
        let opts = SuperviseOpts {
            journal: Some(path.clone()),
            idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
            ..hosted(60)
        };
        let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
            s.setup.stuck_after = Duration::from_millis(30);
        });
        let (records, _) = journal_records(&path);
        let _ = std::fs::remove_dir_all(&dir);
        let kind = |k: &str| -> Vec<String> {
            records
                .iter()
                .filter(|r| r.line.starts_with(k))
                .map(|r| r.line.clone())
                .collect()
        };
        (
            kind("STUCK"),
            kind("STALLED"),
            kind("EXCEPTION"),
            kind("CLEARED"),
            host.told.lock().unwrap().clone(),
            count(&m, "turn"),
            lines,
        )
    };
    let (stuck, stalled, exception, cleared, told, turns, lines) =
        run(Some("✳ Claude Code"), scrolled.clone());
    assert_eq!(stuck.len(), 1, "rung 1 once: {stuck:#?}\n{lines:#?}");
    assert_eq!(stalled.len(), 1, "rung 2 once: {stalled:#?}\n{lines:#?}");
    assert_eq!(
        exception.len(),
        1,
        "rung 3 once: {exception:#?}\n{lines:#?}"
    );
    assert!(
        exception[0].contains("kind=stalled when=later"),
        "{}",
        exception[0]
    );
    assert!(
        cleared
            .iter()
            .any(|l| l.contains("exception=stalled after=") && l.ends_with("the loop ended")),
        "{cleared:#?}"
    );
    assert_eq!(
        told,
        [
            "unread:some",
            "exception:stalled",
            "unread:none",
            "exception:none"
        ],
        "the host hears each rung once, and its end"
    );
    assert_eq!(turns, 0, "the ladder types nothing");
    for (name, title, screen) in [
        ("an animated title", Some("◐ Claude Code"), scrolled.clone()),
        (
            "a live spinner",
            Some("✳ Claude Code"),
            memory_with_a_live_turn(),
        ),
        ("no title", None, scrolled),
    ] {
        let (stuck, stalled, exception, _, told, _, lines) = run(title, screen);
        assert!(
            stuck.is_empty() && stalled.is_empty() && exception.is_empty() && told.is_empty(),
            "{name} climbs nothing: {stuck:#?} {stalled:#?} {told:#?}\n{lines:#?}"
        );
    }
}

#[test]
fn a_background_call_that_makes_no_progress_is_named_at_stalled_and_in_the_exception() {
    use crate::supervise::background::{BackgroundCall, BackgroundEvidence};
    let run = |evidence: BackgroundEvidence, then: Option<BackgroundEvidence>| {
        let (dir, path) = journal_file("stuck-kernel");
        let host = Arc::new(LadderHost {
            evidence,
            then,
            ..LadderHost::default()
        });
        let mut m = Mock::new(vec![busy_screen(), memory_scrolled()]);
        m.title = Some("✳ Claude Code".to_string());
        m.vanish_after = Some(14);
        m.stall_sleep = Some(Duration::from_millis(25));
        let opts = SuperviseOpts {
            journal: Some(path.clone()),
            idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
            ..hosted(60)
        };
        let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
            s.setup.stuck_after = Duration::from_millis(30);
            s.setup.background_settle = Duration::from_millis(30);
        });
        let (records, _) = journal_records(&path);
        let _ = std::fs::remove_dir_all(&dir);
        let kind = |k: &str| -> Vec<String> {
            records
                .iter()
                .filter(|r| r.line.starts_with(k))
                .map(|r| r.line.clone())
                .collect()
        };
        (
            kind("STUCK"),
            kind("STALLED"),
            kind("EXCEPTION"),
            host.told.lock().unwrap().clone(),
            count(&m, "turn"),
            lines,
        )
    };
    let hung = BackgroundEvidence::NoProgress {
        worst: BackgroundCall {
            pid: 27098,
            pgid: 27090,
            exe: "aterm_gui-e0964a".to_string(),
            task: Some("bplle5dxj".to_string()),
            quiet_ms: 31 * 60_000 + 59_000,
            wait: None,
        },
    };
    let (stuck, stalled, exception, told, turns, lines) = run(hung.clone(), None);
    assert!(
        stuck.is_empty(),
        "the kernel explains the screen: no first rung: {stuck:#?}\n{lines:#?}"
    );
    assert_eq!(stalled.len(), 1, "rung 2 once: {stalled:#?}\n{lines:#?}");
    for said in ["bplle5dxj", "pid 27098", "31 min"] {
        assert!(stalled[0].contains(said), "{said:?} in {}", stalled[0]);
    }
    assert_eq!(
        exception.len(),
        1,
        "rung 3 once: {exception:#?}\n{lines:#?}"
    );
    assert!(
        exception[0].contains("kind=stalled when=later")
            && exception[0].contains("pid 27098")
            && exception[0].contains("31 min"),
        "{}",
        exception[0]
    );
    assert_eq!(
        told,
        [
            "no-progress:27098:31",
            "exception:stalled",
            "unread:none",
            "exception:none"
        ],
        "the host hears the call at rung 2, the exception at rung 3, and their end"
    );
    assert_eq!(turns, 0, "the ladder types nothing");
    let other = BackgroundEvidence::NoProgress {
        worst: BackgroundCall {
            pid: 31000,
            pgid: 31000,
            exe: "sleep".to_string(),
            task: None,
            quiet_ms: 20 * 60_000,
            wait: None,
        },
    };
    let (stuck, stalled, exception, told, _, lines) = run(hung, Some(other));
    assert!(stuck.is_empty(), "{stuck:#?}\n{lines:#?}");
    assert_eq!(
        (stalled.len(), exception.len()),
        (2, 1),
        "{stalled:#?} {exception:#?}\n{lines:#?}"
    );
    assert!(
        stalled[0].contains("pid 27098") && stalled[1].contains("pid 31000"),
        "{stalled:#?}"
    );
    assert!(exception[0].contains("pid 31000"), "{}", exception[0]);
    assert_eq!(
        told,
        [
            "no-progress:27098:31",
            "no-progress:31000:20",
            "exception:stalled",
            "unread:none",
            "exception:none"
        ],
        "the second call's record, then its exception"
    );
    let (stuck, stalled, exception, told, _, lines) = run(BackgroundEvidence::Unknown, None);
    assert_eq!(
        (stuck.len(), stalled.len(), exception.len()),
        (1, 1, 1),
        "{stuck:#?} {stalled:#?} {exception:#?}\n{lines:#?}"
    );
    assert!(
        stalled
            .iter()
            .chain(&exception)
            .all(|l| !l.contains("pid 27098")),
        "{stalled:#?} {exception:#?}"
    );
    assert_eq!(
        told,
        [
            "unread:some",
            "exception:stalled",
            "unread:none",
            "exception:none"
        ]
    );
}

#[test]
fn agy_or_muse_background_work_that_never_ends_climbs_the_canary() {
    use aterm_phase::agy::fixtures::lab as a;
    use aterm_phase::muse::fixtures as u;
    use aterm_phase::prompt::fixtures::screen;
    let asks = |rows: Vec<String>, head: &str, with: &str, blank_next: bool| {
        let mut rows = rows;
        let at = rows
            .iter()
            .rposition(|r| r.starts_with(head))
            .unwrap_or_else(|| panic!("a row starting {head:?}"));
        rows[at] = with.to_string();
        if blank_next {
            rows[at + 1] = String::new();
        }
        rows
    };
    let agy_asks = asks(
        screen(a::BG_TASK),
        "  Tool call done.",
        "  Should I deploy it now?",
        false,
    );
    let muse_asks = asks(
        screen(u::END_OF_TURN_BG_RUNNING),
        "◆ Tool call done.",
        "◆ Should I deploy it now?",
        true,
    );
    let run = |program: &str, busy: Vec<String>, work: Vec<String>| {
        let (dir, path) = journal_file("work-ladder");
        let host = Arc::new(LadderHost::default());
        let screens = vec![busy, work];
        let mut m = if program == "agy" {
            agy_mock(screens)
        } else {
            muse_mock(screens)
        };
        m.title = None;
        m.vanish_after = Some(14);
        m.stall_sleep = Some(Duration::from_millis(25));
        let opts = SuperviseOpts {
            journal: Some(path.clone()),
            idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
            ..hosted(60)
        };
        let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
            s.setup.stuck_after = Duration::from_millis(30);
        });
        let (records, _) = journal_records(&path);
        let _ = std::fs::remove_dir_all(&dir);
        let kind = |k: &str| -> Vec<String> {
            records
                .iter()
                .filter(|r| r.line.starts_with(k))
                .map(|r| r.line.clone())
                .collect()
        };
        let typed = m
            .requests
            .iter()
            .any(|r| r.starts_with("turn ") || r.starts_with("send ") || r.starts_with("key "));
        (
            [
                kind("STUCK"),
                kind("STALLED"),
                kind("EXCEPTION"),
                kind("CLEARED"),
            ],
            host.told.lock().unwrap().clone(),
            typed,
            lines,
        )
    };
    for (name, program, busy, work, what) in [
        (
            "agy's question over its task",
            "agy",
            screen(a::BUSY_THINKING),
            agy_asks,
            "a background task running",
        ),
        (
            "Muse Code's question over its shell",
            "muse",
            screen(u::BUSY_1_4_10),
            muse_asks,
            "background work running",
        ),
        (
            "Muse Code's plain end over its shell",
            "muse",
            screen(u::BUSY_1_4_10),
            screen(u::END_OF_TURN_BG_RUNNING),
            "background work running",
        ),
    ] {
        let ([stuck, stalled, exception, cleared], told, typed, lines) = run(program, busy, work);
        assert_eq!(
            stuck.len(),
            1,
            "{name}: rung 1 once: {stuck:#?}\n{lines:#?}"
        );
        assert!(stuck[0].contains(what), "{name}: {}", stuck[0]);
        assert_eq!(stalled.len(), 1, "{name}: rung 2 once: {stalled:#?}");
        assert!(
            stalled[0].contains("background work of the agent's own that has not ended"),
            "{name}: the work's words, no wait unclassified: {}",
            stalled[0]
        );
        assert_eq!(exception.len(), 1, "{name}: rung 3 once: {exception:#?}");
        assert!(
            exception[0].contains("kind=stalled when=later")
                && exception[0].contains("its own background work still runs"),
            "{name}: {}",
            exception[0]
        );
        assert!(
            cleared
                .iter()
                .any(|l| l.contains("exception=stalled after=") && l.ends_with("the loop ended")),
            "{name}: {cleared:#?}"
        );
        assert_eq!(
            told,
            [
                "own-work:some",
                "exception:stalled",
                "unread:none",
                "exception:none"
            ],
            "{name}: the host hears each rung once, and its end"
        );
        assert!(!typed, "{name}: the ladder types nothing\n{lines:#?}");
    }
    for (name, program, busy, live) in [
        (
            "agy's spinner over its task",
            "agy",
            screen(a::BUSY_THINKING),
            screen(a::BG_BUSY),
        ),
        (
            "Muse Code's status row over its shell",
            "muse",
            screen(u::BUSY_1_4_10),
            screen(u::BUSY_WORK_RUNNING),
        ),
    ] {
        let ([stuck, stalled, exception, _], told, _, lines) = run(program, busy, live);
        assert!(
            stuck.is_empty() && stalled.is_empty() && exception.is_empty() && told.is_empty(),
            "{name} climbs nothing: {stuck:#?} {stalled:#?} {told:#?}\n{lines:#?}"
        );
    }
}

#[test]
fn the_canarys_words_on_agy_or_muse_must_stand_still() {
    use crate::supervise::fx::{ClockSource, Fx, VirtualClock};
    use aterm_phase::muse::fixtures as u;
    use aterm_phase::prompt::fixtures::screen;
    let unit = Duration::from_secs(30 * 60);
    let mut m = Mock::new(vec![busy_screen()]);
    m.program = "muse";
    let mut s = session(&mut m, None);
    s.program = Some("muse".to_string());
    s.set_fx(Fx::over(ClockSource::Virtual(Arc::new(VirtualClock::new(
        Instant::now(),
        1_790_000_000_000,
    )))));
    s.setup.stuck_after = unit;
    let read = |rows: Vec<String>| Screen {
        rows,
        ..Screen::default()
    };
    let work = screen(u::END_OF_TURN_BG_RUNNING);
    let reply_row = work
        .iter()
        .rposition(|r| r.starts_with("◆ Tool call done."))
        .expect("the reply");
    let mut other = work.clone();
    other[reply_row] = "◆ Started the server on port 3000.".to_string();
    other[reply_row + 1] = String::new();
    let branch = work
        .iter()
        .rposition(|r| r.starts_with("└ "))
        .expect("the branch");
    let mut ticked = work.clone();
    ticked[branch] = ticked[branch].replace("  2s", " 41m");
    assert_ne!(ticked, work, "the panel's clock ticked");
    let mut heard = Heard::default();
    let since = Some(s.fx.mono());
    let stuck = |heard: &Heard| {
        heard
            .noted
            .iter()
            .filter(|l| l.starts_with("STUCK"))
            .count()
    };
    s.stuck_busy_note(&read(work.clone()), since, &mut heard);
    assert!(s.stuck_still.is_some(), "the words start a clock");
    s.fx.sleep(unit / 2);
    s.stuck_busy_note(&read(other.clone()), since, &mut heard);
    s.fx.sleep(unit / 2 + Duration::from_secs(1));
    s.stuck_busy_note(&read(other.clone()), since, &mut heard);
    assert_eq!(
        stuck(&heard),
        0,
        "a unit from the words' move, not the episode's start"
    );
    s.fx.sleep(unit / 2);
    s.stuck_busy_note(&read(other.clone()), since, &mut heard);
    assert_eq!(
        stuck(&heard),
        1,
        "a unit of words that stood still: {:?}",
        heard.noted
    );
    assert!(
        heard.noted[0].contains("(background work running)"),
        "{:?}",
        heard.noted
    );
    let mut ticking = other.clone();
    ticking[branch] = ticked[branch].clone();
    let held = s.stuck_still.clone();
    s.stuck_busy_note(&read(ticking), since, &mut heard);
    assert_eq!(
        s.stuck_still, held,
        "the panel's clock keeps the words' clock"
    );
    assert_eq!(
        s.stuck_rung,
        super::super::wall_memory::StuckRung::Said,
        "the ladder stands"
    );
    let mut monitor = other.clone();
    monitor[branch] = "└ ◇ Watch the lab socket  watching  14s".to_string();
    assert_eq!(
        s.reader(&monitor).background_wait(&monitor),
        Some("a monitor watching")
    );
    s.stuck_busy_note(&read(monitor), since, &mut heard);
    let words = |still: &Option<(String, Instant)>| still.as_ref().map(|(w, _)| w.clone());
    assert_ne!(
        words(&s.stuck_still),
        words(&held),
        "the work named is part of the words"
    );
    assert_eq!(
        s.stuck_rung,
        super::super::wall_memory::StuckRung::Quiet,
        "the ladder starts over"
    );
    let mut over = other.clone();
    over[branch] = format!(
        "└ ◆ Q3 background wait  ran 1s, exit 0{}1m 40s",
        " ".repeat(100)
    );
    s.stuck_busy_note(&read(over), since, &mut heard);
    assert_eq!(s.stuck_still, None);
    assert_eq!(s.stuck_rung, super::super::wall_memory::StuckRung::Quiet);
    s.program = Some("codex".to_string());
    let mut codex = Heard::default();
    s.stuck_busy_note(&read(work.clone()), since, &mut codex);
    s.fx.sleep(unit * 7);
    s.stuck_busy_note(&read(work), since, &mut codex);
    assert!(codex.noted.is_empty(), "{:?}", codex.noted);
    assert_eq!(s.stuck_still, None);
}

#[test]
fn the_canarys_words_moving_after_its_exception_clear_it() {
    use super::super::wall_memory::StuckRung;
    use crate::supervise::fx::{ClockSource, Fx, VirtualClock};
    use aterm_phase::muse::fixtures as u;
    use aterm_phase::prompt::fixtures::screen;
    let unit = Duration::from_secs(30 * 60);
    let host = Arc::new(LadderHost::default());
    let mut m = Mock::new(vec![busy_screen()]);
    m.program = "muse";
    let mut s = session(&mut m, None);
    s.program = Some("muse".to_string());
    s.stall_host = Some(Arc::clone(&host) as Arc<dyn IdleHost>);
    s.set_fx(Fx::over(ClockSource::Virtual(Arc::new(VirtualClock::new(
        Instant::now(),
        1_790_000_000_000,
    )))));
    s.setup.stuck_after = unit;
    let read = |rows: Vec<String>| Screen {
        rows,
        ..Screen::default()
    };
    let work = screen(u::END_OF_TURN_BG_RUNNING);
    let caret = work
        .iter()
        .rposition(|r| r.trim_end() == "❯")
        .expect("the empty caret row");
    let mut drafted = work.clone();
    drafted[caret] = "❯ deploy it now".to_string();
    assert_eq!(
        s.reader(&drafted).composer(&drafted).map(|(_, d)| d),
        Some(vec!["deploy it now".to_string()]),
        "the draft is read"
    );
    assert_eq!(
        s.reader(&drafted).background_wait(&drafted),
        Some("background work running"),
        "the work still runs under the draft"
    );
    let lines = |heard: &Heard, head: &str| -> Vec<String> {
        heard
            .noted
            .iter()
            .filter(|l| l.starts_with(head))
            .cloned()
            .collect()
    };
    let mut heard = Heard::default();
    let since = Some(s.fx.mono());
    s.stuck_busy_note(&read(work.clone()), since, &mut heard);
    for (after, rung) in [
        (unit, StuckRung::Said),
        (unit * 2, StuckRung::Told),
        (unit * 3, StuckRung::Raised),
    ] {
        s.fx.sleep(after);
        s.stuck_busy_note(&read(work.clone()), since, &mut heard);
        assert_eq!(s.stuck_rung, rung, "{:#?}", heard.noted);
    }
    let stalled = lines(&heard, "STALLED");
    assert_eq!(stalled.len(), 1, "{:#?}", heard.noted);
    assert!(
        stalled[0].contains("background work of the agent's own that has not ended")
            && !stalled[0].contains("does not classify"),
        "the work's words: {}",
        stalled[0]
    );
    let exception = lines(&heard, "EXCEPTION");
    assert!(
        exception.len() == 1
            && exception[0].contains("kind=stalled when=later")
            && exception[0].contains("its own background work still runs"),
        "{exception:#?}"
    );
    assert_eq!(
        *host.told.lock().unwrap(),
        ["own-work:some", "exception:stalled"],
        "the record is the work's"
    );
    s.fx.sleep(Duration::from_secs(60));
    s.stuck_busy_note(&read(drafted.clone()), since, &mut heard);
    let cleared = lines(&heard, "CLEARED");
    assert!(
        cleared.len() == 1
            && cleared[0].contains("exception=stalled after=60s")
            && cleared[0].ends_with("the agent's words moved"),
        "{cleared:#?}"
    );
    assert_eq!(
        *host.told.lock().unwrap(),
        [
            "own-work:some",
            "exception:stalled",
            "unread:none",
            "exception:none"
        ],
        "the host hears the record and the exception end"
    );
    assert_eq!(s.stuck_rung, StuckRung::Quiet);
    s.fx.sleep(unit - Duration::from_secs(1));
    s.stuck_busy_note(&read(drafted.clone()), since, &mut heard);
    assert_eq!(lines(&heard, "STUCK").len(), 1, "short of the unit");
    assert_eq!(s.stuck_rung, StuckRung::Quiet);
    s.fx.sleep(Duration::from_secs(1));
    s.stuck_busy_note(&read(drafted), since, &mut heard);
    assert_eq!(
        lines(&heard, "STUCK").len(),
        2,
        "a unit from the move: {:#?}",
        heard.noted
    );
    assert_eq!(s.stuck_rung, StuckRung::Said);
}

#[test]
fn a_dialog_no_reader_names_climbs_to_a_record_and_a_line_and_its_end_withdraws_both() {
    let notice = crate::supervise::prompt::fixtures::screen(
        crate::supervise::prompt::fixtures::AUTO_CLASSIFIER_BILLING_NOTICE_MEASURED,
    );
    let run = |title: &str, program: &'static str, screen: Vec<String>, never_still: bool| {
        let (dir, path) = journal_file("dialog-ladder");
        let host = Arc::new(LadderHost::default());
        let mut m = Mock::new(vec![busy_screen(), screen]);
        m.title = Some(title.to_string());
        m.program = program;
        m.vanish_after = Some(14);
        m.stall_sleep = Some(Duration::from_millis(25));
        m.idle_never = never_still;
        let opts = SuperviseOpts {
            journal: Some(path.clone()),
            idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
            ..hosted(60)
        };
        let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
            s.setup.stuck_after = Duration::from_millis(90);
        });
        let (records, _) = journal_records(&path);
        let _ = std::fs::remove_dir_all(&dir);
        let kind = |k: &str| -> Vec<String> {
            records
                .iter()
                .filter(|r| r.line.starts_with(k))
                .map(|r| r.line.clone())
                .collect()
        };
        (
            kind("STUCK"),
            kind("STALLED"),
            kind("EXCEPTION"),
            kind("CLEARED"),
            host.told.lock().unwrap().clone(),
            count(&m, "turn"),
            lines,
        )
    };
    for never_still in [false, true] {
        let (stuck, stalled, exception, cleared, told, turns, lines) =
            run("✳ Claude Code", "claude", notice.clone(), never_still);
        let said = format!("never_still={never_still}\n{lines:#?}");
        assert_eq!(stuck.len(), 1, "rung 1 once: {stuck:#?}\n{said}");
        assert!(
            stuck[0].ends_with("Enter to continue · Esc to cancel"),
            "the rows that teach a reader: {}",
            stuck[0]
        );
        assert_eq!(stalled.len(), 1, "rung 2 once: {stalled:#?}\n{said}");
        assert_eq!(exception.len(), 1, "rung 3 once: {exception:#?}\n{said}");
        assert!(
            exception[0].contains("kind=dialog when=later"),
            "{}",
            exception[0]
        );
        assert!(
            cleared
                .iter()
                .any(|l| l.contains("exception=dialog after=") && l.ends_with("the loop ended")),
            "{cleared:#?}"
        );
        assert_eq!(
            told,
            [
                "unread:some",
                "exception:dialog",
                "unread:none",
                "exception:none"
            ],
            "the host hears each rung once, and its end\n{said}"
        );
        assert_eq!(turns, 0, "the canary types nothing");
    }
    for (name, title, program, over_a_composer) in [
        ("an animated title", "◐ Claude Code", "claude", false),
        ("an idle composer", "✳ Claude Code", "claude", true),
        ("a shell under a stale title", "✳ Claude Code", "zsh", false),
    ] {
        let mut screen = notice.clone();
        if over_a_composer {
            screen.extend(composer("  ? for shortcuts"));
        }
        let (stuck, stalled, exception, _, told, _, lines) = run(title, program, screen, false);
        assert!(
            stuck.is_empty() && stalled.is_empty() && exception.is_empty() && told.is_empty(),
            "{name}: {stuck:#?} {stalled:#?} {exception:#?} {told:#?}\n{lines:#?}"
        );
    }
    let mut m = Mock::new(vec![busy_screen()]);
    let mut s = session(&mut m, None);
    let screen = |rows: Vec<String>| Screen {
        rows,
        ..Screen::default()
    };
    let at = |rows: &[String], what: &str| {
        rows.iter()
            .position(|r| r.contains(what))
            .unwrap_or_else(|| panic!("{what}"))
    };
    let shown = |glyph: &str, secs: &str| {
        let mut r = notice.clone();
        let (tool, line) = (at(&r, "⏺ Bash"), at(&r, "Nothing breaks"));
        r[tool] = r[tool].replacen('⏺', glyph, 1);
        r[line].push_str(&format!(" ({secs}s)"));
        r
    };
    let mut longer = notice.clone();
    longer.insert(
        at(&notice, "Enter to continue"),
        "One more thing: it needs a login.".to_string(),
    );
    let still = s.dialog_words(&screen(shown("⏺", "12")));
    assert!(still.is_some(), "the notice has words");
    assert_eq!(still, s.dialog_words(&screen(shown("·", "13"))));
    assert_ne!(still, s.dialog_words(&screen(longer.clone())));
    assert_eq!(s.dialog_words(&screen(idle_screen())), None);
    assert_eq!(s.dialog_words(&screen(busy_screen())), None);
    let mut heard = Heard::default();
    s.stuck_rung = super::super::wall_memory::StuckRung::Said;
    s.dialog_note(&screen(shown("⏺", "12")), &mut heard);
    let began = s.stuck_dialog.clone().expect("the notice starts a clock");
    assert_eq!(
        s.stuck_rung,
        super::super::wall_memory::StuckRung::Quiet,
        "what the busy flavour's ladder held ends at a dialog's first sight"
    );
    std::thread::sleep(Duration::from_millis(2));
    s.dialog_note(&screen(shown("·", "13")), &mut heard);
    assert_eq!(
        s.stuck_dialog,
        Some(began.clone()),
        "a tick keeps the clock"
    );
    s.dialog_note(&screen(longer), &mut heard);
    assert!(
        s.stuck_dialog.as_ref().is_some_and(|(_, at)| *at > began.1),
        "other words start it afresh"
    );
    s.stuck_busy_note(&screen(memory_scrolled()), Some(Instant::now()), &mut heard);
    assert_eq!(
        s.stuck_dialog, None,
        "a busy read ends it, background work alone too"
    );
    s.dialog_note(&screen(shown("⏺", "12")), &mut heard);
    s.dialog_note(&screen(idle_screen()), &mut heard);
    assert_eq!(s.stuck_dialog, None, "a screen with no words ends it");
    assert!(
        heard.said.is_empty() && heard.noted.is_empty(),
        "{:?}",
        heard.noted
    );
}

/// A host whose probe of the session's repository answers from a script
/// ([`IdleHost::repo_work`]): each ask takes the next answer, the last one
/// standing; how often it was asked is counted.
#[derive(Debug)]
struct RepoHost {
    found: std::sync::Mutex<VecDeque<Option<RepoWork>>>,
    asked: std::sync::atomic::AtomicUsize,
}

impl RepoHost {
    fn new(found: Vec<Option<RepoWork>>) -> Self {
        Self {
            found: std::sync::Mutex::new(found.into()),
            asked: std::sync::atomic::AtomicUsize::new(0),
        }
    }
}

impl IdleHost for RepoHost {
    fn wants(&self) -> bool {
        false
    }
    fn at_idle(&self) -> Option<HostStep> {
        None
    }
    fn owns_turn_end(&self) -> bool {
        false
    }
    fn repo_work(&self) -> Option<RepoWork> {
        self.asked.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let mut found = self.found.lock().unwrap_or_else(PoisonError::into_inner);
        if found.len() > 1 {
            found.pop_front().flatten()
        } else {
            found.front().cloned().flatten()
        }
    }
}

#[test]
fn a_claim_over_uncommitted_work_gets_the_repo_suffix_through_the_loop() {
    let report = |lines: &[&str]| {
        let mut r = vec![format!("⏺ {}", lines[0])];
        r.extend(lines[1..].iter().map(|l| format!("  {l}")));
        r.extend(rows(&["", "✻ Worked for 12m 2s · done 4:24 PM", ""]));
        r.extend(composer("  ⏵⏵ bypass permissions on (shift+tab to cycle)"));
        r
    };
    let claim = report(&["Built, tested and landed the parser. All done; nothing left to do."]);
    let dirty = RepoWork {
        repo: "/Users//a/Developer/parser".to_string(),
        changed: 1,
        paths: vec!["src/lexer.rs".to_string()],
        ..RepoWork::default()
    };
    let run = |tag: &str, screens: Vec<Vec<String>>, found: Vec<Option<RepoWork>>| {
        let (dir, journal) = journal_file(tag);
        let host = Arc::new(RepoHost::new(found));
        let mut m = Mock::new(screens);
        m.turn_releases = Some(1);
        m.vanish_after = Some(2);
        let opts = SuperviseOpts {
            policy: SupervisorConfig::default(),
            journal: Some(journal.clone()),
            idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
            ..auto(30)
        };
        let (lines, _) = watch_lines(&mut m, &opts);
        let done: Vec<String> = journal_records(&journal)
            .0
            .into_iter()
            .map(|r| r.line)
            .filter(|l| l.starts_with("DONE "))
            .collect();
        let _ = std::fs::remove_dir_all(&dir);
        let asked = host.asked.load(std::sync::atomic::Ordering::SeqCst);
        let turns: Vec<String> = m
            .requests
            .iter()
            .filter(|r| r.starts_with("turn "))
            .cloned()
            .collect();
        (turns, lines, done, asked)
    };
    let suffix = repo_work_suffix(&dirty);
    assert!(
        suffix.contains("/Users//a/Developer/parser: 1 changed (src/lexer.rs)"),
        "{suffix}"
    );
    let (turns, lines, done, asked) = run(
        "claim-dirty",
        vec![busy_screen(), claim.clone()],
        vec![Some(dirty.clone())],
    );
    assert_eq!(
        turns.first(),
        Some(&continue_request(&format!("{DONE_CHECK} {suffix}"))),
        "the check with the repository's suffix: {lines:#?}"
    );
    assert!(asked >= 1, "the probe was asked at the claim");
    assert!(done.is_empty(), "{done:#?}");
    let (turns, lines, done, asked) = run("claim-clean", vec![busy_screen(), claim], vec![None]);
    assert_eq!(
        turns.first(),
        Some(&continue_request(DONE_CHECK)),
        "the plain check: {lines:#?}"
    );
    assert!(asked >= 1, "the probe was asked at the claim");
    assert!(done.is_empty(), "{done:#?}");
    let progress = report(&["Stage 2 landed; the parser is next."]);
    let (turns, lines, done, asked) = run(
        "claim-progress",
        vec![busy_screen(), progress],
        vec![Some(dirty)],
    );
    assert_eq!(asked, 0, "no done said, no probe: {lines:#?}");
    assert!(done.is_empty(), "{done:#?}");
    assert!(
        !turns.is_empty() && turns.iter().all(|t| !t.contains(&suffix)),
        "continued, no suffix: {turns:#?}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.contains(&format!("rule={RULE_CONTINUE} "))),
        "{lines:#?}"
    );
}
#[test]
fn a_persons_greeting_left_to_them_stays_theirs_in_the_next_loop() {
    let greeting = "Hi! What would you like me to work on?";
    let quick = |said: &str| {
        let mut r = rows(&[&format!("⏺ {said}"), ""]);
        r.extend(composer("  ⏵⏵ bypass permissions on (shift+tab to cycle)"));
        r
    };
    let opts = SuperviseOpts {
        policy: SupervisorConfig {
            human_grace_s: 0,
            ..owners_policy()
        },
        ..auto(1)
    };
    let run = |ledger: &std::path::Path, person: bool| {
        let mut m = Mock::new(vec![busy_screen(), quick(greeting)]);
        m.turn_releases = Some(1);
        m.vanish_after = Some(8);
        if person {
            m.human = vec!["2000"];
        }
        let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
            s.set_turn_end_timing(TurnEndTiming {
                short_backoff: Duration::from_millis(30),
                short_backoff_max: Duration::from_millis(60),
                ..TurnEndTiming::default()
            });
            s.set_approval_ledger(Some(ledger.to_path_buf()));
        });
        (count(&m, "turn"), lines)
    };
    let (dir, ledger) = ledger_at("left-next-loop");
    let (turns, lines) = run(&ledger, true);
    assert_eq!(turns, 0, "{lines:#?}");
    let body = std::fs::read_to_string(&ledger).unwrap_or_default();
    let left: Vec<&str> = body
        .lines()
        .filter(|l| l.contains(&format!("\"rule_id\":\"{RULE_LEFT}\"")))
        .collect();
    assert_eq!(left.len(), 1, "{body}");
    assert!(left[0].contains("\"decision\":\"skipped\""), "{body}");
    let (turns, lines) = run(&ledger, false);
    assert_eq!(turns, 0, "the next loop: {lines:#?}");
    assert!(
        !lines
            .iter()
            .any(|l| l.contains(&format!("rule={RULE_DONE_CHECK} "))),
        "{lines:#?}"
    );
    let closed = format!(
        "{}\n{}\n",
        body.trim_end(),
        approvals::Row {
            rule_id: RULE_CONTINUE,
            outcome: approvals::Outcome::Typed,
            command: "keep going",
            reason: "the turn-end policy",
            box_seq: 1,
        }
        .to_json(i64::MAX / 2, None)
    );
    std::fs::write(&ledger, closed).expect("ledger");
    let (_, lines) = run(&ledger, false);
    assert!(
        lines
            .iter()
            .any(|l| l.contains(&format!("rule={RULE_DONE_CHECK} "))),
        "a typed act closed it: {lines:#?}"
    );
    std::fs::write(&ledger, "").expect("ledger");
    let (_, lines) = run(&ledger, false);
    assert!(
        lines
            .iter()
            .any(|l| l.contains(&format!("rule={RULE_DONE_CHECK} "))),
        "no row: {lines:#?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A host whose transcript LAGS the screen at a decision's first read
/// ([`IdleHost::transcript_tail`]: `fresh`) and has caught up when read
/// again for that lag ([`IdleHost::transcript_tail_again`]: `again`).
#[derive(Debug)]
struct LagHost {
    fresh: String,
    again: String,
}

impl IdleHost for LagHost {
    fn wants(&self) -> bool {
        false
    }
    fn at_idle(&self) -> Option<HostStep> {
        None
    }
    fn owns_turn_end(&self) -> bool {
        false
    }
    fn transcript_tail(&self) -> Option<String> {
        Some(self.fresh.clone())
    }
    fn transcript_tail_again(&self) -> Option<String> {
        Some(self.again.clone())
    }
}

#[test]
fn a_persons_exchange_is_left_under_its_prompts_key() {
    let greeting = "Hi! What would you like me to work on?";
    let prompt = r#"{"isSidechain":false,"type":"user","uuid":"u-1","promptId":"p-1","message":{"role":"user","content":"hi"},"origin":{"kind":"human"},"promptSource":"typed"}"#;
    let call = r#"{"isSidechain":false,"type":"assistant","uuid":"a-0","message":{"model":"claude-opus-5-5","role":"assistant","content":[{"type":"tool_use","id":"t-1","name":"Read","input":{"file_path":"/p/README.md"}}]}}"#;
    let result = r#"{"isSidechain":false,"type":"user","uuid":"r-1","promptId":"p-1","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t-1","content":"ok"}]}}"#;
    let answer = format!(
        r#"{{"isSidechain":false,"type":"assistant","uuid":"a-1","message":{{"model":"claude-opus-5-5","role":"assistant","content":[{{"type":"text","text":"{greeting}"}}]}}}}"#
    );
    let full = format!("{prompt}\n{call}\n{result}\n{answer}\n");
    let quick = || {
        let mut r = rows(&[&format!("⏺ {greeting}"), ""]);
        r.extend(composer("  ⏵⏵ bypass permissions on (shift+tab to cycle)"));
        r
    };
    let run = |ledger: &std::path::Path, host: LagHost, person: bool| {
        let mut m = Mock::new(vec![busy_screen(), quick()]);
        m.turn_releases = Some(1);
        m.vanish_after = Some(8);
        if person {
            m.human = vec!["2000"];
        }
        let opts = SuperviseOpts {
            policy: SupervisorConfig {
                human_grace_s: 0,
                ..owners_policy()
            },
            idle_host: Some(Arc::new(host) as Arc<dyn IdleHost>),
            ..auto(1)
        };
        let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
            s.set_turn_end_timing(TurnEndTiming {
                short_backoff: Duration::from_millis(30),
                short_backoff_max: Duration::from_millis(60),
                ..TurnEndTiming::default()
            });
            s.set_approval_ledger(Some(ledger.to_path_buf()));
        });
        (count(&m, "turn"), lines)
    };
    let left_keys = |ledger: &std::path::Path| -> Vec<String> {
        std::fs::read_to_string(ledger)
            .unwrap_or_default()
            .lines()
            .filter(|l| l.contains(&format!("\"rule_id\":\"{RULE_LEFT}\"")))
            .filter_map(|l| aterm_json::from_str::<aterm_json::Value>(l).ok())
            .filter_map(|v| v.get("command").and_then(|c| c.as_str()).map(str::to_owned))
            .collect()
    };
    let host = |tail: &str| LagHost {
        fresh: tail.to_string(),
        again: tail.to_string(),
    };
    let checked = |lines: &[String]| {
        lines
            .iter()
            .any(|l| l.contains(&format!("rule={RULE_DONE_CHECK} ")))
    };
    let (dir, ledger) = ledger_at("left-under-prompt");
    for (what, tail) in [
        ("the prompt alone, re-read alike", format!("{prompt}\n")),
        (
            "a tool's call after it",
            format!("{prompt}\n{call}\n{result}\n"),
        ),
        ("caught up", full.clone()),
    ] {
        std::fs::write(&ledger, "").expect("ledger");
        let (turns, lines) = run(&ledger, host(&tail), true);
        assert_eq!(turns, 0, "{what}: {lines:#?}");
        assert_eq!(left_keys(&ledger), ["prompt:u-1"], "{what}: {lines:#?}");
        let (turns, lines) = run(&ledger, host(&full), false);
        assert_eq!(turns, 0, "{what}, the next loop: {lines:#?}");
        assert!(!checked(&lines), "{what}, the next loop: {lines:#?}");
    }
    let row = |key: &str| {
        approvals::Row {
            rule_id: RULE_LEFT,
            outcome: approvals::Outcome::Skipped,
            command: key,
            reason: approvals::LEFT_REASON,
            box_seq: 1,
        }
        .to_json(i64::MAX / 2, None)
            + "\n"
    };
    std::fs::write(&ledger, row("answer:a-1")).expect("ledger");
    let (turns, lines) = run(&ledger, host(&full), false);
    assert_eq!(turns, 0, "an earlier build's row: {lines:#?}");
    assert!(!checked(&lines), "an earlier build's row: {lines:#?}");
    std::fs::write(&ledger, row("prompt:u-0")).expect("ledger");
    let (_, lines) = run(&ledger, host(&full), false);
    assert!(checked(&lines), "another prompt's row: {lines:#?}");
    let wake = r#"{"isSidechain":false,"type":"user","uuid":"w-1","promptId":"p-wake","isMeta":true,"promptSource":"system","scheduledTaskId":"t1","message":{"role":"user","content":"check the build"}}"#;
    let woke = format!(
        "{full}{wake}\n{}\n",
        answer.replace(r#""uuid":"a-1""#, r#""uuid":"a-2""#)
    );
    std::fs::write(&ledger, row("prompt:u-1")).expect("ledger");
    let (_, lines) = run(&ledger, host(&woke), false);
    assert!(checked(&lines), "a wake answered since: {lines:#?}");
    let caveat = r#"{"isSidechain":false,"type":"user","uuid":"c-0","promptId":"p-2","isMeta":true,"message":{"role":"user","content":"<local-command-caveat>Caveat: The messages below were generated by the user while running local commands. DO NOT respond to these messages or otherwise consider them in your response unless the user explicitly asks you to.</local-command-caveat>"}}"#;
    let config = r#"{"isSidechain":false,"type":"user","uuid":"c-1","promptId":"p-2","message":{"role":"user","content":"<command-name>/config</command-name>\n            <command-message>config</command-message>\n            <command-args></command-args>"}}"#;
    let config_out = r#"{"isSidechain":false,"type":"user","uuid":"c-2","promptId":"p-2","message":{"role":"user","content":"<local-command-stdout>Disabled auto-compact</local-command-stdout>"}}"#;
    let configured = format!("{full}{caveat}\n{config}\n{config_out}\n");
    std::fs::write(&ledger, row("prompt:u-1")).expect("ledger");
    let (turns, lines) = run(&ledger, host(&configured), false);
    assert_eq!(turns, 0, "/config after the exchange: {lines:#?}");
    assert!(!checked(&lines), "/config after the exchange: {lines:#?}");
    let init = r#"{"isSidechain":false,"type":"user","uuid":"i-1","promptId":"p-5","message":{"role":"user","content":"<command-message>init is analyzing your codebase…</command-message>\n<command-name>/init</command-name>"},"origin":{"kind":"human"}}"#;
    let expansion = r#"{"isSidechain":false,"type":"user","uuid":"i-2","promptId":"p-5","isMeta":true,"message":{"role":"user","content":[{"type":"text","text":"Please analyze this codebase and create a CLAUDE.md file"}]}}"#;
    let asked = format!("{init}\n{expansion}\n");
    std::fs::write(&ledger, "").expect("ledger");
    let (turns, lines) = run(&ledger, host(&asked), true);
    assert_eq!(turns, 0, "/init, unanswered on the record: {lines:#?}");
    assert_eq!(left_keys(&ledger), ["prompt:i-1"], "/init: {lines:#?}");
    let (turns, lines) = run(&ledger, host(&format!("{asked}{answer}\n")), false);
    assert_eq!(turns, 0, "/init, the next loop: {lines:#?}");
    assert!(!checked(&lines), "/init, the next loop: {lines:#?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_persons_exchange_an_older_build_noted_left_stays_theirs_in_the_next_loop() {
    use crate::supervise::journal::JournalRecord;
    let greeting = "Hi! What would you like me to work on?";
    let opened_s: i64 = 1_790_856_000;
    let noted_ms = (opened_s + 180) * 1000;
    let prompt = |stamp: Option<&str>| {
        let at = stamp.map_or_else(String::new, |t| format!(r#","timestamp":"{t}""#));
        format!(
            r#"{{"isSidechain":false,"type":"user","uuid":"u-1","promptId":"p-1"{at},"message":{{"role":"user","content":"hi"}},"origin":{{"kind":"human"}},"promptSource":"typed"}}"#
        )
    };
    let answer = format!(
        r#"{{"isSidechain":false,"type":"assistant","uuid":"a-1","message":{{"model":"claude-opus-5-5","role":"assistant","content":[{{"type":"text","text":"{greeting}"}}]}}}}"#
    );
    let tail = |stamp: Option<&str>| format!("{}\n{answer}\n", prompt(stamp));
    let quick = || {
        let mut r = rows(&[&format!("⏺ {greeting}"), ""]);
        r.extend(composer("  ⏵⏵ bypass permissions on (shift+tab to cycle)"));
        r
    };
    let run = |ledger: &std::path::Path, tail: String| {
        let mut m = Mock::new(vec![busy_screen(), quick()]);
        m.turn_releases = Some(1);
        m.vanish_after = Some(8);
        let opts = SuperviseOpts {
            policy: SupervisorConfig {
                human_grace_s: 0,
                ..owners_policy()
            },
            idle_host: Some(Arc::new(LagHost {
                fresh: tail.clone(),
                again: tail,
            }) as Arc<dyn IdleHost>),
            ..auto(1)
        };
        let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
            s.set_turn_end_timing(TurnEndTiming {
                short_backoff: Duration::from_millis(30),
                short_backoff_max: Duration::from_millis(60),
                ..TurnEndTiming::default()
            });
            s.set_approval_ledger(Some(ledger.to_path_buf()));
        });
        (count(&m, "turn"), lines)
    };
    let checked = |lines: &[String]| {
        lines
            .iter()
            .any(|l| l.contains(&format!("rule={RULE_DONE_CHECK} ")))
    };
    let note = |at: i64, line: &str| JournalRecord::of_line(at, None, line, None).to_json() + "\n";
    let left = note(
        noted_ms,
        &format!(
            "LEFT seq=7 rule={RULE_LEFT} a person's own exchange (their message, answered \
             within 10 min): nothing is typed until the next turn ends"
        ),
    );
    let (dir, ledger) = ledger_at("left-older-note");
    let journal = approvals::journal_beside(&ledger);
    std::fs::write(&ledger, "").expect("ledger");
    std::fs::write(&journal, &left).expect("journal");
    let (turns, lines) = run(&ledger, tail(Some("2026-10-01T12:00:00.000Z")));
    assert_eq!(turns, 0, "{lines:#?}");
    assert!(!checked(&lines), "{lines:#?}");
    let fresh = |journal_text: Option<&str>| {
        std::fs::write(&ledger, "").expect("ledger");
        let _ = std::fs::remove_file(&journal);
        if let Some(text) = journal_text {
            std::fs::write(&journal, text).expect("journal");
        }
    };
    fresh(Some(&left));
    let (_, lines) = run(&ledger, tail(Some("2026-10-01T12:04:00.000Z")));
    assert!(checked(&lines), "a turn opened since: {lines:#?}");
    let acted = format!(
        "{left}{}",
        note(
            noted_ms + 60_000,
            "CONTINUED seq=9 rule=continue@v1 keep going"
        )
    );
    fresh(Some(&acted));
    let (_, lines) = run(&ledger, tail(Some("2026-10-01T12:00:00.000Z")));
    assert!(checked(&lines), "an act since: {lines:#?}");
    let ended = format!(
        "{left}{}",
        note(
            noted_ms + 60_000,
            "DONE seq=9 rule=continue-done-check@v1 the worker said it is done after the done \
             check: nothing more is typed until a person or an orchestrator types"
        )
    );
    fresh(Some(&ended));
    let (_, lines) = run(&ledger, tail(Some("2026-10-01T12:00:00.000Z")));
    assert!(checked(&lines), "a task's end since: {lines:#?}");
    fresh(Some(&left));
    let (_, lines) = run(&ledger, tail(None));
    assert!(checked(&lines), "a prompt with no time: {lines:#?}");
    fresh(None);
    let (_, lines) = run(&ledger, tail(Some("2026-10-01T12:00:00.000Z")));
    assert!(checked(&lines), "no journal: {lines:#?}");
    let done_before = note(
        (opened_s - 300) * 1000,
        "DONE seq=5 rule=finished@v1 the worker reported ATERM-TASK-DONE: its task is \
         finished; nothing more is typed until a person or an orchestrator types",
    );
    fresh(Some(&done_before));
    let (_, lines) = run(&ledger, tail(Some("2026-10-01T12:00:00.000Z")));
    assert!(checked(&lines), "a task's end noted before: {lines:#?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_persons_exchange_at_a_wall_stays_theirs_in_the_next_loop() {
    use crate::supervise::policy::turn_end::{RULE_ANSWER, RULE_API_RETRY};
    let tail = include_str!("fixtures/claude-2.1.294-transcript-529.jsonl");
    let greeting = "Hello! How can I help?";
    let answer = format!(
        r#"{{"isSidechain":false,"type":"assistant","uuid":"a-9","message":{{"model":"claude-opus-5-5","role":"assistant","content":[{{"type":"text","text":"{greeting}"}}]}}}}"#
    );
    let answered = format!("{tail}{answer}\n");
    let wall = || {
        let mut r = rows(&[
            "❯ lag-e5",
            "  ⎿  API Error: Repeated 529 Overloaded errors. The API is at capacity — this is \
             usually temporary. Try again in a moment. If it persists, check your inference \
             gateway (127.0.0.1:61336).",
            "",
            "✻ Churned for 3s · done 9:59 AM",
            "",
        ]);
        r.extend(composer("  ⏵⏵ bypass permissions on (shift+tab to cycle)"));
        r
    };
    let quick = || {
        let mut r = rows(&[&format!("⏺ {greeting}"), ""]);
        r.extend(composer("  ⏵⏵ bypass permissions on (shift+tab to cycle)"));
        r
    };
    let run = |ledger: &std::path::Path, m: &mut Mock, backoff: Duration| {
        let opts = SuperviseOpts {
            policy: SupervisorConfig {
                human_grace_s: 0,
                ..owners_policy()
            },
            idle_host: Some(Arc::new(LagHost {
                fresh: tail.to_string(),
                again: answered.clone(),
            }) as Arc<dyn IdleHost>),
            ..auto(1)
        };
        let (lines, _) = watch_lines_with(m, &opts, |s| {
            s.set_turn_end_timing(TurnEndTiming {
                retry_backoff: vec![backoff; 3],
                short_backoff: Duration::from_millis(30),
                short_backoff_max: Duration::from_millis(60),
                ..TurnEndTiming::default()
            });
            s.set_approval_ledger(Some(ledger.to_path_buf()));
        });
        lines
    };
    let (dir, ledger) = ledger_at("left-at-wall");
    let mut m = Mock::new(vec![busy_screen(), wall()]);
    m.vanish_after = Some(3);
    m.human = vec!["2000"];
    let lines = run(&ledger, &mut m, Duration::from_secs(600));
    assert_eq!(count(&m, "turn"), 0, "{lines:#?}");
    let body = std::fs::read_to_string(&ledger).unwrap_or_default();
    let walled: Vec<&str> = body
        .lines()
        .filter(|l| l.contains(&format!("\"rule_id\":\"{RULE_LEFT}\"")))
        .collect();
    assert_eq!(walled.len(), 1, "{body}\n{lines:#?}");
    const PROMPT: &str = "prompt:6277d654-73c7-45e4-9dac-9971bc9c1e8e";
    let head = format!("\"command\":\"{}{PROMPT}@", approvals::LEFT_AT_WALL);
    let at = walled[0].find(&head).expect(body.as_str()) + head.len() - 1;
    let end = at + walled[0][at..].find('"').expect("the key's end");
    assert!(
        walled[0][at + 1..end].parse::<u64>().is_ok(),
        "the work in seconds: {body}"
    );
    let row = walled[0].to_string();
    let carrying = |work: &str| format!("{}{work}{}\n", &row[..at], &row[end..]);
    let next = |ledger: &std::path::Path| {
        let mut m = Mock::new(vec![wall(), busy_screen(), quick()]);
        m.turn_releases = Some(0);
        m.vanish_after = Some(20);
        m.stall_sleep = Some(Duration::from_millis(20));
        let lines = run(ledger, &mut m, Duration::from_millis(50));
        (count(&m, "turn"), lines)
    };
    let typed =
        |lines: &[String], rule: &str| lines.iter().any(|l| l.contains(&format!("rule={rule} ")));
    let (turns, lines) = next(&ledger);
    assert_eq!(turns, 1, "the retry alone: {lines:#?}");
    assert!(typed(&lines, RULE_API_RETRY), "{lines:#?}");
    assert!(!typed(&lines, RULE_ANSWER), "{lines:#?}");
    std::fs::write(&ledger, carrying("")).expect("ledger");
    let (turns, lines) = next(&ledger);
    assert_eq!(turns, 1, "the retry alone: {lines:#?}");
    std::fs::write(&ledger, carrying("@600")).expect("ledger");
    let (turns, lines) = next(&ledger);
    assert_eq!(turns, 2, "{lines:#?}");
    assert!(typed(&lines, RULE_ANSWER), "{lines:#?}");
    std::fs::write(&ledger, "").expect("ledger");
    let (turns, lines) = next(&ledger);
    assert_eq!(turns, 2, "{lines:#?}");
    assert!(typed(&lines, RULE_API_RETRY), "{lines:#?}");
    assert!(typed(&lines, RULE_ANSWER), "{lines:#?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_persons_exchange_retried_at_a_wall_stays_theirs_in_the_next_loop() {
    use crate::supervise::policy::turn_end::RULE_API_RETRY;
    let tail = include_str!("fixtures/claude-2.1.294-transcript-529.jsonl");
    let greeting = "Hello! How can I help?";
    let wall = || {
        let mut r = rows(&[
            "❯ lag-e5",
            "  ⎿  API Error: Repeated 529 Overloaded errors. The API is at capacity — this is \
             usually temporary. Try again in a moment. If it persists, check your inference \
             gateway (127.0.0.1:61336).",
            "",
            "✻ Churned for 3s · done 9:59 AM",
            "",
        ]);
        r.extend(composer("  ⏵⏵ bypass permissions on (shift+tab to cycle)"));
        r
    };
    let quick = || {
        let mut r = rows(&[&format!("⏺ {greeting}"), ""]);
        r.extend(composer("  ⏵⏵ bypass permissions on (shift+tab to cycle)"));
        r
    };
    let run = |ledger: &std::path::Path, m: &mut Mock, tail: &str| {
        let opts = SuperviseOpts {
            policy: SupervisorConfig {
                human_grace_s: 0,
                ..owners_policy()
            },
            idle_host: Some(Arc::new(LagHost {
                fresh: tail.to_string(),
                again: tail.to_string(),
            }) as Arc<dyn IdleHost>),
            ..auto(60)
        };
        let (lines, _) = watch_lines_with(m, &opts, |s| {
            s.set_turn_end_timing(TurnEndTiming {
                retry_backoff: vec![Duration::from_millis(50); 3],
                short_backoff: Duration::from_millis(30),
                short_backoff_max: Duration::from_millis(60),
                take_within: Duration::from_millis(50),
                ..TurnEndTiming::default()
            });
            s.set_approval_ledger(Some(ledger.to_path_buf()));
        });
        lines
    };
    let (dir, ledger) = ledger_at("retried-at-wall");
    let mut m = Mock::new(vec![busy_screen(), wall()]);
    m.human = vec!["2000"];
    m.exit_when = Some(|m| m.turns > 0);
    let lines = run(&ledger, &mut m, tail);
    assert_eq!(count(&m, "turn"), 1, "{lines:#?}");
    let body = std::fs::read_to_string(&ledger).unwrap_or_default();
    let retry = body
        .lines()
        .filter(|l| l.contains(&format!("\"rule_id\":\"{RULE_API_RETRY}\"")))
        .filter_map(|l| aterm_json::from_str::<aterm_json::Value>(l).ok())
        .find_map(|v| v.get("command").and_then(|c| c.as_str()).map(str::to_owned))
        .expect(body.as_str());
    let open = approvals::open_left_in(&body, None).expect(body.as_str());
    assert!(open.starts_with(approvals::LEFT_RETRIED), "{open}");
    let answered = |ms: u64| {
        format!(
            "{tail}{}\n{}\n{}\n",
            aterm_json::json!({
                "isSidechain": false, "type": "user", "uuid": "r-9", "promptId": "p-9",
                "message": {"role": "user", "content": retry},
            }),
            aterm_json::json!({
                "isSidechain": false, "type": "assistant", "uuid": "a-9",
                "message": {"model": "claude-opus-5-5", "role": "assistant",
                            "content": [{"type": "text", "text": greeting}]},
            }),
            aterm_json::json!({
                "isSidechain": false, "type": "system", "subtype": "turn_duration",
                "durationMs": ms,
            }),
        )
    };
    let next = |rows: &str, record: &str, typed: bool| {
        std::fs::write(&ledger, rows).expect("ledger");
        let mut m = Mock::new(vec![quick()]);
        m.stall_sleep = Some(Duration::from_millis(5));
        if typed {
            m.exit_when = Some(|m| m.turns > 0);
        } else {
            m.vanish_after = Some(40);
        }
        let lines = run(&ledger, &mut m, record);
        (count(&m, "turn"), lines)
    };
    let (turns, lines) = next(&body, &answered(4_000), false);
    assert_eq!(turns, 0, "the retry's answer is the person's: {lines:#?}");
    let after = std::fs::read_to_string(&ledger).unwrap_or_default();
    assert_eq!(
        approvals::open_left_in(&after, None).as_deref(),
        Some("prompt:r-9"),
        "{after}\n{lines:#?}"
    );
    let (turns, lines) = next(&body, &answered(11 * 60 * 1000), true);
    assert_eq!(turns, 1, "CONTROL: a job past the bound: {lines:#?}");
    let closed = format!(
        "{body}{}\n",
        approvals::Row {
            rule_id: "continue@v1",
            outcome: approvals::Outcome::Typed,
            command: "keep going",
            reason: "",
            box_seq: 1,
        }
        .to_json(i64::MAX / 2, None)
    );
    let (turns, lines) = next(&closed, &answered(4_000), true);
    assert_eq!(
        turns, 1,
        "CONTROL: a continuation after the retry: {lines:#?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_persons_question_answered_at_length_is_left_to_them() {
    let answered = |worked: &str| {
        let mut r = rows(&[
            "❯ what do you think?",
            "",
            "  Ran 2 shell commands",
            "",
            "⏺ Keep the token: it costs nothing and survives a restart.",
            "",
            &format!("✻ Worked for {worked} · done 4:24 PM"),
            "",
        ]);
        r.extend(composer("  ⏵⏵ bypass permissions on (shift+tab to cycle)"));
        r
    };
    for (screen, human, typed) in [
        (answered("3m 2s"), "185000", false),
        (answered("12m 2s"), "725000", true),
    ] {
        let (dir, journal) = journal_file("persons-question");
        let mut m = Mock::new(vec![busy_screen(), screen]);
        m.turn_releases = Some(1);
        m.human = vec![human];
        let max = if typed {
            m.exit_when = Some(|m| m.turns > 0);
            60
        } else {
            m.vanish_after = Some(8);
            1
        };
        let opts = SuperviseOpts {
            policy: SupervisorConfig {
                human_grace_s: 0,
                ..owners_policy()
            },
            journal: Some(journal.clone()),
            ..auto(max)
        };
        let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
            s.set_turn_end_timing(TurnEndTiming {
                short_backoff: Duration::from_millis(30),
                short_backoff_max: Duration::from_millis(60),
                ..TurnEndTiming::default()
            });
        });
        assert_eq!(count(&m, "turn") > 0, typed, "{typed}: {lines:#?}");
        let left = journal_records(&journal)
            .0
            .into_iter()
            .filter(|r| r.line.starts_with("LEFT "))
            .count();
        assert_eq!(left, usize::from(!typed), "{typed}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[test]
fn claude_codes_idle_compaction_is_no_turn() {
    use aterm_phase::prompt::fixtures::{
        IDLE_COMPACTED_HINT_MEASURED, IDLE_COMPACTED_MEASURED, screen,
    };
    let measured = screen(IDLE_COMPACTED_MEASURED);
    let hinted = screen(IDLE_COMPACTED_HINT_MEASURED);
    let swap = |rows: &[String], from: &str, to: &str| -> Vec<String> {
        let at = rows.iter().position(|r| r.starts_with(from)).expect(from);
        let mut out = rows.to_vec();
        out[at] = to.to_string();
        out
    };
    let suggested = |rows: &[String]| swap(rows, "❯", "❯ keep going");
    let notice = "⏺ Compacted while idle";
    let worker = "⏺ Compacted the build logs into one file.";
    let ask = "Shall I drop the old table?";
    let greeting = "Hi! What would you like me to work on?";
    let framed = |body: &[&str]| {
        let mut r = rows(body);
        r.extend(composer("  ⏵⏵ bypass permissions on (shift+tab to cycle)"));
        r
    };
    let greeted = framed(&[&format!("⏺ {greeting}"), ""]);
    let compacted = |said: &str| {
        framed(&[
            &format!("⏺ {said}"),
            "",
            "  ⎿  Skills restored (workflow-authoring)",
            "",
            "⏺ Compacted while idle, before the prompt cache expired",
            "",
        ])
    };
    let (left, held) = (compacted(greeting), compacted(ask));
    let run = |screens: Vec<Vec<String>>| {
        vec![
            busy_screen(),
            screens[0].clone(),
            busy_screen(),
            screens[1].clone(),
        ]
    };
    let arms = [
        ("attached", vec![measured.clone()], true, true),
        ("attached, 2.1.302", vec![hinted.clone()], true, true),
        (
            "attached, 2.1.302, a suggestion",
            vec![suggested(&hinted)],
            true,
            true,
        ),
        (
            "attached, unfinished",
            vec![swap(
                &measured,
                "  ATERM-TASK-DONE",
                "  Next I port the reader.",
            )],
            true,
            true,
        ),
        (
            "left",
            run(vec![greeted.clone(), left.clone()]),
            true,
            false,
        ),
        (
            "left, a worker's turn",
            run(vec![greeted.clone(), swap(&left, notice, worker)]),
            true,
            true,
        ),
        ("held", run(vec![ended(ask), held.clone()]), false, false),
        (
            "held, a worker's turn",
            run(vec![ended(ask), swap(&held, notice, worker)]),
            false,
            true,
        ),
    ];
    let mut wrong = Vec::new();
    for (arm, screens, answers, typed) in arms {
        let last = screens.len() - 1;
        let mut m = Mock::new(screens);
        m.turn_releases = Some(last);
        if arm.starts_with("left") {
            m.human = vec!["2000"];
        }
        let max = if typed {
            m.exit_when = Some(|m| m.turns > 0);
            60
        } else {
            m.vanish_after = Some(8);
            1
        };
        let opts = SuperviseOpts {
            policy: SupervisorConfig {
                human_grace_s: 0,
                answer_questions: answers,
                ..owners_policy()
            },
            ..auto(max)
        };
        let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
            s.set_turn_end_timing(TurnEndTiming {
                short_backoff: Duration::from_millis(30),
                short_backoff_max: Duration::from_millis(60),
                ..TurnEndTiming::default()
            });
        });
        if (count(&m, "turn") > 0) != typed {
            wrong.push(format!("{arm}: typed={}: {lines:#?}", !typed));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// A host whose conversation's record runs on past the 256 KiB tail it
/// reads for a wall ([`IdleHost::transcript_tail`]: `tail`): its finish read
/// folds the whole record, `full` ([`IdleHost::finished`]), and names the
/// turn it ends on by that fold ([`IdleHost::last_turn`]) where
/// `record_key`, as the window host's memo does.
#[derive(Debug)]
struct RecordHost {
    full: String,
    tail: String,
    record_key: bool,
}

impl IdleHost for RecordHost {
    fn wants(&self) -> bool {
        false
    }
    fn at_idle(&self) -> Option<HostStep> {
        None
    }
    fn owns_turn_end(&self) -> bool {
        false
    }
    fn transcript_tail(&self) -> Option<String> {
        Some(self.tail.clone())
    }
    fn finished(&self) -> Option<bool> {
        Some(crate::harness::upgrade::transcript_finished(
            &self.full,
            &[],
        ))
    }
    fn last_turn(&self) -> Option<crate::harness::upgrade::LastTurn> {
        let mut scan = crate::harness::upgrade::FinishedScan::default();
        for line in self.full.lines() {
            scan.line(line);
        }
        self.record_key.then(|| scan.last_turn())
    }
}

#[test]
fn a_persons_exchange_stays_theirs_under_an_idle_compaction() {
    let greeting = "Hi! What would you like me to work on?";
    let exchange = format!(
        "{}\n{}\n",
        r#"{"isSidechain":false,"type":"user","uuid":"u-1","message":{"role":"user","content":"hi"},"origin":{"kind":"human"},"promptSource":"typed"}"#,
        format_args!(
            r#"{{"isSidechain":false,"type":"assistant","uuid":"a-1","message":{{"model":"claude-opus-5-5","role":"assistant","content":[{{"type":"text","text":"{greeting}"}}]}}}}"#
        )
    );
    let block = [
        r#"{"type":"system","subtype":"compact_boundary","isSidechain":false,"compactMetadata":{"trigger":"idle"}}"#,
        r#"{"type":"user","uuid":"s-1","isSidechain":false,"isCompactSummary":true,"isVisibleInTranscriptOnly":true,"message":{"role":"user","content":"This session is being continued from a previous conversation."}}"#,
        r#"{"type":"attachment","isSidechain":false,"attachment":{"type":"file","filename":"/tmp/f0.rs","content":"fn main() {}"}}"#,
        r#"{"type":"system","subtype":"informational","isSidechain":false,"content":"Compacted while idle, before the prompt cache expired"}"#,
    ]
    .join("\n")
        + "\n";
    let compacted = {
        let mut r = rows(&[
            &format!("⏺ {greeting}"),
            "",
            "  ⎿  Skills restored (workflow-authoring)",
            "",
            "⏺ Compacted while idle, before the prompt cache expired",
            "",
        ]);
        r.extend(composer("  ⏵⏵ bypass permissions on (shift+tab to cycle)"));
        r
    };
    let (dir, ledger) = ledger_at("left-under-compaction");
    for record_key in [true, false] {
        let row = approvals::Row {
            rule_id: RULE_LEFT,
            outcome: approvals::Outcome::Skipped,
            command: "answer:a-1",
            reason: approvals::LEFT_REASON,
            box_seq: 1,
        }
        .to_json(i64::MAX / 2, None);
        std::fs::write(&ledger, row + "\n").expect("ledger");
        let typed = !record_key;
        let mut m = Mock::new(vec![compacted.clone()]);
        let max = if typed {
            m.exit_when = Some(|m| m.turns > 0);
            60
        } else {
            m.vanish_after = Some(8);
            1
        };
        let (jdir, journal) = journal_file("left-under-compaction");
        let opts = SuperviseOpts {
            policy: SupervisorConfig {
                human_grace_s: 0,
                ..owners_policy()
            },
            journal: Some(journal.clone()),
            idle_host: Some(Arc::new(RecordHost {
                full: format!("{exchange}{block}"),
                tail: block.clone(),
                record_key,
            }) as Arc<dyn IdleHost>),
            ..auto(max)
        };
        let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
            s.set_turn_end_timing(TurnEndTiming {
                short_backoff: Duration::from_millis(30),
                short_backoff_max: Duration::from_millis(60),
                ..TurnEndTiming::default()
            });
            s.set_approval_ledger(Some(ledger.clone()));
        });
        assert_eq!(
            count(&m, "turn") > 0,
            typed,
            "record_key={record_key}: {lines:#?}"
        );
        let left = journal_records(&journal)
            .0
            .into_iter()
            .filter(|r| r.line.starts_with("LEFT "))
            .count();
        assert_eq!(
            left,
            usize::from(!typed),
            "record_key={record_key}: {lines:#?}"
        );
        let _ = std::fs::remove_dir_all(&jdir);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

use crate::supervise::policy::turn_end::{AccountAt, RULE_ACCOUNT, Restart, Unmade};

/// A roster host as a test drives it: its advice ([`IdleHost::account`]) and relaunch answers
/// ([`IdleHost::restart`]) are scripted (each ask the next entry, the last for ever), and all
/// the loop asked or told it is kept in order.
#[derive(Debug, Default)]
struct AccountHost {
    advice: std::sync::Mutex<VecDeque<Option<AccountAdvice>>>,
    advised: std::sync::Mutex<Option<AccountAdvice>>,
    answers: std::sync::Mutex<VecDeque<&'static str>>,
    answered: std::sync::Mutex<Option<&'static str>>,
    said: std::sync::Mutex<Vec<String>>,
    /// [`IdleHost::account_stamp`], as a test moves it.
    stamp: std::sync::atomic::AtomicU64,
    /// The stamp moves by itself right after the first ask for advice: news that lands while
    /// the session waits on that advice.
    news_after_first_ask: std::sync::atomic::AtomicBool,
    /// A relaunched agent's last step is owed: the host asks for the next
    /// idle point and takes it there ([`IdleHost::at_idle`]), once.
    carry: std::sync::atomic::AtomicBool,
    /// The word that step answers; `None`: `done:resumed`, what an account move's relaunch
    /// answers (`relaunch::carried_quietly`: nothing typed).
    step: std::sync::Mutex<Option<&'static str>>,
    /// The step is owed once a relaunch answers `adopted`, as a window's
    /// host parks it (`account_move`).
    parks: std::sync::atomic::AtomicBool,
    /// What the loop told of its exceptions ([`IdleHost::exception`]), in
    /// order, each with how many entries [`Self::said`] held then:
    /// `exception:<kind>:<when>`, `:escalated` where the loop's own
    /// escalation carries it, or `exception:none`.
    told: std::sync::Mutex<Vec<(String, usize)>>,
    /// The limit row the agent's transcript stands at
    /// ([`IdleHost::standing_limit`]: `None` too for one the host moved the
    /// conversation off), and how often it was asked.
    standing: std::sync::Mutex<Option<crate::harness::upgrade::StandingLimit>>,
    standing_asks: std::sync::atomic::AtomicUsize,
    /// The agent's process ([`IdleHost::agent_pid`]), when it started
    /// ([`IdleHost::agent_started`], unix seconds), and the conversation it
    /// holds ([`IdleHost::agent_conversation`]).
    pid: std::sync::Mutex<Option<u32>>,
    started: std::sync::Mutex<Option<i64>>,
    conversation: std::sync::Mutex<Option<String>>,
    /// The conversation the host names once it is asked for the standing
    /// limit: its ask reads the agent again, as the GUI's refreshes its
    /// snapshot.
    named_on_ask: std::sync::Mutex<Option<String>>,
    /// The process the host names once it is asked for the standing limit:
    /// a snapshot kept from before a relaunch, refreshed by the ask.
    pid_on_ask: std::sync::Mutex<Option<u32>>,
    /// [`IdleHost::login_label`]: the session's renewable login, as a test
    /// sets it.
    login_label: std::sync::Mutex<Option<String>>,
    /// [`IdleHost::transcript_tail`]: the agent's transcript, as a test sets
    /// it (`None`: none this host can read).
    tail: std::sync::Mutex<Option<String>>,
    /// [`IdleHost::login_superseded`], as a test sets it; and how many
    /// times the loop asked it.
    login_superseded: std::sync::atomic::AtomicBool,
    superseded_asked: std::sync::atomic::AtomicUsize,
    /// The renewal lands right after the host's next answer that the
    /// process holds its row's own login, before the loop reads anything
    /// else: the stamp moves, and the process is on the login it replaced.
    renews_after_answer: std::sync::atomic::AtomicBool,
    /// [`IdleHost::login_method`]: the method the session's login already
    /// is, as a test sets it.
    login_method: std::sync::Mutex<Option<aterm_phase::LoginMethod>>,
    /// [`IdleHost::roster_word`], as a test sets it; and how many times the
    /// loop asked it.
    roster: std::sync::Mutex<Option<crate::supervise::policy::turn_end::RosterWord>>,
    roster_asked: std::sync::atomic::AtomicUsize,
    /// The owner's `[harness] relaunch = false`, as a window's host reads it
    /// live ([`IdleHost::relaunch_off`]: and then no restart can be asked,
    /// [`IdleHost::can_restart`]); and how many times the loop asked it.
    relaunch_off: std::sync::atomic::AtomicBool,
    relaunch_off_asked: std::sync::atomic::AtomicUsize,
    /// How often the loop told a login back ([`IdleHost::login_back`]) and
    /// a limit over with no sign-in that took ([`IdleHost::limit_over`]).
    backs: std::sync::atomic::AtomicUsize,
    overs: std::sync::atomic::AtomicUsize,
    /// [`IdleHost::relaunch_owed`]: the agent's last relaunch is the host's
    /// own, as a test sets it; and how many times the loop asked it.
    relaunch_owed: std::sync::atomic::AtomicBool,
    relaunch_owed_asked: std::sync::atomic::AtomicUsize,
}

impl AccountHost {
    fn scripted(advice: &[Option<AccountAdvice>], answers: &[&'static str]) -> Arc<Self> {
        Arc::new(Self {
            advice: std::sync::Mutex::new(advice.iter().cloned().collect()),
            answers: std::sync::Mutex::new(answers.iter().copied().collect()),
            ..Self::default()
        })
    }

    fn moving(to: &str, answer: &'static str) -> Arc<Self> {
        Self::scripted(
            &[Some(AccountAdvice::Move { to: to.to_string() })],
            &[answer],
        )
    }

    fn said(&self, prefix: &str) -> Vec<String> {
        self.said
            .lock()
            .unwrap()
            .iter()
            .filter(|s| s.starts_with(prefix))
            .cloned()
            .collect()
    }

    fn all(&self) -> Vec<String> {
        self.said.lock().unwrap().clone()
    }

    fn told(&self) -> Vec<String> {
        self.told
            .lock()
            .unwrap()
            .iter()
            .map(|(word, _)| word.clone())
            .collect()
    }
}

impl IdleHost for AccountHost {
    fn wants(&self) -> bool {
        self.carry.load(Ordering::SeqCst)
    }
    fn at_idle(&self) -> Option<HostStep> {
        if !self.carry.swap(false, Ordering::SeqCst) {
            return None;
        }
        self.said.lock().unwrap().push("carry-on".to_string());
        let step = self.step.lock().unwrap().unwrap_or("done:resumed");
        Some(HostStep {
            line: format!("carry-on step={step}"),
            moved: crate::harness::upgrade_drive::moved(step),
            typed: crate::harness::upgrade_drive::host_turn(step),
            repeat: false,
        })
    }
    fn owns_turn_end(&self) -> bool {
        false
    }
    fn can_restart(&self) -> bool {
        !self.relaunch_off.load(Ordering::SeqCst)
    }
    fn relaunch_off(&self) -> bool {
        self.relaunch_off_asked.fetch_add(1, Ordering::SeqCst);
        self.relaunch_off.load(Ordering::SeqCst)
    }
    fn restart(&self, why: &Restart) -> Option<String> {
        let what = match why {
            Restart::Account { to, .. } => format!("restart:{}:{to}", why.word()),
            other => format!("restart:{}", other.word()),
        };
        self.said.lock().unwrap().push(what);
        let mut last = self.answered.lock().unwrap();
        if let Some(next) = self.answers.lock().unwrap().pop_front() {
            *last = Some(next);
        }
        if *last == Some("adopted") && self.parks.load(Ordering::SeqCst) {
            self.carry.store(true, Ordering::SeqCst);
        }
        last.map(str::to_string)
    }
    fn account(&self, point: AccountPoint) -> Option<AccountAdvice> {
        let at = match point {
            AccountPoint::Wall { kind } => format!("account:wall:{}", kind.name()),
            AccountPoint::LoginEnded => "account:login-ended".to_string(),
            AccountPoint::TurnEnd => "account:turn-end".to_string(),
        };
        self.said.lock().unwrap().push(at);
        let mut last = self.advised.lock().unwrap();
        if let Some(next) = self.advice.lock().unwrap().pop_front() {
            *last = next;
        }
        if self.news_after_first_ask.swap(false, Ordering::SeqCst) {
            self.stamp.fetch_add(1, Ordering::SeqCst);
        }
        last.clone()
    }
    fn account_stamp(&self) -> u64 {
        self.stamp.load(Ordering::SeqCst)
    }
    fn login_label(&self) -> Option<String> {
        self.login_label.lock().unwrap().clone()
    }
    fn login_method(&self) -> Option<aterm_phase::LoginMethod> {
        *self.login_method.lock().unwrap()
    }
    fn roster_word(&self) -> Option<crate::supervise::policy::turn_end::RosterWord> {
        self.roster_asked.fetch_add(1, Ordering::SeqCst);
        self.roster.lock().unwrap().clone()
    }
    fn transcript_tail(&self) -> Option<String> {
        self.tail.lock().unwrap().clone()
    }
    fn login_superseded(&self) -> bool {
        self.superseded_asked.fetch_add(1, Ordering::SeqCst);
        let superseded = self.login_superseded.load(Ordering::SeqCst);
        if !superseded && self.renews_after_answer.swap(false, Ordering::SeqCst) {
            self.login_superseded.store(true, Ordering::SeqCst);
            self.stamp.fetch_add(1, Ordering::SeqCst);
        }
        superseded
    }
    fn login_notice(&self, days: u32) {
        self.said.lock().unwrap().push(format!("notice:{days}"));
    }
    fn exception(&self, standing: Option<&crate::supervise::policy::exception::Exception>) {
        let word = standing.map_or_else(
            || "exception:none".to_string(),
            |e| {
                format!(
                    "exception:{}:{}{}",
                    e.kind.word(),
                    e.when.word(),
                    if e.escalated { ":escalated" } else { "" }
                )
            },
        );
        let at = self.said.lock().unwrap().len();
        self.told.lock().unwrap().push((word, at));
    }
    fn standing_limit(&self) -> Option<crate::harness::upgrade::StandingLimit> {
        self.standing_asks.fetch_add(1, Ordering::SeqCst);
        if let Some(named) = self.named_on_ask.lock().unwrap().take() {
            *self.conversation.lock().unwrap() = Some(named);
        }
        if let Some(pid) = self.pid_on_ask.lock().unwrap().take() {
            *self.pid.lock().unwrap() = Some(pid);
        }
        self.standing.lock().unwrap().clone()
    }
    fn agent_pid(&self) -> Option<u32> {
        *self.pid.lock().unwrap()
    }
    fn relaunch_owed(&self) -> bool {
        self.relaunch_owed_asked.fetch_add(1, Ordering::SeqCst);
        self.relaunch_owed.load(Ordering::SeqCst)
    }
    fn agent_started(&self) -> Option<i64> {
        *self.started.lock().unwrap()
    }
    fn agent_conversation(&self) -> Option<String> {
        self.conversation.lock().unwrap().clone()
    }
    fn login_back(&self) {
        self.backs.fetch_add(1, Ordering::SeqCst);
    }
    fn limit_over(&self) {
        self.overs.fetch_add(1, Ordering::SeqCst);
    }
    fn account_served(&self) {
        self.said.lock().unwrap().push("served".to_string());
    }
    fn account_answered(&self, bad: AccountBad) {
        self.said.lock().unwrap().push(
            match bad {
                AccountBad::Wall => "answered:wall",
                AccountBad::SignedOut => "answered:signed-out",
            }
            .to_string(),
        );
    }
}

const ACCOUNT_NOW: i64 = TEST_NOW + 8 * 3600 + 10 * 60;

fn account_run(
    tag: &str,
    screens: Vec<Vec<String>>,
    host: &Arc<AccountHost>,
    ledger: &Path,
) -> (Vec<String>, Mock, Vec<String>) {
    account_run_with(tag, screens, host, ledger, |_| {})
}

fn account_run_with(
    tag: &str,
    screens: Vec<Vec<String>>,
    host: &Arc<AccountHost>,
    ledger: &Path,
    setup: impl FnOnce(&mut Mock),
) -> (Vec<String>, Mock, Vec<String>) {
    let (dir, path) = journal_file(tag);
    let mut m = Mock::new(screens);
    m.vanish_after = Some(2);
    m.stall_sleep = Some(Duration::from_millis(5));
    setup(&mut m);
    let opts = SuperviseOpts {
        idle_host: Some(Arc::clone(host) as Arc<dyn IdleHost>),
        journal: Some(path.clone()),
        ..hosted(3)
    };
    let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
        s.set_clock(ACCOUNT_NOW, PDT);
        s.set_approval_ledger(Some(ledger.to_path_buf()));
    });
    let (records, _) = journal_records(&path);
    let _ = std::fs::remove_dir_all(&dir);
    (lines, m, records.into_iter().map(|r| r.line).collect())
}

#[test]
fn a_wall_with_an_account_to_move_to_is_the_hosts_relaunch() {
    use crate::supervise::approvals::{account_moves, open_account_move, open_wall};
    let wall = || vec![busy_screen(), limited_screen()];

    let (dir, ledger) = ledger_at("account-made");
    let host = AccountHost::moving("work-2", "adopted");
    let (lines, m, _) = account_run("te-account-made", wall(), &host, &ledger);
    assert_eq!(
        host.said("restart"),
        ["restart:account:work-2"],
        "{lines:#?}"
    );
    assert!(
        lines.iter().any(|l| l.starts_with("RESTARTED seq=")
            && l.contains(" rule=account-rotate@v1 relaunch under account work-2 ")),
        "{lines:#?}"
    );
    assert_eq!(
        count(&m, "turn"),
        0,
        "nothing typed at the wall: {:#?}",
        m.requests
    );
    assert_eq!(count(&m, "meta set attention"), 0, "{:#?}", m.requests);
    let moves = account_moves(&ledger, None);
    assert_eq!(moves.made_ms.len(), 1, "{moves:?}");
    assert!(moves.unmade.is_none());
    let open = open_account_move(&ledger, None).expect("the move, still to be answered");
    assert_eq!((open.to.as_str(), open.at), ("work-2", AccountAt::Wall));
    assert_eq!(
        open_wall(&ledger, None),
        None,
        "the session left that wall's account: its wait is over"
    );
    let text = std::fs::read_to_string(&ledger).expect("the ledger");
    assert!(
        text.contains("\"rule_id\":\"account-rotate@v1\",\"decision\":\"typed\"")
            && text.contains("(account move: to=work-2 at=wall)"),
        "{text}"
    );
    let _ = std::fs::remove_dir_all(&dir);

    let (dir, ledger) = ledger_at("account-waits");
    let host = AccountHost::moving("work-2", "wait:changed");
    let (lines, m, journal) = account_run("te-account-waits", wall(), &host, &ledger);
    assert!(
        journal
            .iter()
            .any(|l| l.starts_with("WAITING ") && l.contains("restart:account waits: wait:changed")),
        "{journal:#?}"
    );
    assert_eq!(count(&m, "turn"), 0, "{:#?}", m.requests);
    assert!(
        !lines.iter().any(|l| l.starts_with("RESTARTED")),
        "{lines:#?}"
    );
    assert_eq!(
        account_moves(&ledger, None),
        Default::default(),
        "nothing made, nothing refused"
    );
    let _ = std::fs::remove_dir_all(&dir);

    let (dir, ledger) = ledger_at("account-refused");
    let host = AccountHost::moving("work-2", "refused:line");
    let (lines, m, journal) = account_run("te-account-refused", wall(), &host, &ledger);
    assert_eq!(
        host.said("restart"),
        ["restart:account:work-2"],
        "asked once: {lines:#?}"
    );
    assert!(
        !lines.iter().any(|l| l.starts_with("RESTARTED")),
        "{lines:#?}"
    );
    assert_eq!(count(&m, "turn"), 0, "{:#?}", m.requests);
    let moves = account_moves(&ledger, None);
    assert!(
        moves.unmade.is_some() && moves.made_ms.is_empty() && moves.open.is_none(),
        "{moves:?}"
    );
    assert!(
        open_wall(&ledger, None).is_some(),
        "the wall still stands, and is still waited on"
    );
    let waited: Vec<&String> = journal
        .iter()
        .filter(|l| l.starts_with("WAITING ") && l.contains("usage-session resets"))
        .collect();
    assert_eq!(waited.len(), 1, "{journal:#?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_move_ahead_the_host_defers_never_holds_the_continuation() {
    let end = || vec![busy_screen(), ended("Stage 3 done; the suite is green.")];
    let (dir, ledger) = ledger_at("account-ahead-waits");
    let host = AccountHost::moving("work-2", "wait:changed");
    let (lines, m, journal) = account_run("te-account-ahead-waits", end(), &host, &ledger);
    assert_eq!(
        host.said("restart"),
        ["restart:account-ahead:work-2"],
        "{lines:#?}"
    );
    assert!(
        m.requests.contains(&continue_request("keep going")),
        "the continuation goes ahead: {:#?}",
        m.requests
    );
    assert_eq!(
        lines.iter().filter(|l| l.starts_with("CONTINUED")).count(),
        1,
        "{lines:#?}"
    );
    assert!(
        journal.iter().any(|l| l.starts_with("DEFERRED ")
            && l.contains("restart:account-ahead")
            && l.contains("wait:changed")),
        "{journal:#?}"
    );
    let _ = std::fs::remove_dir_all(&dir);

    let (dir, ledger) = ledger_at("account-ahead-made");
    let host = AccountHost::moving("work-2", "adopted");
    let (lines, m, _) = account_run("te-account-ahead-made", end(), &host, &ledger);
    assert_eq!(host.said("restart"), ["restart:account-ahead:work-2"]);
    assert!(
        lines.iter().any(|l| l.starts_with("RESTARTED seq=")
            && l.contains(" rule=account-rotate@v1 relaunch under account work-2 ")),
        "{lines:#?}"
    );
    assert_eq!(count(&m, "turn"), 0, "{:#?}", m.requests);
    assert_eq!(
        crate::supervise::approvals::open_account_move(&ledger, None).map(|o| o.at),
        Some(AccountAt::Ahead)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_move_ahead_of_a_wall_types_nothing_and_its_point_is_decided_as_with_no_move() {
    let done = "Stage 3 done; the suite is green.";
    let screens = || vec![busy_screen(), ended(done), ended(done)];
    let turns = |m: &Mock| -> Vec<String> {
        m.requests
            .iter()
            .filter(|r| r.starts_with("turn"))
            .cloned()
            .collect()
    };
    let run = |tag: &str, host: &Arc<AccountHost>| {
        let (dir, ledger) = ledger_at(tag);
        let (lines, m, journal) =
            account_run_with(&format!("te-{tag}"), screens(), host, &ledger, |m| {
                m.vanish_after = Some(4);
                m.cursor_on_caret = true;
            });
        let _ = std::fs::remove_dir_all(&dir);
        (turns(&m), format!("{lines:#?}\n{journal:#?}"), journal)
    };
    let moving = |step: Option<&'static str>| {
        let host = AccountHost::moving("work-2", "adopted");
        host.parks.store(true, Ordering::SeqCst);
        *host.step.lock().unwrap() = step;
        host
    };

    let baseline = AccountHost::scripted(&[None], &["adopted"]);
    let (unmoved, log, _) = run("account-ahead-baseline", &baseline);
    assert!(baseline.said("restart").is_empty(), "{log}");
    assert_eq!(unmoved, [continue_request("keep going")], "{log}");

    let host = moving(None);
    let (moved, log, journal) = run("account-ahead-quiet", &host);
    assert_eq!(
        host.said("restart"),
        ["restart:account-ahead:work-2"],
        "{log}"
    );
    assert_eq!(host.said("carry-on"), ["carry-on"], "{log}");
    assert!(
        journal
            .iter()
            .any(|l| l.starts_with("HOST seq=") && l.ends_with(" carry-on step=done:resumed")),
        "{log}"
    );
    assert!(
        journal.iter().any(|l| l.starts_with("RESTARTED seq=")
            && l.ends_with("(at a turn end; the same conversation)")),
        "{log}"
    );
    assert_eq!(moved, unmoved, "the same decision, typed once: {log}");

    let host = moving(Some("continued"));
    let (carried, log, _) = run("account-ahead-carried", &host);
    assert_eq!(host.said("carry-on"), ["carry-on"], "{log}");
    assert!(carried.is_empty(), "the host's turn took the point: {log}");
    assert_ne!(carried, unmoved, "{log}");
}

fn account_point_of(rows: Vec<String>, phase: Phase) -> Turn {
    Turn {
        phase,
        screen: Screen {
            rows,
            seq: 102,
            cursor_col: 2,
            ..Screen::default()
        },
        timed_out: false,
    }
}

fn account_opts(host: &Arc<AccountHost>) -> SuperviseOpts {
    SuperviseOpts {
        idle_host: Some(Arc::clone(host) as Arc<dyn IdleHost>),
        ..SuperviseOpts::hosted_with(&owners_policy())
    }
}

#[test]
fn the_host_is_asked_for_its_advice_once_a_decision() {
    let host = AccountHost::scripted(
        &[Some(AccountAdvice::Move {
            to: "work-2".to_string(),
        })],
        &["wait:changed"],
    );
    let opts = account_opts(&host);
    let rows = limited_screen();
    let mut m = Mock::new(vec![rows.clone()]);
    let mut s = session(&mut m, Some("@s-1".to_string()));
    s.set_clock(ACCOUNT_NOW, PDT);
    s.set_zone_lookup(test_zones);
    let point = account_point_of(
        rows,
        Phase::Limited {
            message: String::new(),
            reset: None,
        },
    );
    for round in 1..=3 {
        let action = s
            .turn_end_at_point(
                &point,
                (round == 1).then_some(Duration::from_secs(180)),
                &opts,
            )
            .expect("a decision");
        assert!(
            matches!(
                &action,
                TurnEndAction::Restart { why: Restart::Account { to, at: AccountAt::Wall }, rule_id, .. }
                    if to == "work-2" && *rule_id == RULE_ACCOUNT
            ),
            "{action:?}"
        );
        s.turn_end_execute(&point, action, &opts, &[], &mut Alone)
            .expect("the relaunch waits, it does not fail");
        assert_eq!(host.said("account").len(), round, "{:?}", host.all());
        assert_eq!(host.said("restart").len(), round, "{:?}", host.all());
    }
    assert!(
        s.turn_end_due.is_some(),
        "the point is held on the back-off"
    );
}

#[test]
fn a_wait_on_the_hosts_advice_is_decided_again_when_its_stamp_moves() {
    let host = AccountHost::scripted(
        &[
            None,
            Some(AccountAdvice::Move {
                to: "work-2".to_string(),
            }),
        ],
        &["adopted"],
    );
    let opts = account_opts(&host);
    let rows = limited_screen();
    let mut m = Mock::new(vec![rows.clone()]);
    let mut s = session(&mut m, Some("@s-1".to_string()));
    s.set_clock(ACCOUNT_NOW, PDT);
    s.set_zone_lookup(test_zones);
    let point = account_point_of(
        rows,
        Phase::Limited {
            message: String::new(),
            reset: None,
        },
    );
    host.stamp.store(7, Ordering::SeqCst);
    let action = s
        .turn_end_at_point(&point, Some(Duration::from_secs(180)), &opts)
        .expect("a decision");
    assert!(
        matches!(action, TurnEndAction::WaitUntil { .. }),
        "{action:?}"
    );
    s.turn_end_execute(&point, action, &opts, &[], &mut Alone)
        .expect("the wait is set");
    assert!(s.turn_end_due.is_some(), "the wall is waited out");
    assert_eq!(host.said("account").len(), 1, "{:?}", host.all());
    assert!(!s.account_edge(&opts));
    assert!(!s.account_edge(&opts));

    host.stamp.store(8, Ordering::SeqCst);
    assert!(s.account_edge(&opts), "the stamp moved under a wait");
    assert!(!s.account_edge(&opts), "the same stamp is no second edge");
    let again = s
        .turn_end_at_point(&point, None, &opts)
        .expect("a decision");
    assert!(
        matches!(
            &again,
            TurnEndAction::Restart { why: Restart::Account { to, at: AccountAt::Wall }, .. }
                if to == "work-2"
        ),
        "{again:?}"
    );
    assert_eq!(host.said("account").len(), 2, "{:?}", host.all());

    host.stamp.store(9, Ordering::SeqCst);
    s.turn_end_due = None;
    assert!(!s.account_edge(&opts), "nothing is waited on");
}

#[test]
fn news_of_the_accounts_ends_a_sessions_wait_at_its_limit() {
    let wall = || vec![busy_screen(), limited_screen()];
    let script = || {
        AccountHost::scripted(
            &[
                None,
                Some(AccountAdvice::Move {
                    to: "work-2".to_string(),
                }),
            ],
            &["adopted"],
        )
    };

    let (dir, ledger) = ledger_at("account-news");
    let host = script();
    host.news_after_first_ask.store(true, Ordering::SeqCst);
    let (lines, _, journal) = account_run("te-account-news", wall(), &host, &ledger);
    assert_eq!(
        host.said("account").len(),
        2,
        "asked again inside the wait: {:?}\n{journal:#?}",
        host.all()
    );
    assert_eq!(
        host.said("restart"),
        ["restart:account:work-2"],
        "{lines:#?}"
    );
    assert!(
        lines.iter().any(|l| l.starts_with("RESTARTED seq=")),
        "{lines:#?}"
    );
    let _ = std::fs::remove_dir_all(&dir);

    let (dir, ledger) = ledger_at("account-no-news");
    let host = script();
    let (lines, _, _) = account_run("te-account-no-news", wall(), &host, &ledger);
    assert_eq!(host.said("account").len(), 1, "{:?}", host.all());
    assert!(host.said("restart").is_empty(), "{lines:#?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_lost_login_with_no_account_left_types_login_and_asks_its_sign_in_now() {
    use crate::supervise::approvals::{ACCOUNT_MOVE_COMMAND, Outcome, Row, account_switch_reason};
    let mut login = rows(&[
        "⏺ Carrying on.",
        "  ⎿  Not logged in · Please run /login",
        "",
    ]);
    login.extend(composer("  ? for shortcuts"));
    let mut resumed = rows(&[
        "⏺ Resumed: carrying on.",
        "  ⎿  Not logged in · Please run /login",
        "",
    ]);
    resumed.extend(composer("  ? for shortcuts"));
    let screens = || {
        vec![
            busy_screen(),
            login.clone(),
            login.clone(),
            resumed.clone(),
            handoff(),
            handoff(),
            busy_screen(),
            ended("Carried on: stage 3 is done."),
        ]
    };
    let moved = || {
        let now_ms = crate::supervise::journal::unix_ms();
        Row {
            rule_id: RULE_ACCOUNT,
            outcome: Outcome::Typed,
            command: &format!("{ACCOUNT_MOVE_COMMAND}pool-a"),
            reason: &account_switch_reason("pool-a", AccountAt::Wall),
            box_seq: 90,
        }
        .to_json(now_ms - 60_000, None)
    };
    let resumed = || {
        let now_ms = crate::supervise::journal::unix_ms();
        Row {
            rule_id: crate::supervise::policy::turn_end::RULE_LIMIT_RESUME,
            outcome: Outcome::Typed,
            command: "keep going",
            reason: "the turn-end policy",
            box_seq: 91,
        }
        .to_json(now_ms - 50_000, None)
    };
    let run = |tag: &str, seeded: bool, host: &Arc<AccountHost>| {
        let (dir, ledger) = ledger_at(tag);
        if seeded {
            std::fs::write(&ledger, moved() + "\n" + &resumed() + "\n").expect("the ledger");
        }
        let (lines, m, journal) =
            account_run_with(&format!("te-{tag}"), screens(), host, &ledger, |m| {
                m.vanish_after = Some(4);
                m.cursor_on_caret = true;
                m.turn_releases = Some(3);
            });
        let _ = std::fs::remove_dir_all(&dir);
        let log = format!(
            "{lines:#?}\n{journal:#?}\n{:?}\n{:?}\n{:#?}",
            host.all(),
            host.told(),
            m.requests
        );
        (lines, m, journal, log)
    };

    let host = AccountHost::scripted(
        &[
            None,
            Some(AccountAdvice::Move {
                to: "pool-a".to_string(),
            }),
            None,
        ],
        &["adopted"],
    );
    host.news_after_first_ask.store(true, Ordering::SeqCst);
    let (lines, m, journal, log) = run("account-stuck-login", true, &host);
    let logins = |lines: &[String]| {
        lines
            .iter()
            .filter(|l| l.ends_with(" rule=auth-login@v1 /login"))
            .count()
    };
    assert_eq!(logins(&lines), 1, "`/login` once: {log}");
    assert_eq!(
        host.told(),
        ["exception:login:now", "exception:none"],
        "told once, withdrawn once: {log}"
    );
    assert!(
        journal.iter().any(|l| l.starts_with("EXCEPTION ")
            && l.contains("kind=login when=now notice=login-")),
        "{log}"
    );
    assert!(host.said("restart").is_empty(), "{log}");
    assert!(
        !m.requests
            .iter()
            .any(|r| r.starts_with("meta set attention")),
        "no badge beside the notice: {log}"
    );

    let host = AccountHost::scripted(&[None], &["adopted"]);
    let (lines, _, _, log) = run("account-stuck-login-none", false, &host);
    assert_eq!(logins(&lines), 1, "{log}");
    assert_eq!(
        host.told(),
        ["exception:login:now", "exception:none"],
        "{log}"
    );
    assert!(
        !host
            .said("account")
            .iter()
            .any(|s| s == "account:wall:auth"),
        "no host is asked about the login the person lost: {log}"
    );
}

#[test]
fn a_one_login_machine_types_login_once_and_asks_its_sign_in_now() {
    let mut login = rows(&[
        "⏺ Carrying on.",
        "  ⎿  Not logged in · Please run /login",
        "",
    ]);
    login.extend(composer("  ? for shortcuts"));
    let mut noticed = rows(&[
        "⏺ Carried on: stage 3 is done.",
        "",
        "✻ Worked for 3m 2s · done 4:24 PM",
        &format!(
            "{:>118}",
            "\u{26a0} Your login expires in 2 days \u{b7} run /login to renew"
        ),
    ]);
    noticed.extend(composer("  ? for shortcuts"));
    let mut resumed = rows(&[
        "⏺ Resumed: carrying on.",
        "  ⎿  Not logged in · Please run /login",
        "",
    ]);
    resumed.extend(composer("  ? for shortcuts"));
    let screens = || {
        vec![
            busy_screen(),
            login.clone(),
            login.clone(),
            resumed.clone(),
            handoff(),
            handoff(),
            busy_screen(),
            noticed.clone(),
        ]
    };
    let run = |tag: &str, host: &Arc<AccountHost>, logins: bool| {
        let (dir, ledger) = ledger_at(tag);
        let (journal_dir, path) = journal_file(&format!("te-{tag}"));
        let mut m = Mock::new(screens());
        m.vanish_after = Some(4);
        m.stall_sleep = Some(Duration::from_millis(5));
        m.cursor_on_caret = true;
        m.turn_releases = Some(3);
        let mut opts = SuperviseOpts {
            idle_host: Some(Arc::clone(host) as Arc<dyn IdleHost>),
            journal: Some(path.clone()),
            ..hosted(3)
        };
        opts.policy.accounts = crate::harness::accounts::Mode::Off;
        opts.policy.logins = logins;
        let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
            s.set_clock(ACCOUNT_NOW, PDT);
            s.set_approval_ledger(Some(ledger.clone()));
        });
        let (records, _) = journal_records(&path);
        let journal: Vec<String> = records.into_iter().map(|r| r.line).collect();
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&journal_dir);
        let log = format!(
            "{lines:#?}\n{journal:#?}\n{:?}\n{:?}\n{:#?}",
            host.all(),
            host.told(),
            m.requests
        );
        (lines, m, journal, log)
    };
    let logins = |lines: &[String]| {
        lines
            .iter()
            .filter(|l| l.ends_with(" rule=auth-login@v1 /login"))
            .count()
    };
    let host = AccountHost::scripted(&[None], &["adopted"]);
    *host.login_label.lock().unwrap() = Some("m7-a".to_string());
    let (lines, m, journal, log) = run("one-login-renew", &host, true);
    assert_eq!(logins(&lines), 1, "`/login` once: {log}");
    assert_eq!(host.said("notice"), ["notice:2"], "told once: {log}");
    assert_eq!(
        host.told(),
        ["exception:login:now", "exception:none"],
        "{log}"
    );
    assert!(
        journal.iter().any(|l| l.starts_with("EXCEPTION ")
            && l.contains("kind=login when=now notice=login-")),
        "{log}"
    );
    assert!(
        !journal.iter().any(|l| l.contains("accounts renew")),
        "no renewal asked: {log}"
    );
    assert!(host.said("restart").is_empty(), "{log}");
    assert!(
        !m.requests
            .iter()
            .any(|r| r.starts_with("meta set attention")),
        "no badge beside the notice: {log}"
    );

    let host = AccountHost::scripted(&[None], &["adopted"]);
    *host.login_label.lock().unwrap() = Some("m7-a".to_string());
    let (lines, _, _, log) = run("one-login-off", &host, false);
    assert_eq!(logins(&lines), 1, "{log}");
    assert!(host.said("account").is_empty(), "nothing asked: {log}");
}

#[test]
fn the_sign_ins_notice_outlasts_its_dialogs_busy_reads() {
    let mut login = rows(&[
        "⏺ Carrying on.",
        "  ⎿  Not logged in · Please run /login",
        "",
    ]);
    login.extend(composer("  ? for shortcuts"));
    let mut spinning =
        aterm_phase::prompt::fixtures::screen(aterm_phase::prompt::fixtures::LOGIN_OPENING);
    for row in &mut spinning {
        if row.contains("Opening browser to sign in") {
            *row = "  ✻ Opening browser to sign in… (3s · esc to interrupt)".to_string();
        }
    }
    assert!(crate::supervise::policy::login::dialog_stands(&spinning));
    let run = |tag: &str, busy: Vec<String>| {
        let (dir, path) = journal_file(tag);
        let mut m = Mock::new(vec![
            busy_screen(),
            login.clone(),
            login.clone(),
            login.clone(),
            handoff(),
            handoff(),
            busy,
            handoff(),
        ]);
        m.vanish_after = Some(3);
        m.cursor_on_caret = true;
        m.turn_releases = Some(3);
        let opts = SuperviseOpts {
            journal: Some(path.clone()),
            ..hosted(3)
        };
        let (lines, _) = watch_lines(&mut m, &opts);
        let (records, _) = journal_records(&path);
        let _ = std::fs::remove_dir_all(&dir);
        let journal: Vec<String> = records.into_iter().map(|r| r.line).collect();
        let log = format!("{lines:#?}\n{journal:#?}\n{:#?}", m.requests);
        (journal, log)
    };
    let raised = |journal: &[String]| {
        journal
            .iter()
            .filter(|l| {
                l.starts_with("EXCEPTION ") && l.contains("kind=login when=now notice=login-")
            })
            .count()
    };
    let working = |journal: &[String]| {
        journal
            .iter()
            .any(|l| l.starts_with("CLEARED ") && l.ends_with(" the agent working"))
    };
    let (journal, log) = run("te-signin-busy", spinning);
    assert_eq!(raised(&journal), 1, "told once: {log}");
    assert!(!working(&journal), "the dialog's spinner is no work: {log}");
    let (journal, log) = run("te-signin-work", busy_screen());
    assert!(raised(&journal) >= 1, "{log}");
    assert!(working(&journal), "{log}");
}

#[test]
fn the_login_success_gets_one_fenced_enter() {
    let mut login = rows(&[
        "⏺ Carrying on.",
        "  ⎿  Not logged in · Please run /login",
        "",
    ]);
    login.extend(composer("  ? for shortcuts"));
    let success =
        aterm_phase::prompt::fixtures::screen(aterm_phase::prompt::fixtures::LOGIN_SUCCESS);
    let run = |tag: &str, person_at_key: usize| {
        let (dir, path) = journal_file(tag);
        let mut m = Mock::new(vec![
            busy_screen(),
            login.clone(),
            login.clone(),
            login.clone(),
            success.clone(),
            success.clone(),
        ]);
        m.vanish_after = Some(3);
        m.cursor_on_caret = true;
        m.turn_releases = Some(3);
        m.sends_gen = true;
        m.gen_fence = true;
        m.human_seq = Some(3);
        m.person_at_key = person_at_key;
        let opts = SuperviseOpts {
            journal: Some(path.clone()),
            ..hosted(3)
        };
        let (lines, _) = watch_lines(&mut m, &opts);
        let (records, _) = journal_records(&path);
        let _ = std::fs::remove_dir_all(&dir);
        let journal: Vec<String> = records.into_iter().map(|r| r.line).collect();
        let log = format!("{lines:#?}\n{journal:#?}\n{:#?}", m.requests);
        (lines, journal, m, log)
    };
    let enters = |m: &Mock| -> Vec<String> {
        m.requests
            .iter()
            .filter(|r| r.starts_with("key ") && r.ends_with(" enter"))
            .cloned()
            .collect()
    };
    let done = |lines: &[String]| {
        lines
            .iter()
            .filter(|l| l.starts_with("TYPED ") && l.contains(" rule=login-done@v1 enter at "))
            .count()
    };
    let (lines, _, m, log) = run("te-login-done", 0);
    let pressed = enters(&m);
    assert_eq!(pressed.len(), 1, "one Enter: {log}");
    assert!(
        pressed[0].contains("if-human=3 ")
            && pressed[0].contains(" if-gen=")
            && pressed[0].contains(" if=^\\x20{2}Login\\x20successful"),
        "fenced on the read and its row: {log}"
    );
    assert_eq!(done(&lines), 1, "{log}");
    let (lines, journal, _, log) = run("te-login-done-person", 1);
    assert_eq!(done(&lines), 0, "{log}");
    assert!(
        journal
            .iter()
            .any(|l| l.starts_with("SKIPPED ") && l.contains(" rule=login-done@v1 enter: ")),
        "{log}"
    );
    let direct = |program: &str, stalled: bool, counted: bool, generated: bool| {
        let mut m = fenced(vec![success.clone()]);
        let mut heard = Heard::default();
        {
            let mut s = session(&mut m, None);
            s.set_clock(TEST_NOW, PDT);
            s.set_zone_lookup(test_zones);
            s.program = Some(program.to_string());
            let read = s.screen().expect("the captured success screen");
            let mut point = point_at(&s, read.rows.clone(), read.seq);
            point.screen = read;
            if !counted {
                point.screen.human_seq = None;
            }
            if !generated {
                point.screen.generation = None;
            }
            s.stall_suspect = stalled;
            let row = success
                .iter()
                .find(|row| row.contains("Login successful"))
                .unwrap()
                .clone();
            s.turn_end_execute(
                &point,
                TurnEndAction::Confirm {
                    rule_id: crate::supervise::policy::login::RULE_LOGIN_DONE,
                    row,
                },
                &hosted(3),
                &[],
                &mut heard,
            )
            .expect("the captured action is handled");
        }
        (enters(&m), heard)
    };
    let (pressed, heard) = direct("claude", false, true, true);
    assert_eq!(pressed.len(), 1, "ordinary guarded execution");
    assert!(pressed[0].contains("if-human=7 ") && pressed[0].contains(" if-gen="));
    assert_eq!(done(&heard.said), 1);
    for (program, stalled, counted, generated) in [
        ("zsh", false, true, true),
        ("claude", true, true, true),
        ("claude", false, false, true),
        ("claude", false, true, false),
    ] {
        let (pressed, heard) = direct(program, stalled, counted, generated);
        assert!(
            pressed.is_empty(),
            "{program} {stalled} {counted} {generated}"
        );
        assert_eq!(done(&heard.said), 0);
    }
}

#[test]
fn the_picker_of_its_own_login_gets_the_method_the_login_is() {
    let mut login = rows(&[
        "⏺ Carrying on.",
        "  ⎿  Not logged in · Please run /login",
        "",
    ]);
    login.extend(composer("  ? for shortcuts"));
    let picker = aterm_phase::prompt::fixtures::screen(aterm_phase::prompt::fixtures::LOGIN_PICKER);
    let run = |tag: &str, method: Option<aterm_phase::LoginMethod>, counted: bool| {
        let (dir, path) = journal_file(tag);
        let mut m = fenced(vec![
            busy_screen(),
            login.clone(),
            login.clone(),
            login.clone(),
            picker.clone(),
            handoff(),
        ]);
        m.vanish_after = Some(3);
        m.cursor_on_caret = true;
        m.turn_releases = Some(3);
        m.key_releases = Some(4);
        if !counted {
            m.human_seq = None;
        }
        let host = AccountHost::scripted(&[None], &["adopted"]);
        *host.login_method.lock().unwrap() = method;
        let opts = SuperviseOpts {
            idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
            journal: Some(path.clone()),
            ..hosted(3)
        };
        let (lines, _) = watch_lines(&mut m, &opts);
        let (records, _) = journal_records(&path);
        let _ = std::fs::remove_dir_all(&dir);
        let journal: Vec<String> = records.into_iter().map(|r| r.line).collect();
        let log = format!("{lines:#?}\n{journal:#?}\n{:#?}", m.requests);
        (m, log)
    };
    let keys = |m: &Mock| -> Vec<String> {
        m.requests
            .iter()
            .filter(|r| r.starts_with("key "))
            .cloned()
            .collect()
    };
    let subscription = Some(aterm_phase::LoginMethod::Subscription);
    let (m, log) = run("te-picker", subscription, true);
    let pressed = keys(&m);
    assert_eq!(
        pressed.len(),
        1,
        "the focused subscription row's Enter: {log}"
    );
    assert!(
        pressed[0].starts_with("key if-human=7 ")
            && pressed[0].contains("Claude\\x20account\\x20with\\x20subscription")
            && pressed[0].ends_with(" enter"),
        "fenced on the person and its row: {log}"
    );
    let (m, log) = run("te-picker-unsaid", None, true);
    assert!(keys(&m).is_empty(), "the owner's pick: {log}");
    let (m, log) = run("te-picker-uncounted", subscription, false);
    assert!(keys(&m).is_empty(), "no key without its fence: {log}");
}

#[test]
fn a_renewed_login_nothing_relaunches_the_tab_onto_is_never_renewed_again() {
    let mut login = rows(&[
        "⏺ Carrying on.",
        "  ⎿  Not logged in · Please run /login",
        "",
    ]);
    login.extend(composer("  ? for shortcuts"));
    let run = |tag: &str, superseded: bool| {
        let (dir, ledger) = ledger_at(tag);
        let (journal_dir, path) = journal_file(&format!("te-{tag}"));
        let mut screens = vec![busy_screen(), login.clone(), login.clone()];
        if !superseded {
            screens.push(handoff());
        }
        let mut m = Mock::new(screens);
        m.vanish_after = Some(3);
        m.cursor_on_caret = true;
        m.turn_releases = Some(1);
        let host = AccountHost::scripted(&[None], &["adopted"]);
        *host.login_label.lock().unwrap() = Some("m7-a".to_string());
        host.login_superseded.store(superseded, Ordering::SeqCst);
        let mut opts = SuperviseOpts {
            idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
            journal: Some(path.clone()),
            ..hosted(3)
        };
        opts.policy.accounts = crate::harness::accounts::Mode::Off;
        opts.policy.relaunch = false;
        let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
            s.set_clock(ACCOUNT_NOW, PDT);
            s.set_approval_ledger(Some(ledger.clone()));
        });
        let (records, _) = journal_records(&path);
        let journal: Vec<String> = records.into_iter().map(|r| r.line).collect();
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&journal_dir);
        let log = format!(
            "{lines:#?}\n{journal:#?}\n{:?}\n{:?}\n{:#?}",
            host.all(),
            host.told(),
            m.requests
        );
        (m, journal, host, log)
    };
    let (m, journal, host, log) = run("renewed-by-hand", true);
    assert!(
        host.superseded_asked.load(Ordering::SeqCst) > 0,
        "asked at the lost login: {log}"
    );
    let by_hand = crate::supervise::policy::notice::RENEWED_BY_HAND.id;
    assert!(
        journal.iter().any(|l| l.starts_with("EXCEPTION ")
            && l.contains(&format!("kind=login when=now notice={by_hand}"))),
        "{log}"
    );
    assert!(
        !m.requests
            .iter()
            .any(|r| r.starts_with("meta set attention")),
        "the notice is the one surface: no badge beside it: {log}"
    );
    assert!(
        journal.iter().any(|l| l.starts_with("NOT ESCALATED ")
            && l.contains(&format!("the notice {by_hand} stands"))),
        "the loop's escalation stands down for the notice: {log}"
    );
    assert!(
        !journal.iter().any(|l| l.contains("accounts renew m7-a")),
        "no renewal asked again: {log}"
    );
    assert!(
        !m.requests
            .iter()
            .any(|r| !r.starts_with("meta ") && r.contains("/login")),
        "no /login: {log}"
    );
    assert!(host.said("restart").is_empty(), "nothing relaunched: {log}");

    let (m, journal, _, log) = run("renew-by-hand", false);
    assert!(
        journal.iter().any(|l| l.starts_with("EXCEPTION ")
            && l.contains("kind=login when=now notice=login-")),
        "{log}"
    );
    assert!(
        m.requests
            .iter()
            .any(|r| r.starts_with("turn ") && r.ends_with(" /login")),
        "{log}"
    );
    assert!(
        !journal.iter().any(|l| l.contains("accounts renew m7-a")),
        "{log}"
    );
}

#[test]
fn the_renewal_edge_keeps_what_the_decision_read() {
    let mut login = rows(&[
        "⏺ Carrying on.",
        "  ⎿  Not logged in · Please run /login",
        "",
    ]);
    login.extend(composer("  ? for shortcuts"));
    let screen = Screen {
        rows: login.clone(),
        seq: 102,
        cursor_col: 2,
        ..Screen::default()
    };
    let host = AccountHost::scripted(&[None], &["adopted"]);
    *host.login_label.lock().unwrap() = Some("m7-a".to_string());
    let mut opts = account_opts(&host);
    opts.policy.accounts = crate::harness::accounts::Mode::Off;
    opts.policy.relaunch = false;
    let mut m = Mock::new(vec![login]);
    let mut s = session(&mut m, Some("@s-1".to_string()));
    s.set_clock(ACCOUNT_NOW, PDT);
    let asked = || host.superseded_asked.load(Ordering::SeqCst);
    let r = s.turn_end_reading(&screen, None, &opts);
    assert_eq!(r.login_label.as_deref(), Some("m7-a"));
    assert!(!r.login_superseded);
    assert_eq!(asked(), 1);
    for _ in 0..3 {
        assert!(!s.account_edge(&opts));
    }
    assert_eq!(asked(), 1);
    host.stamp.fetch_add(1, Ordering::SeqCst);
    assert!(!s.account_edge(&opts));
    assert!(!s.account_edge(&opts));
    assert_eq!(asked(), 2);
    host.renews_after_answer.store(true, Ordering::SeqCst);
    let r = s.turn_end_reading(&screen, None, &opts);
    assert!(!r.login_superseded);
    let books = s.turn_end_reread(&screen, &opts);
    assert!(books.login_superseded);
    assert!(s.account_edge(&opts), "the renewal decides the point again");
    assert!(!s.account_edge(&opts), "once");
}

#[test]
fn the_rosters_word_is_asked_at_a_weekly_limit_alone() {
    use crate::supervise::policy::turn_end::{RosterWord, TurnEndReading};
    let host = Arc::new(AccountHost {
        roster: std::sync::Mutex::new(Some(RosterWord::Unread)),
        ..AccountHost::default()
    });
    let opts = SuperviseOpts {
        idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
        ..hosted(30)
    };
    let walled = |message: &str| {
        let mut r = rows(&["⏺ Carrying on.", &format!("  ⎿  {message}"), ""]);
        r.extend(composer("  ? for shortcuts"));
        TurnEndReading::of(
            &aterm_phase::ClaudeReader.read(&r, Some(2)),
            &r,
            false,
            None,
            None,
            None,
        )
    };
    let mut m = Mock::new(vec![busy_screen()]);
    let mut s = session(&mut m, None);
    let mut weekly =
        walled("You've hit your weekly limit · resets Oct 23 at 8am (America/Los_Angeles)");
    assert_eq!(weekly.wall, Some(aterm_phase::WallKind::UsageWeekly));
    s.account_advise(&mut weekly, 7, &opts);
    assert_eq!(weekly.roster, Some(RosterWord::Unread));
    assert_eq!(host.roster_asked.load(Ordering::SeqCst), 1);
    for message in [
        "You've hit your session limit · resets 3pm (America/Los_Angeles)",
        "",
    ] {
        let mut r = walled(message);
        assert_ne!(
            r.wall,
            Some(aterm_phase::WallKind::UsageWeekly),
            "{message}"
        );
        s.account_advise(&mut r, 8, &opts);
        assert_eq!(r.roster, None, "{message}");
    }
    assert_eq!(host.roster_asked.load(Ordering::SeqCst), 1);
}

#[test]
fn a_login_left_at_a_weekly_limit_asks_the_rosters_word() {
    use crate::supervise::policy::exception::exception_for;
    use crate::supervise::policy::turn_end::{RosterWord, SignInNext, TurnEndReading};
    use aterm_phase::prompt::fixtures::{LOGIN_EXPIRED, screen};
    let weekly_row = "⏺ You've hit your weekly limit · resets Oct 23 at 8am (America/Los_Angeles)";
    let with_tail = |tail: &[&str]| {
        let mut r = screen(LOGIN_EXPIRED);
        let at = r
            .iter()
            .position(|row| row.starts_with("⏺ Login expired"))
            .expect("the wall row");
        r.splice(at..=at, tail.iter().map(|s| (*s).to_string()));
        r
    };
    let read = |rows: &[String], worked: Option<Duration>| {
        TurnEndReading::of(
            &aterm_phase::ClaudeReader.read(rows, Some(2)),
            rows,
            false,
            worked,
            None,
            None,
        )
    };
    let next = RosterWord::Row {
        next: Some(SignInNext {
            label: "m8-2".to_string(),
            renew: true,
        }),
        ready: None,
        held: None,
    };
    for word in [
        next,
        // NEGATIVE CONTROL.
        RosterWord::Unlisted,
    ] {
        let host = Arc::new(AccountHost {
            roster: std::sync::Mutex::new(Some(word.clone())),
            ..AccountHost::default()
        });
        let opts = account_opts(&host);
        let mut m = Mock::new(vec![busy_screen()]);
        let mut s = session(&mut m, None);
        let t = Instant::now();
        let mut weekly = read(&with_tail(&[weekly_row]), Some(Duration::from_secs(180)));
        assert_eq!(weekly.wall, Some(aterm_phase::WallKind::UsageWeekly));
        s.turn_end.observe(&weekly, t);
        s.account_advise(&mut weekly, 7, &opts);
        let asked = host.roster_asked.load(Ordering::SeqCst);
        assert_eq!(asked, 1, "{word:?}");
        let rows = with_tail(&[weekly_row, "", "❯ /login", "  ⎿  Login interrupted"]);
        let mut left = TurnEndReading {
            person: Some(Duration::from_secs(5)),
            restartable: opts.idle_host.as_ref().is_some_and(|h| h.can_restart()),
            ..read(&rows, Some(Duration::from_secs(20)))
        };
        assert_eq!(left.wall, None, "the row drawn over");
        assert!(left.login_interrupted);
        let now = t + Duration::from_secs(90);
        s.turn_end.observe(&left, now);
        s.account_advise(&mut left, 8, &opts);
        assert_eq!(
            host.roster_asked.load(Ordering::SeqCst),
            asked + 1,
            "{word:?}"
        );
        assert_eq!(left.roster.as_ref(), Some(&word));
        assert_eq!(
            exception_for(&s.turn_end, &left, &opts.policy, now),
            None,
            "{word:?}"
        );
    }
}

#[test]
fn a_relaunch_the_host_switched_off_is_named_at_a_weekly_limit() {
    use crate::supervise::policy::exception::exception_for;
    use crate::supervise::policy::turn_end::{HeldBy, Ready, RosterWord, TurnEndReading};
    let walled = |message: &str, host: &AccountHost| {
        let mut r = rows(&["⏺ Carrying on.", &format!("  ⎿  {message}"), ""]);
        r.extend(composer("  ? for shortcuts"));
        TurnEndReading {
            restartable: host.can_restart(),
            ..TurnEndReading::of(
                &aterm_phase::ClaudeReader.read(&r, Some(2)),
                &r,
                false,
                None,
                None,
                None,
            )
        }
    };
    let word = RosterWord::Row {
        next: None,
        ready: Some(Ready {
            label: "m8-3".to_string(),
        }),
        held: Some(HeldBy::Earlier),
    };
    for off in [true, false] {
        let host = Arc::new(AccountHost {
            roster: std::sync::Mutex::new(Some(word.clone())),
            relaunch_off: std::sync::atomic::AtomicBool::new(off),
            ..AccountHost::default()
        });
        let opts = account_opts(&host);
        assert!(opts.policy.relaunch, "the host's config never says it off");
        let mut m = Mock::new(vec![busy_screen()]);
        let mut s = session(&mut m, None);
        let t = Instant::now();
        let mut weekly = walled(
            "You've hit your weekly limit · resets Oct 23 at 8am (America/Los_Angeles)",
            &host,
        );
        assert_eq!(weekly.wall, Some(aterm_phase::WallKind::UsageWeekly));
        weekly.reset_at = Some(t + Duration::from_secs(6 * 24 * 3600));
        s.turn_end.observe(&weekly, t);
        s.account_advise(&mut weekly, 7, &opts);
        assert_eq!(weekly.relaunch_off, off);
        assert_eq!(host.relaunch_off_asked.load(Ordering::SeqCst), 1, "{off}");
        assert_eq!(
            exception_for(&s.turn_end, &weekly, &opts.policy, t),
            None,
            "{off}"
        );
        let mut session_limit = walled(
            "You've hit your session limit · resets 3pm (America/Los_Angeles)",
            &host,
        );
        s.account_advise(&mut session_limit, 8, &opts);
        assert!(!session_limit.relaunch_off, "{off}");
        assert_eq!(host.relaunch_off_asked.load(Ordering::SeqCst), 1, "{off}");
    }
}

#[test]
fn a_relaunch_switched_on_reaches_a_tab_waiting_at_its_weekly_limit() {
    use crate::supervise::policy::turn_end::{Ready, RosterWord};
    let wait_at = |message: &str| {
        let host = AccountHost::moving("m8-3", "adopted");
        *host.roster.lock().unwrap() = Some(RosterWord::Row {
            next: None,
            ready: Some(Ready {
                label: "m8-3".to_string(),
            }),
            held: None,
        });
        host.relaunch_off.store(true, Ordering::SeqCst);
        host.stamp.store(7, Ordering::SeqCst);
        let rows: Vec<String> = limited_screen()
            .into_iter()
            .map(|r| {
                r.replace(
                    "You've hit your session limit · resets 7:30pm (America/Los_Angeles)",
                    message,
                )
            })
            .collect();
        (host, rows)
    };
    let weekly = "You've hit your weekly limit · resets Sep 23 at 8am (America/Los_Angeles)";
    let session_limit = "You've hit your session limit · resets 7:30pm (America/Los_Angeles)";
    for (message, edge) in [
        (weekly, true),
        // NEGATIVE CONTROL: a session limit reads no roster word.
        (session_limit, false),
    ] {
        let (host, rows) = wait_at(message);
        let opts = account_opts(&host);
        let mut m = Mock::new(vec![rows.clone()]);
        let mut s = session(&mut m, Some("@s-1".to_string()));
        s.set_clock(ACCOUNT_NOW, PDT);
        s.set_zone_lookup(test_zones);
        let point = account_point_of(
            rows,
            Phase::Limited {
                message: String::new(),
                reset: None,
            },
        );
        let action = s
            .turn_end_at_point(&point, Some(Duration::from_secs(180)), &opts)
            .expect("a decision");
        assert!(
            matches!(action, TurnEndAction::WaitUntil { .. }),
            "{message}: {action:?}"
        );
        s.turn_end_execute(&point, action, &opts, &[], &mut Alone)
            .expect("the wait is set");
        assert!(
            s.turn_end_due.is_some(),
            "{message}: the limit is waited out"
        );
        assert!(host.said("account").is_empty(), "{:?}", host.all());
        assert_eq!(
            host.roster_asked.load(Ordering::SeqCst),
            usize::from(edge),
            "{message}"
        );
        assert!(!s.account_edge(&opts), "{message}");
        host.relaunch_off.store(false, Ordering::SeqCst);
        host.stamp.store(8, Ordering::SeqCst);
        assert_eq!(s.account_edge(&opts), edge, "{message}");
        assert!(!s.account_edge(&opts), "{message}: once");
        if !edge {
            continue;
        }
        let again = s
            .turn_end_at_point(&point, None, &opts)
            .expect("a decision");
        assert!(
            matches!(
                &again,
                TurnEndAction::Restart { why: Restart::Account { to, at: AccountAt::Wall }, .. }
                    if to == "m8-3"
            ),
            "{again:?}"
        );
        assert_eq!(host.said("account"), ["account:wall:usage-weekly"]);
    }
}

fn after_weekly_limit(message: &str, tail: &[&str]) -> Vec<String> {
    let mut body = vec![
        "⏺ Running the suite again.".to_string(),
        format!("  ⎿  {message}"),
        String::new(),
    ];
    body.extend(tail.iter().map(|r| (*r).to_string()));
    body.push(String::new());
    let refs: Vec<&str> = body.iter().map(String::as_str).collect();
    let mut r = rows(&refs);
    r.extend(composer("  ? for shortcuts"));
    r
}

#[test]
fn an_account_added_at_a_login_left_at_a_weekly_limit_reaches_the_tab() {
    use crate::supervise::policy::turn_end::{Ready, RosterWord};
    let weekly = "You've hit your weekly limit · resets Sep 23 at 8am (America/Los_Angeles)";
    let walled: Vec<String> = limited_screen()
        .into_iter()
        .map(|r| {
            r.replace(
                "You've hit your session limit · resets 7:30pm (America/Los_Angeles)",
                weekly,
            )
        })
        .collect();
    let left = after_weekly_limit(weekly, &["❯ /login", "  ⎿  Login interrupted"]);
    let told = |s: &Session<'_, Mock>| {
        s.exception_now
            .as_ref()
            .and_then(|e| e.notice.as_ref())
            .map(|n| n.id)
    };
    let tail = include_str!("fixtures/claude-2.1.294-transcript-weekly-limit.jsonl").replace(
        "You've hit your weekly limit · resets Oct 11 at 9:35am (America/Los_Angeles)",
        weekly,
    );
    for news in [true, false] {
        let host = Arc::new(AccountHost {
            roster: std::sync::Mutex::new(Some(RosterWord::Row {
                next: None,
                ready: None,
                held: None,
            })),
            tail: std::sync::Mutex::new(Some(tail.clone())),
            ..AccountHost::default()
        });
        host.stamp.store(7, Ordering::SeqCst);
        let mut opts = account_opts(&host);
        opts.policy.resume_limits = false;
        let mut m = Mock::new(vec![walled.clone()]);
        let mut s = session(&mut m, Some("@s-1".to_string()));
        s.set_clock(ACCOUNT_NOW, PDT);
        s.set_zone_lookup(test_zones);
        let wall = account_point_of(
            walled.clone(),
            Phase::Limited {
                message: String::new(),
                reset: None,
            },
        );
        let action = s
            .turn_end_at_point(&wall, Some(Duration::from_secs(180)), &opts)
            .expect("a decision");
        s.turn_end_execute(&wall, action, &opts, &[], &mut Alone)
            .expect("carried out");
        s.ctl.screens = vec![left.clone()];
        let point = s.turn_of(Screen {
            rows: left.clone(),
            seq: 103,
            cursor_col: 2,
            ..Screen::default()
        });
        let action = s
            .turn_end_at_point(&point, Some(Duration::from_secs(20)), &opts)
            .expect("a decision");
        assert_eq!(action, TurnEndAction::Nothing, "{news}");
        assert_eq!(told(&s), None, "{news}");
        s.turn_end_execute(&point, action, &opts, &[], &mut Alone)
            .expect("carried out");
        assert!(s.turn_end_due.is_none(), "{news}: nothing is waited on");
        assert!(!s.account_edge(&opts), "{news}");
        *host.roster.lock().unwrap() = Some(RosterWord::Row {
            next: None,
            ready: Some(Ready {
                label: "work".to_string(),
            }),
            held: None,
        });
        if news {
            host.stamp.store(8, Ordering::SeqCst);
        }
        assert_eq!(s.account_edge(&opts), news, "the accounts' news");
        assert!(!s.account_edge(&opts), "{news}: once");
        if !news {
            assert_eq!(told(&s), None);
            continue;
        }
        s.turn_end_now(&point, &opts, &[], &mut Alone)
            .expect("decided again");
        assert_eq!(told(&s), None);
        assert!(s.turn_end_due.is_none(), "nothing is waited on still");
    }
}

#[test]
fn a_limits_end_tells_the_host_how_it_ended() {
    let weekly = "You've hit your weekly limit · resets Sep 23 at 8am (America/Los_Angeles)";
    let walled: Vec<String> = limited_screen()
        .into_iter()
        .map(|r| {
            r.replace(
                "You've hit your session limit · resets 7:30pm (America/Los_Angeles)",
                weekly,
            )
        })
        .collect();
    let served = |before: &[&str]| {
        let mut tail = before.to_vec();
        if !tail.is_empty() {
            tail.push("");
        }
        tail.extend([
            "❯ keep going",
            "",
            "⏺ The suite passes.",
            "",
            "✻ Worked for 5m 2s · done 5:20 PM",
        ]);
        after_weekly_limit(weekly, &tail)
    };
    let signed_in = ["❯ /login", "  ⎿  Login successful"];
    for sign_in in [false, true] {
        let host = Arc::new(AccountHost::default());
        let opts = account_opts(&host);
        let mut m = Mock::new(vec![walled.clone()]);
        let mut s = session(&mut m, Some("@s-1".to_string()));
        s.set_clock(ACCOUNT_NOW, PDT);
        s.set_zone_lookup(test_zones);
        let told = |host: &AccountHost| {
            (
                host.backs.load(Ordering::SeqCst),
                host.overs.load(Ordering::SeqCst),
            )
        };
        let mut seq = 102;
        let mut decide = |s: &mut Session<'_, Mock>, rows: &[String], worked: Option<Duration>| {
            s.ctl.screens = vec![rows.to_vec()];
            seq += 1;
            let point = s.turn_of(Screen {
                rows: rows.to_vec(),
                seq,
                cursor_col: 2,
                ..Screen::default()
            });
            s.turn_end_at_point(&point, worked, &opts)
                .expect("a decision")
        };
        let _ = decide(&mut s, &walled, Some(Duration::from_secs(180)));
        assert_eq!(told(&host), (0, 0), "{sign_in}");
        if sign_in {
            let _ = decide(
                &mut s,
                &after_weekly_limit(weekly, &signed_in),
                Some(Duration::from_secs(28)),
            );
            assert_eq!(told(&host), (0, 0), "the sign-in alone tells nothing");
            let _ = decide(&mut s, &served(&signed_in), Some(Duration::from_secs(300)));
            assert_eq!(told(&host), (1, 0), "worked past it after the sign-in");
        } else {
            let _ = decide(&mut s, &served(&[]), Some(Duration::from_secs(300)));
            assert_eq!(told(&host), (0, 1), "worked past it, no sign-in");
        }
    }
}

#[test]
fn a_draft_cleared_at_a_weekly_limit_lets_the_move_be_asked() {
    use crate::supervise::policy::turn_end::{Ready, RosterWord};
    let weekly = "You've hit your weekly limit · resets Sep 23 at 8am (America/Los_Angeles)";
    let cleared: Vec<String> = limited_screen()
        .into_iter()
        .map(|r| {
            r.replace(
                "You've hit your session limit · resets 7:30pm (America/Los_Angeles)",
                weekly,
            )
        })
        .collect();
    let draft = "❯ please also update the changelog";
    let mut drafted = cleared.clone();
    let caret = drafted
        .iter()
        .position(|r| r == "❯")
        .expect("the caret row");
    drafted[caret] = draft.to_string();
    let at = |s: &Session<'_, Mock>, rows: &[String], cursor_col: usize| {
        s.turn_of(Screen {
            rows: rows.to_vec(),
            seq: 102,
            cursor_col,
            ..Screen::default()
        })
    };
    let end_of_draft = draft.chars().count();
    let run = |clear: bool| {
        let host = AccountHost::moving("m8-3", "adopted");
        *host.roster.lock().unwrap() = Some(RosterWord::Row {
            next: None,
            ready: Some(Ready {
                label: "m8-3".to_string(),
            }),
            held: None,
        });
        let opts = account_opts(&host);
        let mut m = Mock::new(vec![drafted.clone()]);
        m.cursor_col = Some(end_of_draft);
        let mut s = session(&mut m, Some("@s-1".to_string()));
        s.set_clock(ACCOUNT_NOW, PDT);
        s.set_zone_lookup(test_zones);
        let point = at(&s, &drafted, end_of_draft);
        let action = s
            .turn_end_at_point(&point, Some(Duration::from_secs(180)), &opts)
            .expect("a decision");
        assert!(
            matches!(action, TurnEndAction::WaitUntil { .. }),
            "{action:?}"
        );
        let told = |s: &Session<'_, Mock>| {
            s.exception_now
                .as_ref()
                .and_then(|e| e.notice.as_ref())
                .map(|n| n.id)
        };
        assert_eq!(told(&s), None);
        s.turn_end_execute(&point, action, &opts, &[], &mut Alone)
            .expect("the wait is set");
        assert!(host.said("account").is_empty(), "{:?}", host.all());
        let (rows, col) = if clear {
            (cleared.clone(), 2)
        } else {
            (drafted.clone(), end_of_draft)
        };
        s.ctl.screens = vec![rows.clone()];
        s.ctl.cursor_col = Some(col);
        let seen = at(&s, &rows, col);
        let decided = s
            .draft_look(&seen, &opts, &[], &mut Alone)
            .expect("the look");
        let told = told(&s);
        drop(s);
        (decided, host.said("account"), told)
    };
    let (decided, asked, told) = run(true);
    assert!(decided, "decided again at the draft's going");
    assert_eq!(asked, ["account:wall:usage-weekly"]);
    assert_eq!(told, None);
    let (decided, asked, told) = run(false);
    assert!(!decided);
    assert!(asked.is_empty(), "{asked:?}");
    assert_eq!(told, None);
}

#[test]
fn a_moves_lost_login_is_not_served_by_its_login_dialog() {
    use crate::supervise::policy::turn_end::{TurnEndReading, decide_turn_end};
    use aterm_phase::prompt::fixtures::{LOGIN_HANDOFF, LOGIN_INTERRUPTED, LOGIN_SUCCESS, screen};
    let mut login = rows(&[
        "⏺ Carrying on.",
        "  ⎿  Not logged in · Please run /login",
        "",
    ]);
    login.extend(composer("  ? for shortcuts"));
    let run = |after: &[(Vec<String>, Option<Duration>)]| {
        let host = AccountHost::scripted(&[None], &["adopted"]);
        let opts = account_opts(&host);
        let mut m = Mock::new(vec![login.clone()]);
        let mut s = session(&mut m, Some("@s-1".to_string()));
        s.set_clock(ACCOUNT_NOW, PDT);
        let idle = ended("Carried on: stage 3 is done.");
        let r = TurnEndReading::of(
            &aterm_phase::ClaudeReader.read(&idle, Some(2)),
            &idle,
            false,
            None,
            None,
            None,
        );
        let t = Instant::now();
        s.turn_end.restarted(
            &Restart::Account {
                to: "work-2".to_string(),
                at: AccountAt::Wall,
            },
            true,
            &r,
            t,
        );
        s.turn_end
            .host_typed(t, crate::supervise::policy::turn_end::HostTurn::CarryOn);
        let point = |rows: &[String], seq: u64| Screen {
            rows: rows.to_vec(),
            seq,
            cursor_col: 2,
            ..Screen::default()
        };
        let lost = point(&login, 102);
        let mut first = s.turn_end_reading(&lost, Some(Duration::from_secs(180)), &opts);
        s.fold_point(&mut first, t, lost.seq, &opts);
        assert_eq!(host.said("answered:"), ["answered:signed-out"]);
        let mut at = t;
        let mut a = decide_turn_end(&s.turn_end, &first, &opts.policy, at);
        for _ in 0..4 {
            if let TurnEndAction::WaitUntil { until, .. } = a {
                at = until;
                a = decide_turn_end(&s.turn_end, &first, &opts.policy, at);
            }
        }
        assert_eq!(a.rule_id(), Some(RULE_LOGIN), "{a:?}");
        s.turn_end.acted(&a, &first, at);
        s.turn_end.took();
        for (k, (rows, worked)) in after.iter().enumerate() {
            let seq = 103 + k as u64;
            let shown = point(rows, seq);
            let now = at + Duration::from_secs(30 * (k as u64 + 1));
            let mut r = s.turn_end_reading(&shown, *worked, &opts);
            s.fold_point(&mut r, now, seq, &opts);
        }
        (
            host.said("served").len(),
            s.turn_end.account_move().is_some(),
        )
    };
    let busy = Some(Duration::from_secs(4));
    let left = run(&[
        (screen(LOGIN_HANDOFF), busy),
        (screen(LOGIN_INTERRUPTED), busy),
        (screen(LOGIN_INTERRUPTED), busy),
    ]);
    assert_eq!(left, (0, true), "the dialog served nothing");
    let served = run(&[
        (screen(LOGIN_HANDOFF), busy),
        (screen(LOGIN_SUCCESS), busy),
        (ended("Carried on: stage 4 is done."), busy),
    ]);
    assert_eq!(served, (1, false), "signed in, then served: once");
}

#[test]
fn the_host_is_told_once_what_answered_the_move() {
    let wall = limited_screen();
    let idle = ended("Carried on: stage 3 is done.");
    let mut login = rows(&[
        "⏺ Carrying on.",
        "  ⎿  Not logged in · Please run /login",
        "",
    ]);
    login.extend(composer("  ? for shortcuts"));
    let moved_to = |s: &mut Session<'_, Mock>, asked: bool| {
        let r = crate::supervise::policy::turn_end::TurnEndReading::of(
            &aterm_phase::ClaudeReader.read(&idle, Some(2)),
            &idle,
            false,
            None,
            None,
            None,
        );
        s.turn_end.restarted(
            &Restart::Account {
                to: "work-2".to_string(),
                at: AccountAt::Wall,
            },
            true,
            &r,
            Instant::now(),
        );
        if asked {
            s.turn_end.host_typed(
                Instant::now(),
                crate::supervise::policy::turn_end::HostTurn::CarryOn,
            );
        }
    };
    let told = |rows: &[String], moved: Option<bool>, worked: Option<Duration>| {
        let host = AccountHost::moving("pool-a", "adopted");
        let opts = account_opts(&host);
        let mut m = Mock::new(vec![rows.to_vec()]);
        let mut s = session(&mut m, Some("@s-1".to_string()));
        s.set_clock(ACCOUNT_NOW, PDT);
        s.set_zone_lookup(test_zones);
        if let Some(asked) = moved {
            moved_to(&mut s, asked);
        }
        let screen = Screen {
            rows: rows.to_vec(),
            seq: 102,
            cursor_col: 2,
            ..Screen::default()
        };
        let mut first = s.turn_end_reading(&screen, worked, &opts);
        s.fold_point(&mut first, Instant::now(), screen.seq, &opts);
        let again = s.turn_end_reading(&screen, None, &opts);
        assert_eq!(first.account, again.account);
        (host.all(), first.account)
    };
    let work = Some(Duration::from_secs(180));
    let next = Some(AccountAdvice::Move {
        to: "pool-a".to_string(),
    });
    assert_eq!(
        told(&wall, Some(true), work),
        (
            vec![
                "answered:wall".to_string(),
                "account:wall:usage-session".to_string()
            ],
            next.clone()
        )
    );
    assert_eq!(
        told(&login, Some(true), work),
        (
            vec![
                "answered:signed-out".to_string(),
                "account:wall:auth".to_string()
            ],
            next.clone()
        )
    );
    assert_eq!(
        told(&idle, Some(true), work),
        (
            vec!["served".to_string(), "account:turn-end".to_string()],
            next.clone()
        )
    );
    assert_eq!(
        told(&wall, None, work).0,
        ["account:wall:usage-session"],
        "no move was made: nothing to tell"
    );
    assert_eq!(
        told(&wall, Some(false), work).0,
        Vec::<String>::new(),
        "no turn was asked under the new login: the limit is the old account's notice, and \
         the wall's resume is what the point gets"
    );
    assert_eq!(
        told(&idle, Some(false), work).0,
        Vec::<String>::new(),
        "nobody asked that turn — and with the move unanswered, no move ahead is asked for"
    );
    assert_eq!(
        told(&login, Some(false), None),
        (Vec::<String>::new(), None),
        "no turn was asked under the new login: the lost login is the old process's notice, \
         nothing is struck and no host is asked"
    );
    assert_eq!(
        told(&login, None, None).0,
        Vec::<String>::new(),
        "a login the person lost: the login rule's, and no host is asked"
    );
}

#[test]
fn a_lost_login_no_read_saw_busy_is_struck_before_the_host_is_asked() {
    use crate::supervise::policy::turn_end::RULE_LIMIT_RESUME;
    let mut login = rows(&[
        "⏺ Resumed: carrying on.",
        "  ⎿  Not logged in · Please run /login",
        "",
    ]);
    login.extend(composer("  ? for shortcuts"));
    let wall = limited_screen();
    let run = |points: &[(&[String], Duration, Option<Duration>)]| {
        let host = AccountHost::moving("pool-a", "adopted");
        let opts = account_opts(&host);
        let mut m = Mock::new(points.iter().map(|p| p.0.to_vec()).collect());
        let action = {
            let mut s = session(&mut m, Some("@s-1".to_string()));
            s.set_clock(ACCOUNT_NOW, PDT);
            s.set_zone_lookup(test_zones);
            let r = crate::supervise::policy::turn_end::TurnEndReading::of(
                &aterm_phase::ClaudeReader.read(&wall, Some(2)),
                &wall,
                false,
                None,
                None,
                None,
            );
            let now = Instant::now();
            s.turn_end.restarted(
                &Restart::Account {
                    to: "work-2".to_string(),
                    at: AccountAt::Wall,
                },
                true,
                &r,
                now,
            );
            s.turn_end.acted(
                &TurnEndAction::Type {
                    text: "keep going".to_string(),
                    rule_id: RULE_LIMIT_RESUME,
                },
                &r,
                now,
            );
            let mut action = None;
            for (seq, (rows, take_within, worked)) in (102..).zip(points) {
                s.turn_end.timing.take_within = *take_within;
                let point = point_at(&s, rows.to_vec(), seq);
                let decided = s
                    .turn_end_at_point(&point, *worked, &opts)
                    .expect("a decision");
                if matches!(decided, TurnEndAction::Restart { .. }) {
                    s.turn_end_execute(&point, decided.clone(), &opts, &[], &mut Alone)
                        .expect("carried out");
                }
                action = Some(decided);
            }
            action.expect("a point")
        };
        (action, host.all())
    };
    let window = TurnEndTiming::default().take_within;
    let moved = [
        "answered:signed-out",
        "account:wall:auth",
        "restart:account:pool-a",
    ];
    let to_pool_a = Restart::Account {
        to: "pool-a".to_string(),
        at: AccountAt::Wall,
    };
    let (action, said) = run(&[(&login, window, None)]);
    assert!(
        matches!(&action, TurnEndAction::WaitUntil { why, .. } if why.starts_with("the worker to take")),
        "{action:?}"
    );
    assert!(
        said.is_empty(),
        "no advice for a login nobody struck: {said:?}"
    );
    let (action, said) = run(&[(&login, Duration::ZERO, None)]);
    assert!(
        matches!(&action, TurnEndAction::Restart { why, .. } if *why == to_pool_a),
        "{action:?}"
    );
    assert_eq!(said, moved, "struck on work-2 before the advice");
    let mut status = rows(&[
        "⏺ Resumed: carrying on.",
        "  ⎿  Not logged in · Please run /login",
        "",
        "❯ /status",
        "  ⎿  Status dialog dismissed",
        "",
    ]);
    status.extend(composer("  ? for shortcuts"));
    let (action, said) = run(&[(&login, window, None), (&status, Duration::ZERO, None)]);
    assert!(
        matches!(&action, TurnEndAction::Restart { why, .. } if *why == to_pool_a),
        "{action:?}"
    );
    assert_eq!(said, moved, "struck on work-2 before the advice");
    let mut back = rows(&[
        "⏺ Resumed: carrying on.",
        "  ⎿  Not logged in · Please run /login",
        "",
        "❯ /login",
        "  ⎿  Login successful",
        "",
    ]);
    back.extend(composer("  ? for shortcuts"));
    let (_, said) = run(&[(&login, window, None), (&back, Duration::ZERO, None)]);
    assert!(
        !said
            .iter()
            .any(|s| s == "answered:signed-out" || s == "account:wall:auth"),
        "{said:?}"
    );
}

#[test]
fn a_loop_started_on_an_old_told_move_waits_at_its_lost_login() {
    use crate::supervise::approvals::{
        ACCOUNT_MOVE_COMMAND, Outcome, Row, account_answer_reason, account_switch_reason,
    };
    use crate::supervise::policy::turn_end::{AccountAnswer, AccountBad};
    let mut login = rows(&[
        "⏺ Resumed: carrying on.",
        "  ⎿  Not logged in · Please run /login",
        "",
    ]);
    login.extend(composer("  ? for shortcuts"));
    let run = |tag: &str, told: String, screens: Vec<Vec<String>>| {
        let (dir, ledger) = ledger_at(tag);
        let now_ms = crate::supervise::journal::unix_ms();
        let command = format!("{ACCOUNT_MOVE_COMMAND}pool-a");
        let row = |outcome, reason: &str, box_seq, ago_min: i64| {
            Row {
                rule_id: RULE_ACCOUNT,
                outcome,
                command: &command,
                reason,
                box_seq,
            }
            .to_json(now_ms - ago_min * 60_000, None)
        };
        let written = [
            row(
                Outcome::Typed,
                &account_switch_reason("pool-a", AccountAt::Wall),
                90,
                90,
            ),
            row(Outcome::Waiting, &told, 91, 89),
        ];
        std::fs::write(&ledger, written.join("\n") + "\n").expect("the ledger");
        let host = AccountHost::scripted(
            &[Some(AccountAdvice::Wait {
                until: Instant::now() + Duration::from_secs(3600),
            })],
            &[],
        );
        let (lines, m, journal) =
            account_run_with(&format!("te-{tag}"), screens, &host, &ledger, |_| {});
        let _ = std::fs::remove_dir_all(&dir);
        let log = format!(
            "{lines:#?}\n{journal:#?}\n{:?}\n{:?}\n{:#?}",
            host.all(),
            host.told(),
            m.requests
        );
        (lines, m, host, log)
    };
    let by = AccountAnswer::Bad(AccountBad::SignedOut);
    let marked = account_answer_reason("pool-a", by);
    let at_login = || vec![login.clone(), login.clone()];
    let (_, m, host, log) = run("account-old-told", marked, at_login());
    assert!(
        !m.requests.iter().any(|r| r.contains("/login")),
        "no /login: {log}"
    );
    assert!(
        host.said("account")
            .iter()
            .any(|s| s == "account:wall:auth"),
        "the host is asked about the lost login: {log}"
    );
    assert!(
        !host.told().iter().any(|t| t == "exception:login:now"),
        "a wait with an end asks nothing: {log}"
    );
    let older = "the turn-end policy (account move answered: to=pool-a by=signed-out)";
    let (lines, _, _, log) = run("account-old-told-older", older.to_string(), at_login());
    assert!(
        lines
            .iter()
            .any(|l| l.ends_with(" rule=auth-login@v1 /login")),
        "{log}"
    );
}

#[test]
fn the_point_read_again_after_an_account_relaunch_tells_nothing() {
    let wall = limited_screen();
    let started = ended("Resumed: stage 2.");
    let run = |answer: &'static str| {
        let host = AccountHost::moving("pool-a", answer);
        let opts = account_opts(&host);
        let mut m = Mock::new(vec![wall.clone()]);
        {
            let (mut s, point) = account_session(&mut m);
            let r = crate::supervise::policy::turn_end::TurnEndReading::of(
                &aterm_phase::ClaudeReader.read(&started, Some(2)),
                &started,
                false,
                Some(Duration::from_secs(1)),
                None,
                None,
            );
            let now = Instant::now();
            s.turn_end.restarted(
                &Restart::Account {
                    to: "work-2".to_string(),
                    at: AccountAt::Ahead,
                },
                true,
                &r,
                now,
            );
            s.turn_end.observe(&r, now);
            let action = s
                .turn_end_at_point(&point, Some(Duration::from_secs(180)), &opts)
                .expect("a decision");
            assert!(
                matches!(&action, TurnEndAction::Restart { why: Restart::Account { to, .. }, .. } if to == "pool-a"),
                "{action:?}"
            );
            s.turn_end_execute(&point, action, &opts, &[], &mut Alone)
                .expect("carried out");
        }
        host.all()
    };
    assert_eq!(
        run("adopted"),
        ["account:wall:usage-session", "restart:account:pool-a"],
        "nothing told of the old screen once the relaunch is made"
    );
    assert_eq!(
        run("refused:line"),
        [
            "account:wall:usage-session",
            "restart:account:pool-a",
            "answered:wall"
        ],
        "no relaunch: the process is still work-2's"
    );
}

#[test]
fn a_loop_that_starts_after_another_keeps_its_bound_and_its_latch() {
    use crate::supervise::approvals::{
        ACCOUNT_MOVE_COMMAND, Outcome, Row, account_switch_reason, account_unmade_reason,
    };
    let wall = || vec![busy_screen(), limited_screen()];
    let now_ms = crate::supervise::journal::unix_ms();
    let row = |outcome: Outcome, reason: &str, ago_min: i64| {
        Row {
            rule_id: RULE_ACCOUNT,
            outcome,
            command: &format!("{ACCOUNT_MOVE_COMMAND}pool-a"),
            reason,
            box_seq: 90,
        }
        .to_json(now_ms - ago_min * 60_000, None)
    };
    let run = |tag: &str, rows: &[String]| {
        let (dir, ledger) = ledger_at(tag);
        if !rows.is_empty() {
            std::fs::write(&ledger, rows.join("\n") + "\n").expect("the ledger");
        }
        let host = AccountHost::moving("work-2", "adopted");
        let (lines, _, journal) = account_run(&format!("te-{tag}"), wall(), &host, &ledger);
        let _ = std::fs::remove_dir_all(&dir);
        (host.said("restart"), lines, journal)
    };
    let made = account_switch_reason("pool-a", AccountAt::Wall);
    let resumed = Row {
        rule_id: crate::supervise::policy::turn_end::RULE_LIMIT_RESUME,
        outcome: Outcome::Typed,
        command: "keep going",
        reason: "the turn-end policy",
        box_seq: 91,
    }
    .to_json(now_ms - 5 * 60_000, None);
    let four: Vec<String> = (1..=4)
        .rev()
        .map(|ago| row(Outcome::Typed, &made, 10 * ago))
        .chain(std::iter::once(resumed))
        .collect();
    let (asked, lines, journal) = run("account-seeded-bound", &four);
    assert!(asked.is_empty(), "{lines:#?}");
    assert!(
        journal
            .iter()
            .any(|l| l.starts_with("WAITING ") && l.contains("a breaker")),
        "{journal:#?}"
    );
    let unmade = account_unmade_reason("pool-a", AccountAt::Wall, Some("refused:line"));
    let (asked, lines, journal) = run("account-seeded-latch", &[row(Outcome::Skipped, &unmade, 5)]);
    assert!(asked.is_empty(), "{lines:#?}");
    assert!(
        journal
            .iter()
            .any(|l| l.starts_with("WAITING ") && l.contains("usage-session resets")),
        "{journal:#?}"
    );
    let (asked, lines, _) = run("account-seeded-none", &[]);
    assert_eq!(asked, ["restart:account:work-2"], "{lines:#?}");
    let old: Vec<String> = (1..=4)
        .map(|ago| row(Outcome::Typed, &made, 120 * ago))
        .collect();
    let (asked, lines, _) = run("account-seeded-old", &old);
    assert_eq!(asked, ["restart:account:work-2"], "{lines:#?}");
}

#[test]
fn a_launch_refused_before_the_agent_started_holds_no_move_off() {
    use crate::supervise::approvals::{ACCOUNT_MOVE_COMMAND, Outcome, Row, account_unmade_reason};
    let written_ms = (ACCOUNT_NOW - 300) * 1000;
    let run = |step: &str, started: i64| {
        let tag = format!("launch-latch-{}-{started}", step.replace(':', "-"));
        let (dir, ledger) = ledger_at(&tag);
        let row = Row {
            rule_id: RULE_ACCOUNT,
            outcome: Outcome::Skipped,
            command: &format!("{ACCOUNT_MOVE_COMMAND}pool-a"),
            reason: &account_unmade_reason("pool-a", AccountAt::Wall, Some(step)),
            box_seq: 90,
        }
        .to_json(written_ms, None);
        std::fs::write(&ledger, row + "\n").expect("the ledger");
        let host = AccountHost::moving("work-2", "adopted");
        *host.started.lock().unwrap() = Some(started);
        let screens = vec![busy_screen(), limited_screen()];
        let (lines, _, _) = account_run(&format!("te-{tag}"), screens, &host, &ledger);
        let _ = std::fs::remove_dir_all(&dir);
        (host.said("restart"), lines)
    };
    let started_since = ACCOUNT_NOW - 120;
    let started_before = ACCOUNT_NOW - 600;
    for step in [
        "refused:line",
        "refused:not-a-shell-job",
        "failed:account-not-applied",
    ] {
        let (asked, lines) = run(step, started_since);
        assert_eq!(asked, ["restart:account:work-2"], "{step}: {lines:#?}");
        let (asked, lines) = run(step, started_before);
        assert!(asked.is_empty(), "{step}: {asked:?}\n{lines:#?}");
    }
    let (asked, lines) = run("busy:x", started_since);
    assert!(asked.is_empty(), "{asked:?}\n{lines:#?}");
}

#[test]
fn the_ledger_reads_a_sessions_account_moves_back() {
    use crate::supervise::approvals::{
        ACCOUNT_MOVE_COMMAND, AccountMoves, Outcome, RULE_WALL_WAIT_END, Row, account_moves,
        account_switch_reason, account_unmade_reason,
    };
    let (dir, ledger) = ledger_at("account-ledger");
    let row = |rule: &str, outcome: Outcome, command: &str, reason: &str, ts: i64, sid| {
        Row {
            rule_id: rule,
            outcome,
            command,
            reason,
            box_seq: 1,
        }
        .to_json(ts, sid)
    };
    let to = format!("{ACCOUNT_MOVE_COMMAND}work-2");
    let write = |rows: &[String]| std::fs::write(&ledger, rows.join("\n") + "\n").expect("ledger");
    let me = Some("s-1");
    let unmade = |at, ts| {
        row(
            RULE_ACCOUNT,
            Outcome::Skipped,
            &to,
            &account_unmade_reason("work-2", at, Some("refused:line")),
            ts,
            me,
        )
    };
    write(&[unmade(AccountAt::Wall, 1_000)]);
    assert!(account_moves(&ledger, Some("@s-1")).unmade.is_some());
    write(&[
        unmade(AccountAt::Wall, 1_000),
        row(
            RULE_WALL_WAIT_END,
            Outcome::Waiting,
            "",
            "the wall left the screen",
            2_000,
            me,
        ),
    ]);
    assert!(
        account_moves(&ledger, Some("@s-1")).unmade.is_none(),
        "the wall is over"
    );
    write(&[
        row(
            RULE_ACCOUNT,
            Outcome::Typed,
            &to,
            &account_switch_reason("work-2", AccountAt::Wall),
            1_000,
            Some("s-2"),
        ),
        row(
            RULE_ACCOUNT,
            Outcome::Typed,
            "my own words",
            "the turn-end policy",
            2_000,
            me,
        ),
        unmade(AccountAt::Ahead, 3_000),
    ]);
    assert_eq!(
        account_moves(&ledger, Some("@s-1")),
        AccountMoves::default()
    );
    assert_eq!(
        account_moves(&dir.join("absent.jsonl"), me),
        AccountMoves::default()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_account_move_made_closes_the_model_switch_for_the_loop_after_it() {
    use crate::supervise::approvals::{
        ACCOUNT_MOVE_COMMAND, Outcome, Row, account_switch_reason, account_unmade_reason,
        model_switch_reason, open_model_switch,
    };
    use crate::supervise::policy::turn_end::RULE_MODEL_FALLBACK;
    let (dir, ledger) = ledger_at("account-closes-switch");
    let me = Some("s-1");
    let back_at = 1_789_700_000;
    let row = |rule: &str, outcome: Outcome, command: &str, reason: &str, ts: i64, sid| {
        Row {
            rule_id: rule,
            outcome,
            command,
            reason,
            box_seq: 1,
        }
        .to_json(ts, sid)
    };
    let switch = |ts: i64| {
        row(
            RULE_MODEL_FALLBACK,
            Outcome::Typed,
            "relaunch --model opus",
            &model_switch_reason(Some("fable"), Some(back_at), 1),
            ts,
            me,
        )
    };
    let to = format!("{ACCOUNT_MOVE_COMMAND}work-2");
    let moved = |ts: i64, sid| {
        row(
            RULE_ACCOUNT,
            Outcome::Typed,
            &to,
            &account_switch_reason("work-2", AccountAt::Wall),
            ts,
            sid,
        )
    };
    let write = |rows: &[String]| std::fs::write(&ledger, rows.join("\n") + "\n").expect("ledger");
    let open = || open_model_switch(&ledger, Some("@s-1")).map(|o| (o.to, o.back_at_unix));
    let standing = Some(("opus".to_string(), Some(back_at)));
    write(&[switch(1_000)]);
    assert_eq!(open(), standing);
    write(&[switch(1_000), moved(2_000, me)]);
    assert_eq!(open(), None, "the move let the switch go");
    let unmade = row(
        RULE_ACCOUNT,
        Outcome::Skipped,
        &to,
        &account_unmade_reason("work-2", AccountAt::Wall, Some("refused:line")),
        2_000,
        me,
    );
    let draft = row(
        RULE_ACCOUNT,
        Outcome::Typed,
        "my own words",
        "the turn-end policy",
        2_000,
        me,
    );
    for (what, after) in [
        ("a move not made", unmade),
        ("another session's move", moved(2_000, Some("s-2"))),
        ("a draft under the rule", draft),
    ] {
        write(&[switch(1_000), after]);
        assert_eq!(open(), standing, "{what}");
    }
    write(&[switch(1_000), moved(2_000, me), switch(3_000)]);
    assert_eq!(open(), standing);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_finished_task_opened_again_is_asked_about_at_the_limit_its_turn_ends_on() {
    let limited = || Phase::Limited {
        message: String::new(),
        reset: None,
    };
    let run = |worked: Option<Duration>| {
        let host = AccountHost::moving("work-2", "wait:changed");
        let mut opts = account_opts(&host);
        opts.policy.autopilot = crate::supervise::config::Autopilot::Land;
        let mut m = Mock::new(vec![limited_screen()]);
        let mut s = session(&mut m, Some("@s-1".to_string()));
        s.set_clock(ACCOUNT_NOW, PDT);
        s.set_zone_lookup(test_zones);
        let done = account_point_of(ended("All landed."), Phase::Idle);
        let _ = s
            .turn_end_at_point(&done, Some(Duration::from_secs(300)), &opts)
            .expect("a decision");
        assert!(s.turn_end.seed_task_done());
        let wall = account_point_of(limited_screen(), limited());
        let action = s
            .turn_end_at_point(&wall, worked, &opts)
            .expect("a decision");
        (action, host.said("account:wall"))
    };
    let (action, asked) = run(Some(Duration::from_secs(180)));
    assert_eq!(asked, ["account:wall:usage-session"]);
    assert!(
        matches!(
            &action,
            TurnEndAction::Restart { why: Restart::Account { to, at: AccountAt::Wall }, rule_id, .. }
                if to == "work-2" && *rule_id == RULE_ACCOUNT
        ),
        "{action:?}"
    );
    let (action, asked) = run(None);
    assert_eq!(action, TurnEndAction::Nothing);
    assert!(asked.is_empty(), "{asked:?}");
}

fn account_session<'m>(m: &'m mut Mock) -> (Session<'m, Mock>, Turn) {
    let mut s = session(m, Some("@s-1".to_string()));
    s.set_clock(ACCOUNT_NOW, PDT);
    s.set_zone_lookup(test_zones);
    let wall = account_point_of(
        limited_screen(),
        Phase::Limited {
            message: String::new(),
            reset: None,
        },
    );
    (s, wall)
}

#[test]
fn the_host_is_asked_on_the_folded_point_where_a_latch_has_ended() {
    let host = AccountHost::moving("work-2", "refused:line");
    let opts = account_opts(&host);
    let mut m = Mock::new(vec![limited_screen()]);
    let (mut s, wall) = account_session(&mut m);
    let action = s
        .turn_end_at_point(&wall, Some(Duration::from_secs(180)), &opts)
        .expect("a decision");
    s.turn_end_execute(&wall, action, &opts, &[], &mut Alone)
        .expect("the refusal is taken");
    assert!(s.turn_end.account_refused());
    assert_eq!(host.said("account"), ["account:wall:usage-session"]);
    let again = s.turn_end_at_point(&wall, None, &opts).expect("a decision");
    assert!(
        matches!(again, TurnEndAction::WaitUntil { .. }),
        "{again:?}"
    );
    assert_eq!(host.said("account").len(), 1, "{:?}", host.all());
    let end = account_point_of(ended("Stage 3 done; the suite is green."), Phase::Idle);
    let action = s
        .turn_end_at_point(&end, Some(Duration::from_secs(300)), &opts)
        .expect("a decision");
    assert_eq!(
        host.said("account"),
        ["account:wall:usage-session", "account:turn-end"],
        "asked at the very point that ended the latch"
    );
    assert!(
        matches!(
            &action,
            TurnEndAction::Restart { why: Restart::Account { to, at: AccountAt::Ahead }, .. }
                if to == "work-2"
        ),
        "{action:?}"
    );
}

#[test]
fn a_refused_moves_latch_keeps_the_hosts_word() {
    use crate::supervise::approvals::{
        ACCOUNT_MOVE_COMMAND, account_moves_in, account_unmade_reason,
    };
    for (step, want) in [
        ("refused:not-a-shell-job", Unmade::NoShell),
        ("refused:line", Unmade::Launch),
        ("failed:account-not-applied", Unmade::Launch),
        ("busy:x", Unmade::Again),
    ] {
        let host = AccountHost::moving("work-2", step);
        let opts = account_opts(&host);
        let mut m = Mock::new(vec![limited_screen()]);
        let (mut s, wall) = account_session(&mut m);
        let action = s.turn_end_at_point(&wall, None, &opts).expect("a decision");
        assert!(
            matches!(action, TurnEndAction::Restart { .. }),
            "{step}: {action:?}"
        );
        s.turn_end_execute(&wall, action, &opts, &[], &mut Alone)
            .expect("the refusal is taken");
        assert_eq!(s.turn_end.account_refusal(), Some(want), "{step}");
        let row = account_unmade_reason("work-2", AccountAt::Wall, Some(step));
        let body = format!(
            "{{\"sid\":\"s-1\",\"rule_id\":\"{RULE_ACCOUNT}\",\"decision\":\"skipped\",\"command\":\"{ACCOUNT_MOVE_COMMAND}work-2\",\"reason\":\"{row}\"}}\n"
        );
        assert_eq!(
            account_moves_in(&body, Some("s-1")).unmade,
            Some(want),
            "{step}: {body}"
        );
    }
    let row = account_unmade_reason("work-2", AccountAt::Wall, None);
    let body = format!(
        "{{\"sid\":\"s-1\",\"rule_id\":\"{RULE_ACCOUNT}\",\"decision\":\"skipped\",\"command\":\"{ACCOUNT_MOVE_COMMAND}work-2\",\"reason\":\"{row}\"}}\n"
    );
    assert_eq!(
        account_moves_in(&body, Some("s-1")).unmade,
        Some(Unmade::Again),
        "{body}"
    );
}

#[test]
fn news_of_the_accounts_clears_a_latch_and_no_news_asks_nothing() {
    let host = AccountHost::moving("work-2", "refused:line");
    let opts = account_opts(&host);
    let mut m = Mock::new(vec![limited_screen()]);
    let (mut s, wall) = account_session(&mut m);
    let decide = |s: &mut Session<'_, Mock>| {
        let action = s.turn_end_at_point(&wall, None, &opts).expect("a decision");
        let asked = matches!(action, TurnEndAction::Restart { .. });
        s.turn_end_execute(&wall, action, &opts, &[], &mut Alone)
            .expect("carried out");
        asked
    };
    assert!(decide(&mut s), "the move is asked");
    assert!(s.turn_end.account_refused() && s.turn_end_due.is_some());
    assert!(!s.account_edge(&opts));
    assert!(
        !decide(&mut s),
        "a refusal repeated with no news is not re-asked"
    );
    assert_eq!(host.said("restart").len(), 1, "{:?}", host.all());
    host.stamp.store(1, Ordering::SeqCst);
    assert!(
        s.account_edge(&opts),
        "the stamp moved under a latched wait"
    );
    assert!(
        !s.turn_end.account_refused(),
        "the latch is the news's to end"
    );
    assert!(decide(&mut s), "asked again");
    assert_eq!(host.said("restart").len(), 2, "{:?}", host.all());
    assert!(s.turn_end.account_refused());
    host.stamp.store(2, Ordering::SeqCst);
    assert!(!s.account_edge(&opts), "inside the minute");
    s.account_news_at = Instant::now().checked_sub(Duration::from_secs(61));
    assert!(s.account_edge(&opts), "the minute is out");
}

#[test]
fn under_accounts_off_nothing_is_told_or_asked() {
    let run = |mode: crate::harness::accounts::Mode| {
        let host = AccountHost::moving("pool-a", "adopted");
        let mut opts = account_opts(&host);
        opts.policy.accounts = mode;
        let mut m = Mock::new(vec![limited_screen()]);
        opts.policy.logins = mode != crate::harness::accounts::Mode::Off;
        let (mut s, wall) = account_session(&mut m);
        let idle = ended("Carried on: stage 3 is done.");
        let r = crate::supervise::policy::turn_end::TurnEndReading::of(
            &aterm_phase::ClaudeReader.read(&idle, Some(2)),
            &idle,
            false,
            None,
            None,
            None,
        );
        let made = Restart::Account {
            to: "work-2".to_string(),
            at: AccountAt::Wall,
        };
        s.turn_end.restarted(&made, true, &r, Instant::now());
        s.turn_end.host_typed(
            Instant::now(),
            crate::supervise::policy::turn_end::HostTurn::CarryOn,
        );
        let _ = s
            .turn_end_at_point(&wall, Some(Duration::from_secs(180)), &opts)
            .expect("a decision");
        host.stamp.store(5, Ordering::SeqCst);
        s.set_due(
            Instant::now() + Duration::from_secs(3600),
            "usage-session resets",
        );
        (host.all(), s.account_edge(&opts))
    };
    assert_eq!(
        run(crate::harness::accounts::Mode::Off),
        (Vec::<String>::new(), false)
    );
    let (said, edge) = run(crate::harness::accounts::Mode::Early);
    assert_eq!(said.first().map(String::as_str), Some("answered:wall"));
    assert!(edge);
}

#[test]
fn a_move_made_at_a_wall_is_carried_on_past_the_limit_it_left() {
    let moved = |carry: bool| {
        let host = AccountHost::moving("work-2", "adopted");
        let opts = account_opts(&host);
        let mut m = Mock::new(vec![limited_screen()]);
        let (mut s, wall) = account_session(&mut m);
        let action = s
            .turn_end_at_point(&wall, Some(Duration::from_secs(180)), &opts)
            .expect("a decision");
        let mut deadline = Instant::now() + Duration::from_secs(3600);
        s.limit_after(&wall, &mut deadline, true, &mut Alone)
            .expect("the episode opens");
        assert!(s.limit.is_some());
        s.turn_end_execute(&wall, action, &opts, &[], &mut Alone)
            .expect("the relaunch is made");
        assert_eq!(host.said("restart"), ["restart:account:work-2"]);
        assert!(
            s.limit.is_none(),
            "the episode was the limit of the account it left"
        );
        host.carry.store(carry, Ordering::SeqCst);
        let stepped = s
            .host_steps_here(&wall, &opts, &mut false, &mut Alone)
            .expect("the host's step");
        let action = s.turn_end_at_point(&wall, None, &opts).expect("a decision");
        (stepped, action, host.all())
    };
    let resume = TurnEndAction::Type {
        text: "keep going".to_string(),
        rule_id: crate::supervise::policy::turn_end::RULE_LIMIT_RESUME,
    };
    let (stepped, action, said) = moved(true);
    assert!(stepped, "{said:?}");
    assert_eq!(
        said,
        [
            "account:wall:usage-session",
            "restart:account:work-2",
            "carry-on"
        ]
    );
    assert_eq!(action, resume);
    let (stepped, action, said) = moved(false);
    assert!(!stepped);
    assert_eq!(action, resume);
    assert_eq!(
        said,
        ["account:wall:usage-session", "restart:account:work-2"]
    );
}

#[test]
fn a_limit_only_the_transcript_shows_is_the_wall_and_holds_the_carry_on() {
    let rows = {
        let mut r = rows(&[
            "⏺ Running the suites before the release notes.",
            "",
            "⏺ Auto mode lets Claude handle permission prompts automatically — Claude checks \
             each tool call for risky actions and prompt injection",
            "  before executing. Ideal for long-running tasks. Shift+Tab to change mode.",
            "",
        ]);
        r.extend(composer(
            "  ⏵⏵ auto mode on (shift+tab to cycle) · ← for agents",
        ));
        r
    };
    let weekly = |reset: i64| crate::harness::upgrade::StandingLimit {
        text: "You've hit your weekly limit · resets Sep 18 at 8pm (America/Los_Angeles)"
            .to_string(),
        until: u64::try_from(reset).ok(),
        window: Some("seven_day".to_string()),
        at: u64::try_from(ACCOUNT_NOW - 3600).ok(),
        row: Some("row-weekly".to_string()),
    };
    let first_point = |standing: Option<crate::harness::upgrade::StandingLimit>, carry: bool| {
        let host = AccountHost::scripted(&[None], &["adopted"]);
        *host.standing.lock().unwrap() = standing;
        *host.pid.lock().unwrap() = Some(4242);
        *host.step.lock().unwrap() = Some("continued");
        host.carry.store(carry, Ordering::SeqCst);
        let opts = account_opts(&host);
        let mut m = Mock::new(vec![rows.clone()]);
        let mut s = session(&mut m, Some("@s-restored".to_string()));
        s.set_clock(ACCOUNT_NOW, PDT);
        s.set_zone_lookup(test_zones);
        let mut point = account_point_of(rows.clone(), Phase::Idle);
        point.screen.cursor_row = rows.iter().position(|r| r.starts_with('❯')).expect("caret");
        let stepped = s
            .host_steps_here(&point, &opts, &mut false, &mut Alone)
            .expect("the host's step");
        let action = s
            .turn_end_at_point(&point, Some(Duration::from_secs(4)), &opts)
            .expect("a decision");
        s.point_as_decided(&mut point);
        let mut deadline = Instant::now() + Duration::from_secs(60);
        s.limit_after(&point, &mut deadline, false, &mut Alone)
            .expect("the episode");
        let episode = s.limit.is_some();
        host.carry.store(!stepped, Ordering::SeqCst);
        let again = s
            .host_steps_here(&point, &opts, &mut false, &mut Alone)
            .expect("the host's next step");
        (stepped, action, point.phase, episode, again, host)
    };
    let (stepped, action, phase, episode, again, host) =
        first_point(Some(weekly(ACCOUNT_NOW + 3 * 3600)), true);
    assert!(!stepped, "the carry-on is withheld");
    assert!(
        matches!(&action, TurnEndAction::WaitUntil { why, .. } if why.contains("usage-weekly")),
        "{action:?}"
    );
    assert!(
        matches!(&phase, Phase::Limited { message, reset: Some(r) }
            if message.starts_with("You've hit your weekly limit") && r.starts_with("Sep 18 at 8pm")),
        "{phase:?}"
    );
    assert!(episode, "the limit's episode opens");
    assert!(!again, "the episode withholds the host's next step");
    assert!(host.said("carry-on").is_empty(), "{:?}", host.all());

    let host = AccountHost::scripted(&[None], &[]);
    *host.pid.lock().unwrap() = Some(4242);
    let opts = account_opts(&host);
    let mut m = Mock::new(vec![idle_screen()]);
    let mut s = session(&mut m, Some("@s-1".to_string()));
    s.set_clock(ACCOUNT_NOW, PDT);
    let idle = account_point_of(idle_screen(), Phase::Idle);
    let _ = s.turn_end_at_point(&idle, Some(Duration::from_secs(60)), &opts);
    assert_eq!(
        host.standing_asks.load(Ordering::SeqCst),
        1,
        "the first decision"
    );
    for _ in 0..3 {
        let _ = s.turn_end_at_point(&idle, None, &opts);
    }
    assert_eq!(host.standing_asks.load(Ordering::SeqCst), 1, "not again");
    *host.pid.lock().unwrap() = Some(5151);
    let _ = s.turn_end_at_point(&idle, Some(Duration::from_secs(60)), &opts);
    assert_eq!(
        host.standing_asks.load(Ordering::SeqCst),
        2,
        "a new process"
    );
    *host.conversation.lock().unwrap() = Some("c-1".to_string());
    let _ = s.turn_end_at_point(&idle, None, &opts);
    let _ = s.turn_end_at_point(&idle, None, &opts);
    assert_eq!(
        host.standing_asks.load(Ordering::SeqCst),
        3,
        "a conversation named since"
    );
    *host.conversation.lock().unwrap() = Some("c-2".to_string());
    let _ = s.turn_end_at_point(&idle, None, &opts);
    assert_eq!(
        host.standing_asks.load(Ordering::SeqCst),
        4,
        "another conversation"
    );
    let host = AccountHost::scripted(&[None], &[]);
    *host.pid.lock().unwrap() = Some(6161);
    let opts = account_opts(&host);
    let mut m = Mock::new(vec![limited_screen()]);
    let (mut s, wall) = account_session(&mut m);
    let _ = s.turn_end_at_point(&wall, Some(Duration::from_secs(60)), &opts);
    assert_eq!(host.standing_asks.load(Ordering::SeqCst), 0, "walled");
    let _ = s.turn_end_at_point(&idle, Some(Duration::from_secs(60)), &opts);
    assert_eq!(
        host.standing_asks.load(Ordering::SeqCst),
        1,
        "the first open decision"
    );
    let host = AccountHost::scripted(&[None], &[]);
    *host.pid.lock().unwrap() = Some(7171);
    *host.named_on_ask.lock().unwrap() = Some("c-1".to_string());
    let opts = account_opts(&host);
    let mut m = Mock::new(vec![idle_screen()]);
    let mut s = session(&mut m, Some("@s-1".to_string()));
    s.set_clock(ACCOUNT_NOW, PDT);
    let _ = s.turn_end_at_point(&idle, Some(Duration::from_secs(60)), &opts);
    for _ in 0..3 {
        let _ = s.turn_end_at_point(&idle, None, &opts);
    }
    assert_eq!(
        host.standing_asks.load(Ordering::SeqCst),
        1,
        "named in answering"
    );

    let (stepped, action, phase, episode, _, host) = first_point(None, true);
    assert!(stepped);
    assert_eq!(host.said("carry-on"), ["carry-on"]);
    assert_eq!(phase, Phase::Idle);
    assert!(!episode);
    assert!(
        matches!(&action, TurnEndAction::Type { rule_id, .. }
            if *rule_id == crate::supervise::policy::turn_end::RULE_CONTINUE),
        "the carry-on answered, the continue policy's point: {action:?}"
    );
    let spent = ACCOUNT_NOW
        - i64::try_from(crate::harness::upgrade_drive::LIMIT_GRACE_S).expect("grace")
        - 60;
    let (stepped, _, _, _, _, host) = first_point(Some(weekly(spent)), true);
    assert!(stepped, "{:?}", host.all());
    assert_eq!(host.said("carry-on"), ["carry-on"]);
    let (stepped, action, _, _, _, _) = first_point(Some(weekly(spent)), false);
    assert!(!stepped);
    assert_eq!(
        action,
        TurnEndAction::Type {
            text: "keep going".to_string(),
            rule_id: crate::supervise::policy::turn_end::RULE_LIMIT_RESUME,
        }
    );
}

#[test]
fn a_limit_the_screen_shows_ended_under_its_notice_is_no_standing_limit() {
    let screen = |reset: &str| {
        let mut r = rows(&[
            "\u{276F} [aterm harness] Autopilot is Land: land ALL of this work into the remote main \
             branch, no exceptions.",
            &format!(
                "  \u{23BF}  You've hit your session limit \u{b7} resets {reset} (America/Los_Angeles)"
            ),
            "     /upgrade to increase your usage limit.",
            "",
            &format!(
                "\u{23FA} Usage limit reached \u{b7} continuing automatically at {reset} \u{b7} esc to \
                 cancel"
            ),
            "",
            "\u{273B} Churned for 0s \u{b7} done 4:58 PM",
            "",
            "\u{23FA} Usage limit reset \u{b7} continuing automatically",
            "",
            "\u{23FA} Nothing left: the parser is committed and pushed to origin/main.",
            "",
            "\u{273B} Crunched for 3s \u{b7} done 5:04 PM",
            "",
        ]);
        r.extend(composer(
            "  \u{23F5}\u{23F5} auto mode on (shift+tab to cycle)",
        ));
        r
    };
    let session_limit = crate::harness::upgrade::StandingLimit {
        text: "You've hit your session limit \u{b7} resets 5:04pm (America/Los_Angeles)"
            .to_string(),
        until: u64::try_from(ACCOUNT_NOW - 60).ok(),
        window: Some("five_hour".to_string()),
        at: u64::try_from(ACCOUNT_NOW - 3 * 60).ok(),
        row: Some("row-session".to_string()),
    };
    let decide = |rows: Vec<String>| {
        let host = AccountHost::scripted(&[None], &[]);
        *host.standing.lock().unwrap() = Some(session_limit.clone());
        *host.pid.lock().unwrap() = Some(4242);
        let opts = account_opts(&host);
        let mut m = Mock::new(vec![rows.clone()]);
        let mut s = session(&mut m, Some("@s-1".to_string()));
        s.set_clock(ACCOUNT_NOW, PDT);
        s.set_zone_lookup(test_zones);
        let mut point = account_point_of(rows.clone(), Phase::Idle);
        point.screen.cursor_row = rows
            .iter()
            .rposition(|r| r.starts_with('\u{276F}'))
            .expect("caret");
        let action = s
            .turn_end_at_point(&point, Some(Duration::from_secs(4)), &opts)
            .expect("a decision");
        s.point_as_decided(&mut point);
        (
            action,
            point.phase,
            host.standing_asks.load(Ordering::SeqCst),
        )
    };
    let (action, phase, asks) = decide(screen("5:04pm"));
    assert_eq!(asks, 1, "the host was asked");
    assert_eq!(phase, Phase::Idle, "{action:?}");
    assert!(
        !matches!(&action, TurnEndAction::WaitUntil { why, .. } if why.contains("usage-session")),
        "{action:?}"
    );
    let (action, phase, _) = decide(screen("2:10pm"));
    assert!(
        matches!(&phase, Phase::Limited { message, .. }
            if message.starts_with("You've hit your session limit")),
        "{phase:?} {action:?}"
    );
}

#[test]
fn a_continuation_the_transcript_holds_ends_the_limit_with_its_notice_scrolled_off() {
    let tail = include_str!("fixtures/claude-2.1.306-transcript-session-limit-continued.jsonl");
    let rows_of_tail: Vec<&str> = tail.lines().collect();
    let cut = |id: &str| {
        let at = rows_of_tail
            .iter()
            .position(|r| r.contains(&format!(r#""uuid":"{id}""#)))
            .expect(id);
        rows_of_tail[..at].join("\n")
    };
    let read = tail.trim_end().to_string();
    let before = cut("67ad4c9a-a719-4358-a459-60b578fa4deb");
    let mut screen = rows(&[
        "\u{23FA} Usage limit reset \u{b7} continuing automatically",
        "",
        "\u{23FA} Nothing left: the parser is committed and pushed to origin/main.",
        "",
        "\u{273B} Crunched for 3s \u{b7} done 5:49 AM",
        "",
    ]);
    screen.extend(composer(
        "  \u{23F5}\u{23F5} auto mode on (shift+tab to cycle)",
    ));
    let now = 1_792_586_958;
    let decide = |tail: &str| {
        let host = AccountHost::scripted(&[None], &[]);
        *host.standing.lock().unwrap() = crate::harness::upgrade::transcript_standing_limit(tail);
        *host.pid.lock().unwrap() = Some(4242);
        let opts = account_opts(&host);
        let mut m = Mock::new(vec![screen.clone()]);
        let mut s = session(&mut m, Some("@s-1".to_string()));
        s.set_clock(now, PDT);
        s.set_zone_lookup(test_zones);
        let mut point = account_point_of(screen.clone(), Phase::Idle);
        point.screen.cursor_row = screen
            .iter()
            .rposition(|r| r.starts_with('\u{276F}'))
            .expect("caret");
        let action = s
            .turn_end_at_point(&point, Some(Duration::from_secs(4)), &opts)
            .expect("a decision");
        s.point_as_decided(&mut point);
        (
            action,
            point.phase,
            host.standing_asks.load(Ordering::SeqCst),
        )
    };
    let (action, phase, asks) = decide(&read);
    assert_eq!(asks, 1, "the host was asked");
    assert_eq!(phase, Phase::Idle, "{action:?}");
    assert!(
        !matches!(&action, TurnEndAction::WaitUntil { why, .. } if why.contains("usage-session")),
        "{action:?}"
    );
    let (action, phase, _) = decide(&before);
    assert!(
        matches!(&phase, Phase::Limited { message, .. }
            if message.starts_with("You've hit your session limit")),
        "{phase:?} {action:?}"
    );
}

#[test]
fn a_row_found_right_after_a_turn_is_asked_about_again_at_the_next_decision() {
    const CONVERSATION: &str = "d3fd9c69-e561-4a64-a7fd-c1302af4e2d1";
    let fixture = include_str!("fixtures/claude-2.1.306-transcript-session-limit-continued.jsonl");
    let lines: Vec<&str> = fixture.lines().collect();
    let at = |id: &str| {
        lines
            .iter()
            .position(|r| r.contains(&format!(r#""uuid":"{id}""#)))
            .expect(id)
    };
    let walled = lines[..at("ffaa6070-57f6-492e-8205-c74329f9f7b7")].join("\n");
    let act = lines[at("13c38282-13a0-477a-84df-5594968622a6")]
        .replace("13c38282-13a0-477a-84df-5594968622a6", "act-past-the-reset")
        .replace(
            "47d9d78e-f8a1-4e2d-b891-c6d7f3ab41ff",
            "587886e0-3ef3-466d-a6d9-7ea888a46a44",
        )
        .replace("2026-10-21T12:45:15.100Z", "2026-10-21T12:51:00.000Z");
    let reply = lines[at("501239ab-c5da-4032-9991-787ec24f148f")]
        .replace(
            "Implemented the parser module and its unit tests in this turn.",
            "Nothing left: the parser is committed and pushed to origin/main.",
        )
        .replace("501239ab-c5da-4032-9991-787ec24f148f", "reply-to-the-act")
        .replace("c809f4b4-382a-4496-838f-6fe48a564208", "act-past-the-reset")
        .replace("2026-10-21T12:43:14.255Z", "2026-10-21T12:51:03.000Z");
    let lagging = format!("{walled}\n{act}");
    let caught_up = format!("{lagging}\n{reply}");
    assert!(
        crate::harness::upgrade::transcript_standing_limit(&lagging).is_some(),
        "the row stands on the tail that lags"
    );
    assert_eq!(
        crate::harness::upgrade::transcript_standing_limit(&caught_up),
        None
    );
    let screen = {
        let mut r = rows(&[
            "\u{276F} [aterm harness] Autopilot is Land: land ALL of this work into the remote main \
             branch, no exceptions.",
            "",
            "\u{23FA} Nothing left: the parser is committed and pushed to origin/main.",
            "",
            "\u{273B} Crunched for 3s \u{b7} done 5:51 AM",
            "",
        ]);
        r.extend(composer(
            "  \u{23F5}\u{23F5} auto mode on (shift+tab to cycle)",
        ));
        r
    };
    let limited = |phase: &Phase| {
        matches!(phase, Phase::Limited { message, .. }
            if message.starts_with("You've hit your session limit"))
    };
    let run = |steps: &[(Option<Duration>, &str)]| {
        let host = AccountHost::scripted(&[None], &[]);
        *host.pid.lock().unwrap() = Some(4242);
        *host.conversation.lock().unwrap() = Some(CONVERSATION.to_string());
        let opts = account_opts(&host);
        let mut m = Mock::new(vec![screen.clone()]);
        let mut s = session(&mut m, Some("@s-1".to_string()));
        s.set_clock(1_792_587_064, PDT);
        s.set_zone_lookup(test_zones);
        let mut seen = Vec::new();
        for (worked, tail) in steps {
            *host.standing.lock().unwrap() =
                crate::harness::upgrade::transcript_standing_limit(tail);
            *host.tail.lock().unwrap() = Some((*tail).to_string());
            let mut point = account_point_of(screen.clone(), Phase::Idle);
            point.screen.cursor_row = screen
                .iter()
                .rposition(|r| r.starts_with('\u{276F}'))
                .expect("caret");
            let action = s
                .turn_end_at_point(&point, *worked, &opts)
                .expect("a decision");
            s.point_as_decided(&mut point);
            seen.push((
                point.phase,
                host.standing_asks.load(Ordering::SeqCst),
                action,
            ));
        }
        seen
    };
    let waits_on_the_limit = |action: &TurnEndAction| {
        matches!(action, TurnEndAction::WaitUntil { why, .. }
            if why.contains("usage-session") || why.contains("remembered limit"))
    };
    let turn = Some(Duration::from_secs(4));
    let seen = run(&[(turn, &lagging), (None, &caught_up), (None, &caught_up)]);
    assert!(limited(&seen[0].0) && seen[0].1 == 1, "{:?}", seen[0]);
    for (i, (phase, asks, action)) in seen.iter().enumerate().skip(1) {
        assert_eq!((phase, *asks), (&Phase::Idle, 2), "{i}: {action:?}");
        assert!(!waits_on_the_limit(action), "{i}: {action:?}");
    }
    let seen = run(&[(turn, &lagging), (None, &lagging), (None, &caught_up)]);
    assert!(limited(&seen[1].0) && seen[1].1 == 2, "{:?}", seen[1]);
    assert!(limited(&seen[2].0) && seen[2].1 == 2, "{:?}", seen[2]);
    let seen = run(&[(None, &lagging), (None, &caught_up)]);
    assert!(limited(&seen[0].0) && seen[0].1 == 1, "{:?}", seen[0]);
    assert!(limited(&seen[1].0) && seen[1].1 == 1, "{:?}", seen[1]);
}

#[test]
fn a_relaunchs_first_decision_on_a_kept_snapshot_reads_the_standing_limit() {
    let rows = {
        let mut r = rows(&[
            "⏺ Running the suites before the release notes.",
            "",
            "⏺ Auto mode lets Claude handle permission prompts automatically — Claude checks \
             each tool call for risky actions and prompt injection",
            "  before executing. Ideal for long-running tasks. Shift+Tab to change mode.",
            "",
        ]);
        r.extend(composer(
            "  ⏵⏵ auto mode on (shift+tab to cycle) · ← for agents",
        ));
        r
    };
    let weekly = crate::harness::upgrade::StandingLimit {
        text: "You've hit your weekly limit · resets Sep 18 at 8pm (America/Los_Angeles)"
            .to_string(),
        until: u64::try_from(ACCOUNT_NOW + 3 * 3600).ok(),
        window: Some("seven_day".to_string()),
        at: u64::try_from(ACCOUNT_NOW - 3600).ok(),
        row: Some("row-weekly".to_string()),
    };
    let relaunched = |carry: bool| {
        let host = AccountHost::scripted(&[None], &["adopted"]);
        *host.pid.lock().unwrap() = Some(4242);
        *host.step.lock().unwrap() = Some("continued");
        let opts = account_opts(&host);
        let mut m = Mock::new(vec![rows.clone()]);
        let mut s = session(&mut m, Some("@s-restored".to_string()));
        s.set_clock(ACCOUNT_NOW, PDT);
        s.set_zone_lookup(test_zones);
        let idle = account_point_of(idle_screen(), Phase::Idle);
        let _ = s.turn_end_at_point(&idle, Some(Duration::from_secs(1800)), &opts);
        assert_eq!(
            host.standing_asks.load(Ordering::SeqCst),
            1,
            "the old process"
        );
        *host.standing.lock().unwrap() = Some(weekly.clone());
        *host.pid_on_ask.lock().unwrap() = Some(5151);
        host.carry.store(carry, Ordering::SeqCst);
        let mut point = account_point_of(rows.clone(), Phase::Idle);
        point.screen.cursor_row = rows.iter().position(|r| r.starts_with('❯')).expect("caret");
        let action = s
            .turn_end_at_point(&point, Some(Duration::from_secs(4)), &opts)
            .expect("a decision");
        s.point_as_decided(&mut point);
        let stepped = s
            .host_steps_here(&point, &opts, &mut false, &mut Alone)
            .expect("the host's step");
        (action, point.phase, stepped, host)
    };
    let (action, phase, stepped, host) = relaunched(true);
    assert!(
        matches!(&action, TurnEndAction::WaitUntil { why, .. } if why.contains("usage-weekly")),
        "{action:?}"
    );
    assert!(matches!(phase, Phase::Limited { .. }), "{phase:?}");
    assert!(!stepped, "the carry-on is withheld");
    assert!(host.said("carry-on").is_empty(), "{:?}", host.all());
    let (action, phase, _, host) = relaunched(false);
    assert!(
        matches!(&action, TurnEndAction::WaitUntil { why, .. } if why.contains("usage-weekly")),
        "no step wanted: {action:?}"
    );
    assert!(matches!(phase, Phase::Limited { .. }), "{phase:?}");
    assert_eq!(host.standing_asks.load(Ordering::SeqCst), 2, "asked again");
    let host = AccountHost::scripted(&[None], &[]);
    *host.pid.lock().unwrap() = Some(4242);
    let opts = account_opts(&host);
    let mut m = Mock::new(vec![rows.clone()]);
    let mut s = session(&mut m, Some("@s-restored".to_string()));
    s.set_clock(ACCOUNT_NOW, PDT);
    s.set_zone_lookup(test_zones);
    let idle = account_point_of(idle_screen(), Phase::Idle);
    let _ = s.turn_end_at_point(&idle, Some(Duration::from_secs(1800)), &opts);
    *host.standing.lock().unwrap() = Some(weekly.clone());
    let mut point = account_point_of(rows.clone(), Phase::Idle);
    point.screen.cursor_row = rows.iter().position(|r| r.starts_with('❯')).expect("caret");
    let action = s
        .turn_end_at_point(&point, None, &opts)
        .expect("a decision");
    s.point_as_decided(&mut point);
    assert_eq!(point.phase, Phase::Idle, "{action:?}");
    assert_eq!(
        host.standing_asks.load(Ordering::SeqCst),
        1,
        "asked once a process"
    );
}

#[test]
fn a_relaunched_agents_redrawn_countdown_is_waited_as_nobodys() {
    let rows = {
        let mut r = limited_screen();
        let notice = r
            .iter()
            .position(|row| row.contains("You've hit your session limit"))
            .expect("the notice");
        r.splice(
            notice + 1..notice + 1,
            [
                "",
                "⏺ Usage limit reached · continuing automatically at 7:30pm · esc to cancel",
            ]
            .map(str::to_string),
        );
        let mode = r
            .iter()
            .rposition(|row| row.contains("auto mode on"))
            .expect("the footer's mode row");
        r.splice(
            mode..mode,
            [
                "  ⚠ Usage limit reached · limit resets 7:30pm",
                "    Continuing automatically at 7:30pm · esc to cancel",
            ]
            .map(str::to_string),
        );
        r
    };
    let decide = |relaunched: bool| {
        let host = AccountHost::scripted(&[None], &[]);
        *host.started.lock().unwrap() = Some(ACCOUNT_NOW - 3600);
        let opts = account_opts(&host);
        let mut m = Mock::new(vec![rows.clone()]);
        let mut s = session(&mut m, Some("@s-1".to_string()));
        s.set_clock(ACCOUNT_NOW, PDT);
        s.set_zone_lookup(test_zones);
        let limited = Phase::Limited {
            message: String::new(),
            reset: None,
        };
        let point = account_point_of(rows.clone(), limited.clone());
        let first = s
            .turn_end_at_point(&point, Some(Duration::from_secs(180)), &opts)
            .expect("a decision");
        if relaunched {
            *host.started.lock().unwrap() = Some(ACCOUNT_NOW + 60);
        }
        let mut again = account_point_of(rows.clone(), limited);
        again.screen.seq += 1;
        let second = s
            .turn_end_at_point(&again, Some(Duration::from_secs(2)), &opts)
            .expect("a decision");
        (first, second)
    };
    let (first, second) = decide(true);
    assert!(
        matches!(&first, TurnEndAction::WaitUntil { why, .. } if why.contains("goes on by itself")),
        "{first:?}"
    );
    assert!(
        matches!(&second, TurnEndAction::WaitUntil { why, .. } if why == "usage-session resets"),
        "{second:?}"
    );
    let (_, second) = decide(false);
    assert!(
        matches!(&second, TurnEndAction::WaitUntil { why, .. } if why.contains("goes on by itself")),
        "{second:?}"
    );
}

#[test]
fn an_account_move_made_is_no_typed_act_of_the_budgets() {
    use crate::supervise::approvals::{
        ACCOUNT_MOVE_COMMAND, Outcome, Row, account_switch_reason, typed_at,
    };
    let (dir, ledger) = ledger_at("account-budget");
    let row = |rule: &str, command: &str, reason: &str, ts: i64| {
        Row {
            rule_id: rule,
            outcome: Outcome::Typed,
            command,
            reason,
            box_seq: 1,
        }
        .to_json(ts, Some("s-1"))
    };
    let rows = [
        row("continue@v1", "keep going", "the turn-end policy", 1_000),
        row(
            RULE_ACCOUNT,
            &format!("{ACCOUNT_MOVE_COMMAND}work-2"),
            &account_switch_reason("work-2", AccountAt::Wall),
            2_000,
        ),
        row(RULE_ACCOUNT, "my own words", "the turn-end policy", 3_000),
    ];
    std::fs::write(&ledger, rows.join("\n") + "\n").expect("ledger");
    assert_eq!(typed_at(&ledger, Some("@s-1")), [1_000, 3_000]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_usage_resume_typed_into_the_account_a_session_left_is_not_taken_up() {
    use crate::supervise::approvals::{
        ACCOUNT_MOVE_COMMAND, Outcome, Row, account_switch_reason, account_unmade_reason,
        act_reason, usage_attempt,
    };
    use crate::supervise::policy::turn_end::{LimitTry, RULE_ANSWER, RULE_LIMIT_RESUME};
    let (dir, ledger) = ledger_at("account-usage-seed");
    let in_place = LimitTry {
        bucket: false,
        in_place: true,
        attempts: 1,
        entitlement: false,
    };
    let row = |rule: &str, outcome: Outcome, command: &str, reason: &str, ts: i64| {
        Row {
            rule_id: rule,
            outcome,
            command,
            reason,
            box_seq: 1,
        }
        .to_json(ts, Some("s-1"))
    };
    let resume = |ts: i64| {
        row(
            RULE_LIMIT_RESUME,
            Outcome::Typed,
            "keep going",
            "the turn-end policy",
            ts,
        )
    };
    let to = format!("{ACCOUNT_MOVE_COMMAND}work-2");
    let moved = |ts: i64| {
        row(
            RULE_ACCOUNT,
            Outcome::Typed,
            &to,
            &account_switch_reason("work-2", AccountAt::Wall),
            ts,
        )
    };
    let unmade = row(
        RULE_ACCOUNT,
        Outcome::Skipped,
        &to,
        &account_unmade_reason("work-2", AccountAt::Wall, Some("refused:line")),
        2_000,
    );
    let write = |rows: &[String]| std::fs::write(&ledger, rows.join("\n") + "\n").expect("ledger");
    let read = || usage_attempt(&ledger, Some("@s-1")).map(|a| a.at_ms);
    write(&[resume(1_000), moved(2_000)]);
    assert_eq!(read(), None, "typed into the account it left");
    write(&[resume(1_000)]);
    assert_eq!(read(), Some(1_000));
    write(&[resume(1_000), moved(2_000), resume(3_000)]);
    assert_eq!(read(), Some(3_000), "the new account's own");
    write(&[resume(1_000), unmade]);
    assert_eq!(read(), Some(1_000), "no move was made");
    let worded = |wind: Option<&str>, reason: String| match wind {
        Some(w) => reason.replacen(
            "the turn-end policy",
            &format!("the turn-end policy {w}"),
            1,
        ),
        None => reason,
    };
    let placed = |rule: &str, wind: Option<&str>, ts: i64| {
        row(
            rule,
            Outcome::Typed,
            "…",
            &worded(wind, act_reason(Some(in_place))),
            ts,
        )
    };
    write(&[placed(RULE_DONE_CHECK, None, 1_000)]);
    assert_eq!(read(), Some(1_000));
    write(&[resume(1_000), placed(RULE_ANSWER, None, 3_000)]);
    assert_eq!(
        read(),
        Some(3_000),
        "the act in the limit's place, the latest"
    );
    write(&[placed(RULE_DONE_CHECK, None, 1_000), resume(3_000)]);
    assert_eq!(read(), Some(3_000), "the continuation, the latest");
    write(&[placed(RULE_DONE_CHECK, None, 1_000), moved(2_000)]);
    assert_eq!(read(), None, "typed into the account it left");
    write(&[moved(2_000), placed(RULE_DONE_CHECK, None, 3_000)]);
    assert_eq!(read(), Some(3_000), "the new account's own");
    let wind = "(model switch: kind=wind-down from=GPT-6-Astra effort=ultra to=gpt-6-luna \
                back_at=5000 marker=ATERM-SAVED-3f9a1c2e goal=paused phase=winding)";
    write(&[placed(RULE_DONE_CHECK, Some(wind), 1_000)]);
    assert_eq!(read(), Some(1_000), "under the switch's words");
    let window = LimitTry {
        bucket: false,
        in_place: false,
        attempts: 1,
        entitlement: false,
    };
    let at_bucket = LimitTry {
        bucket: true,
        ..window
    };
    for (rule, wind, limit) in [
        (RULE_DONE_CHECK, None, in_place),
        (
            RULE_ANSWER,
            Some(wind),
            LimitTry {
                bucket: true,
                ..in_place
            },
        ),
        (RULE_LIMIT_RESUME, None, at_bucket),
        (RULE_LIMIT_RESUME, Some(wind), window),
        (
            RULE_LIMIT_RESUME,
            None,
            LimitTry {
                attempts: 3,
                ..window
            },
        ),
        (
            RULE_DONE_CHECK,
            Some(wind),
            LimitTry {
                attempts: 2,
                ..in_place
            },
        ),
        (
            RULE_LIMIT_RESUME,
            None,
            LimitTry {
                attempts: 2,
                entitlement: true,
                ..at_bucket
            },
        ),
        (
            RULE_DONE_CHECK,
            Some(wind),
            LimitTry {
                attempts: 3,
                entitlement: true,
                ..in_place
            },
        ),
    ] {
        write(&[row(
            rule,
            Outcome::Typed,
            "…",
            &worded(wind, act_reason(Some(limit))),
            1_000,
        )]);
        assert_eq!(
            usage_attempt(&ledger, Some("@s-1")).map(|a| (a.at_ms, a.limit)),
            Some((1_000, limit)),
            "{rule} {wind:?}"
        );
    }
    write(&[resume(1_000)]);
    assert_eq!(
        usage_attempt(&ledger, Some("@s-1")).map(|a| (a.at_ms, a.limit)),
        Some((1_000, window))
    );
    for wind in [None, Some(wind)] {
        write(&[row(
            RULE_DONE_CHECK,
            Outcome::Typed,
            "…",
            &worded(wind, act_reason(None)),
            1_000,
        )]);
        assert_eq!(read(), None, "an ordinary done check");
    }
    let theirs = Row {
        rule_id: RULE_DONE_CHECK,
        outcome: Outcome::Typed,
        command: "…",
        reason: &act_reason(Some(in_place)),
        box_seq: 1,
    }
    .to_json(1_000, Some("s-9"));
    write(&[theirs]);
    assert_eq!(read(), None, "another session's");
    write(&[row(
        RULE_DONE_CHECK,
        Outcome::Skipped,
        "…",
        &act_reason(Some(in_place)),
        1_000,
    )]);
    assert_eq!(read(), None, "nothing typed");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_try_met_again_is_read_from_the_walls_row_after_it() {
    use crate::supervise::approvals::{
        LIMIT_ENDED, Outcome, RULE_WALL_WAIT, RULE_WALL_WAIT_END, Row, act_reason, usage_attempt,
        wall_wait_reason,
    };
    use crate::supervise::policy::turn_end::{LimitTry, RULE_LIMIT_RESUME};
    let (dir, ledger) = ledger_at("try-met-again");
    let window = LimitTry {
        bucket: false,
        in_place: false,
        attempts: 1,
        entitlement: false,
    };
    let at_bucket = LimitTry {
        bucket: true,
        ..window
    };
    let row = |rule: &str, outcome: Outcome, reason: &str, ts: i64| {
        Row {
            rule_id: rule,
            outcome,
            command: if outcome == Outcome::Typed {
                "keep going"
            } else {
                ""
            },
            reason,
            box_seq: 1,
        }
        .to_json(ts, Some("s-1"))
    };
    let try_at = |limit: LimitTry, ts: i64| {
        row(
            RULE_LIMIT_RESUME,
            Outcome::Typed,
            &act_reason(Some(limit)),
            ts,
        )
    };
    let wall = |message: &str, ts: i64| {
        row(
            RULE_WALL_WAIT,
            Outcome::Waiting,
            &format!("the turn-end policy (wall: reset_unix=-; reset=-; message={message})"),
            ts,
        )
    };
    let wall_named = |kind: aterm_phase::WallKind, message: &str, ts: i64| {
        row(
            RULE_WALL_WAIT,
            Outcome::Waiting,
            &wall_wait_reason(None, None, kind, message),
            ts,
        )
    };
    let generic = "You've hit your limit · resets 4pm (America/Los_Angeles)";
    let counting = format!(
        "{generic} \u{2014} seven_day_opus \u{2014} \u{26a0} Usage limit reached \u{b7} limit resets \
         4pm \u{2014} Continuing automatically at 4pm \u{b7} esc to cancel"
    );
    let bucket_kind = aterm_phase::WallKind::ModelBucket { consent: false };
    let spend = |ts: i64| wall("You've hit your monthly spend limit.", ts);
    let sonnet = |ts: i64| {
        wall(
            "You've hit your Sonnet limit · resets Oct 17 at 4:20pm (America/Los_Angeles)",
            ts,
        )
    };
    let named_words = |limit: &str| {
        crate::supervise::run::wall_confirm::limit_words(
            "You've hit your limit · resets 4pm (America/Los_Angeles)",
            Some(limit),
        )
    };
    let opus = |ts: i64| wall(&named_words("seven_day_opus"), ts);
    let window_named = |ts: i64| wall(&named_words("five_hour"), ts);
    let left = |ts: i64| {
        row(
            RULE_WALL_WAIT_END,
            Outcome::Waiting,
            "the wall left the screen or the worker worked",
            ts,
        )
    };
    let worked = |ts: i64| row(RULE_WALL_WAIT_END, Outcome::Waiting, LIMIT_ENDED, ts);
    let cases = [
        (
            "a spend wall read after a window's try",
            vec![try_at(window, 1_000), spend(2_000)],
            Some((1_000, true)),
        ),
        (
            "a bucket its rateLimitType named, read after a bucket's try",
            vec![try_at(at_bucket, 1_000), opus(2_000)],
            Some((1_000, true)),
        ),
        (
            "control: a bucket its rateLimitType named, a window's try",
            vec![try_at(window, 1_000), opus(2_000)],
            Some((1_000, false)),
        ),
        (
            "control: a window its rateLimitType named, a bucket's try",
            vec![try_at(at_bucket, 1_000), window_named(2_000)],
            Some((1_000, false)),
        ),
        (
            "a bucket the row names, its countdown's words after the type",
            vec![
                try_at(at_bucket, 1_000),
                wall_named(bucket_kind, &counting, 2_000),
            ],
            Some((1_000, true)),
        ),
        (
            "a bucket the row names, words that carry no type (seven_day_overage_included)",
            vec![
                try_at(at_bucket, 1_000),
                wall_named(bucket_kind, generic, 2_000),
            ],
            Some((1_000, true)),
        ),
        (
            "control: the same words, a row that names no kind",
            vec![try_at(at_bucket, 1_000), wall(&counting, 2_000)],
            Some((1_000, false)),
        ),
        (
            "control: a window the row names, a bucket's try",
            vec![
                try_at(at_bucket, 1_000),
                wall_named(aterm_phase::WallKind::UsageSession, generic, 2_000),
            ],
            Some((1_000, false)),
        ),
        (
            "control: a bucket the row names, a window's try",
            vec![
                try_at(window, 1_000),
                wall_named(bucket_kind, generic, 2_000),
            ],
            Some((1_000, false)),
        ),
        (
            "closed since by a live turn",
            vec![try_at(window, 1_000), spend(2_000), left(3_000)],
            Some((1_000, true)),
        ),
        (
            "a bucket read after a bucket's try",
            vec![try_at(at_bucket, 1_000), sonnet(2_000), left(3_000)],
            Some((1_000, true)),
        ),
        (
            "control: no wall after the try",
            vec![spend(500), try_at(window, 1_000)],
            Some((1_000, false)),
        ),
        (
            "control: the other limit's wall",
            vec![try_at(window, 1_000), sonnet(2_000)],
            Some((1_000, false)),
        ),
        (
            "control: the other limit's wall, a bucket's try",
            vec![try_at(at_bucket, 1_000), spend(2_000)],
            Some((1_000, false)),
        ),
        (
            "control: a newer try",
            vec![try_at(window, 1_000), spend(2_000), try_at(window, 3_000)],
            Some((3_000, false)),
        ),
        (
            "control: the worker's work ended it",
            vec![try_at(window, 1_000), spend(2_000), worked(3_000)],
            None,
        ),
        (
            "control: a try after the work",
            vec![try_at(window, 1_000), worked(2_000), try_at(window, 3_000)],
            Some((3_000, false)),
        ),
    ];
    let mut wrong = Vec::new();
    for (case, rows, want) in cases {
        std::fs::write(&ledger, rows.join("\n") + "\n").expect("ledger");
        let got = usage_attempt(&ledger, Some("@s-1")).map(|a| (a.at_ms, a.met_ms.is_some()));
        if got != want {
            wrong.push(format!("{case}: {got:?}, not {want:?}"));
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    assert!(wrong.is_empty(), "{wrong:#?}");
}

#[test]
fn a_try_met_again_is_seeded_from_the_wall_that_met_it() {
    use crate::supervise::approvals::{
        Outcome, RULE_WALL_WAIT, RULE_WALL_WAIT_END, Row, WALL_TAKEN, act_reason, usage_attempt,
        wall_wait_reason,
    };
    use crate::supervise::policy::turn_end::{LimitTry, RULE_LIMIT_RESUME};
    use aterm_phase::WallKind;
    let (dir, ledger) = ledger_at("try-met-by");
    let window = LimitTry {
        bucket: false,
        in_place: false,
        attempts: 1,
        entitlement: false,
    };
    let at_spend = LimitTry {
        entitlement: true,
        ..window
    };
    let at_bucket = LimitTry {
        bucket: true,
        ..window
    };
    let row = |rule: &str, outcome: Outcome, reason: &str, ts: i64| {
        Row {
            rule_id: rule,
            outcome,
            command: if outcome == Outcome::Typed {
                "keep going"
            } else {
                ""
            },
            reason,
            box_seq: 1,
        }
        .to_json(ts, Some("s-1"))
    };
    let try_at = |limit: LimitTry, ts: i64| {
        row(
            RULE_LIMIT_RESUME,
            Outcome::Typed,
            &act_reason(Some(limit)),
            ts,
        )
    };
    let wall = |kind: WallKind, placed: Option<i64>, message: &str, ts: i64| {
        row(
            RULE_WALL_WAIT,
            Outcome::Waiting,
            &wall_wait_reason(placed, None, kind, message),
            ts,
        )
    };
    let spend = |ts: i64| {
        wall(
            WallKind::Spend,
            None,
            "You've hit your monthly spend limit.",
            ts,
        )
    };
    let five = |ts: i64| {
        wall(
            WallKind::UsageSession,
            Some(1_792_279_200),
            "You've hit your session limit · resets 4pm (America/Los_Angeles)",
            ts,
        )
    };
    let opus = |ts: i64| {
        wall(
            WallKind::ModelBucket { consent: false },
            None,
            "You've hit your Opus limit",
            ts,
        )
    };
    let opus_counting = |ts: i64| {
        wall(
            WallKind::ModelBucket { consent: false },
            None,
            "You've hit your Opus limit \u{2014} \u{26a0} Usage limit reached \u{b7} Continuing \
             automatically at 4pm \u{b7} esc to cancel",
            ts,
        )
    };
    let end = |why: &str, ts: i64| row(RULE_WALL_WAIT_END, Outcome::Waiting, why, ts);
    let left = |ts: i64| end("the wall left the screen or the worker worked", ts);
    let cases = [
        (
            "the try's own turn met the limit: that row's time",
            vec![try_at(at_spend, 1_000), left(1_020), spend(1_260)],
            Some(1_260),
            true,
        ),
        (
            "the first row of the meeting, a later one moving nothing",
            vec![
                try_at(at_spend, 1_000),
                spend(1_030),
                left(2_000),
                spend(3_000),
            ],
            Some(1_030),
            true,
        ),
        (
            "after the take, the try's own turn's row",
            vec![
                try_at(at_spend, 1_000),
                spend(1_005),
                end(WALL_TAKEN, 1_020),
                spend(4_000),
            ],
            Some(4_000),
            true,
        ),
        (
            "a five-hour try met by the spend limit: the entitlement ladder",
            vec![try_at(window, 1_000), spend(1_003)],
            Some(1_003),
            true,
        ),
        (
            "a bucket's try met by a bucket no reset ends",
            vec![try_at(at_bucket, 1_000), opus(1_003)],
            Some(1_003),
            true,
        ),
        (
            "control: a spend try met by a window, its reset placed",
            vec![try_at(at_spend, 1_000), five(1_003)],
            Some(1_003),
            false,
        ),
        (
            "control: a bucket that goes on by itself",
            vec![try_at(at_bucket, 1_000), opus_counting(1_003)],
            Some(1_003),
            false,
        ),
        (
            "control: a five-hour try met by its window",
            vec![try_at(window, 1_000), five(1_003)],
            Some(1_003),
            false,
        ),
        (
            "control: not met, the mark's ladder",
            vec![try_at(at_spend, 1_000)],
            None,
            true,
        ),
        (
            "control: taken, not met since",
            vec![try_at(window, 1_000), spend(1_005), end(WALL_TAKEN, 1_020)],
            None,
            true,
        ),
    ];
    let mut wrong = Vec::new();
    for (case, rows, met, entitlement) in cases {
        std::fs::write(&ledger, rows.join("\n") + "\n").expect("ledger");
        let got = usage_attempt(&ledger, Some("@s-1")).map(|a| (a.met_ms, a.limit.entitlement));
        if got != Some((met, entitlement)) {
            wrong.push(format!("{case}: {got:?}, not {:?}", (met, entitlement)));
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    assert!(wrong.is_empty(), "{wrong:#?}");
}

#[test]
fn the_done_check_the_ledger_says_is_owed_its_answer() {
    use crate::supervise::approvals::{
        Carried, CheckOwed, LIMIT_ENDED, OFF_STOOD, Outcome, RULE_OFF_STOOD, RULE_POINT,
        RULE_TASK_DONE, RULE_WALL_WAIT, RULE_WALL_WAIT_END, Row, TASK_DONE, WALL_TAKEN, act_reason,
        done_check_owed, point_reason, wall_wait_reason,
    };
    use crate::supervise::policy::turn_end::{
        LimitTry, RULE_CONTINUE, RULE_DONE_CHECK, RULE_LIMIT_RESUME, RULE_OPEN_ITEMS,
        RULE_USAGE_PROBE,
    };
    let (dir, ledger) = ledger_at("check-owed");
    let row = |rule: &str, outcome: Outcome, reason: &str, ts: i64| {
        Row {
            rule_id: rule,
            outcome,
            command: "keep going",
            reason,
            box_seq: 1,
        }
        .to_json(ts, Some("s-1"))
    };
    let typed = |rule: &str, ts: i64| row(rule, Outcome::Typed, "the turn-end policy", ts);
    let in_place = |ts: i64| {
        row(
            RULE_DONE_CHECK,
            Outcome::Typed,
            &act_reason(Some(LimitTry {
                bucket: false,
                in_place: true,
                attempts: 1,
                entitlement: false,
            })),
            ts,
        )
    };
    let wall = |ts: i64| {
        row(
            RULE_WALL_WAIT,
            Outcome::Waiting,
            &wall_wait_reason(
                None,
                None,
                aterm_phase::WallKind::UsageWeekly,
                "You've hit your weekly limit · resets Sep 19 at 11am (America/Los_Angeles)",
            ),
            ts,
        )
    };
    let end = |why: &str, ts: i64| row(RULE_WALL_WAIT_END, Outcome::Waiting, why, ts);
    let point = |ts: i64| {
        row(
            RULE_POINT,
            Outcome::Waiting,
            &point_reason(Carried::streak(0), false),
            ts,
        )
    };
    let task_done = |ts: i64| row(RULE_TASK_DONE, Outcome::Waiting, TASK_DONE, ts);
    let off = |ts: i64| row(RULE_OFF_STOOD, Outcome::Waiting, OFF_STOOD, ts);
    let of = |rule: &'static str, cut: bool, check: Option<(i64, bool)>| {
        Some(CheckOwed {
            rule,
            cut,
            check,
            read_ms: None,
            unread: !cut && check.is_some_and(|(_, in_place)| in_place),
            answered: false,
            compact: false,
        })
    };
    let owed = |cut: bool, check: Option<(i64, bool)>| of(RULE_DONE_CHECK, cut, check);
    let challenge = |cut: bool, check: Option<(i64, bool)>| of(RULE_OPEN_ITEMS, cut, check);
    let read = |o: Option<CheckOwed>, ms: i64| {
        o.map(|o| CheckOwed {
            read_ms: Some(ms),
            unread: false,
            ..o
        })
    };
    let answered = |o: Option<CheckOwed>| {
        o.map(|o| CheckOwed {
            answered: true,
            ..o
        })
    };
    let cases = [
        (
            "the check",
            vec![typed(RULE_DONE_CHECK, 1_000)],
            owed(false, Some((1_000, false))),
        ),
        (
            "the challenge",
            vec![typed(RULE_OPEN_ITEMS, 1_000)],
            challenge(false, Some((1_000, false))),
        ),
        (
            "the challenge cut, its retry delivers it",
            vec![
                typed(RULE_OPEN_ITEMS, 1_000),
                wall(2_000),
                typed(RULE_LIMIT_RESUME, 3_000),
            ],
            challenge(false, None),
        ),
        (
            "the challenge cut: its first read is the wall's row",
            vec![typed(RULE_OPEN_ITEMS, 1_000), wall(2_000)],
            read(challenge(true, Some((1_000, false))), 2_000),
        ),
        (
            "the challenge cut, the wall's point judged: the retry's to deliver still",
            vec![typed(RULE_OPEN_ITEMS, 1_000), wall(2_000), point(2_001)],
            read(challenge(true, Some((1_000, false))), 2_000),
        ),
        (
            "the challenge, its answer judged no done report",
            vec![typed(RULE_OPEN_ITEMS, 1_000), point(2_000)],
            None,
        ),
        (
            "the challenge, its answer judged done",
            vec![
                typed(RULE_OPEN_ITEMS, 1_000),
                point(2_000),
                task_done(2_001),
            ],
            answered(challenge(false, None)),
        ),
        (
            "the challenge cut, Off stood",
            vec![typed(RULE_OPEN_ITEMS, 1_000), wall(2_000), off(3_000)],
            None,
        ),
        (
            "the challenge Off stood over, no wall: still owed",
            vec![typed(RULE_OPEN_ITEMS, 1_000), off(3_000)],
            challenge(false, Some((1_000, false))),
        ),
        (
            "the challenge cut, its retry's answer judged done",
            vec![
                typed(RULE_OPEN_ITEMS, 1_000),
                wall(2_000),
                typed(RULE_LIMIT_RESUME, 3_000),
                point(4_000),
                task_done(4_001),
            ],
            answered(challenge(false, None)),
        ),
        (
            "control: another act after the challenge",
            vec![typed(RULE_OPEN_ITEMS, 1_000), typed(RULE_CONTINUE, 2_000)],
            None,
        ),
        (
            "control: a second point judged after the challenge",
            vec![
                typed(RULE_OPEN_ITEMS, 1_000),
                point(2_000),
                point(3_000),
                task_done(3_001),
            ],
            None,
        ),
        (
            "the check in a limit's place",
            vec![in_place(1_000)],
            owed(false, Some((1_000, true))),
        ),
        (
            "the check in a limit's place, Off stood over it: no read",
            vec![in_place(1_000), off(2_000)],
            owed(false, Some((1_000, true))),
        ),
        (
            "the check in a limit's place, its turn read running: read",
            vec![in_place(1_000), end(LIMIT_ENDED, 2_000)],
            owed(false, Some((1_000, true))).map(|o| CheckOwed { unread: false, ..o }),
        ),
        (
            "the check in a limit's place, cut by a wall: read there",
            vec![in_place(1_000), wall(2_000)],
            read(owed(true, Some((1_000, true))), 2_000),
        ),
        (
            "the check, the probe's /usage after it",
            vec![
                typed(RULE_DONE_CHECK, 1_000),
                typed(RULE_USAGE_PROBE, 2_000),
            ],
            owed(false, Some((1_000, false))),
        ),
        (
            "cut by a wall",
            vec![typed(RULE_DONE_CHECK, 1_000), wall(2_000)],
            read(owed(true, Some((1_000, false))), 2_000),
        ),
        (
            "cut, its retry delivers it",
            vec![
                typed(RULE_DONE_CHECK, 1_000),
                wall(2_000),
                typed(RULE_LIMIT_RESUME, 3_000),
            ],
            owed(false, None),
        ),
        (
            "the retry cut again",
            vec![
                typed(RULE_DONE_CHECK, 1_000),
                wall(2_000),
                typed(RULE_LIMIT_RESUME, 3_000),
                wall(4_000),
            ],
            read(owed(true, None), 4_000),
        ),
        (
            "the wall the screen it was typed at: taken",
            vec![
                typed(RULE_DONE_CHECK, 1_000),
                wall(2_000),
                typed(RULE_LIMIT_RESUME, 3_000),
                wall(3_005),
                end(WALL_TAKEN, 3_020),
            ],
            owed(false, None),
        ),
        (
            "control: another act after the check",
            vec![typed(RULE_DONE_CHECK, 1_000), typed(RULE_CONTINUE, 2_000)],
            None,
        ),
        (
            "control: a wall's retry, no check cut",
            vec![
                typed(RULE_DONE_CHECK, 1_000),
                typed(RULE_LIMIT_RESUME, 2_000),
            ],
            None,
        ),
        (
            "control: the cut wall ended by work",
            vec![
                typed(RULE_DONE_CHECK, 1_000),
                wall(2_000),
                end(LIMIT_ENDED, 3_000),
            ],
            None,
        ),
        ("control: no check", vec![typed(RULE_CONTINUE, 1_000)], None),
    ];
    let mut wrong = Vec::new();
    for (case, rows, want) in cases {
        std::fs::write(&ledger, rows.join("\n") + "\n").expect("ledger");
        let got = done_check_owed(&ledger, Some("@s-1"));
        if got != want {
            wrong.push(format!("{case}: {got:?}, not {want:?}"));
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    assert!(wrong.is_empty(), "{wrong:#?}");
}

#[test]
fn a_bucket_its_rate_limit_type_named_is_that_bucket_in_the_ledgers_wall() {
    use crate::supervise::approvals::OpenWall;
    use crate::supervise::run::wall_confirm::{limit_named, limit_words, words_kind};
    use aterm_phase::WallKind;
    const RESET: i64 = 1_792_279_200;
    let bucket = WallKind::ModelBucket { consent: false };
    let text = "You've hit your limit · resets 4pm (America/Los_Angeles)";
    let opus = limit_words(text, Some("seven_day_opus"));
    assert_eq!(limit_named(text, Some("seven_day_opus")), Some(bucket));
    assert!(opus.ends_with(" \u{2014} seven_day_opus"), "{opus}");
    assert_eq!(
        limit_named(text, Some("seven_day")),
        Some(WallKind::UsageWeekly)
    );
    let sonnet = "You've hit your Sonnet limit · resets Oct 17 at 4:20pm (America/Los_Angeles)";
    let counting = format!(
        "{opus} \u{2014} \u{26a0} Usage limit reached \u{b7} limit resets 4pm \u{2014} \
         Continuing automatically at 4pm \u{b7} esc to cancel"
    );
    let mut wrong = Vec::new();
    for (case, words, named, want) in [
        (
            "a bucket its rateLimitType named",
            opus.clone(),
            None,
            Some(bucket),
        ),
        (
            "a bucket the row names, its countdown's words after the type",
            counting.clone(),
            Some(bucket),
            Some(bucket),
        ),
        (
            "a bucket the row names, words that carry no type (seven_day_overage_included)",
            text.to_string(),
            Some(bucket),
            Some(bucket),
        ),
        (
            "control: the countdown's words, a row that names no kind",
            counting.clone(),
            None,
            Some(WallKind::UsageSession),
        ),
        (
            "control: a window the row names",
            text.to_string(),
            Some(WallKind::UsageSession),
            Some(WallKind::UsageSession),
        ),
        (
            "control: a window its rateLimitType named",
            limit_words(text, Some("five_hour")),
            None,
            Some(WallKind::UsageSession),
        ),
        (
            "control: a bucket its words name",
            limit_words(sonnet, Some("seven_day_sonnet")),
            None,
            aterm_phase::classify_wall(sonnet),
        ),
        (
            "control: a dash and a type no bucket's",
            format!("{text} \u{2014} seven_day"),
            None,
            aterm_phase::classify_wall(&format!("{text} \u{2014} seven_day")),
        ),
    ] {
        let read = crate::supervise::approvals::wall_kind(named, &words);
        if named.is_none() {
            assert_eq!(
                read,
                words_kind(&words),
                "{case}: a row naming none is its words'"
            );
        }
        let mut m = Mock::new(vec![]);
        let mut s = session(&mut m, Some("@s-1".to_string()));
        s.set_clock(RESET - 3600, PDT);
        s.seed_wall_memory(Some(OpenWall {
            conversation: crate::supervise::policy::turn_end::Conversation::Hostless,
            program: None,
            at_ms: (RESET - 4000) * 1000,
            message: words.clone(),
            reset: Some("4pm (America/Los_Angeles)".to_string()),
            reset_unix: Some(RESET),
            tap_unix: None,
            kind: named,
            judged_ms: None,
            retry_ms: None,
        }));
        let tracked = [bucket, WallKind::UsageSession]
            .into_iter()
            .find(|&k| s.turn_end.wall_attempts(k).is_some());
        let want_track = want.map(|k| match k {
            WallKind::ModelBucket { .. } => bucket,
            _ => WallKind::UsageSession,
        });
        if read != want || tracked != want_track {
            wrong.push(format!(
                "{case} ({words:?}): read {read:?}, not {want:?}; tracked {tracked:?}"
            ));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

fn whole_secs(d: Duration) -> Duration {
    Duration::from_secs(u64::try_from((d.as_millis() + 500) / 1000).unwrap_or(u64::MAX))
}

fn past_take_window(
    s: &mut Session<'_, Mock>,
    first: TurnEndAction,
    screen: &[String],
    unix: i64,
    opts: &SuperviseOpts,
) -> (TurnEndAction, Duration) {
    match first {
        TurnEndAction::WaitUntil { until, .. } if s.turn_end.untaken_act().is_some() => {
            let gone = until.saturating_duration_since(Instant::now());
            s.turn_end.slept(gone);
            s.set_clock(
                unix + i64::try_from(gone.as_secs()).unwrap_or(i64::MAX),
                PDT,
            );
            let take = std::mem::replace(&mut s.turn_end.timing.take_within, Duration::ZERO);
            let point = point_at(s, screen.to_vec(), 102);
            let again = s
                .turn_end_at_point(&point, None, opts)
                .unwrap_or_else(|e| panic!("{e:?}"));
            s.turn_end.timing.take_within = take;
            (again, gone)
        }
        other => (other, Duration::ZERO),
    }
}

#[test]
fn an_act_in_a_limits_place_is_the_try_a_successor_backs_off_from() {
    use crate::supervise::approvals::{
        ACCOUNT_MOVE_COMMAND, Carried, Ledger, OpenWall, Outcome, Row, account_switch_reason,
        act_reason, act_reason_after, usage_attempt,
    };
    use crate::supervise::policy::turn_end::{
        LimitTry, RULE_ANSWER, RULE_LIMIT_RESUME, decide_turn_end,
    };
    use aterm_phase::prompt::fixtures as fx;
    const RESET: i64 = TEST_NOW + 6 * 3600 + 40 * 60;
    let now = RESET + 20 * 60;
    let done = "All done: the change is committed and pushed, and every suite passes.";
    let opts = SuperviseOpts {
        policy: owners_policy(),
        ..auto(30)
    };
    let replied = |reply: &str| -> Vec<String> {
        let mut out = Vec::new();
        for r in fx::screen(fx::WALL_SESSION_RESUMED_BY_ITSELF_MEASURED) {
            if [
                "carry on with the task",
                "You've hit your",
                "/upgrade to",
                "Usage limit reached",
            ]
            .iter()
            .any(|g| r.contains(g))
            {
                continue;
            }
            if r.contains("(reply 3)") {
                out.extend(reply.lines().enumerate().map(|(i, l)| {
                    if i == 0 {
                        format!("⏺ {l}")
                    } else {
                        format!("  {l}")
                    }
                }));
                continue;
            }
            out.push(r);
        }
        out
    };
    let wall = aterm_phase::read(Some("claude"), &auto_continue_wall_at("3:35pm"), Some(2))
        .wall
        .expect("the armed wall");
    let met_again = {
        let mut r = rows(&[
            &format!("⏺ {done}"),
            "",
            &format!("❯ {DONE_CHECK}"),
            "  ⎿  You've hit your session limit · resets 3:35pm (America/Los_Angeles)",
            "     /upgrade to increase your usage limit.",
            "",
        ]);
        r.extend(composer("  ⏵⏵ auto mode on (shift+tab to cycle)"));
        r
    };
    let typed_rows = |ledger: &Path| -> Vec<aterm_json::Value> {
        std::fs::read_to_string(ledger)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| aterm_json::from_str::<aterm_json::Value>(l).ok())
            .filter(|v| v.get("decision").and_then(aterm_json::Value::as_str) == Some("typed"))
            .collect()
    };
    let first_loop = |tag: &str, reply: &str, limit: bool| {
        let (dir, ledger) = ledger_at(tag);
        let screen = if limit { replied(reply) } else { ended(reply) };
        let mut m = Mock::new(vec![screen.clone()]);
        let mut s = session(&mut m, Some("@s-1".to_string()));
        s.set_clock(now, PDT);
        s.set_zone_lookup(test_zones);
        s.ledger = Ledger::open(Some(&ledger), Some("@s-1"), &mut Vec::new());
        if limit {
            s.seed_wall_memory(Some(OpenWall {
                conversation: crate::supervise::policy::turn_end::Conversation::Hostless,
                program: None,
                at_ms: (RESET - 4 * 60) * 1000,
                message: wall.message.clone(),
                reset: wall.reset.clone(),
                reset_unix: Some(RESET),
                tap_unix: None,
                kind: Some(wall.kind),
                judged_ms: None,
                retry_ms: None,
            }));
        }
        let point = point_at(&s, screen, 101);
        let worked = (!limit).then_some(Duration::from_secs(12 * 60));
        let action = s
            .turn_end_at_point(&point, worked, &opts)
            .unwrap_or_else(|e| panic!("{e:?}"));
        s.turn_end_execute(&point, action.clone(), &opts, &[], &mut Alone)
            .unwrap_or_else(|e| panic!("{e:?}"));
        let attempts = s
            .turn_end
            .wall_attempts(aterm_phase::WallKind::UsageSession);
        let place = s.turn_end.acted_in_limits_place();
        let reason = typed_rows(&ledger)
            .last()
            .and_then(|v| v.get("reason").and_then(aterm_json::Value::as_str))
            .map(str::to_string);
        assert_eq!(
            reason,
            Some(act_reason_after(
                Carried {
                    short: s.turn_end.short_streak(),
                    retry: None,
                },
                s.turn_end.limit_try()
            )),
            "{tag}"
        );
        if limit {
            let point = point_at(&s, met_again.clone(), 102);
            let read = s
                .turn_end_at_point(&point, None, &opts)
                .unwrap_or_else(|e| panic!("{e:?}"));
            assert!(
                matches!(read, TurnEndAction::WaitUntil { .. }),
                "{tag}: the window: {read:?}"
            );
        }
        (dir, ledger, (action.rule_id(), place), attempts)
    };
    let successor = |ledger: &Path, act_ms: i64| {
        let mut m = Mock::new(vec![met_again.clone()]);
        let mut s = session(&mut m, Some("@s-1".to_string()));
        s.set_clock(now + 60, PDT);
        s.set_zone_lookup(test_zones);
        let base = Instant::now();
        let open = s.seed_from_ledger(ledger, act_ms + 60_000, false);
        s.seed_wall_memory(open);
        let point = point_at(&s, met_again.clone(), 101);
        let first = s
            .turn_end_at_point(&point, None, &opts)
            .unwrap_or_else(|e| panic!("{e:?}"));
        let (first, slept) = past_take_window(&mut s, first, &met_again, now + 60, &opts);
        let (after, next) = match &first {
            TurnEndAction::WaitUntil { until, .. } => {
                let r = s.turn_end_reading(&point.screen, None, &opts);
                (
                    Some(whole_secs(until.saturating_duration_since(base) + slept)),
                    decide_turn_end(&s.turn_end, &r, &opts.policy, *until).rule_id(),
                )
            }
            other => (None, other.rule_id()),
        };
        let backoff = s.turn_end.timing.limit_backoff.first().copied();
        (
            after,
            next,
            s.turn_end
                .wall_attempts(aterm_phase::WallKind::UsageSession),
            backoff,
        )
    };
    let mut wrong = Vec::new();
    let (dir, ledger, rule, attempts) = first_loop(
        "place-seed-resume",
        "Stage 2 of 5 is in; stage 3 is next.",
        true,
    );
    assert_eq!(
        (rule, attempts),
        ((Some(RULE_LIMIT_RESUME), false), Some(1))
    );
    let act_ms = typed_rows(&ledger)
        .last()
        .and_then(|v| v.get("ts").and_then(aterm_json::Value::as_i64))
        .expect("the act's row");
    let control = successor(&ledger, act_ms);
    let _ = std::fs::remove_dir_all(&dir);
    let (after, next, attempts, backoff) = control;
    let take = TurnEndTiming::default().take_within;
    let rung = backoff.expect("a ladder") + take - Duration::from_secs(60);
    assert_eq!(
        (after, next, attempts),
        (Some(rung), Some(RULE_LIMIT_RESUME), Some(1)),
        "the usage-resume row: the first rung from where the try was judged met, then the \
         continuation"
    );
    for (tag, reply, want) in [
        ("place-seed-check", done, RULE_DONE_CHECK),
        (
            "place-seed-answer",
            "Shall I force-push the rewritten history over main?",
            RULE_ANSWER,
        ),
    ] {
        let (dir, ledger, rule, attempts) = first_loop(tag, reply, true);
        assert_eq!(
            (rule, attempts),
            ((Some(want), true), Some(1)),
            "{tag}: the act in the limit's place, the wall's attempt"
        );
        let act_ms = typed_rows(&ledger)
            .last()
            .and_then(|v| v.get("ts").and_then(aterm_json::Value::as_i64))
            .expect("the act's row");
        let seeded = successor(&ledger, act_ms);
        if seeded != control {
            wrong.push(format!("{tag}: {seeded:?}\nthe control's: {control:?}"));
        }
        let moved = Row {
            rule_id: RULE_ACCOUNT,
            outcome: Outcome::Typed,
            command: &format!("{ACCOUNT_MOVE_COMMAND}work-2"),
            reason: &account_switch_reason("work-2", AccountAt::Wall),
            box_seq: 102,
        }
        .to_json(act_ms + 1_000, Some("s-1"));
        let mut body = std::fs::read_to_string(&ledger).expect("the ledger");
        body.push_str(&moved);
        body.push('\n');
        std::fs::write(&ledger, body).expect("the ledger");
        let (after, next, attempts, _) = successor(&ledger, act_ms);
        if (after, next, attempts) != (None, Some(RULE_LIMIT_RESUME), Some(0)) {
            wrong.push(format!(
                "{tag}, moved since: {after:?} {next:?} {attempts:?}"
            ));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
    let (dir, ledger, rule, _) = first_loop("place-seed-rung", done, true);
    assert_eq!(rule, (Some(RULE_DONE_CHECK), true));
    let check_ms = typed_rows(&ledger)
        .last()
        .and_then(|v| v.get("ts").and_then(aterm_json::Value::as_i64))
        .expect("the check's row");
    let resume_ms = check_ms + 11 * 60_000;
    let second = Row {
        rule_id: RULE_LIMIT_RESUME,
        outcome: Outcome::Typed,
        command: "keep going",
        reason: &act_reason(Some(LimitTry {
            bucket: false,
            in_place: false,
            attempts: 2,
            entitlement: false,
        })),
        box_seq: 102,
    }
    .to_json(resume_ms, Some("s-1"));
    let mut body = std::fs::read_to_string(&ledger).expect("the ledger");
    body.push_str(&second);
    body.push('\n');
    std::fs::write(&ledger, body).expect("the ledger");
    let (after, next, attempts, _) = successor(&ledger, resume_ms);
    let rung = TurnEndTiming::default().limit_backoff[1] + take;
    assert_eq!(
        (after, next, attempts),
        (Some(rung), Some(RULE_LIMIT_RESUME), Some(2)),
        "the second rung from the continuation"
    );
    let _ = std::fs::remove_dir_all(&dir);
    let (dir, ledger, rule, attempts) = first_loop("place-seed-plain", done, false);
    assert_eq!((rule, attempts), ((Some(RULE_DONE_CHECK), false), None));
    assert_eq!(usage_attempt(&ledger, Some("@s-1")), None);
    let (after, next, attempts, _) = successor(&ledger, 0);
    assert_eq!(
        (after, next, attempts),
        (None, Some(RULE_LIMIT_RESUME), Some(0))
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_done_check_at_an_ordinary_turn_end_is_no_limits_try_whatever_was_seeded() {
    use crate::supervise::approvals::{
        Carried, Ledger, Outcome, Row, act_reason, act_reason_after, usage_attempt,
    };
    use crate::supervise::policy::turn_end::{LimitTry, decide_turn_end};
    let now = TEST_NOW + 6 * 3600;
    let done = "All done: the change is committed and pushed, and every suite passes.";
    let opts = SuperviseOpts {
        policy: owners_policy(),
        ..auto(30)
    };
    let seeded_ms = (now - 5 * 60) * 1000;
    let mut got = Vec::new();
    for (tag, seed) in [
        ("ordinary-check-seeded", true),
        ("ordinary-check-control", false),
    ] {
        let (dir, ledger) = ledger_at(tag);
        if seed {
            let row = Row {
                rule_id: RULE_DONE_CHECK,
                outcome: Outcome::Typed,
                command: DONE_CHECK,
                reason: &act_reason(Some(LimitTry {
                    bucket: false,
                    in_place: true,
                    attempts: 1,
                    entitlement: false,
                })),
                box_seq: 90,
            }
            .to_json(seeded_ms, Some("s-1"));
            std::fs::write(&ledger, format!("{row}\n")).expect("the ledger");
        }
        let screen = ended(done);
        let mut m = Mock::new(vec![screen.clone()]);
        let mut s = session(&mut m, Some("@s-1".to_string()));
        s.set_clock(now, PDT);
        s.set_zone_lookup(test_zones);
        s.ledger = Ledger::open(Some(&ledger), Some("@s-1"), &mut Vec::new());
        seed_usage_act(
            &mut s.turn_end,
            &ledger,
            s.setup.sid.as_deref(),
            now * 1000,
            Instant::now(),
        );
        let attempts = s
            .turn_end
            .wall_attempts(aterm_phase::WallKind::UsageSession);
        let point = point_at(&s, screen, 101);
        let first = s
            .turn_end_at_point(&point, None, &opts)
            .unwrap_or_else(|e| panic!("{e:?}"));
        let act = match &first {
            TurnEndAction::WaitUntil { until, .. } => {
                let r = s.turn_end_reading(&point.screen, None, &opts);
                decide_turn_end(&s.turn_end, &r, &opts.policy, *until)
            }
            other => other.clone(),
        };
        s.last = Some(point.screen.clone());
        s.turn_end_execute(&point, act.clone(), &opts, &[], &mut Alone)
            .unwrap_or_else(|e| panic!("{e:?}"));
        let row = std::fs::read_to_string(&ledger)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| aterm_json::from_str::<aterm_json::Value>(l).ok())
            .rfind(|v| v.get("decision").and_then(aterm_json::Value::as_str) == Some("typed"))
            .map(|v| {
                let text = |k: &str| {
                    v.get(k)
                        .and_then(aterm_json::Value::as_str)
                        .map(str::to_string)
                };
                (text("rule_id"), text("reason"))
            });
        got.push((
            tag,
            act.rule_id(),
            row,
            (
                attempts,
                s.turn_end
                    .wall_attempts(aterm_phase::WallKind::UsageSession),
            ),
            usage_attempt(&ledger, Some("@s-1")).map(|a| a.at_ms),
        ));
        drop(s);
        let _ = std::fs::remove_dir_all(&dir);
    }
    let check = Some((
        Some(RULE_DONE_CHECK.to_string()),
        Some(act_reason_after(Carried::streak(1), None)),
    ));
    assert_eq!(
        got,
        [
            (
                "ordinary-check-seeded",
                Some(RULE_DONE_CHECK),
                check.clone(),
                (Some(1), Some(1)),
                Some(seeded_ms),
            ),
            (
                "ordinary-check-control",
                Some(RULE_DONE_CHECK),
                check,
                (None, None),
                None,
            ),
        ]
    );
}

#[test]
fn a_land_act_in_a_limits_place_is_the_try_a_successor_backs_off_from() {
    use crate::supervise::approvals::{
        Carried, Ledger, Outcome, RULE_WALL_WAIT, Row, act_reason, act_reason_after, open_wall,
        typed_texts, usage_attempt, wall_wait_reason,
    };
    use crate::supervise::config::Autopilot;
    use crate::supervise::policy::turn_end::{
        LimitTry, RULE_LAND, RULE_LAND_ANSWER, RULE_LIMIT_RESUME, decide_turn_end,
    };
    use aterm_phase::prompt::fixtures as fx;
    const RESET: i64 = TEST_NOW + 6 * 3600 + 40 * 60;
    let now = RESET + 20 * 60;
    let done = "All done: the change is committed and pushed, and every suite passes.";
    let force = "Shall I force-push the rewritten history over main?";
    let opts = |mode: Autopilot| SuperviseOpts {
        policy: SupervisorConfig {
            autopilot: mode,
            ..owners_policy()
        },
        ..auto(30)
    };
    let replied = |reply: &str| -> Vec<String> {
        let mut out = Vec::new();
        for r in fx::screen(fx::WALL_SESSION_RESUMED_BY_ITSELF_MEASURED) {
            if [
                "carry on with the task",
                "You've hit your",
                "/upgrade to",
                "Usage limit reached",
            ]
            .iter()
            .any(|g| r.contains(g))
            {
                continue;
            }
            if r.contains("(reply 3)") {
                out.push(format!("⏺ {reply}"));
                continue;
            }
            out.push(r);
        }
        out
    };
    let wall = aterm_phase::read(Some("claude"), &auto_continue_wall_at("3:35pm"), Some(2))
        .wall
        .expect("the armed wall");
    let opened = Row {
        rule_id: RULE_WALL_WAIT,
        outcome: Outcome::Waiting,
        command: "",
        reason: &wall_wait_reason(Some(RESET), wall.reset.as_deref(), wall.kind, &wall.message),
        box_seq: 100,
    }
    .to_json((RESET - 4 * 60) * 1000, Some("s-1"));
    let met_again = {
        let mut r = rows(&[
            &format!("⏺ {done}"),
            "",
            &format!("❯ {DONE_CHECK}"),
            "  ⎿  You've hit your session limit · resets 3:35pm (America/Los_Angeles)",
            "     /upgrade to increase your usage limit.",
            "",
        ]);
        r.extend(composer("  ⏵⏵ auto mode on (shift+tab to cycle)"));
        r
    };
    type Last = Option<(String, String, String, i64)>;
    let last_typed = |ledger: &Path| -> Last {
        std::fs::read_to_string(ledger)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| aterm_json::from_str::<aterm_json::Value>(l).ok())
            .rfind(|v| v.get("decision").and_then(aterm_json::Value::as_str) == Some("typed"))
            .map(|v| {
                let text = |k: &str| {
                    v.get(k)
                        .and_then(aterm_json::Value::as_str)
                        .unwrap_or_default()
                        .to_string()
                };
                (
                    text("rule_id"),
                    text("command"),
                    text("reason"),
                    v.get("ts").and_then(aterm_json::Value::as_i64).unwrap_or(0),
                )
            })
    };
    let first_loop = |tag: &str, mode: Autopilot, reply: &str, limit: bool, held: &[&str]| {
        let (dir, ledger) = ledger_at(tag);
        let body: String = held.iter().map(|r| format!("{r}\n")).collect();
        std::fs::write(&ledger, body).expect("the ledger");
        let screen = if limit { replied(reply) } else { ended(reply) };
        let opts = opts(mode);
        let mut m = Mock::new(vec![screen.clone()]);
        let mut s = session(&mut m, Some("@s-1".to_string()));
        s.set_clock(now, PDT);
        s.set_zone_lookup(test_zones);
        s.ledger = Ledger::open(Some(&ledger), Some("@s-1"), &mut Vec::new());
        seed_usage_act(
            &mut s.turn_end,
            &ledger,
            s.setup.sid.as_deref(),
            now * 1000,
            Instant::now(),
        );
        let before = s
            .turn_end
            .wall_attempts(aterm_phase::WallKind::UsageSession);
        s.seed_wall_memory(open_wall(&ledger, s.setup.sid.as_deref()));
        let point = point_at(&s, screen, 101);
        let first = s
            .turn_end_at_point(&point, None, &opts)
            .unwrap_or_else(|e| panic!("{e:?}"));
        let action = match &first {
            TurnEndAction::WaitUntil { until, .. } => {
                let r = s.turn_end_reading(&point.screen, None, &opts);
                decide_turn_end(&s.turn_end, &r, &opts.policy, *until)
            }
            other => other.clone(),
        };
        s.last = Some(point.screen.clone());
        s.turn_end_execute(&point, action.clone(), &opts, &[], &mut Alone)
            .unwrap_or_else(|e| panic!("{e:?}"));
        let after = s
            .turn_end
            .wall_attempts(aterm_phase::WallKind::UsageSession);
        let place = s.turn_end.acted_in_limits_place();
        drop(s);
        (dir, ledger, (action.rule_id(), place, (before, after)))
    };
    let successor = |ledger: &Path, mode: Autopilot, act_ms: i64| {
        let opts = opts(mode);
        let mut m = Mock::new(vec![met_again.clone()]);
        let mut s = session(&mut m, Some("@s-1".to_string()));
        s.set_clock(now + 60, PDT);
        s.set_zone_lookup(test_zones);
        let base = Instant::now();
        seed_usage_act(
            &mut s.turn_end,
            ledger,
            s.setup.sid.as_deref(),
            act_ms + 60_000,
            base,
        );
        let point = point_at(&s, met_again.clone(), 101);
        let first = s
            .turn_end_at_point(&point, None, &opts)
            .unwrap_or_else(|e| panic!("{e:?}"));
        let (after, next) = match &first {
            TurnEndAction::WaitUntil { until, .. } => {
                let r = s.turn_end_reading(&point.screen, None, &opts);
                (
                    Some(until.saturating_duration_since(base)),
                    decide_turn_end(&s.turn_end, &r, &opts.policy, *until).rule_id(),
                )
            }
            other => (None, other.rule_id()),
        };
        (
            after,
            next,
            s.turn_end
                .wall_attempts(aterm_phase::WallKind::UsageSession),
        )
    };
    let in_place = LimitTry {
        bucket: false,
        in_place: true,
        attempts: 1,
        entitlement: false,
    };
    let rung = TurnEndTiming::default().limit_backoff[0] - Duration::from_secs(60);
    let mut wrong = Vec::new();
    let (dir, ledger, act) = first_loop(
        "land-place-check",
        Autopilot::Cruise,
        done,
        true,
        &[&opened],
    );
    assert_eq!(act, (Some(RULE_DONE_CHECK), true, (None, Some(1))));
    let (_, _, reason, act_ms) = last_typed(&ledger).expect("the check's row");
    assert_eq!(reason, act_reason_after(Carried::streak(1), Some(in_place)));
    let control = successor(&ledger, Autopilot::Cruise, act_ms);
    assert_eq!(
        control,
        (Some(rung), Some(RULE_LIMIT_RESUME), Some(1)),
        "the check's row: the first rung from the act, then the continuation"
    );
    let _ = std::fs::remove_dir_all(&dir);
    for (tag, reply, want) in [
        ("land-place-instruction", done, RULE_LAND),
        ("land-place-answer", force, RULE_LAND_ANSWER),
    ] {
        let (dir, ledger, act) = first_loop(tag, Autopilot::Land, reply, true, &[&opened]);
        if act != (Some(want), true, (None, Some(1))) {
            wrong.push(format!("{tag}: the act {act:?}"));
        }
        let Some((rule, command, reason, act_ms)) = last_typed(&ledger) else {
            wrong.push(format!("{tag}: no row"));
            continue;
        };
        if (rule.as_str(), reason.as_str())
            != (want, &*act_reason_after(Carried::streak(1), Some(in_place)))
        {
            wrong.push(format!("{tag}: the row {rule} {reason:?}"));
        }
        if usage_attempt(&ledger, Some("@s-1")).map(|a| (a.at_ms, a.limit))
            != Some((act_ms, in_place))
        {
            wrong.push(format!(
                "{tag}: read back {:?}",
                usage_attempt(&ledger, Some("@s-1")).map(|a| (a.at_ms, a.limit))
            ));
        }
        let theirs = typed_texts(&ledger, Some("@s-1"));
        if open_wall(&ledger, Some("@s-1")).is_some()
            || !theirs
                .iter()
                .any(|t| t.at_ms == act_ms && t.text == command)
        {
            wrong.push(format!("{tag}: the readers {theirs:?}"));
        }
        for mode in [Autopilot::Land, Autopilot::Cruise] {
            let seeded = successor(&ledger, mode, act_ms);
            if seeded != control {
                wrong.push(format!(
                    "{tag}, the successor under {mode:?}: {seeded:?}, the check's {control:?}"
                ));
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
    let (dir, ledger, act) = first_loop("land-place-plain", Autopilot::Land, done, false, &[]);
    assert_eq!(act, (Some(RULE_LAND), false, (None, None)));
    let (_, _, reason, _) = last_typed(&ledger).expect("the instruction's row");
    assert_eq!(reason, act_reason_after(Carried::streak(1), None));
    assert_eq!(usage_attempt(&ledger, Some("@s-1")), None);
    assert_eq!(
        successor(&ledger, Autopilot::Land, 0),
        (None, Some(RULE_LIMIT_RESUME), Some(0))
    );
    let _ = std::fs::remove_dir_all(&dir);
    let seeded_ms = (now - 5 * 60) * 1000;
    let check = Row {
        rule_id: RULE_DONE_CHECK,
        outcome: Outcome::Typed,
        command: DONE_CHECK,
        reason: &act_reason(Some(in_place)),
        box_seq: 90,
    }
    .to_json(seeded_ms, Some("s-1"));
    let (dir, ledger, act) =
        first_loop("land-place-seeded", Autopilot::Land, done, false, &[&check]);
    assert_eq!(act, (Some(RULE_LAND), false, (Some(1), Some(1))));
    let (rule, _, reason, _) = last_typed(&ledger).expect("the instruction's row");
    assert_eq!(
        (rule.as_str(), reason),
        (RULE_LAND, act_reason_after(Carried::streak(1), None))
    );
    assert_eq!(
        usage_attempt(&ledger, Some("@s-1")).map(|a| (a.at_ms, a.limit)),
        Some((seeded_ms, in_place))
    );
    let _ = std::fs::remove_dir_all(&dir);
    let (dir, ledger, act) = first_loop("land-place-off", Autopilot::Off, done, true, &[&opened]);
    assert_eq!(act, (None, false, (None, Some(0))));
    assert_eq!(last_typed(&ledger), None);
    assert_eq!(usage_attempt(&ledger, Some("@s-1")), None);
    assert!(open_wall(&ledger, Some("@s-1")).is_some());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_loop_at_a_buckets_open_wall_waits_on_its_reset_beside_the_act_it_takes_up() {
    use crate::supervise::approvals::{
        Ledger, Outcome, RULE_WALL_WAIT, Row, act_reason, open_wall, wall_wait_reason,
    };
    use crate::supervise::policy::turn_end::{LimitTry, RULE_LIMIT_RESUME, decide_turn_end};
    use aterm_phase::prompt::fixtures as fx;
    const RESET: i64 = 1_792_279_200;
    let now = RESET - 2 * 86_400;
    let opts = SuperviseOpts {
        policy: SupervisorConfig {
            model_fallback: None,
            ..owners_policy()
        },
        ..auto(30)
    };
    let armed = fx::screen(fx::WALL_SONNET_BUCKET_AUTO_CONTINUE_ARMED_2_1_300);
    let wall = aterm_phase::read(Some("claude"), &armed, Some(2))
        .wall
        .expect("the bucket");
    let cancelled = {
        let mut rows: Vec<String> = armed
            .iter()
            .filter(|r| {
                !r.contains("Usage limit reached") && !r.contains("Continuing automatically")
            })
            .cloned()
            .collect();
        let top = rows
            .iter()
            .position(|r| r.starts_with('─'))
            .expect("the composer's frame");
        rows.splice(
            top..top,
            [
                "⏺ Automatic continue cancelled · /rate-limit-options to re-arm".to_string(),
                String::new(),
            ],
        );
        rows
    };
    let resumed = fx::screen(fx::RESUMED_NO_LIMIT_AT_ARMED_LIMIT_2_1_300);
    let act_ms = (now - 60) * 1000;
    let act = |rule: &str, command: &str, reason: &str| {
        Row {
            rule_id: rule,
            outcome: Outcome::Typed,
            command,
            reason,
            box_seq: 90,
        }
        .to_json(act_ms, Some("s-1"))
    };
    let opened = Row {
        rule_id: RULE_WALL_WAIT,
        outcome: Outcome::Waiting,
        command: "",
        reason: &wall_wait_reason(Some(RESET), wall.reset.as_deref(), wall.kind, &wall.message),
        box_seq: 91,
    }
    .to_json(act_ms + 5_000, Some("s-1"));
    let mut wrong = Vec::new();
    let mut n = 0;
    for (screen_name, screen) in [("Esc", &cancelled), ("/resume", &resumed)] {
        for (row_name, row) in [
            (
                "the check in the continuation's place",
                act(
                    RULE_DONE_CHECK,
                    DONE_CHECK,
                    &act_reason(Some(LimitTry {
                        bucket: true,
                        in_place: true,
                        attempts: 1,
                        entitlement: false,
                    })),
                ),
            ),
            (
                "the continuation, as 0.104.0 wrote it",
                act(RULE_LIMIT_RESUME, "keep going", "the turn-end policy"),
            ),
            (
                "control: an ordinary done check",
                act(RULE_DONE_CHECK, DONE_CHECK, &act_reason(None)),
            ),
        ] {
            n += 1;
            let (dir, ledger) = ledger_at(&format!("bucket-open-wall-{n}"));
            std::fs::write(&ledger, format!("{row}\n{opened}\n")).expect("the ledger");
            let mut m = Mock::new(vec![screen.clone()]);
            let mut s = session(&mut m, Some("@s-1".to_string()));
            s.set_clock(now, PDT);
            s.set_zone_lookup(test_zones);
            s.ledger = Ledger::open(Some(&ledger), Some("@s-1"), &mut Vec::new());
            seed_usage_act(
                &mut s.turn_end,
                &ledger,
                s.setup.sid.as_deref(),
                now * 1000,
                Instant::now(),
            );
            let open = open_wall(&ledger, s.setup.sid.as_deref());
            let had_open = open.is_some();
            s.seed_wall_memory(open);
            let point = point_at(&s, screen.clone(), 101);
            let first = s
                .turn_end_at_point(&point, None, &opts)
                .unwrap_or_else(|e| panic!("{e:?}"));
            let got = match &first {
                TurnEndAction::WaitUntil { until, .. } => {
                    let r = s.turn_end_reading(&point.screen, None, &opts);
                    (
                        *until >= s.instant_of(RESET),
                        decide_turn_end(&s.turn_end, &r, &opts.policy, *until).rule_id(),
                    )
                }
                other => (false, other.rule_id()),
            };
            let waits = s.turn_end.waits_on_named_reset();
            drop(s);
            let _ = std::fs::remove_dir_all(&dir);
            if (had_open, waits, got) != (true, true, (true, Some(RULE_LIMIT_RESUME))) {
                wrong.push(format!(
                    "{screen_name}, {row_name}: open wall {had_open}, waits on the reset \
                     {waits}, {first:?} then {:?}",
                    got.1
                ));
            }
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

#[test]
fn a_limit_its_own_try_met_again_stays_open_for_a_loop_that_follows() {
    use crate::supervise::approvals::{Ledger, Outcome, act_reason, open_wall, usage_attempt};
    use crate::supervise::policy::turn_end::{LimitTry, RULE_LIMIT_RESUME, decide_turn_end};
    use aterm_phase::prompt::fixtures as fx;
    const WINDOW_RESET: i64 = TEST_NOW + 6 * 3600 + 40 * 60;
    const BUCKET_RESET: i64 = 1_792_279_200;
    let opts = SuperviseOpts {
        policy: SupervisorConfig {
            model_fallback: None,
            ..owners_policy()
        },
        ..auto(30)
    };
    let resumed = fx::screen(fx::RESUMED_NO_LIMIT_AT_ARMED_LIMIT_2_1_300);
    let bucket = fx::screen(fx::WALL_SONNET_BUCKET_AUTO_CONTINUE_ARMED_2_1_300);
    let bucket_cancelled = {
        let mut rows: Vec<String> = bucket
            .iter()
            .filter(|r| {
                !r.contains("Usage limit reached") && !r.contains("Continuing automatically")
            })
            .cloned()
            .collect();
        let top = rows
            .iter()
            .position(|r| r.starts_with('─'))
            .expect("the composer's frame");
        rows.splice(
            top..top,
            [
                "⏺ Automatic continue cancelled · /rate-limit-options to re-arm".to_string(),
                String::new(),
            ],
        );
        rows
    };
    type Decided = Option<(i64, Option<&'static str>)>;
    let decided = |s: &mut Session<'_, Mock>, rows: &[String], seq: u64| -> Decided {
        let point = point_at(s, rows.to_vec(), seq);
        match s
            .turn_end_at_point(&point, None, &opts)
            .expect("a decision")
        {
            TurnEndAction::WaitUntil { until, .. } => {
                let r = s.turn_end_reading(&point.screen, None, &opts);
                Some((
                    s.unix_of(until),
                    decide_turn_end(&s.turn_end, &r, &opts.policy, until).rule_id(),
                ))
            }
            other => Some((s.now_unix(), other.rule_id())),
        }
    };
    let mut wrong = Vec::new();
    let mut n = 0;
    for (limit, row) in [
        ("the window", None),
        ("the bucket", Some((RULE_LIMIT_RESUME, "keep going", false))),
        ("the bucket", Some((RULE_DONE_CHECK, DONE_CHECK, true))),
    ] {
        let bucketed = row.is_some();
        let (armed, met_again, cancelled, reset, now) = if bucketed {
            (
                bucket.clone(),
                bucket.clone(),
                bucket_cancelled.clone(),
                BUCKET_RESET,
                BUCKET_RESET - 2 * 86_400,
            )
        } else {
            (
                auto_continue_wall_at("3:35pm"),
                own_act_met_a_limit("3:35pm"),
                own_act_limit_cancelled("3:35pm"),
                WINDOW_RESET,
                WINDOW_RESET + 20 * 60,
            )
        };
        for (screen_name, screen) in [("Esc", &cancelled), ("/resume", &resumed)] {
            for worked in [false, true] {
                n += 1;
                let case = format!(
                    "{limit} ({:?}), {screen_name}, {}",
                    row.map(|(rule, ..)| rule),
                    if worked {
                        "control: work answered the act"
                    } else {
                        "the same notice answered the act"
                    }
                );
                let (dir, ledger) = ledger_at(&format!("own-try-met-again-{n}"));
                let mut m = Mock::new(vec![armed.clone()]);
                let mut s = session(&mut m, Some("@s-1".to_string()));
                s.set_zone_lookup(test_zones);
                s.ledger = Ledger::open(Some(&ledger), Some("@s-1"), &mut Vec::new());
                s.set_turn_end_timing(TurnEndTiming {
                    take_within: Duration::ZERO,
                    ..TurnEndTiming::default()
                });
                s.set_clock(now.min(reset) - 4 * 60, PDT);
                let _ = decided(&mut s, &armed, 100);
                s.set_clock(now, PDT);
                if let Some((rule, command, in_place)) = row {
                    s.ledger_row(
                        rule,
                        Outcome::Typed,
                        command,
                        &act_reason(Some(LimitTry {
                            bucket: true,
                            in_place,
                            attempts: 1,
                            entitlement: false,
                        })),
                        101,
                    );
                } else {
                    let p = point_at(&s, armed.clone(), 101);
                    let a = s.turn_end_at_point(&p, None, &opts).expect("the try");
                    assert_eq!(a.rule_id(), Some(RULE_LIMIT_RESUME), "{case}");
                    s.pass_wake_settle();
                    s.turn_end_execute(&p, a, &opts, &[], &mut Alone)
                        .expect("typed");
                }
                let act_ms = usage_attempt(&ledger, Some("@s-1"))
                    .expect("the try's row")
                    .at_ms;
                let closed = open_wall(&ledger, Some("@s-1")).is_none();
                s.set_clock(now + 30, PDT);
                if worked {
                    let p = point_at(&s, ended("Fixed the parser; the suite is green."), 102);
                    let _ = s.turn_end_at_point(&p, Some(Duration::from_secs(12 * 60)), &opts);
                } else {
                    let _ = decided(&mut s, &met_again, 102);
                }
                let open = open_wall(&ledger, Some("@s-1"));
                s.set_clock(now + 120, PDT);
                let first = (!worked).then(|| decided(&mut s, screen, 103)).flatten();
                drop(s);
                let mut m2 = Mock::new(vec![screen.clone()]);
                let mut s2 = session(&mut m2, Some("@s-1".to_string()));
                s2.set_clock(now + 120, PDT);
                s2.set_zone_lookup(test_zones);
                s2.ledger = Ledger::open(Some(&ledger), Some("@s-1"), &mut Vec::new());
                seed_usage_act(
                    &mut s2.turn_end,
                    &ledger,
                    s2.setup.sid.as_deref(),
                    act_ms + 120_000,
                    Instant::now(),
                );
                s2.seed_wall_memory(open.clone());
                let waits = s2.turn_end.waits_on_named_reset();
                let next = (!worked).then(|| decided(&mut s2, screen, 101)).flatten();
                drop(s2);
                let _ = std::fs::remove_dir_all(&dir);
                if worked {
                    if !closed || open.is_some() || waits {
                        wrong.push(format!(
                            "{case}: closed by the act {closed}, open wall {open:?}, waits {waits}"
                        ));
                    }
                    continue;
                }
                let named = open.as_ref().is_some_and(|o| o.reset_unix.is_some());
                let works = |d: Decided| {
                    d.is_some_and(|(until, rule)| {
                        until >= reset && until > now + 120 && rule == Some(RULE_LIMIT_RESUME)
                    })
                };
                if !closed || !named || !waits || !works(first) || !works(next) {
                    wrong.push(format!(
                        "{case}: closed by the act {closed}, open wall {open:?}, successor \
                         waits on its reset {waits}, first loop {first:?}, successor {next:?}"
                    ));
                }
            }
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

#[test]
fn a_buckets_notice_cancelled_before_the_loops_first_read_answers_its_try() {
    use crate::supervise::approvals::{Ledger, Outcome, act_reason, open_wall, usage_attempt};
    use crate::supervise::policy::turn_end::{RULE_LIMIT_RESUME, decide_turn_end};
    use aterm_phase::prompt::fixtures as fx;
    const RESET: i64 = 1_792_279_200;
    let opts = SuperviseOpts {
        policy: SupervisorConfig {
            model_fallback: None,
            ..owners_policy()
        },
        ..auto(30)
    };
    let armed = fx::screen(fx::WALL_SONNET_BUCKET_AUTO_CONTINUE_ARMED_2_1_300);
    let then = |above: &[&str]| -> Vec<String> {
        let mut rows: Vec<String> = armed
            .iter()
            .filter(|r| {
                !r.contains("Usage limit reached") && !r.contains("Continuing automatically")
            })
            .cloned()
            .collect();
        let top = rows
            .iter()
            .position(|r| r.starts_with('─'))
            .expect("the composer's frame");
        rows.splice(top..top, above.iter().map(|r| (*r).to_string()));
        rows
    };
    let answered = then(&[
        "❯ keep going",
        "  ⎿  You've hit your Sonnet limit · resets Oct 17 at 4:20pm (America/Los_Angeles)",
        "",
        "✻ Churned for 0s · done 8:49 AM",
        "",
        "⏺ Automatic continue cancelled · /rate-limit-options to re-arm",
        "",
    ]);
    assert!(
        aterm_phase::left_wall(&answered).is_some_and(|w| w.kind
            == aterm_phase::WallKind::ModelBucket { consent: false }
            && w.reset.is_some()),
        "the screen draws the bucket a cancel left: {answered:#?}"
    );
    let generic = then(&[
        "❯ keep going",
        "  ⎿  You've hit your Sonnet limit · resets Oct 17 at 4:20pm (America/Los_Angeles)",
        "     /upgrade to increase your usage limit.",
        "",
        "⏺ Usage limit reached · continuing automatically at Oct 17 at 4:20pm · esc to cancel",
        "",
        "✻ Churned for 0s · done 8:49 AM",
        "",
        "⏺ Automatic continue cancelled · /rate-limit-options to re-arm",
        "",
    ]);
    assert!(
        aterm_phase::left_wall(&generic).is_some_and(|w| w.kind
            == aterm_phase::WallKind::ModelBucket { consent: false }
            && w.reset.is_some()
            && w.message.contains("Sonnet")),
        "the generic row's cancel left the bucket: {:#?}",
        aterm_phase::left_wall(&generic)
    );
    type Decided = Option<(i64, Option<&'static str>)>;
    let decided = |s: &mut Session<'_, Mock>, rows: &[String], seq: u64| -> Decided {
        let point = point_at(s, rows.to_vec(), seq);
        match s
            .turn_end_at_point(&point, None, &opts)
            .expect("a decision")
        {
            TurnEndAction::WaitUntil { until, .. } => {
                let r = s.turn_end_reading(&point.screen, None, &opts);
                Some((
                    s.unix_of(until),
                    decide_turn_end(&s.turn_end, &r, &opts.policy, until).rule_id(),
                ))
            }
            other => Some((s.now_unix(), other.rule_id())),
        }
    };
    let mut wrong = Vec::new();
    let mut n = 0;
    for (shape, answered) in [("its dialog", &answered), ("the generic row", &generic)] {
        for (when, now) in [
            ("past its reset, the loop's decision", RESET + 20 * 60),
            ("two days before it, the loop's books", RESET - 2 * 86_400),
        ] {
            for worked in [false, true] {
                n += 1;
                let case = format!(
                    "{shape}, {when}, {}",
                    if worked {
                        "control: work answered the try"
                    } else {
                        "the bucket answered the try, then Esc"
                    }
                );
                let (dir, ledger) = ledger_at(&format!("bucket-left-answer-{n}"));
                let mut m = Mock::new(vec![armed.clone()]);
                let mut s = session(&mut m, Some("@s-1".to_string()));
                s.set_zone_lookup(test_zones);
                s.ledger = Ledger::open(Some(&ledger), Some("@s-1"), &mut Vec::new());
                s.set_turn_end_timing(TurnEndTiming {
                    take_within: Duration::ZERO,
                    ..TurnEndTiming::default()
                });
                s.set_clock(now.min(RESET) - 4 * 60, PDT);
                let _ = decided(&mut s, &armed, 100);
                s.set_clock(now, PDT);
                let p = point_at(&s, armed.clone(), 101);
                if now > RESET {
                    let a = s.turn_end_at_point(&p, None, &opts).expect("the try");
                    assert_eq!(a.rule_id(), Some(RULE_LIMIT_RESUME), "{case}");
                    s.pass_wake_settle();
                    s.turn_end_execute(&p, a, &opts, &[], &mut Alone)
                        .expect("typed");
                } else {
                    let r = s.turn_end_reread(&p.screen, &opts);
                    s.turn_end.acted(
                        &TurnEndAction::Type {
                            text: "keep going".to_string(),
                            rule_id: RULE_LIMIT_RESUME,
                        },
                        &r,
                        Instant::now(),
                    );
                    let reason = act_reason(s.turn_end.limit_try());
                    s.ledger_row(
                        RULE_LIMIT_RESUME,
                        Outcome::Typed,
                        "keep going",
                        &reason,
                        101,
                    );
                }
                let act_ms = usage_attempt(&ledger, Some("@s-1"))
                    .expect("the try's row")
                    .at_ms;
                s.set_clock(now + 30, PDT);
                let at_answer = if worked {
                    let p = point_at(&s, ended("Fixed the parser; the suite is green."), 102);
                    let _ = s.turn_end_at_point(&p, Some(Duration::from_secs(12 * 60)), &opts);
                    None
                } else {
                    decided(&mut s, answered, 102)
                };
                let open = open_wall(&ledger, Some("@s-1"));
                s.set_clock(now + 120, PDT);
                let first = (!worked).then(|| decided(&mut s, answered, 103)).flatten();
                drop(s);
                let mut m2 = Mock::new(vec![answered.clone()]);
                let mut s2 = session(&mut m2, Some("@s-1".to_string()));
                s2.set_clock(now + 120, PDT);
                s2.set_zone_lookup(test_zones);
                s2.ledger = Ledger::open(Some(&ledger), Some("@s-1"), &mut Vec::new());
                seed_usage_act(
                    &mut s2.turn_end,
                    &ledger,
                    s2.setup.sid.as_deref(),
                    act_ms + 120_000,
                    Instant::now(),
                );
                s2.seed_wall_memory(open.clone());
                let waits = s2.turn_end.waits_on_named_reset();
                let next = (!worked).then(|| decided(&mut s2, answered, 101)).flatten();
                drop(s2);
                let _ = std::fs::remove_dir_all(&dir);
                let reset = open.as_ref().and_then(|o| o.reset_unix);
                if worked {
                    if open.is_some() || waits {
                        wrong.push(format!(
                            "{case}: open wall {open:?}, successor waits {waits}"
                        ));
                    }
                    continue;
                }
                let waits_out = |d: Decided| {
                    d.is_some_and(|(until, rule)| {
                        until > now + 120 && until >= RESET && rule == Some(RULE_LIMIT_RESUME)
                    })
                };
                let no_sooner = first
                    .zip(next)
                    .is_some_and(|((mine, _), (theirs, _))| theirs + 30 >= mine);
                if reset != Some(RESET)
                    || !waits
                    || !waits_out(at_answer)
                    || !waits_out(first)
                    || !waits_out(next)
                    || !no_sooner
                {
                    wrong.push(format!(
                        "{case}: open wall at {reset:?}, at the answer {at_answer:?}, first loop \
                     {first:?}, successor waits {waits} {next:?}"
                    ));
                }
            }
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

#[test]
fn the_end_of_a_wall_its_try_closed_is_written_for_the_refused_moves_latch() {
    use crate::supervise::approvals::{
        ACCOUNT_MOVE_COMMAND, LIMIT_ENDED, Ledger, Outcome, RULE_WALL_WAIT_END, account_moves,
        account_unmade_reason, act_reason,
    };
    use crate::supervise::policy::turn_end::{
        AccountAt, LimitTry, RULE_ACCOUNT, RULE_LIMIT_RESUME,
    };
    const RESET: i64 = TEST_NOW + 6 * 3600 + 40 * 60;
    let opts = SuperviseOpts {
        policy: SupervisorConfig {
            model_fallback: None,
            ..owners_policy()
        },
        ..auto(30)
    };
    let armed = auto_continue_wall_at("3:35pm");
    let live = {
        let mut r = rows(&[
            "❯ please look at the schema",
            "",
            "⏺ Working.",
            "",
            "✻ Synthesizing… (18s)",
            "",
        ]);
        r.extend(composer("  esc to interrupt"));
        r
    };
    for (n, (case, want)) in [
        ("the wall ended by the worker's work", (0, 1, false)),
        (
            "a live turn read busy, its work ending on the wall",
            (1, 0, false),
        ),
        ("control: the wall stands", (0, 0, true)),
    ]
    .into_iter()
    .enumerate()
    {
        let (dir, ledger) = ledger_at(&format!("written-end-{n}"));
        let mut m = Mock::new(vec![armed.clone()]);
        let mut s = session(&mut m, Some("@s-1".to_string()));
        s.set_zone_lookup(test_zones);
        s.ledger = Ledger::open(Some(&ledger), Some("@s-1"), &mut Vec::new());
        s.set_clock(RESET - 4 * 60, PDT);
        let p = point_at(&s, armed.clone(), 100);
        let _ = s.turn_end_at_point(&p, None, &opts);
        s.set_clock(RESET + 20 * 60, PDT);
        s.ledger_row(
            RULE_LIMIT_RESUME,
            Outcome::Typed,
            "keep going",
            &act_reason(Some(LimitTry {
                bucket: false,
                in_place: false,
                attempts: 1,
                entitlement: false,
            })),
            101,
        );
        s.ledger_row(
            RULE_ACCOUNT,
            Outcome::Skipped,
            &format!("{ACCOUNT_MOVE_COMMAND}work-2"),
            &account_unmade_reason("work-2", AccountAt::Wall, Some("wait:busy")),
            102,
        );
        let latched = account_moves(&ledger, Some("@s-1")).unmade.is_some();
        if n == 0 {
            s.set_clock(RESET + 35 * 60, PDT);
            let p = point_at(&s, ended("Fixed the parser; the suite is green."), 103);
            let _ = s.turn_end_at_point(&p, Some(Duration::from_secs(12 * 60)), &opts);
        } else if n == 1 {
            s.set_clock(RESET + 23 * 60, PDT);
            let reader = s.reader(&live);
            s.end_memory_on_live_turn(reader, &live, 103);
            s.set_clock(RESET + 35 * 60, PDT);
            let p = point_at(&s, armed.clone(), 104);
            let _ = s.turn_end_at_point(&p, Some(Duration::from_secs(12 * 60)), &opts);
        }
        drop(s);
        let unmade = account_moves(&ledger, Some("@s-1")).unmade.is_some();
        let body = std::fs::read_to_string(&ledger).unwrap_or_default();
        let ends: Vec<&str> = body
            .lines()
            .filter(|l| l.contains(&format!("\"rule_id\":\"{RULE_WALL_WAIT_END}\"")))
            .collect();
        let worked = ends.iter().filter(|l| l.contains(LIMIT_ENDED)).count();
        let got = (ends.len() - worked, worked, unmade);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(latched, "{case}: the refused move latched");
        assert_eq!(
            got, want,
            "{case}: the memory's end rows, the policy's, the latch"
        );
    }
}

#[test]
fn a_limits_try_the_workers_work_answered_is_no_seed_of_a_loop_that_follows() {
    use crate::supervise::approvals::{
        LIMIT_ENDED, Ledger, RULE_WALL_WAIT_END, open_wall, usage_attempt,
    };
    use crate::supervise::policy::turn_end::{RULE_LIMIT_RESUME, decide_turn_end};
    let opts = SuperviseOpts {
        policy: SupervisorConfig {
            model_fallback: None,
            usage_credits: true,
            ..owners_policy()
        },
        ..auto(30)
    };
    let walled = |notice: &str| {
        let mut r = rows(&[
            "⏺ Running the suite.",
            &format!("  ⎿  {notice}"),
            "",
            "✻ Worked for 3m 2s · done 4:51 PM",
            "",
        ]);
        r.extend(composer("  ? for shortcuts"));
        r
    };
    let spend = walled("You've hit your monthly spend limit.");
    let live = {
        let mut r = rows(&[
            "❯ please look at the schema",
            "",
            "⏺ Working.",
            "",
            "✻ Synthesizing… (18s)",
            "",
        ]);
        r.extend(composer("  esc to interrupt"));
        r
    };
    type Decided = Option<(i64, Option<&'static str>)>;
    let decided = |s: &mut Session<'_, Mock>,
                   rows: &[String],
                   seq: u64,
                   worked: Option<Duration>|
     -> Decided {
        let point = point_at(s, rows.to_vec(), seq);
        match s
            .turn_end_at_point(&point, worked, &opts)
            .expect("a decision")
        {
            TurnEndAction::WaitUntil { until, .. } => {
                let r = s.turn_end_reading(&point.screen, None, &opts);
                Some((
                    s.unix_of(until),
                    decide_turn_end(&s.turn_end, &r, &opts.policy, until).rule_id(),
                ))
            }
            other => Some((s.now_unix(), other.rule_id())),
        }
    };
    let t = TEST_NOW;
    let mut wrong = Vec::new();
    let mut n = 0;
    for (new_wall, served, person, busy) in [
        ("the spend limit again", true, false, false),
        ("a window, its reset unread", true, false, false),
        (
            "the try's own work met the spend limit again",
            true,
            false,
            false,
        ),
        ("control: the try met again", false, false, false),
        (
            "control: the try met again, then a person's turn",
            false,
            true,
            false,
        ),
        (
            "control: the try met again, then a person's turn read busy, attached into",
            false,
            true,
            true,
        ),
    ] {
        let direct = new_wall.starts_with("the try's own work");
        let again = if new_wall.starts_with("a window") {
            walled("You've hit your session limit")
        } else {
            spend.clone()
        };
        n += 1;
        let (dir, ledger) = ledger_at(&format!("try-worked-{n}"));
        let mid_turn = dir.join("mid-turn.jsonl");
        let mut m = Mock::new(vec![spend.clone()]);
        let mut s = session(&mut m, Some("@s-1".to_string()));
        s.set_zone_lookup(test_zones);
        s.ledger = Ledger::open(Some(&ledger), Some("@s-1"), &mut Vec::new());
        s.set_turn_end_timing(TurnEndTiming {
            take_within: Duration::ZERO,
            ..TurnEndTiming::default()
        });
        s.set_clock(t, PDT);
        let wait = decided(&mut s, &spend, 100, Some(Duration::from_secs(3 * 60)));
        let try_at = wait.map_or(t, |(until, _)| until).max(t + 2 * 3600);
        s.set_clock(try_at, PDT);
        let p = point_at(&s, spend.clone(), 101);
        let a = s.turn_end_at_point(&p, None, &opts).expect("the try");
        assert_eq!(a.rule_id(), Some(RULE_LIMIT_RESUME), "{new_wall}");
        s.pass_wake_settle();
        s.turn_end_execute(&p, a, &opts, &[], &mut Alone)
            .expect("typed");
        let act_ms = usage_attempt(&ledger, Some("@s-1"))
            .expect("the try's row")
            .at_ms;
        let mut open_at_once = None;
        let (wall_at, first) = if direct {
            s.set_clock(try_at + 70 * 60, PDT);
            let p = point_at(&s, again.clone(), 102);
            let a = s
                .turn_end_at_point(&p, Some(Duration::from_secs(70 * 60)), &opts)
                .expect("a decision");
            open_at_once = Some(open_wall(&ledger, Some("@s-1")).is_some());
            let TurnEndAction::WaitUntil { until, .. } = a else {
                panic!("{new_wall}: {a:?}");
            };
            let r = s.turn_end_reading(&p.screen, None, &opts);
            (
                try_at + 70 * 60,
                Some((
                    s.unix_of(until),
                    decide_turn_end(&s.turn_end, &r, &opts.policy, until).rule_id(),
                )),
            )
        } else if served {
            s.set_clock(try_at + 50 * 60, PDT);
            let p = point_at(&s, ended("Migrated the second table."), 102);
            let a = s
                .turn_end_at_point(&p, Some(Duration::from_secs(50 * 60)), &opts)
                .expect("the continuation");
            s.pass_wake_settle();
            s.turn_end_execute(&p, a, &opts, &[], &mut Alone)
                .expect("typed");
            s.set_clock(try_at + 120 * 60, PDT);
            (
                try_at + 120 * 60,
                decided(&mut s, &again, 103, Some(Duration::from_secs(60 * 60))),
            )
        } else {
            s.set_clock(try_at + 30, PDT);
            let met = decided(&mut s, &again, 102, None);
            if person {
                if busy {
                    s.set_clock(try_at + 20 * 60, PDT);
                    let reader = s.reader(&live);
                    s.end_memory_on_live_turn(reader, &live, 103);
                    std::fs::copy(&ledger, &mid_turn).expect("the ledger mid-turn");
                }
                s.set_clock(try_at + 40 * 60, PDT);
                (
                    try_at + 40 * 60,
                    decided(&mut s, &again, 104, Some(Duration::from_secs(20 * 60))),
                )
            } else {
                (try_at + 30, met)
            }
        };
        drop(s);
        let (attach, seen) = if person {
            (wall_at, Some(Duration::from_secs(10 * 60)))
        } else {
            (wall_at + 10 * 60, None)
        };
        let books = if busy { &mid_turn } else { &ledger };
        let seed = usage_attempt(books, Some("@s-1")).map(|a| (a.at_ms, a.limit));
        let open = open_wall(books, Some("@s-1"));
        let mut m2 = Mock::new(vec![again.clone()]);
        let mut s2 = session(&mut m2, Some("@s-1".to_string()));
        s2.set_clock(attach, PDT);
        s2.set_zone_lookup(test_zones);
        s2.ledger = Ledger::open(Some(books), Some("@s-1"), &mut Vec::new());
        seed_usage_act(
            &mut s2.turn_end,
            books,
            s2.setup.sid.as_deref(),
            act_ms + (attach - try_at) * 1000,
            Instant::now(),
        );
        s2.seed_wall_memory(open.clone());
        let next = decided(&mut s2, &again, 101, seen);
        drop(s2);
        let body = std::fs::read_to_string(&ledger).unwrap_or_default();
        let worked_ends = body
            .lines()
            .filter(|l| l.contains(RULE_WALL_WAIT_END) && l.contains(LIMIT_ENDED))
            .count();
        let _ = std::fs::remove_dir_all(&dir);
        let due = |d: Decided| d.filter(|&(_, rule)| rule == Some(RULE_LIMIT_RESUME));
        let (Some((mine, _)), Some((theirs, _))) = (due(first), due(next)) else {
            wrong.push(format!(
                "{new_wall}: first loop {first:?}, successor {next:?}, open wall {open:?}"
            ));
            continue;
        };
        let ok = if served {
            seed.is_none()
                && worked_ends == 1
                && theirs > attach
                && theirs >= mine
                && open.is_some()
                && open_at_once != Some(false)
        } else {
            seed.is_some_and(|(_, l)| l.attempts == 1 && l.entitlement)
                && worked_ends == 0
                && theirs == mine
                && (!busy || open.is_none())
        };
        if !ok {
            wrong.push(format!(
                "{new_wall}: try at +{}s, the new wall at +{}s, first loop until +{}s, \
                 successor at +{}s took up {seed:?} and waits until +{}s; {worked_ends} \
                 end row(s) of work, open wall {open:?}",
                try_at - t,
                wall_at - t,
                mine - t,
                attach - t,
                theirs - t,
            ));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

#[test]
fn a_limits_track_dropped_with_no_work_ends_its_try_for_a_loop_that_follows() {
    use crate::supervise::approvals::{Ledger, open_wall, usage_attempt};
    use crate::supervise::policy::turn_end::{RULE_LIMIT_RESUME, decide_turn_end};
    let opts = SuperviseOpts {
        policy: SupervisorConfig {
            model_fallback: None,
            usage_credits: true,
            ..owners_policy()
        },
        ..auto(30)
    };
    let spend = {
        let mut r = rows(&[
            "⏺ Running the suite.",
            "  ⎿  You've hit your monthly spend limit.",
            "",
            "✻ Worked for 3m 2s · done 4:51 PM",
            "",
        ]);
        r.extend(composer("  ? for shortcuts"));
        r
    };
    let end_529 = aterm_phase::prompt::fixtures::screen(aterm_phase::prompt::fixtures::END_529);
    let live = {
        let mut r = rows(&[
            "❯ please look at the schema",
            "",
            "⏺ Working.",
            "",
            "✻ Synthesizing… (18s)",
            "",
        ]);
        r.extend(composer("  esc to interrupt"));
        r
    };
    type Decided = Option<(i64, Option<&'static str>)>;
    let decided = |s: &mut Session<'_, Mock>,
                   rows: &[String],
                   seq: u64,
                   worked: Option<Duration>|
     -> Decided {
        let point = point_at(s, rows.to_vec(), seq);
        match s
            .turn_end_at_point(&point, worked, &opts)
            .expect("a decision")
        {
            TurnEndAction::WaitUntil { until, .. } => {
                let r = s.turn_end_reading(&point.screen, None, &opts);
                Some((
                    s.unix_of(until),
                    decide_turn_end(&s.turn_end, &r, &opts.policy, until).rule_id(),
                ))
            }
            other => Some((s.now_unix(), other.rule_id())),
        }
    };
    let typed = |s: &mut Session<'_, Mock>, rows: &[String], seq: u64| -> Option<&'static str> {
        let p = point_at(s, rows.to_vec(), seq);
        let a = s.turn_end_at_point(&p, None, &opts).expect("an act");
        let rule = a.rule_id();
        s.pass_wake_settle();
        s.turn_end_execute(&p, a, &opts, &[], &mut Alone)
            .expect("typed");
        rule
    };
    let t = TEST_NOW;
    let mut wrong = Vec::new();
    for (n, (how, into_turn)) in [
        ("a /clear", false),
        ("a /clear", true),
        ("a /clear after a person's turn read busy", false),
        ("a 529 ending the try's own turn", false),
        ("a 529 ending the try's own turn", true),
        ("control: no drop", false),
        ("control: no drop", true),
    ]
    .into_iter()
    .enumerate()
    {
        let case = format!(
            "{how}, the successor {}",
            if into_turn {
                "attached into a person's turn"
            } else {
                "on the wall"
            }
        );
        let (dir, ledger) = ledger_at(&format!("track-dropped-{n}"));
        let books = dir.join("books.jsonl");
        let mut m = Mock::new(vec![spend.clone()]);
        let mut s = session(&mut m, Some("@s-1".to_string()));
        s.set_zone_lookup(test_zones);
        s.ledger = Ledger::open(Some(&ledger), Some("@s-1"), &mut Vec::new());
        s.set_turn_end_timing(TurnEndTiming {
            take_within: Duration::ZERO,
            ..TurnEndTiming::default()
        });
        s.set_clock(t, PDT);
        let wait = decided(&mut s, &spend, 100, Some(Duration::from_secs(3 * 60)));
        let try_at = wait.map_or(t, |(until, _)| until).max(t + 2 * 3600);
        s.set_clock(try_at, PDT);
        assert_eq!(
            typed(&mut s, &spend, 101),
            Some(RULE_LIMIT_RESUME),
            "{case}"
        );
        let act_ms = usage_attempt(&ledger, Some("@s-1")).expect("the try").at_ms;
        let control = how.starts_with("control");
        let wall_at = if control {
            try_at + 30
        } else {
            try_at + 105 * 60
        };
        let mut step = if how.contains("529") {
            s.set_clock(try_at + 20 * 60, PDT);
            let p = point_at(&s, end_529.clone(), 102);
            let a = s
                .turn_end_at_point(&p, Some(Duration::from_secs(20 * 60)), &opts)
                .expect("the 529");
            let due = match a {
                TurnEndAction::WaitUntil { until, .. } => s.unix_of(until),
                _ => s.now_unix(),
            };
            s.set_clock(due, PDT);
            typed(&mut s, &end_529, 103);
            s.set_clock(wall_at, PDT);
            decided(&mut s, &spend, 104, Some(Duration::from_secs(60 * 60)))
        } else {
            s.set_clock(try_at + 30, PDT);
            let met = decided(&mut s, &spend, 102, None);
            if control {
                met
            } else {
                // A person's turn read busy ends the remembered wall (and its
                // row closes the ledger's): the `/clear` then ends the
                // policy's track alone.
                if how.contains("busy") {
                    s.set_clock(try_at + 3 * 60, PDT);
                    let reader = s.reader(&live);
                    s.end_memory_on_live_turn(reader, &live, 103);
                }
                s.set_clock(try_at + 5 * 60, PDT);
                decided(&mut s, &cleared_at_an_armed_limit(), 103, None);
                s.set_clock(wall_at, PDT);
                decided(&mut s, &spend, 104, Some(Duration::from_secs(5 * 60)))
            }
        };
        let attach = wall_at + 5 * 60;
        let (decide_at, seen) = if into_turn {
            s.set_clock(wall_at + 3 * 60, PDT);
            let reader = s.reader(&live);
            s.end_memory_on_live_turn(reader, &live, 105);
            std::fs::copy(&ledger, &books).expect("the books");
            s.set_clock(wall_at + 10 * 60, PDT);
            step = decided(&mut s, &spend, 106, Some(Duration::from_secs(7 * 60)));
            (wall_at + 10 * 60, Some(Duration::from_secs(5 * 60)))
        } else {
            std::fs::copy(&ledger, &books).expect("the books");
            (attach, None)
        };
        drop(s);
        let seed = usage_attempt(&books, Some("@s-1"));
        let mut m2 = Mock::new(vec![spend.clone()]);
        let mut s2 = session(&mut m2, Some("@s-1".to_string()));
        s2.set_clock(decide_at, PDT);
        s2.set_zone_lookup(test_zones);
        s2.ledger = Ledger::open(Some(&books), Some("@s-1"), &mut Vec::new());
        seed_usage_act(
            &mut s2.turn_end,
            &books,
            s2.setup.sid.as_deref(),
            act_ms + (decide_at - try_at) * 1000,
            Instant::now(),
        );
        s2.seed_wall_memory(open_wall(&books, Some("@s-1")));
        let next = decided(&mut s2, &spend, 101, seen);
        drop(s2);
        let _ = std::fs::remove_dir_all(&dir);
        let due = |d: Decided| d.filter(|&(_, rule)| rule == Some(RULE_LIMIT_RESUME));
        let (Some((mine, _)), Some((theirs, _))) = (due(step), due(next)) else {
            wrong.push(format!("{case}: first loop {step:?}, successor {next:?}"));
            continue;
        };
        let ok = if control {
            seed.as_ref()
                .is_some_and(|a| a.met_ms.is_some() && a.limit.attempts == 1)
                && theirs == mine
        } else {
            seed.is_none() && theirs >= mine
        };
        if !ok {
            wrong.push(format!(
                "{case}: the new wall at +{}s; first loop until +{}s; the successor took up \
                 {seed:?} and waits until +{}s",
                wall_at - try_at,
                mine - try_at,
                theirs - try_at
            ));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

#[test]
fn a_wall_read_inside_a_tries_take_window_is_no_try_met_again_where_it_is_taken() {
    use crate::supervise::approvals::{Ledger, WALL_TAKEN, open_wall, usage_attempt};
    use crate::supervise::policy::turn_end::{RULE_LIMIT_RESUME, decide_turn_end};
    let opts = SuperviseOpts {
        policy: SupervisorConfig {
            model_fallback: None,
            usage_credits: true,
            ..owners_policy()
        },
        ..auto(30)
    };
    let spend = {
        let mut r = rows(&[
            "⏺ Running the suite.",
            "  ⎿  You've hit your monthly spend limit.",
            "",
            "✻ Worked for 3m 2s · done 4:51 PM",
            "",
        ]);
        r.extend(composer("  ? for shortcuts"));
        r
    };
    let live = |prompt: &str| {
        let mut r = rows(&[
            &format!("❯ {prompt}"),
            "",
            "⏺ Working.",
            "",
            "✻ Synthesizing… (18s)",
            "",
        ]);
        r.extend(composer("  esc to interrupt"));
        r
    };
    type Decided = Option<(i64, Option<&'static str>)>;
    let decided = |s: &mut Session<'_, Mock>,
                   rows: &[String],
                   seq: u64,
                   worked: Option<Duration>|
     -> Decided {
        let point = point_at(s, rows.to_vec(), seq);
        match s
            .turn_end_at_point(&point, worked, &opts)
            .expect("a decision")
        {
            TurnEndAction::WaitUntil { until, .. } => {
                let r = s.turn_end_reading(&point.screen, None, &opts);
                Some((
                    s.unix_of(until),
                    decide_turn_end(&s.turn_end, &r, &opts.policy, until).rule_id(),
                ))
            }
            other => Some((s.now_unix(), other.rule_id())),
        }
    };
    let t = TEST_NOW;
    let mut wrong = Vec::new();
    let local = {
        let mut r = rows(&[
            "⏺ Running the suite.",
            "  ⎿  You've hit your monthly spend limit.",
            "",
            "✻ Worked for 3m 2s · done 4:51 PM",
            "",
            "❯ /model",
            "  ⎿  Kept model as Sonnet 5.5 (default)",
            "",
        ]);
        r.extend(composer("  ? for shortcuts"));
        r
    };
    for (n, case) in [
        "the notice read inside the take window, then the try taken",
        "the notice read inside the take window, a local command, then a person's 3 h turn takes the try",
        "control: no read inside the take window",
        "control: the notice read inside the take window, the try met again, then a person's turn",
    ]
    .into_iter()
    .enumerate()
    {
        let reread = !case.contains("no read");
        let person = case.ends_with("a person's turn");
        let long = case.contains("3 h turn");
        let (dir, ledger) = ledger_at(&format!("take-window-{n}"));
        let books = dir.join("books.jsonl");
        let mut m = Mock::new(vec![spend.clone()]);
        let mut s = session(&mut m, Some("@s-1".to_string()));
        s.set_zone_lookup(test_zones);
        s.ledger = Ledger::open(Some(&ledger), Some("@s-1"), &mut Vec::new());
        let window = if person {
            Duration::from_millis(50)
        } else {
            TurnEndTiming::default().take_within
        };
        s.set_turn_end_timing(TurnEndTiming {
            take_within: window,
            ..TurnEndTiming::default()
        });
        s.set_clock(t, PDT);
        s.set_mono_standing();
        let wait = decided(&mut s, &spend, 100, Some(Duration::from_secs(3 * 60)));
        let try_at = wait.map_or(t, |(until, _)| until).max(t + 2 * 3600);
        s.set_clock(try_at, PDT);
        let p = point_at(&s, spend.clone(), 101);
        let a = s.turn_end_at_point(&p, None, &opts).expect("the try");
        assert_eq!(a.rule_id(), Some(RULE_LIMIT_RESUME), "{case}");
        s.pass_wake_settle();
        s.turn_end_execute(&p, a, &opts, &[], &mut Alone)
            .expect("typed");
        let act_ms = usage_attempt(&ledger, Some("@s-1")).expect("the try").at_ms;
        if reread {
            s.set_clock(try_at + 5, PDT);
            decided(&mut s, &spend, 102, None);
        }
        if long {
            s.set_clock(try_at + 7, PDT);
            decided(&mut s, &local, 103, None);
        }
        if person {
            s.test_clock().skip(window * 2);
            s.set_clock(try_at + 40, PDT);
            decided(&mut s, &spend, 103, None);
        }
        let busy_at = if person { 20 * 60 } else { 20 };
        s.set_clock(try_at + busy_at, PDT);
        let turn = live(if person || long {
            "please look at the schema"
        } else {
            "keep going"
        });
        let reader = s.reader(&turn);
        s.end_memory_on_live_turn(reader, &turn, 104);
        s.turn_end.took();
        s.set_clock(try_at + busy_at + 10 * 60, PDT);
        std::fs::copy(&ledger, &books).expect("the books");
        let (end_at, work) = if person {
            (try_at + 40 * 60, 20 * 60)
        } else if long {
            (try_at + 20 + 3 * 3600, 3 * 3600)
        } else {
            (try_at + 20 * 60, 19 * 60)
        };
        s.set_clock(end_at, PDT);
        let first = decided(&mut s, &spend, 105, Some(Duration::from_secs(work)));
        drop(s);
        let rows = std::fs::read_to_string(&books).unwrap_or_default();
        let taken = rows.lines().filter(|l| l.contains(WALL_TAKEN)).count();
        let seed = usage_attempt(&books, Some("@s-1"));
        let mut m2 = Mock::new(vec![spend.clone()]);
        let mut s2 = session(&mut m2, Some("@s-1".to_string()));
        s2.set_clock(end_at, PDT);
        s2.set_mono_standing();
        s2.set_zone_lookup(test_zones);
        s2.ledger = Ledger::open(Some(&books), Some("@s-1"), &mut Vec::new());
        let seeded_at = s2.test_clock().mono();
        seed_usage_act(
            &mut s2.turn_end,
            &books,
            s2.setup.sid.as_deref(),
            act_ms + (end_at - try_at) * 1000,
            seeded_at,
        );
        s2.seed_wall_memory(open_wall(&books, Some("@s-1")));
        let next = decided(&mut s2, &spend, 101, Some(Duration::from_secs(10 * 60)));
        drop(s2);
        let _ = std::fs::remove_dir_all(&dir);
        let due = |d: Decided| d.filter(|&(_, rule)| rule == Some(RULE_LIMIT_RESUME));
        let (Some((mine, _)), Some((theirs, _))) = (due(first), due(next)) else {
            wrong.push(format!("{case}: first loop {first:?}, successor {next:?}"));
            continue;
        };
        let ok = if person {
            taken == 0
                && seed.as_ref().is_some_and(|a| a.met_ms.is_some())
                && theirs == try_at + 30 + 120 * 60
                && theirs + 40 >= mine
        } else {
            taken == usize::from(reread)
                && seed.as_ref().is_some_and(|a| a.met_ms.is_none())
                && mine == end_at + 30 * 60
                && theirs == mine
        };
        if !ok {
            wrong.push(format!(
                "{case}: {taken} take row(s); the first loop until +{}s, the successor took up \
                 {seed:?} and waits until +{}s",
                mine - try_at,
                theirs - try_at
            ));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

#[test]
fn a_bucket_the_fallbacks_relaunch_left_is_no_wall_of_the_relaunched_session() {
    use crate::supervise::approvals::{Ledger, Outcome, model_switch_reason, open_wall};
    use crate::supervise::policy::turn_end::{
        RULE_CONTINUE, RULE_MODEL_FALLBACK, Restart, decide_turn_end,
    };
    use aterm_phase::prompt::fixtures as fx;
    const RESET: i64 = 1_792_279_200;
    let now = RESET - 2 * 86_400;
    let opts = SuperviseOpts {
        policy: owners_policy(),
        ..auto(30)
    };
    assert_eq!(opts.policy.model_fallback.as_deref(), Some("opus"));
    let armed = fx::screen(fx::WALL_SONNET_BUCKET_AUTO_CONTINUE_ARMED_2_1_300);
    let redraw = |model: &str| -> Vec<String> {
        armed
            .iter()
            .filter(|r| {
                ![
                    "Usage limit reached",
                    "Continuing automatically",
                    "/rate-limit-options",
                    "Claude Code will continue automatically",
                    "to cancel the wait",
                ]
                .iter()
                .any(|g| r.contains(g))
            })
            .map(|r| {
                r.replace("Sonnet 5.5 · Claude API", "Opus 5.5 · Claude API")
                    .replace("your Sonnet limit", &format!("your {model} limit"))
            })
            .collect()
    };
    let redrawn = redraw("Sonnet");
    let met = {
        let mut rows = redrawn.clone();
        let top = rows
            .iter()
            .position(|r| r.starts_with('─'))
            .expect("the composer's frame");
        rows.splice(
            top..top,
            [
                "❯ keep going",
                "  ⎿  You've hit your Sonnet limit · resets Oct 17 at 4:20pm (America/Los_Angeles)",
                "",
                "✻ Churned for 0s · done 8:49 AM",
                "",
            ]
            .map(str::to_string),
        );
        rows
    };
    type Decided = Option<(i64, Option<&'static str>)>;
    let decided = |s: &mut Session<'_, Mock>, rows: &[String], seq: u64| -> Decided {
        let point = point_at(s, rows.to_vec(), seq);
        match s
            .turn_end_at_point(&point, None, &opts)
            .expect("a decision")
        {
            TurnEndAction::WaitUntil { until, .. } => {
                let r = s.turn_end_reading(&point.screen, None, &opts);
                Some((
                    s.unix_of(until),
                    decide_turn_end(&s.turn_end, &r, &opts.policy, until).rule_id(),
                ))
            }
            other => Some((s.now_unix(), other.rule_id())),
        }
    };
    let successor = |ledger: &std::path::Path, at: i64, rows: &[String]| -> Decided {
        let mut m = Mock::new(vec![rows.to_vec()]);
        let mut s = session(&mut m, Some("@s-1".to_string()));
        s.set_clock(at, PDT);
        s.set_zone_lookup(test_zones);
        s.ledger = Ledger::open(Some(ledger), Some("@s-1"), &mut Vec::new());
        s.seed_model_switch(ledger);
        s.seed_wall_memory(open_wall(ledger, Some("@s-1")));
        decided(&mut s, rows, 101)
    };
    let short_turn = |d: Decided, from: i64| d == Some((from + 120, Some(RULE_CONTINUE)));
    let waits_out = |d: Decided| d.is_some_and(|(until, _)| until >= RESET);
    let mut wrong = Vec::new();
    for (case, model) in [
        ("the bucket the relaunch left", "Sonnet"),
        ("control: another model's bucket", "Opus"),
        ("control: a point since the relaunch", "Sonnet"),
    ] {
        let moved_on = case.ends_with("since the relaunch");
        let (dir, ledger) = ledger_at(&format!("bucket-left-{model}-{moved_on}"));
        let mut m = Mock::new(vec![armed.clone()]);
        let mut s = session(&mut m, Some("@s-1".to_string()));
        s.set_zone_lookup(test_zones);
        s.ledger = Ledger::open(Some(&ledger), Some("@s-1"), &mut Vec::new());
        s.set_turn_end_timing(TurnEndTiming {
            take_within: Duration::ZERO,
            ..TurnEndTiming::default()
        });
        s.set_clock(now, PDT);
        let _ = decided(&mut s, &armed, 100);
        let p = point_at(&s, armed.clone(), 101);
        let r = s.turn_end_reread(&p.screen, &opts);
        s.turn_end.restarted(
            &Restart::Model {
                to: "opus".to_string(),
            },
            true,
            &r,
            Instant::now(),
        );
        s.ledger_row(
            RULE_MODEL_FALLBACK,
            Outcome::Typed,
            "relaunch --model opus",
            &model_switch_reason(Some("sonnet"), Some(RESET), now),
            101,
        );
        let screen = redraw(model);
        if moved_on {
            s.set_clock(now + 20, PDT);
            let _ = decided(&mut s, &ended("Here is the summary you asked for."), 102);
            s.set_clock(now + 30, PDT);
            let again = decided(&mut s, &met, 103);
            drop(s);
            let _ = std::fs::remove_dir_all(&dir);
            if !waits_out(again) {
                wrong.push(format!("{case}: {again:?}"));
            }
            continue;
        }
        s.set_clock(now + 30, PDT);
        let first = decided(&mut s, &screen, 102);
        let open = open_wall(&ledger, Some("@s-1")).is_some();
        let next = successor(&ledger, now + 60, &screen);
        if model == "Opus" {
            if !waits_out(first) || !waits_out(next) || !open {
                wrong.push(format!(
                    "{case}: first loop {first:?}, open wall {open}, successor {next:?}"
                ));
            }
            drop(s);
            let _ = std::fs::remove_dir_all(&dir);
            continue;
        }
        if !short_turn(first, now + 30) || open || !short_turn(next, now + 60) {
            wrong.push(format!(
                "{case}: first loop {first:?}, open wall {open}, successor {next:?}"
            ));
        }
        s.set_clock(now + 150, PDT);
        let p = point_at(&s, screen.clone(), 103);
        let a = s
            .turn_end_at_point(&p, None, &opts)
            .expect("the continuation");
        assert_eq!(a.rule_id(), Some(RULE_CONTINUE), "{case}");
        s.pass_wake_settle();
        s.turn_end_execute(&p, a, &opts, &[], &mut Alone)
            .expect("typed");
        let early = successor(&ledger, now + 160, &met);
        s.set_clock(now + 180, PDT);
        let again = decided(&mut s, &met, 104);
        drop(s);
        let after = successor(&ledger, now + 240, &met);
        let _ = std::fs::remove_dir_all(&dir);
        if !waits_out(early) || !waits_out(again) || !waits_out(after) {
            wrong.push(format!(
                "{case}, the continuation met the bucket: a successor before its read \
                 {early:?}, first loop {again:?}, a successor after {after:?}"
            ));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

fn bucket_notice_transcript(prompt: &str, notice: &str, at: i64) -> String {
    let stamp = crate::harness::usage::rfc3339_utc(at);
    include_str!("fixtures/claude-2.1.294-transcript-session-limit.jsonl")
        .lines()
        .skip(1)
        .filter(|l| !l.contains("\"subtype\":\"informational\""))
        .map(|l| {
            let mut row = l
                .replace("seventh try", prompt)
                .replace(
                    "You've hit your session limit · resets 11:33am (America/Los_Angeles)",
                    notice,
                )
                .replace("\"five_hour\"", "\"seven_day_sonnet\"");
            for ms in ["921", "951", "952", "953"] {
                row = row.replace(
                    &format!("\"2026-10-08T16:34:58.{ms}Z\""),
                    &format!("\"{stamp}\""),
                );
            }
            row
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n"
}

#[test]
fn a_turn_asked_on_the_fallback_is_written_for_a_loop_that_follows() {
    use crate::supervise::approvals::{Ledger, open_wall};
    use crate::supervise::policy::turn_end::{HostTurn, RULE_CONTINUE, decide_turn_end};
    use aterm_phase::prompt::fixtures as fx;
    const RESET: i64 = 1_792_279_200;
    let now = RESET - 2 * 86_400;
    let armed = fx::screen(fx::WALL_SONNET_BUCKET_AUTO_CONTINUE_ARMED_2_1_300);
    let redrawn: Vec<String> = armed
        .iter()
        .filter(|r| {
            ![
                "Usage limit reached",
                "Continuing automatically",
                "/rate-limit-options",
                "Claude Code will continue automatically",
                "to cancel the wait",
            ]
            .iter()
            .any(|g| r.contains(g))
        })
        .map(|r| r.replace("Sonnet 5.5 · Claude API", "Opus 5.5 · Claude API"))
        .collect();
    let above_composer = |base: &[String], add: &[&str]| -> Vec<String> {
        let mut rows = base.to_vec();
        let top = rows
            .iter()
            .position(|r| r.starts_with('─'))
            .expect("the composer's frame");
        rows.splice(top..top, add.iter().map(|s| (*s).to_string()));
        rows
    };
    let to_sonnet = above_composer(
        &redrawn,
        &["❯ /model sonnet", "  ⎿  Set model to Sonnet 5.5", ""],
    );
    let met = |prompt: &str, base: &[String]| {
        above_composer(
            base,
            &[
                &format!("❯ {prompt}"),
                "  ⎿  You've hit your Sonnet limit · resets Oct 17 at 4:20pm (America/Los_Angeles)",
                "",
                "✻ Churned for 0s · done 8:49 AM",
                "",
            ],
        )
    };
    type Decided = Option<(i64, Option<&'static str>)>;
    let decided = |s: &mut Session<'_, Mock>,
                   rows: &[String],
                   seq: u64,
                   worked: Option<Duration>,
                   opts: &SuperviseOpts|
     -> Decided {
        let point = point_at(s, rows.to_vec(), seq);
        match s
            .turn_end_at_point(&point, worked, opts)
            .expect("a decision")
        {
            TurnEndAction::WaitUntil { until, .. } => {
                let r = s.turn_end_reading(&point.screen, None, opts);
                Some((
                    s.unix_of(until),
                    decide_turn_end(&s.turn_end, &r, &opts.policy, until).rule_id(),
                ))
            }
            other => Some((s.now_unix(), other.rule_id())),
        }
    };
    let before = bucket_notice_transcript(
        "carry on with the task",
        "You've hit your Sonnet limit · resets Oct 17 at 4:20pm (America/Los_Angeles)",
        now - 600,
    );
    let successor_host = AccountHost::scripted(&[None], &[]);
    *successor_host.tail.lock().unwrap() = Some(before.clone());
    let plain = SuperviseOpts {
        policy: owners_policy(),
        ..auto(30)
    };
    let known_plain = SuperviseOpts {
        idle_host: Some(successor_host as Arc<dyn IdleHost>),
        ..plain.clone()
    };
    let successor =
        |ledger: &std::path::Path, at: i64, rows: &[String], opts: &SuperviseOpts| -> Decided {
            let mut m = Mock::new(vec![rows.to_vec()]);
            let mut s = session(&mut m, Some("@s-1".to_string()));
            s.set_mono_standing();
            s.set_clock(at, PDT);
            s.set_zone_lookup(test_zones);
            s.ledger = Ledger::open(Some(ledger), Some("@s-1"), &mut Vec::new());
            s.seed_model_switch(ledger);
            s.seed_wall_memory(open_wall(ledger, Some("@s-1")));
            decided(&mut s, rows, 101, None, opts)
        };
    let waits_out = |d: Decided| d.is_some_and(|(until, _)| until >= RESET);
    let mut wrong = Vec::new();
    for known in [false, true] {
        let plain = if known {
            known_plain.clone()
        } else {
            plain.clone()
        };
        for (n, case) in [
            "a person's /model sonnet, then their prompt met by the bucket",
            "a person's exchange, then their prompt met by the bucket",
            "the host's carry-on typed inside the relaunch, met by the bucket",
            "control: the redraw, nothing between",
            "the host's carry-on typed at its own step, met by the bucket",
        ]
        .into_iter()
        .enumerate()
        {
            let carried = n == 2;
            let (dir, ledger) = ledger_at(&format!("fallback-asked-{known}-{n}"));
            let host =
                AccountHost::scripted(&[None], &[if carried { "continued" } else { "relaunched" }]);
            if known {
                *host.tail.lock().unwrap() = Some(before.clone());
            }
            let opts = SuperviseOpts {
                idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
                ..plain.clone()
            };
            let mut m = Mock::new(vec![armed.clone()]);
            let mut s = session(&mut m, Some("@s-1".to_string()));
            s.set_mono_standing();
            s.set_zone_lookup(test_zones);
            s.ledger = Ledger::open(Some(&ledger), Some("@s-1"), &mut Vec::new());
            s.set_turn_end_timing(TurnEndTiming {
                take_within: Duration::ZERO,
                ..TurnEndTiming::default()
            });
            s.set_clock(now, PDT);
            let p = point_at(&s, armed.clone(), 100);
            let a = s.turn_end_at_point(&p, None, &opts).expect("the fallback");
            assert!(
                matches!(&a, TurnEndAction::Restart { why: Restart::Model { to }, .. } if to == "opus"),
                "{case}: {a:?}"
            );
            s.turn_end_execute(&p, a, &opts, &[], &mut Alone)
                .expect("relaunched");
            assert_eq!(host.said("restart"), ["restart:model"], "{case}");
            let (last, early) = match n {
                0 | 1 => {
                    s.set_clock(now + 30, PDT);
                    let _ = decided(&mut s, &redrawn, 102, None, &opts);
                    s.set_clock(now + 60, PDT);
                    let last = if n == 0 {
                        let _ = decided(&mut s, &to_sonnet, 103, None, &opts);
                        met("please run the integration tests", &to_sonnet)
                    } else {
                        let _ = decided(
                            &mut s,
                            &ended("Here is the summary you asked for."),
                            103,
                            Some(Duration::from_secs(20)),
                            &opts,
                        );
                        met("now do it on sonnet", &to_sonnet)
                    };
                    let early = successor(&ledger, now + 85, &last, &plain);
                    s.set_clock(now + 90, PDT);
                    (last, early)
                }
                2 | 4 => {
                    if n == 4 {
                        s.host_turn(HostTurn::CarryOn, 102);
                    }
                    let last = met("Continue from where you left off.", &redrawn);
                    let early = successor(&ledger, now + 25, &last, &plain);
                    s.set_clock(now + 30, PDT);
                    (last, early)
                }
                _ => {
                    let early = successor(&ledger, now + 25, &redrawn, &plain);
                    s.set_clock(now + 30, PDT);
                    (redrawn.clone(), early)
                }
            };
            let first = decided(
                &mut s,
                &last,
                104,
                n.lt(&2).then(|| Duration::from_secs(1)),
                &opts,
            );
            drop(s);
            let after = successor(&ledger, now + 120, &last, &plain);
            let third = successor(&ledger, now + 600, &last, &plain);
            let _ = std::fs::remove_dir_all(&dir);
            let ok = if n != 3 {
                [early, first, after, third].into_iter().all(waits_out)
            } else {
                let first_ok = first == Some((now + 30 + 120, Some(RULE_CONTINUE)));
                early == Some((now + 25 + 120, Some(RULE_CONTINUE)))
                    && first_ok
                    && after == Some((now + 120 + 120, Some(RULE_CONTINUE)))
            };
            if !ok {
                wrong.push(format!(
                    "{case}, known={known}: a successor before the first loop's read {early:?}, the first loop \
                 {first:?}, a successor after {after:?}, one after that {third:?}"
                ));
            }
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

#[test]
fn a_buckets_notice_the_transcript_wrote_after_the_relaunch_is_the_wall() {
    use crate::supervise::approvals::{Ledger, open_model_switch, open_wall};
    use crate::supervise::policy::turn_end::{RULE_CONTINUE, decide_turn_end};
    use aterm_phase::prompt::fixtures as fx;
    const RESET: i64 = 1_792_279_200;
    let now = RESET - 2 * 86_400;
    let armed = fx::screen(fx::WALL_SONNET_BUCKET_AUTO_CONTINUE_ARMED_2_1_300);
    let redrawn: Vec<String> = armed
        .iter()
        .filter(|r| {
            ![
                "Usage limit reached",
                "Continuing automatically",
                "/rate-limit-options",
                "Claude Code will continue automatically",
                "to cancel the wait",
            ]
            .iter()
            .any(|g| r.contains(g))
        })
        .map(|r| r.replace("Sonnet 5.5 · Claude API", "Opus 5.5 · Claude API"))
        .collect();
    let notice = "You've hit your Sonnet limit · resets Oct 17 at 4:20pm (America/Los_Angeles)";
    let met = {
        let mut rows = redrawn.clone();
        let top = rows
            .iter()
            .position(|r| r.starts_with('─'))
            .expect("the composer's frame");
        rows.splice(
            top..top,
            [
                "❯ please run the integration tests".to_string(),
                format!("  ⎿  {notice}"),
                String::new(),
                "✻ Churned for 0s · done 8:49 AM".to_string(),
                String::new(),
            ],
        );
        rows
    };
    let tail = |prompt: &str, at: i64| bucket_notice_transcript(prompt, notice, at);
    let before = tail("carry on with the task", now - 600);
    let after = tail("please run the integration tests", now + 85);
    type Decided = Option<(i64, Option<&'static str>)>;
    let decided = |s: &mut Session<'_, Mock>,
                   rows: &[String],
                   seq: u64,
                   worked: Option<Duration>,
                   opts: &SuperviseOpts|
     -> Decided {
        let point = point_at(s, rows.to_vec(), seq);
        match s
            .turn_end_at_point(&point, worked, opts)
            .expect("a decision")
        {
            TurnEndAction::WaitUntil { until, .. } => {
                let r = s.turn_end_reading(&point.screen, None, opts);
                Some((
                    s.unix_of(until),
                    decide_turn_end(&s.turn_end, &r, &opts.policy, until).rule_id(),
                ))
            }
            other => Some((s.now_unix(), other.rule_id())),
        }
    };
    let hosted = |transcript: &str, answers: &[&'static str]| {
        let host = AccountHost::scripted(&[None], answers);
        *host.tail.lock().unwrap() = Some(transcript.to_string());
        let opts = SuperviseOpts {
            idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
            policy: owners_policy(),
            ..auto(30)
        };
        (host, opts)
    };
    let successor =
        |ledger: &std::path::Path, at: i64, rows: &[String], transcript: &str| -> Decided {
            let (_host, opts) = hosted(transcript, &["relaunched"]);
            let mut m = Mock::new(vec![rows.to_vec()]);
            let mut s = session(&mut m, Some("@s-1".to_string()));
            s.set_clock(at, PDT);
            s.set_mono_standing();
            s.set_zone_lookup(test_zones);
            s.ledger = Ledger::open(Some(ledger), Some("@s-1"), &mut Vec::new());
            s.seed_model_switch(ledger);
            s.seed_wall_memory(open_wall(ledger, Some("@s-1")));
            decided(&mut s, rows, 101, None, &opts)
        };
    let waits_out = |d: Decided| d.is_some_and(|(until, _)| until >= RESET);
    let short_turn = |d: Decided, from: i64| d == Some((from + 120, Some(RULE_CONTINUE)));
    let mut wrong = Vec::new();
    for (n, case) in [
        "a person's prompt answered by the bucket, no point between",
        "control: the redraw",
        "the relaunch adopted at the next ask, a person's prompt answered by the bucket before it",
        "control: the relaunch adopted at the next ask, the redraw",
    ]
    .into_iter()
    .enumerate()
    {
        let answered = !case.ends_with("the redraw");
        let adopted_later = case.contains("the next ask");
        let answers: &[&'static str] = if adopted_later {
            &["wait:resume", "relaunched"]
        } else {
            &["relaunched"]
        };
        let (dir, ledger) = ledger_at(&format!("bucket-written-after-{n}"));
        let (host, opts) = hosted(&before, answers);
        let mut m = Mock::new(vec![armed.clone()]);
        let mut s = session(&mut m, Some("@s-1".to_string()));
        s.set_zone_lookup(test_zones);
        s.ledger = Ledger::open(Some(&ledger), Some("@s-1"), &mut Vec::new());
        s.set_turn_end_timing(TurnEndTiming {
            take_within: Duration::ZERO,
            ..TurnEndTiming::default()
        });
        s.set_clock(now, PDT);
        s.set_mono_standing();
        let p = point_at(&s, armed.clone(), 100);
        let a = s.turn_end_at_point(&p, None, &opts).expect("the fallback");
        assert!(
            matches!(&a, TurnEndAction::Restart { why: Restart::Model { to }, .. } if to == "opus"),
            "{case}: {a:?}"
        );
        s.turn_end_execute(&p, a, &opts, &[], &mut Alone)
            .expect("relaunched");
        assert_eq!(host.said("restart"), ["restart:model"], "{case}");
        if adopted_later {
            let screen = if answered {
                *host.tail.lock().unwrap() =
                    Some(tail("please run the integration tests", now + 60));
                met.clone()
            } else {
                redrawn.clone()
            };
            s.set_clock(now + 90, PDT);
            let p = point_at(&s, screen, 101);
            let a = s
                .turn_end_at_point(&p, None, &opts)
                .expect("the fallback again");
            assert!(
                matches!(
                    &a,
                    TurnEndAction::Restart {
                        why: Restart::Model { .. },
                        ..
                    }
                ),
                "{case}: {a:?}"
            );
            s.pass_wake_settle();
            s.turn_end_execute(&p, a, &opts, &[], &mut Alone)
                .expect("adopted");
            assert_eq!(
                host.said("restart"),
                ["restart:model", "restart:model"],
                "{case}"
            );
            assert!(
                open_model_switch(&ledger, Some("@s-1")).is_some_and(|o| o.made_unix == now),
                "{case}: the relaunch made at the run's first ask"
            );
            if answered {
                s.set_clock(now + 100, PDT);
                s.pass_wake_settle();
                let first = decided(&mut s, &met, 102, None, &opts);
                drop(s);
                let after_60 = tail("please run the integration tests", now + 60);
                let later = successor(&ledger, now + 120, &met, &after_60);
                let third = successor(&ledger, now + 600, &met, &after_60);
                let _ = std::fs::remove_dir_all(&dir);
                if ![first, later, third].into_iter().all(waits_out) {
                    wrong.push(format!(
                        "{case}: the first loop {first:?}, a successor {later:?}, one after \
                         that {third:?}"
                    ));
                }
                continue;
            }
        }
        let redraw_at = if adopted_later { now + 120 } else { now + 30 };
        s.set_clock(redraw_at, PDT);
        s.pass_wake_settle();
        let redraw = decided(&mut s, &redrawn, 102, None, &opts);
        if !answered {
            drop(s);
            let next = successor(&ledger, redraw_at + 30, &redrawn, &before);
            let _ = std::fs::remove_dir_all(&dir);
            let continues = |d: Decided| matches!(d, Some((at, Some(RULE_CONTINUE))) if at < RESET);
            let first_ok = if adopted_later {
                continues(redraw)
            } else {
                short_turn(redraw, redraw_at)
            };
            if !first_ok || !short_turn(next, redraw_at + 30) {
                wrong.push(format!(
                    "{case}: the first loop {redraw:?}, a successor {next:?}"
                ));
            }
            continue;
        }
        *host.tail.lock().unwrap() = Some(after.clone());
        let early = successor(&ledger, now + 88, &met, &after);
        s.set_clock(now + 90, PDT);
        let first = decided(&mut s, &met, 103, Some(Duration::from_secs(1)), &opts);
        drop(s);
        let later = successor(&ledger, now + 120, &met, &after);
        let third = successor(&ledger, now + 600, &met, &after);
        let _ = std::fs::remove_dir_all(&dir);
        if !short_turn(redraw, now + 30) || ![early, first, later, third].into_iter().all(waits_out)
        {
            wrong.push(format!(
                "{case}: the redraw {redraw:?}; a successor before the first loop's read \
                 {early:?}, the first loop {first:?}, a successor after {later:?}, one after \
                 that {third:?}"
            ));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

#[test]
fn a_limits_try_at_a_bucket_is_the_try_a_successor_backs_off_from() {
    use crate::supervise::approvals::{Ledger, OpenWall, Outcome, Row};
    use crate::supervise::policy::turn_end::{RULE_LIMIT_RESUME, decide_turn_end};
    use aterm_phase::WallKind;
    use aterm_phase::prompt::fixtures as fx;
    const RESET: i64 = 1_792_279_200;
    let now = RESET + 20 * 60;
    let bucket = WallKind::ModelBucket { consent: false };
    let opts = SuperviseOpts {
        policy: SupervisorConfig {
            model_fallback: None,
            ..owners_policy()
        },
        ..auto(30)
    };
    let done = "All done: the change is committed and pushed, and every suite passes.";
    let armed = fx::screen(fx::WALL_SONNET_BUCKET_AUTO_CONTINUE_ARMED_2_1_300);
    let wall = aterm_phase::read(Some("claude"), &armed, Some(2))
        .wall
        .expect("the bucket");
    let notice = armed
        .iter()
        .find(|r| r.contains("You've hit your"))
        .expect("the notice row")
        .clone();
    let replied = |reply: &str| -> Vec<String> {
        let mut out = Vec::new();
        for r in fx::screen(fx::WALL_SESSION_RESUMED_BY_ITSELF_MEASURED) {
            if [
                "carry on with the task",
                "You've hit your",
                "/upgrade to",
                "Usage limit reached",
            ]
            .iter()
            .any(|g| r.contains(g))
            {
                continue;
            }
            if r.contains("(reply 3)") {
                out.push(format!("⏺ {reply}"));
                continue;
            }
            out.push(r);
        }
        out
    };
    let met_again = {
        let mut r = rows(&[
            &format!("⏺ {done}"),
            "",
            &format!("❯ {DONE_CHECK}"),
            &notice,
            "",
        ]);
        r.extend(composer("  ⏵⏵ auto mode on (shift+tab to cycle)"));
        r
    };
    let typed_ts = |ledger: &Path| -> Option<i64> {
        std::fs::read_to_string(ledger)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| aterm_json::from_str::<aterm_json::Value>(l).ok())
            .filter(|v| v.get("decision").and_then(aterm_json::Value::as_str) == Some("typed"))
            .filter_map(|v| v.get("ts").and_then(aterm_json::Value::as_i64))
            .next_back()
    };
    let successor = |ledger: &Path, act_ms: i64| {
        let mut m = Mock::new(vec![met_again.clone()]);
        let mut s = session(&mut m, Some("@s-1".to_string()));
        s.set_clock(now + 60, PDT);
        s.set_zone_lookup(test_zones);
        s.ledger = Ledger::open(Some(ledger), Some("@s-1"), &mut Vec::new());
        let base = Instant::now();
        let open = s.seed_from_ledger(ledger, act_ms + 60_000, false);
        s.seed_wall_memory(open);
        let point = point_at(&s, met_again.clone(), 101);
        let first = s
            .turn_end_at_point(&point, None, &opts)
            .unwrap_or_else(|e| panic!("{e:?}"));
        let (first, slept) = past_take_window(&mut s, first, &met_again, now + 60, &opts);
        let (after, next) = match &first {
            TurnEndAction::WaitUntil { until, .. } => {
                let r = s.turn_end_reading(&point.screen, None, &opts);
                (
                    Some(whole_secs(until.saturating_duration_since(base) + slept)),
                    decide_turn_end(&s.turn_end, &r, &opts.policy, *until).rule_id(),
                )
            }
            other => (None, other.rule_id()),
        };
        (after, next, s.turn_end.wall_attempts(bucket))
    };
    let timing = TurnEndTiming::default();
    let rung = timing.limit_backoff.first().copied().expect("a ladder") + timing.take_within;
    let mut wrong = Vec::new();
    for (tag, reply, want) in [
        ("bucket-try-check", done, RULE_DONE_CHECK),
        (
            "bucket-try-resume",
            "Stage 2 of 5 is in; stage 3 is next.",
            RULE_LIMIT_RESUME,
        ),
    ] {
        let (dir, ledger) = ledger_at(tag);
        let screen = replied(reply);
        let mut m = Mock::new(vec![screen.clone()]);
        let mut s = session(&mut m, Some("@s-1".to_string()));
        s.set_clock(now, PDT);
        s.set_zone_lookup(test_zones);
        s.ledger = Ledger::open(Some(&ledger), Some("@s-1"), &mut Vec::new());
        s.seed_wall_memory(Some(OpenWall {
            conversation: crate::supervise::policy::turn_end::Conversation::Hostless,
            program: None,
            at_ms: (now - 3600) * 1000,
            message: wall.message.clone(),
            reset: wall.reset.clone(),
            reset_unix: Some(RESET),
            tap_unix: None,
            kind: Some(wall.kind),
            judged_ms: None,
            retry_ms: None,
        }));
        let point = point_at(&s, screen, 101);
        let action = s
            .turn_end_at_point(&point, None, &opts)
            .unwrap_or_else(|e| panic!("{e:?}"));
        s.turn_end_execute(&point, action.clone(), &opts, &[], &mut Alone)
            .unwrap_or_else(|e| panic!("{e:?}"));
        let first = (action.rule_id(), s.turn_end.wall_attempts(bucket));
        drop(s);
        assert_eq!(first, (Some(want), Some(1)), "{tag}: the bucket's try");
        let act_ms = typed_ts(&ledger).expect("the act's row");
        let got = successor(&ledger, act_ms);
        if got != (Some(rung), Some(RULE_LIMIT_RESUME), Some(1)) {
            wrong.push(format!("{tag}: {got:?}"));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
    let (dir, ledger) = ledger_at("bucket-try-window");
    let act_ms = (now - 60) * 1000;
    let row = Row {
        rule_id: RULE_LIMIT_RESUME,
        outcome: Outcome::Typed,
        command: "keep going",
        reason: "the turn-end policy",
        box_seq: 90,
    }
    .to_json(act_ms, Some("s-1"));
    std::fs::write(&ledger, format!("{row}\n")).expect("the ledger");
    assert_eq!(
        successor(&ledger, act_ms),
        (None, Some(RULE_LIMIT_RESUME), Some(0)),
        "a usage window's try is none at the bucket"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_move_seeded_from_the_ledger_is_answered_once_across_loops() {
    use crate::supervise::approvals::{
        ACCOUNT_MOVE_COMMAND, Outcome, Row, account_answer_reason, account_moves,
        account_switch_reason,
    };
    use crate::supervise::policy::turn_end::{AccountAnswer, RULE_LIMIT_RESUME};
    let now_ms = crate::supervise::journal::unix_ms();
    let row = |rule: &str, outcome: Outcome, command: &str, reason: &str, ago_s: i64| {
        Row {
            rule_id: rule,
            outcome,
            command,
            reason,
            box_seq: 90,
        }
        .to_json(now_ms - ago_s * 1_000, None)
    };
    let to = format!("{ACCOUNT_MOVE_COMMAND}work-2");
    let made = row(
        RULE_ACCOUNT,
        Outcome::Typed,
        &to,
        &account_switch_reason("work-2", AccountAt::Wall),
        120,
    );
    let resumed = row(
        RULE_LIMIT_RESUME,
        Outcome::Typed,
        "keep going",
        "the turn-end policy",
        110,
    );
    let served = || vec![busy_screen(), ended("Carried on: stage 3 is done.")];
    let run = |tag: &str, rows: &[String], screens: Vec<Vec<String>>| {
        let (dir, ledger) = ledger_at(tag);
        std::fs::write(&ledger, rows.join("\n") + "\n").expect("the ledger");
        let host = AccountHost::scripted(&[None], &["adopted"]);
        let _ = account_run(&format!("te-{tag}"), screens, &host, &ledger);
        let after = account_moves(&ledger, None);
        let text = std::fs::read_to_string(&ledger).expect("the ledger");
        let _ = std::fs::remove_dir_all(&dir);
        (host.all(), after, text)
    };
    let (said, after, text) = run(
        "account-seeded-served",
        &[made.clone(), resumed.clone()],
        served(),
    );
    assert_eq!(said.first().map(String::as_str), Some("served"), "{said:?}");
    assert_eq!(said.iter().filter(|s| *s == "served").count(), 1);
    assert_eq!(after.open, None, "served: the move is behind the session");
    assert!(
        text.contains("\"rule_id\":\"account-rotate@v1\",\"decision\":\"waiting\"")
            && text.contains("(account move answered: to=work-2 by=served)"),
        "{text}"
    );
    let answered = row(
        RULE_ACCOUNT,
        Outcome::Waiting,
        &to,
        &account_answer_reason("work-2", AccountAnswer::Served),
        100,
    );
    let (said, ..) = run(
        "account-seeded-served-again",
        &[made.clone(), resumed.clone(), answered],
        served(),
    );
    assert!(!said.iter().any(|s| s == "served"), "{said:?}");
    let lost = || {
        let mut login = rows(&[
            "⏺ Carrying on.",
            "  ⎿  Not logged in · Please run /login",
            "",
        ]);
        login.extend(composer("  ? for shortcuts"));
        vec![busy_screen(), login]
    };
    let told = row(
        RULE_ACCOUNT,
        Outcome::Waiting,
        &to,
        &account_answer_reason(
            "work-2",
            AccountAnswer::Bad(crate::supervise::policy::turn_end::AccountBad::SignedOut),
        ),
        100,
    );
    let (said, after, _) = run("account-seeded-told", &[made.clone(), told], lost());
    assert!(!said.iter().any(|s| s.starts_with("answered")), "{said:?}");
    assert!(after.open.is_some_and(|open| open.told));
    let (said, after, _) = run(
        "account-seeded-untold",
        &[made.clone(), resumed.clone()],
        lost(),
    );
    assert_eq!(
        said.first().map(String::as_str),
        Some("answered:signed-out"),
        "{said:?}"
    );
    assert!(after.open.is_some_and(|open| open.told), "and written down");
    let (said, after, text) = run("account-seeded-unasked", &[made], lost());
    assert!(!said.iter().any(|s| s.starts_with("answered")), "{said:?}");
    assert!(after.open.is_some_and(|open| !open.told));
    assert_eq!(
        text.matches("\"rule_id\":\"usage-resume@v1\",\"decision\":\"typed\"")
            .count(),
        1,
        "the wall's resume, once: {text}"
    );
}

#[test]
fn the_owners_clearing_continues_a_session_at_its_wall_once() {
    use crate::supervise::policy::turn_end::RULE_LIMIT_RESUME;
    let run = |advice: Option<AccountAdvice>| {
        let host = AccountHost::scripted(&[advice], &["adopted"]);
        let opts = account_opts(&host);
        let mut m = Mock::new(vec![limited_screen()]);
        let attempts = {
            let (mut s, wall) = account_session(&mut m);
            let first = s
                .turn_end_at_point(&wall, Some(Duration::from_secs(180)), &opts)
                .expect("a decision");
            s.turn_end_execute(&wall, first.clone(), &opts, &[], &mut Alone)
                .expect("carried out");
            let again = s
                .turn_end_at_point(&wall, Some(Duration::from_secs(3)), &opts)
                .expect("a decision");
            assert!(
                matches!(again, TurnEndAction::WaitUntil { .. }),
                "{again:?}"
            );
            s.turn_end_execute(&wall, again, &opts, &[], &mut Alone)
                .expect("carried out");
            (
                first,
                s.turn_end
                    .wall_attempts(aterm_phase::WallKind::UsageSession),
            )
        };
        let typed = m.requests.iter().filter(|r| r.contains("keep going"));
        (attempts, typed.count(), host.all())
    };
    let ((first, attempts), turns, said) = run(Some(AccountAdvice::Resume {
        cleared_at: ACCOUNT_NOW - 30,
    }));
    assert_eq!(
        first,
        TurnEndAction::Type {
            text: "keep going".to_string(),
            rule_id: RULE_LIMIT_RESUME,
        }
    );
    assert_eq!(
        (attempts, turns),
        (Some(1), 1),
        "one continuation: {said:?}"
    );
    assert_eq!(
        said,
        ["account:wall:usage-session", "account:wall:usage-session"],
        "asked at each decision, and no relaunch"
    );
    let ((first, attempts), turns, _) = run(None);
    assert!(
        matches!(first, TurnEndAction::WaitUntil { .. }),
        "{first:?}"
    );
    assert_eq!((attempts, turns), (Some(0), 0));
}

#[test]
fn the_stamp_a_loop_first_reads_is_no_news_to_its_latch() {
    let host = AccountHost::moving("work-2", "refused:line");
    host.stamp.store(3, Ordering::SeqCst);
    let opts = account_opts(&host);
    let mut m = Mock::new(vec![limited_screen()]);
    let (mut s, wall) = account_session(&mut m);
    s.turn_end.seed_account_unmade(Unmade::Again, None);
    let action = s
        .turn_end_at_point(&wall, Some(Duration::from_secs(180)), &opts)
        .expect("a decision");
    assert!(
        matches!(action, TurnEndAction::WaitUntil { .. }),
        "{action:?}"
    );
    assert!(
        s.turn_end.account_refused(),
        "the latch the loop before left stands"
    );
    assert!(host.all().is_empty(), "{:?}", host.all());
    s.turn_end_execute(&wall, action, &opts, &[], &mut Alone)
        .expect("the wait is set");
    assert!(!s.account_edge(&opts), "nothing new since the first read");
    host.stamp.store(4, Ordering::SeqCst);
    assert!(s.account_edge(&opts));
    let again = s.turn_end_at_point(&wall, None, &opts).expect("a decision");
    assert!(matches!(again, TurnEndAction::Restart { .. }), "{again:?}");
    assert_eq!(host.said("account"), ["account:wall:usage-session"]);
}

#[test]
fn a_loop_that_takes_up_a_move_made_at_a_wall_carries_it_on_past_the_notice() {
    use crate::supervise::approvals::{ACCOUNT_MOVE_COMMAND, Outcome, Row, account_switch_reason};
    let now_ms = crate::supervise::journal::unix_ms();
    let made = |at: AccountAt| {
        Row {
            rule_id: RULE_ACCOUNT,
            outcome: Outcome::Typed,
            command: &format!("{ACCOUNT_MOVE_COMMAND}work-2"),
            reason: &account_switch_reason("work-2", at),
            box_seq: 90,
        }
        .to_json(now_ms - 120_000, None)
    };
    let run = |tag: &str, rows: &[String], carry: bool| {
        let (dir, ledger) = ledger_at(tag);
        if !rows.is_empty() {
            std::fs::write(&ledger, rows.join("\n") + "\n").expect("the ledger");
        }
        let host = AccountHost::scripted(&[None], &["adopted"]);
        host.carry.store(carry, Ordering::SeqCst);
        let (lines, m, journal) =
            account_run(&format!("te-{tag}"), vec![limited_screen()], &host, &ledger);
        let _ = std::fs::remove_dir_all(&dir);
        let typed = m.requests.iter().filter(|r| r.contains("keep going"));
        (
            host.all(),
            typed.count(),
            format!("{lines:#?}\n{journal:#?}"),
        )
    };
    let (said, typed, log) = run("account-taken-up-carry", &[made(AccountAt::Wall)], true);
    assert_eq!(said, ["carry-on"], "{log}");
    assert_eq!(typed, 1, "the wall's resume, once: {log}");
    assert!(log.contains("carry-on step=done:resumed"), "{log}");
    assert!(
        log.contains(&format!(
            "rule={}",
            crate::supervise::policy::turn_end::RULE_LIMIT_RESUME
        )),
        "{log}"
    );
    let (said, typed, log) = run("account-taken-up-resume", &[made(AccountAt::Wall)], false);
    assert!(said.is_empty(), "{said:?}\n{log}");
    assert_eq!(typed, 1, "{log}");
    let (said, typed, log) = run("account-taken-up-none", &[], true);
    assert_eq!(said, ["account:wall:usage-session"], "{log}");
    assert_eq!(typed, 0, "{log}");
    let (said, typed, log) = run("account-taken-up-ahead", &[made(AccountAt::Ahead)], true);
    assert_eq!(said, ["account:wall:usage-session"], "{log}");
    assert_eq!(typed, 0, "{log}");
}

#[test]
fn a_host_that_answers_a_short_wait_is_asked_again_when_it_runs_out() {
    let decided = |first: Option<AccountAdvice>| {
        let host = AccountHost::scripted(
            &[
                first,
                Some(AccountAdvice::Move {
                    to: "work-2".to_string(),
                }),
            ],
            &["adopted"],
        );
        let opts = account_opts(&host);
        let mut m = Mock::new(vec![limited_screen()]);
        let (mut s, wall) = account_session(&mut m);
        let asked_at = Instant::now();
        let action = s
            .turn_end_at_point(&wall, Some(Duration::from_secs(180)), &opts)
            .expect("a decision");
        let TurnEndAction::WaitUntil { until, .. } = &action else {
            panic!("{action:?}")
        };
        let waits = until.saturating_duration_since(asked_at);
        s.turn_end_execute(&wall, action, &opts, &[], &mut Alone)
            .expect("the wait is set");
        assert!(!s.turn_end.account_refused(), "a wait latches nothing");
        assert!(host.said("restart").is_empty(), "{:?}", host.all());
        let again = s.turn_end_at_point(&wall, None, &opts).expect("a decision");
        (waits, again, host.said("account").len())
    };
    let soon = Instant::now() + Duration::from_secs(5);
    let (waits, again, asked) = decided(Some(AccountAdvice::Wait { until: soon }));
    assert!(
        (Duration::from_secs(60)..Duration::from_secs(90)).contains(&waits),
        "cut to the host's moment, never nearer than a minute: {waits:?}"
    );
    assert_eq!(asked, 2, "asked again at the next decision");
    assert!(
        matches!(&again, TurnEndAction::Restart { why: Restart::Account { to, .. }, .. } if to == "work-2"),
        "{again:?}"
    );
    let (waits, ..) = decided(None);
    assert!(waits > Duration::from_secs(3600), "{waits:?}");
}

#[test]
fn a_clearing_the_loop_before_answered_is_not_answered_again() {
    use crate::supervise::approvals::{Outcome, Row, act_reason};
    use crate::supervise::policy::turn_end::{LimitTry, RULE_LIMIT_RESUME};
    let now_ms = crate::supervise::journal::unix_ms();
    let window = LimitTry {
        bucket: false,
        in_place: false,
        attempts: 1,
        entitlement: false,
    };
    let row = |rule_id: &str, command: &str, reason: &str| {
        Row {
            rule_id,
            outcome: Outcome::Typed,
            command,
            reason,
            box_seq: 90,
        }
        .to_json(now_ms - 120_000, None)
    };
    let wall = aterm_phase::read(Some("claude"), &limited_screen(), Some(2))
        .wall
        .expect("the limit");
    let read = Row {
        rule_id: crate::supervise::approvals::RULE_WALL_WAIT,
        outcome: Outcome::Waiting,
        command: "",
        reason: &crate::supervise::approvals::wall_wait_reason(
            None,
            wall.reset.as_deref(),
            wall.kind,
            &wall.message,
        ),
        box_seq: 91,
    }
    .to_json(now_ms - 119_000, None);
    for (what, resumed) in [
        (
            "resumed",
            row(RULE_LIMIT_RESUME, "keep going", &act_reason(Some(window))),
        ),
        (
            "resumed-unmarked",
            row(RULE_LIMIT_RESUME, "keep going", "the turn-end policy"),
        ),
        (
            "checked",
            row(
                RULE_DONE_CHECK,
                DONE_CHECK,
                &act_reason(Some(LimitTry {
                    in_place: true,
                    ..window
                })),
            ),
        ),
    ] {
        let typed_again = |tag: &str, cleared_ago_s: i64| {
            let (dir, ledger) = ledger_at(&format!("{tag}-{what}"));
            std::fs::write(&ledger, format!("{resumed}\n{read}\n")).expect("the ledger");
            let host = AccountHost::scripted(
                &[Some(AccountAdvice::Resume {
                    cleared_at: now_ms / 1000 - cleared_ago_s,
                })],
                &["adopted"],
            );
            let (lines, m, journal) = account_run(
                &format!("te-{tag}-{what}"),
                vec![limited_screen()],
                &host,
                &ledger,
            );
            let _ = std::fs::remove_dir_all(&dir);
            assert_eq!(
                host.said("account"),
                ["account:wall:usage-session"],
                "{what}: {lines:#?}\n{journal:#?}"
            );
            m.requests
                .iter()
                .filter(|r| r.contains("keep going"))
                .count()
        };
        assert_eq!(
            typed_again("account-cleared-before", 300),
            0,
            "{what}: the loop before answered that clearing"
        );
        assert_eq!(typed_again("account-cleared-after", 60), 1, "{what}");
    }
}

use aterm_phase::prompt::fixtures as panel_fx;

const PROBE_NOW: i64 = 1_790_914_800;

const PROBE_SAID: &str = "Stage 3 done; the suite is green.";

fn probe_world(frames: &[&str], host: &ProbeHost) -> Mock {
    let mut m = probe_mock(vec![busy_screen(), ended(PROBE_SAID)], frames, host);
    world(&mut m)
        .then
        .push_back(vec![busy_screen(), ended(STOP)]);
    m
}

fn probe_waits<C: Ctl>(s: &mut Session<'_, C>) {
    let ms = Duration::from_millis;
    s.set_probe_waits(ms(60), Duration::from_secs(30), ms(10), ms(100));
}

fn probe_run(
    tag: &str,
    m: &mut Mock,
    host: &Arc<ProbeHost>,
    ledger: &Path,
    policy: SupervisorConfig,
    setup: impl FnOnce(&mut Watched<'_, '_>),
) -> (Vec<String>, Vec<String>) {
    let (dir, path) = journal_file(tag);
    let opts = SuperviseOpts {
        idle_host: Some(Arc::clone(host) as Arc<dyn IdleHost>),
        journal: Some(path.clone()),
        policy,
        ..hosted(60)
    };
    let (lines, _) = watch_lines_with(m, &opts, |s| {
        s.set_clock(PROBE_NOW, PDT);
        s.set_approval_ledger(Some(ledger.to_path_buf()));
        probe_waits(s);
        setup(s);
    });
    let (records, _) = journal_records(&path);
    let _ = std::fs::remove_dir_all(&dir);
    (lines, records.into_iter().map(|r| r.line).collect())
}

const PROBE_STEPS: usize = 100;

fn probe_run_for(
    tag: &str,
    m: &mut Mock,
    host: &Arc<ProbeHost>,
    ledger: &Path,
    policy: SupervisorConfig,
    setup: impl FnOnce(&mut Watched<'_, '_>),
) -> (Vec<String>, Vec<String>) {
    m.exit_at_request = Some(m.requests.len() + PROBE_STEPS);
    let (lines, journal) = probe_run(tag, m, host, ledger, policy, setup);
    m.exit_at_request = None;
    assert!(
        lines
            .last()
            .is_some_and(|l| l.starts_with("EXIT session gone")),
        "{tag}: the run ends on its steps, never on its minute: {lines:#?}"
    );
    (lines, journal)
}

fn probe_policy(human_grace_s: u32) -> SupervisorConfig {
    SupervisorConfig {
        human_grace_s,
        ..hosted(60).policy
    }
}

fn probe_rows(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter(|l| l.contains("\"rule_id\":\"usage-probe@v1\""))
        .map(|l| {
            let field = |name: &str| {
                l.split(&format!("\"{name}\":\""))
                    .nth(1)
                    .and_then(|rest| rest.split('"').next())
                    .unwrap_or("")
                    .to_string()
            };
            format!(
                "{} {} {}",
                field("decision"),
                field("command"),
                field("reason")
            )
        })
        .collect()
}

fn gates_said(lines: &[String]) -> Vec<String> {
    lines
        .iter()
        .filter_map(|l| l.strip_prefix("SKIPPED seq="))
        .filter_map(|rest| {
            let (seq, rest) = rest.split_once(' ')?;
            let gate = rest.strip_prefix("rule=usage-probe@v1 gate=")?;
            Some(format!("{seq} {gate}"))
        })
        .collect()
}

fn gate_words(lines: &[String]) -> Vec<String> {
    gates_said(lines)
        .iter()
        .filter_map(|g| g.split_once(' '))
        .map(|(_, word)| word.to_string())
        .collect()
}

fn other_skips(lines: &[String]) -> Vec<&String> {
    lines
        .iter()
        .filter(|l| l.starts_with("SKIPPED ") && !l.contains(" gate="))
        .collect()
}

#[test]
fn a_claimed_probe_types_usage_samples_the_settled_panel_and_closes_it() {
    let (dir, ledger) = ledger_at("probe-settled");
    let host = ProbeHost::probing(true);
    let mut m = probe_world(
        &[panel_fx::USAGE_REFRESHING, panel_fx::USAGE_SETTLED],
        &host,
    );
    let (lines, journal) = probe_run(
        "te-probe-settled",
        &mut m,
        &host,
        &ledger,
        probe_policy(120),
        |_| {},
    );
    assert_eq!(
        m.writes(),
        ["text:/usage", "enter", "esc", "text:keep going", "enter"],
        "{:#?}",
        m.requests
    );
    let wrote = |what: &str| {
        m.requests
            .iter()
            .find(|r| matches!(verb_of(r), "send" | "key") && r.ends_with(what))
            .cloned()
            .unwrap_or_default()
    };
    assert_eq!(
        wrote("-- /usage"),
        "send if-human=0 if-gen=1.102 if=^❯\\s*$ -- /usage"
    );
    assert_eq!(
        wrote("usage\\s*$ enter"),
        "key if-human=0 if-gen=1.104 if=^❯\\x20/usage\\s*$ enter"
    );
    assert_eq!(
        wrote(" esc"),
        "key if-human=0 if=^...Settings..Status...Config...Usage esc",
        "the Escape: the person fence and the panel's header row, no generation"
    );
    assert_eq!(count(&m, "turn"), 0, "never the unfenced fallback");
    assert_eq!(
        host.said(""),
        [
            "turn-ran",
            "account:turn-end",
            "probe:claimed",
            "probed:ok",
            "account:turn-end",
            "turn-ran",
            "account:turn-end",
        ],
        "no turn ran between the probe and the point decided again: {lines:#?}"
    );
    let samples = host.samples.lock().unwrap().clone();
    assert_eq!(samples.len(), 1);
    let (sample, pan, hand) = &samples[0];
    assert_eq!(
        (sample.session_pct, sample.week_pct, sample.at),
        (Some(1), Some(10), PROBE_NOW),
        "the settled frame's figures, never the refreshing one's 9 %"
    );
    assert_eq!(
        (sample.session_reset, sample.week_reset),
        (Some(PROBE_NOW + 16_800), Some(PROBE_NOW + 560_400)),
        "2am and Oct 8 at 9am, Pacific"
    );
    assert_eq!((*pan, *hand), (2, false), "handed over at a settled panel");
    let kinds: Vec<&str> = lines
        .iter()
        .filter_map(|l| l.split_whitespace().next())
        .collect();
    assert_eq!(
        kinds,
        ["EVENT", "PROBED", "CONTINUED", "EVENT", "SKIPPED", "EXIT"],
        "{lines:#?}"
    );
    assert_eq!(
        lines[1],
        "PROBED seq=102 rule=usage-probe@v1 /usage session=1% week=10%"
    );
    assert_eq!(
        gate_words(&journal),
        ["escalated"],
        "the later point's, which is a person's: {journal:#?}"
    );
    assert!(
        !gates_said(&journal).iter().any(|g| g.starts_with("102 ")),
        "{journal:#?}"
    );
    assert_eq!(gates_said(&lines), gates_said(&journal), "said, so printed");
    assert_eq!(
        probe_rows(&ledger),
        ["typed /usage the accounts probe", "waiting /usage closed"]
    );
    assert_eq!(
        crate::supervise::approvals::typed_at(&ledger, None).len(),
        1,
        "the budget's reader counts the continuation alone"
    );
    assert!(!crate::supervise::approvals::probe_owed(
        &std::fs::read_to_string(&ledger).unwrap_or_default(),
        None
    ));
    assert!(
        !journal
            .iter()
            .any(|l| l.starts_with("WAITING ") || l.starts_with("HELD ")),
        "{journal:#?}"
    );
    assert!(other_skips(&journal).is_empty(), "{journal:#?}");
    assert_eq!(usage_probe_violations(&m, &host), Vec::<String>::new());
    let _ = std::fs::remove_dir_all(&dir);

    let run = |host: Option<&Arc<ProbeHost>>, tag: &str| {
        let (dir, ledger) = ledger_at(tag);
        let none = ProbeHost::probing(true);
        let mut m = probe_world(&[panel_fx::USAGE_SETTLED], host.unwrap_or(&none));
        let opts = SuperviseOpts {
            idle_host: host.map(|h| Arc::clone(h) as Arc<dyn IdleHost>),
            ..hosted(60)
        };
        let _ = watch_lines_with(&mut m, &opts, |s| {
            s.set_clock(PROBE_NOW, PDT);
            s.set_approval_ledger(Some(ledger.clone()));
        });
        let _ = std::fs::remove_dir_all(&dir);
        m.requests
            .iter()
            .map(|r| match r.rsplit_once(" timeout ") {
                Some((head, ms)) if ms.bytes().all(|b| b.is_ascii_digit()) => head.to_string(),
                _ => r.clone(),
            })
            .collect::<Vec<String>>()
    };
    let rosterless = ProbeHost::scripted(&[None], &[true]);
    let with_host = run(Some(&rosterless), "probe-no-roster");
    assert!(rosterless.said("probe").is_empty(), "never asked to claim");
    assert_eq!(
        rosterless.said("account"),
        ["account:turn-end", "account:turn-end"]
    );
    let probe_modes = |at: usize, r: &String| {
        r == "modes"
            && with_host
                .get(at + 1)
                .is_none_or(|next| !next.starts_with("offscreen"))
    };
    assert!(
        !with_host
            .iter()
            .enumerate()
            .any(|(at, r)| probe_modes(at, r) || r.contains("/usage") || r.ends_with(" esc")),
        "{with_host:#?}"
    );
    assert_eq!(with_host, run(None, "probe-no-host"));
}

#[test]
fn the_windows_host_types_the_probes_usage_under_its_hand() {
    use crate::harness::upgrade_drive::{
        hand_give_back_here_request_as, hand_take_here_request_as,
    };
    let run = |hand: bool| {
        let (dir, ledger) = ledger_at(&format!("probe-hand-{hand}"));
        let host = ProbeHost::probing(true);
        let mut m = probe_world(
            &[panel_fx::USAGE_REFRESHING, panel_fx::USAGE_SETTLED],
            &host,
        );
        let acquired = || {
            ok(
                "OK lease acquired holder=aterm-harness@1 ttl_ms=60000 expires_in_ms=60000 \
                hard=1\n",
            )
        };
        let released = || ok("OK lease released\n");
        m.verb_replies.insert(
            "lease",
            VecDeque::from([acquired(), released(), acquired(), released()]),
        );
        let _ = probe_run(
            &format!("te-probe-hand-{hand}"),
            &mut m,
            &host,
            &ledger,
            probe_policy(120),
            |s| {
                s.set_supervisor_name(Some(HAND_HOLDER.to_string()));
                s.type_under_hand(hand);
            },
        );
        let _ = std::fs::remove_dir_all(&dir);
        let (take, back) = (
            hand_take_here_request_as(HAND_HOLDER),
            hand_give_back_here_request_as(HAND_HOLDER),
        );
        m.requests
            .iter()
            .filter_map(|r| match verb_of(r) {
                "lease" if r.ends_with(&take) => Some("take".to_string()),
                "lease" if r.ends_with(&back) => Some("back".to_string()),
                "lease" => Some(r.clone()),
                "key" => r.rsplit_once(' ').map(|(_, k)| k.to_string()),
                "send" => r.split_once(" -- ").map(|(_, t)| format!("text:{t}")),
                _ => None,
            })
            .collect::<Vec<String>>()
    };
    assert_eq!(
        run(true),
        [
            "take",
            "text:/usage",
            "enter",
            "back",
            "esc",
            "take",
            "text:keep going",
            "enter",
            "back"
        ]
    );
    assert_eq!(
        run(false),
        ["text:/usage", "enter", "esc", "text:keep going", "enter"],
        "no hand asked for"
    );
}

/// What a step of the loop said (printed, journaled, and in a host's log) and what it only
/// noted (the journal alone).
#[derive(Default)]
struct Heard {
    said: Vec<String>,
    noted: Vec<String>,
}

impl Review for Heard {
    fn approved(&mut self, _seq: u64, _command: &str) -> Result<(), String> {
        Ok(())
    }
    fn review(&mut self, _turn: &Turn) -> Result<bool, String> {
        Ok(true)
    }
    fn say(&mut self, line: &str) -> Result<(), String> {
        self.said.push(line.to_string());
        Ok(())
    }
    fn note(&mut self, line: &str) {
        self.noted.push(line.to_string());
    }
    fn unattended(&self) -> bool {
        true
    }
}

type Shut = fn(&mut Session<'_, Mock>, &Turn, &ProbeHost, &mut TurnEndAction);

fn due_probe_at_a_gate(
    tag: &str,
    shut: Option<Shut>,
) -> (
    Result<Probed, Fail>,
    Vec<String>,
    Vec<&'static str>,
    Vec<String>,
) {
    let (dir, ledger) = ledger_at(tag);
    let host = ProbeHost::probing(true);
    let mut m = probe_mock(
        vec![ended(PROBE_SAID)],
        &[panel_fx::USAGE_REFRESHING, panel_fx::USAGE_SETTLED],
        &host,
    );
    let opts = SuperviseOpts {
        idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
        policy: SupervisorConfig {
            continue_policy: false,
            ..hosted(30).policy
        },
        ..hosted(30)
    };
    let mut s = session(&mut m, None);
    s.set_clock(PROBE_NOW, PDT);
    s.set_zone_lookup(test_zones);
    s.set_approval_ledger(Some(ledger.clone()));
    s.ledger = crate::supervise::approvals::Ledger::open(Some(&ledger), None, &mut Vec::new());
    probe_waits(&mut s);
    let screen = s.screen().expect("the point");
    let point = s.turn_of(screen);
    let mut decided = s
        .turn_end_at_point(&point, Some(Duration::from_secs(182)), &opts)
        .unwrap_or_else(|e| panic!("{e:?}"));
    assert_eq!(host.said("account"), ["account:turn-end"], "advised");
    s.turn_live = true;
    if let Some(shut) = shut {
        shut(&mut s, &point, &host, &mut decided);
    }
    let mut heard = Heard::default();
    let first = s.usage_probe(&point, &decided, &opts, &mut heard);
    if first == Ok(Probed::No) {
        assert_eq!(
            s.usage_probe(&point, &decided, &opts, &mut heard),
            Ok(Probed::No),
            "{tag}"
        );
    }
    s.turn_end_now(&point, &opts, &[], &mut heard)
        .unwrap_or_else(|e| panic!("{tag}: {e:?}"));
    drop(s);
    let _ = std::fs::remove_dir_all(&dir);
    (first, heard.said, probe_writes(&m), host.said("probe"))
}

fn a_night_slept(s: &mut Session<'_, Mock>, _: &Turn, _: &ProbeHost, _: &mut TurnEndAction) {
    s.set_clock(PROBE_NOW + 8 * 3600, PDT);
    let _ = s.call(&["status"]).expect("a read at the wake");
}

#[test]
fn a_due_probe_that_is_not_run_says_which_gate_shut_once_a_point() {
    let gates: [(&str, Shut); 16] = [
        ("watching", |s, _, _, _| {
            s.claim = claim::Claim::Behind {
                holder: "another".to_string(),
                tried: Instant::now(),
            };
        }),
        ("no-ledger", |s, _, _, _| s.set_approval_ledger(None)),
        ("probes-off", |s, _, _, _| s.probe_off = true),
        ("panel-owed", |s, _, _, _| {
            s.probe = UsageProbe::Owed {
                own: true,
                since: Instant::now(),
            };
        }),
        ("not-live", |s, _, _, _| s.turn_live = false),
        ("escalated", |_, _, _, decided| {
            *decided = TurnEndAction::Escalate {
                reason: "a person's".to_string(),
            };
        }),
        ("fold-latch", |s, point, _, _| {
            s.probe_fold = Some((point.screen.cols, point.screen.pane_rows));
        }),
        ("host-wants", |_, _, host, _| {
            host.wanting.store(true, Ordering::SeqCst);
        }),
        ("stalled", |s, _, _, _| s.stall_suspect = true),
        ("wake-settle", a_night_slept),
        ("no-fence", |s, _, _, _| s.caps.gen_fence = Some(false)),
        ("not-claude", |s, _, _, _| s.ctl.program = "zsh"),
        ("person", |s, _, _, _| s.ctl.human_ms = Some(5_000)),
        ("not-fullscreen", |s, _, _, _| {
            s.ctl.alt_screen = Some(false);
        }),
        ("not-fullscreen", |s, _, _, _| s.ctl.alt_screen = None),
        ("claim host=not-due", |_, _, host, _| {
            *host.claims.lock().unwrap() = VecDeque::from([false]);
        }),
    ];
    for (word, shut) in gates {
        let tag = format!("probe-gate-{}", word.replace([' ', '='], "-"));
        let (did, said, wrote, claims) = due_probe_at_a_gate(&tag, Some(shut));
        assert_eq!(did, Ok(Probed::No), "{word}");
        assert_eq!(
            said,
            [format!("SKIPPED seq=101 rule=usage-probe@v1 gate={word}")],
            "{word}: one line, once"
        );
        assert_eq!(wrote, Vec::<&str>::new(), "{word}: nothing written");
        let asked = if word.starts_with("claim") { 2 } else { 0 };
        assert_eq!(claims, vec!["probe:declined"; asked], "{word}");
    }
    let (did, said, wrote, claims) = due_probe_at_a_gate("probe-gate-none", None);
    assert_eq!(did, Ok(Probed::Done));
    assert_eq!(
        said,
        ["PROBED seq=101 rule=usage-probe@v1 /usage session=1% week=10%"]
    );
    assert_eq!(wrote, ["/usage", "esc"]);
    assert_eq!(claims, ["probe:claimed", "probed:ok"]);
}

#[test]
fn the_loop_says_once_why_a_due_probe_was_not_run() {
    let idle_policy = || SupervisorConfig {
        continue_policy: false,
        ..probe_policy(120)
    };
    let run = |tag: &str, screens: Vec<Vec<String>>, host: &Arc<ProbeHost>| {
        let (dir, ledger) = ledger_at(tag);
        let mut m = probe_mock(screens, &[panel_fx::USAGE_SETTLED], host);
        m.cursor_on_caret = tag.ends_with("-offered");
        let (lines, journal) = probe_run(
            &format!("te-{tag}"),
            &mut m,
            host,
            &ledger,
            idle_policy(),
            |_| {},
        );
        let _ = std::fs::remove_dir_all(&dir);
        (lines, journal, probe_writes(&m))
    };

    let host = ProbeHost::probing(true);
    let (lines, journal, wrote) = run("probe-loop-not-live", vec![ended(PROBE_SAID)], &host);
    assert_eq!(gates_said(&lines), ["101 not-live"], "{lines:#?}");
    assert_eq!(gates_said(&journal), gates_said(&lines), "{journal:#?}");
    let at = lines
        .iter()
        .position(|l| l.starts_with("SKIPPED "))
        .expect("said");
    assert!(
        lines[at - 1].starts_with("EVENT idle seq=101 "),
        "right after the point's own line: {lines:#?}"
    );
    assert_eq!(wrote, Vec::<&str>::new());
    assert!(host.said("probe").is_empty(), "no claim asked");

    let live = vec![busy_screen(), ended(PROBE_SAID)];
    for (tag, offered) in [
        ("probe-loop-host-wants-offered", true),
        ("probe-loop-host-wants-withheld", false),
    ] {
        let host = ProbeHost::probing(true);
        host.wanting.store(true, Ordering::SeqCst);
        let (lines, journal, wrote) = run(tag, live.clone(), &host);
        assert_eq!(gates_said(&lines), ["102 host-wants"], "{tag}: {lines:#?}");
        assert_eq!(gates_said(&journal), gates_said(&lines), "{tag}");
        assert_eq!(wrote, Vec::<&str>::new(), "{tag}");
        assert!(host.said("probe").is_empty(), "{tag}");
        assert_eq!(!host.said("at-idle").is_empty(), offered, "{tag}");
    }

    let host = ProbeHost::scripted(&[None], &[true]);
    let (lines, journal, wrote) = run("probe-loop-no-roster", live, &host);
    assert_eq!(gates_said(&journal), Vec::<String>::new(), "{lines:#?}");
    assert_eq!(wrote, Vec::<&str>::new());

    let (dir, ledger) = ledger_at("probe-loop-same-words");
    let host = ProbeHost::probing(false);
    let mut m = probe_mock(
        vec![busy_screen(), ended(PROBE_SAID)],
        &[panel_fx::USAGE_SETTLED],
        &host,
    );
    world(&mut m).then.extend([
        vec![busy_screen(), ended(PROBE_SAID)],
        vec![busy_screen(), ended(STOP)],
    ]);
    let (lines, _) = probe_run(
        "te-probe-loop-same-words",
        &mut m,
        &host,
        &ledger,
        probe_policy(120),
        |_| {},
    );
    assert_eq!(
        gate_words(&lines),
        ["claim host=not-due", "claim host=not-due", "escalated"],
        "{lines:#?}"
    );
    assert_eq!(probe_writes(&m), Vec::<&str>::new());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_point_decided_again_says_not_new() {
    let (dir, ledger) = ledger_at("probe-gate-not-new");
    let host = ProbeHost::scripted(
        &[None, Some(AccountAdvice::Probe { otherwise: None })],
        &[true],
    );
    let mut m = probe_mock(vec![ended(PROBE_SAID)], &[panel_fx::USAGE_SETTLED], &host);
    let opts = SuperviseOpts {
        idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
        policy: SupervisorConfig {
            continue_policy: false,
            ..hosted(30).policy
        },
        ..hosted(30)
    };
    let mut s = session(&mut m, None);
    s.set_clock(PROBE_NOW, PDT);
    s.set_approval_ledger(Some(ledger));
    probe_waits(&mut s);
    let screen = s.screen().expect("the point");
    let point = s.turn_of(screen);
    let mut heard = Heard::default();
    let decided = s
        .turn_end_at_point(&point, Some(Duration::from_secs(182)), &opts)
        .unwrap_or_else(|e| panic!("{e:?}"));
    s.turn_live = true;
    assert_eq!(
        s.usage_probe(&point, &decided, &opts, &mut heard),
        Ok(Probed::No)
    );
    assert_eq!(heard.said, Vec::<String>::new(), "control: nothing advised");
    for _ in 0..3 {
        s.turn_end_now(&point, &opts, &[], &mut heard)
            .unwrap_or_else(|e| panic!("{e:?}"));
    }
    assert_eq!(
        host.said("account").len(),
        4,
        "asked at every decision: {:?}",
        host.said("")
    );
    assert_eq!(heard.said.len(), 1, "{:?}", heard.said);
    assert!(
        heard.said[0].starts_with("SKIPPED seq=")
            && heard.said[0].ends_with(" rule=usage-probe@v1 gate=not-new"),
        "{:?}",
        heard.said
    );
    assert!(host.said("probe").is_empty(), "no claim asked");
    drop(s);
    assert_eq!(probe_writes(&m), Vec::<&str>::new());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_probe_books_no_act_and_moves_none_of_the_policys_memory() {
    use crate::supervise::policy::turn_end::{RULE_COMPACT, Then};
    let (dir, ledger) = ledger_at("probe-books");
    let host = ProbeHost::probing(true);
    let mut m = probe_mock(
        vec![ended(PROBE_SAID)],
        &[panel_fx::USAGE_REFRESHING, panel_fx::USAGE_SETTLED],
        &host,
    );
    let opts = SuperviseOpts {
        idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
        ..hosted(30)
    };
    let mut s = session(&mut m, None);
    s.set_clock(PROBE_NOW, PDT);
    s.set_zone_lookup(test_zones);
    s.set_approval_ledger(Some(ledger.clone()));
    s.ledger = crate::supervise::approvals::Ledger::open(Some(&ledger), None, &mut Vec::new());
    probe_waits(&mut s);
    let screen = s.screen().expect("the point");
    let point = s.turn_of(screen);
    let decided = s
        .turn_end_at_point(&point, Some(Duration::from_secs(182)), &opts)
        .unwrap_or_else(|e| panic!("{e:?}"));
    assert!(
        matches!(decided, TurnEndAction::Type { .. }),
        "the point's own act is a continuation: {decided:?}"
    );

    s.turn_live = true;
    let before = s.turn_end.clone();
    let now = Instant::now();
    let booked = before.acts_in_window(now);
    assert_eq!(
        s.usage_probe(&point, &decided, &opts, &mut Alone),
        Ok(Probed::Done)
    );
    assert_eq!(host.said("probe"), ["probe:claimed", "probed:ok"]);
    assert_eq!(
        s.turn_end, before,
        "the policy's memory, whole: nothing of it moved"
    );
    assert_eq!(s.turn_end.acts_in_window(Instant::now()), booked);
    assert!(!s.turn_end.act_in_flight());
    assert_eq!(s.probe, UsageProbe::Idle);
    assert!(!s.turn_live || s.probe_fold.is_none() && !s.probe_off);

    let reading = s.turn_end_reading(&point.screen, None, &opts);
    let mut acted = before.clone();
    acted.acted(
        &TurnEndAction::TypeCommand {
            command: "/usage".to_string(),
            rule_id: RULE_COMPACT,
            then: Then::Nothing,
        },
        &reading,
        now,
    );
    assert_ne!(acted, before);
    assert_eq!(acted.acts_in_window(now), booked + 1);
    assert!(acted.act_in_flight());
    drop(s);
    assert_eq!(probe_writes(&m), ["/usage", "esc"]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_panel_that_never_opens_gets_no_escape() {
    let (dir, ledger) = ledger_at("probe-not-opened");
    let host = ProbeHost::probing(true);
    let mut m = probe_world(&[panel_fx::USAGE_SETTLED], &host);
    world(&mut m).opens = false;
    let (lines, journal) = probe_run(
        "te-probe-not-opened",
        &mut m,
        &host,
        &ledger,
        probe_policy(120),
        |_| {},
    );
    assert_eq!(
        m.writes(),
        ["text:/usage", "enter", "text:keep going", "enter"],
        "{lines:#?}"
    );
    assert_eq!(probe_writes(&m), ["/usage"], "no Escape");
    assert_eq!(host.said("probe"), ["probe:claimed", "probed:not-opened"]);
    assert!(host.samples.lock().unwrap().is_empty());
    assert!(
        journal
            .iter()
            .any(|l| l == "PROBED seq=102 rule=usage-probe@v1 /usage no sample (not-opened)"),
        "{journal:#?}"
    );
    assert_eq!(
        probe_rows(&ledger),
        ["typed /usage the accounts probe", "waiting /usage closed"]
    );
    assert_eq!(usage_probe_violations(&m, &host), Vec::<String>::new());
    let _ = std::fs::remove_dir_all(&dir);

    let (dir, ledger) = ledger_at("probe-turn-instead");
    let host = ProbeHost::probing(true);
    let mut m = probe_world(&[panel_fx::USAGE_SETTLED], &host);
    world(&mut m).turn_instead = Some(vec![busy_screen(), busy_screen(), ended(STOP)]);
    let (lines, journal) = probe_run(
        "te-probe-turn-instead",
        &mut m,
        &host,
        &ledger,
        probe_policy(120),
        |_| {},
    );
    assert_eq!(m.writes(), ["text:/usage", "enter"], "{lines:#?}");
    assert_eq!(host.said("probe"), ["probe:claimed", "probed:not-opened"]);
    assert!(
        journal
            .iter()
            .any(|l| l.contains("the point changed across the probe")),
        "{journal:#?}"
    );
    assert_eq!(usage_probe_violations(&m, &host), Vec::<String>::new());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_panel_that_never_settles_is_closed_with_no_sample() {
    let (dir, ledger) = ledger_at("probe-unsettled");
    let host = ProbeHost::probing(true);
    let mut m = probe_world(&[panel_fx::USAGE_REFRESHING], &host);
    let (lines, journal) = probe_run(
        "te-probe-unsettled",
        &mut m,
        &host,
        &ledger,
        probe_policy(120),
        |s| {
            let ms = Duration::from_millis;
            s.set_probe_waits(ms(60), ms(200), ms(10), ms(100));
        },
    );
    assert_eq!(
        m.writes(),
        ["text:/usage", "enter", "esc", "text:keep going", "enter"],
        "{lines:#?}"
    );
    assert_eq!(host.said("probe"), ["probe:claimed", "probed:unsettled"]);
    assert!(host.samples.lock().unwrap().is_empty());
    assert!(
        journal
            .iter()
            .any(|l| l.ends_with("/usage no sample (unsettled)")),
        "{journal:#?}"
    );
    assert!(!lines.iter().any(|l| l.starts_with("PROBED")), "{lines:#?}");
    assert_eq!(usage_probe_violations(&m, &host), Vec::<String>::new());
    let _ = std::fs::remove_dir_all(&dir);
}

fn two_turn_ends(tag: &str, frames: &[&str], taller: usize) -> (Vec<&'static str>, Vec<String>) {
    let (dir, ledger) = ledger_at(tag);
    let host = ProbeHost::probing(true);
    let mut m = probe_mock(vec![busy_screen(), ended(PROBE_SAID)], frames, &host);
    let mut second = vec![String::new(); taller];
    second.extend(ended("Stage 4 done; the suite is green."));
    world(&mut m).then.extend([
        vec![busy_screen(), second],
        vec![busy_screen(), ended(STOP)],
    ]);
    let (lines, _) = probe_run(
        &format!("te-{tag}"),
        &mut m,
        &host,
        &ledger,
        probe_policy(120),
        |_| {},
    );
    assert_eq!(
        usage_probe_violations(&m, &host),
        Vec::<String>::new(),
        "{lines:#?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
    (probe_writes(&m), host.said("probe"))
}

#[test]
fn a_folded_panel_is_no_sample_and_its_grid_size_is_not_probed_again() {
    let (wrote, told) = two_turn_ends("probe-fold", &[panel_fx::USAGE_SHORT], 0);
    assert_eq!(wrote, ["/usage", "esc"], "one probe, one Escape");
    assert_eq!(
        told,
        ["probe:claimed", "probed:fold"],
        "the second turn end asks for no claim"
    );

    let (wrote, told) = two_turn_ends("probe-fold-resized", &[panel_fx::USAGE_SHORT], 3);
    assert_eq!(wrote, ["/usage", "esc", "/usage", "esc"]);
    assert_eq!(
        told,
        [
            "probe:claimed",
            "probed:fold",
            "probe:claimed",
            "probed:fold"
        ]
    );

    let lost = [
        panel_fx::USAGE_REFRESHING,
        panel_fx::USAGE_SETTLED,
        panel_fx::USAGE_FOLD,
    ];
    let (wrote, told) = two_turn_ends("probe-fold-race", &lost, 0);
    assert_eq!(wrote, ["/usage", "esc", "/usage", "esc"]);
    assert_eq!(
        told,
        [
            "probe:claimed",
            "probed:fold",
            "probe:claimed",
            "probed:fold"
        ],
        "no sample from one settled read, and no latch on the size"
    );
}

#[test]
fn a_person_at_the_session_is_never_typed_under() {
    let (dir, ledger) = ledger_at("probe-person-at-send");
    let host = ProbeHost::probing(true);
    let mut m = probe_world(&[panel_fx::USAGE_SETTLED], &host);
    m.person_at_send = 1;
    let (lines, journal) = probe_run(
        "te-probe-person-at-send",
        &mut m,
        &host,
        &ledger,
        probe_policy(120),
        |_| {},
    );
    assert_eq!(m.writes(), ["text:/usage"], "asked once: {lines:#?}");
    assert_eq!(m.person_skipped.len(), 1, "and skipped for the person");
    assert_eq!(world(&mut m).at, PanelAt::Rest, "nothing in the composer");
    assert_eq!(host.said("probe"), ["probe:claimed", "probed:person"]);
    assert!(
        journal
            .iter()
            .any(|l| l.starts_with("SKIPPED seq=102 rule=usage-probe@v1 ")),
        "{journal:#?}"
    );
    assert!(
        probe_rows(&ledger).is_empty(),
        "nothing written, nothing owed"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_refused_escape_is_sent_again_and_never_a_second_usage() {
    let (dir, ledger) = ledger_at("probe-refused");
    let host = ProbeHost::probing(true);
    let mut m = probe_world(
        &[panel_fx::USAGE_REFRESHING, panel_fx::USAGE_SETTLED],
        &host,
    );
    world(&mut m).esc_replies = [err("halted by the owner"), err("busy turn=1")].into();
    let (lines, journal) = probe_run(
        "te-probe-refused",
        &mut m,
        &host,
        &ledger,
        probe_policy(120),
        |_| {},
    );
    assert_eq!(
        probe_writes(&m),
        ["/usage", "esc", "esc", "esc"],
        "{lines:#?}"
    );
    assert_eq!(host.said("probe"), ["probe:claimed", "probed:ok"]);
    assert_ne!(world(&mut m).at, PanelAt::Open, "closed by the third");
    assert!(
        journal.iter().any(|l| l.starts_with("HALTED "))
            && journal.iter().any(|l| l.starts_with("REFUSED ")),
        "{journal:#?}"
    );
    assert!(
        !journal
            .iter()
            .any(|l| l.starts_with("WAITING ") && l.contains("panel is open")),
        "a refusal spends no try: {journal:#?}"
    );
    assert_eq!(
        probe_rows(&ledger),
        ["typed /usage the accounts probe", "waiting /usage closed"]
    );
    assert_eq!(usage_probe_violations(&m, &host), Vec::<String>::new());
    let _ = std::fs::remove_dir_all(&dir);
}

fn panel_standing(frame: &str, host: &ProbeHost) -> Mock {
    let mut m = probe_mock(vec![panel_fx::screen(frame)], &[frame], host);
    let w = world(&mut m);
    w.at = PanelAt::Open;
    w.rest = Some(ended(PROBE_SAID));
    m
}

fn owed_ledger(path: &Path) {
    let row = crate::supervise::approvals::Row {
        rule_id: "usage-probe@v1",
        outcome: crate::supervise::approvals::Outcome::Typed,
        command: "/usage",
        reason: crate::supervise::approvals::PROBE_TYPED_REASON,
        box_seq: 7,
    };
    std::fs::write(path, format!("{}\n", row.to_json(1_000, None))).expect("the ledger");
}

#[test]
fn a_successor_closes_only_a_panel_the_ledger_says_the_harness_opened() {
    let (dir, ledger) = ledger_at("probe-successor");
    owed_ledger(&ledger);
    let host = ProbeHost::probing(true);
    let mut m = panel_standing(panel_fx::USAGE_SETTLED, &host);
    let started = Instant::now();
    let (lines, _) = probe_run(
        "te-probe-successor",
        &mut m,
        &host,
        &ledger,
        probe_policy(1),
        |_| {},
    );
    assert_eq!(probe_writes(&m), ["esc"], "{lines:#?}");
    let esc = &world(&mut m).probes[0];
    assert!(
        esc.at.duration_since(started) >= Duration::from_secs(1),
        "one grace in the successor's own sight: {:?}",
        esc.at.duration_since(started)
    );
    assert_eq!(
        esc.request,
        "key if-human=0 if=^...Settings..Status...Config...Usage esc"
    );
    let _ = std::fs::remove_dir_all(&dir);

    let (dir, ledger) = ledger_at("probe-not-ours");
    let host = ProbeHost::probing(true);
    let mut m = panel_standing(panel_fx::USAGE_SETTLED, &host);
    let (lines, _) = probe_run_for(
        "te-probe-not-ours",
        &mut m,
        &host,
        &ledger,
        probe_policy(0),
        |_| {},
    );
    assert_eq!(m.writes(), Vec::<String>::new(), "{lines:#?}");
    assert_eq!(world(&mut m).at, PanelAt::Open, "it stands as it stood");
    assert!(probe_rows(&ledger).is_empty());
    let _ = std::fs::remove_dir_all(&dir);

    let (dir, ledger) = ledger_at("probe-no-header");
    owed_ledger(&ledger);
    let host = ProbeHost::probing(true);
    let mut m = panel_standing(panel_fx::USAGE_INLINE_SCROLLED, &host);
    let (lines, _) = probe_run_for(
        "te-probe-no-header",
        &mut m,
        &host,
        &ledger,
        probe_policy(0),
        |_| {},
    );
    assert_eq!(m.writes(), Vec::<String>::new(), "{lines:#?}");
    assert_eq!(world(&mut m).at, PanelAt::Open);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_loop_watching_behind_another_claim_asks_no_escape_of_an_owed_panel() {
    let run = |tag: &str, behind: bool| {
        let (dir, ledger) = ledger_at(tag);
        owed_ledger(&ledger);
        let host = ProbeHost::probing(true);
        let mut m = panel_standing(panel_fx::USAGE_SETTLED, &host);
        if behind {
            m.claim_replies = VecDeque::from([err("busy supervisor=gui%3A1")]);
            world(&mut m).leaves_after = Some(3);
        }
        let (lines, journal) = probe_run(tag, &mut m, &host, &ledger, probe_policy(0), |s| {
            s.set_claim_timing(Duration::from_secs(3600), 90_000)
        });
        let rows = probe_rows(&ledger);
        let _ = std::fs::remove_dir_all(&dir);
        (m, lines, journal, rows)
    };
    let refused = |journal: &[String]| {
        journal
            .iter()
            .filter(|l| l.starts_with("REFUSED "))
            .cloned()
            .collect::<Vec<_>>()
    };

    let (mut m, lines, journal, rows) = run("probe-watching", true);
    assert!(
        lines[0].starts_with("WATCHING ") && lines[0].contains("gui:1"),
        "{lines:#?}"
    );
    assert_eq!(probe_writes(&m), Vec::<&str>::new(), "{lines:#?}");
    assert!(
        !m.requests.iter().any(|r| r.ends_with(" esc")),
        "{:#?}",
        m.requests
    );
    assert_eq!(refused(&journal), Vec::<String>::new());
    assert_eq!(world(&mut m).at, PanelAt::Rest, "the holder closed it");
    assert_eq!(
        rows,
        ["typed /usage the accounts probe", "waiting /usage closed"]
    );

    let (m, lines, journal, rows) = run("probe-watching-free", false);
    assert_eq!(probe_writes(&m), ["esc"], "{lines:#?}");
    assert_eq!(refused(&journal), Vec::<String>::new());
    assert_eq!(
        rows,
        ["typed /usage the accounts probe", "waiting /usage closed"]
    );
}

#[test]
fn an_outage_in_the_read_leaves_the_panel_owed_and_the_same_loop_closes_it() {
    let (dir, ledger) = ledger_at("probe-outage");
    let host = ProbeHost::probing(true);
    let mut m = probe_world(
        &[panel_fx::USAGE_REFRESHING, panel_fx::USAGE_SETTLED],
        &host,
    );
    world(&mut m).lose_read = Some(2);
    let (lines, _) = probe_run(
        "te-probe-outage",
        &mut m,
        &host,
        &ledger,
        probe_policy(120),
        |s| s.setup.pause = Duration::from_millis(5),
    );
    assert_eq!(host.said("probe"), ["probe:claimed", "probed:lost"]);
    assert!(host.samples.lock().unwrap().is_empty());
    assert_eq!(probe_writes(&m), ["/usage", "esc"], "{lines:#?}");
    assert!(
        lines.iter().any(|l| l.starts_with("RECONNECT ")),
        "{lines:#?}"
    );
    assert_eq!(world(&mut m).at, PanelAt::Over, "closed, and the act typed");
    assert_eq!(
        probe_rows(&ledger),
        ["typed /usage the accounts probe", "waiting /usage closed"]
    );
    assert_eq!(usage_probe_violations(&m, &host), Vec::<String>::new());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_owed_panels_repaint_is_never_the_worker_working() {
    let grace = || probe_policy(0);
    let (dir, ledger) = ledger_at("probe-repaint");
    owed_ledger(&ledger);
    let host = ProbeHost::probing(true);
    let mut m = panel_standing(panel_fx::USAGE_SETTLED, &host);
    world(&mut m).repaints = true;
    let (lines, _) = probe_run("te-probe-repaint", &mut m, &host, &ledger, grace(), |_| {});
    assert_eq!(probe_writes(&m), ["esc"], "{lines:#?}");
    assert_eq!(host.said("turn-ran"), Vec::<String>::new(), "{lines:#?}");
    let _ = std::fs::remove_dir_all(&dir);

    let (dir, ledger) = ledger_at("probe-repaint-not-ours");
    let host = ProbeHost::probing(true);
    let mut m = panel_standing(panel_fx::USAGE_SETTLED, &host);
    world(&mut m).repaints = true;
    world(&mut m).leaves_after = Some(8);
    let (lines, _) = probe_run(
        "te-probe-repaint-not-ours",
        &mut m,
        &host,
        &ledger,
        grace(),
        |_| {},
    );
    assert_eq!(probe_writes(&m), Vec::<&str>::new(), "{lines:#?}");
    assert_eq!(host.said("turn-ran"), ["turn-ran"], "{lines:#?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_sample_that_shows_the_account_low_moves_the_session_at_the_same_point() {
    let (dir, ledger) = ledger_at("probe-then-move");
    let host = ProbeHost::scripted(
        &[
            Some(AccountAdvice::Probe { otherwise: None }),
            Some(AccountAdvice::Move {
                to: "work-2".to_string(),
            }),
        ],
        &[true],
    );
    *host.answer.lock().unwrap() = Some("adopted");
    let mut m = probe_world(
        &[panel_fx::USAGE_REFRESHING, panel_fx::USAGE_SETTLED],
        &host,
    );
    let (lines, _) = probe_run(
        "te-probe-then-move",
        &mut m,
        &host,
        &ledger,
        probe_policy(120),
        |_| {},
    );
    assert_eq!(
        host.said(""),
        [
            "turn-ran",
            "account:turn-end",
            "probe:claimed",
            "probed:ok",
            "account:turn-end",
            "restart:account-ahead:work-2",
        ],
        "{lines:#?}"
    );
    assert_eq!(m.writes(), ["text:/usage", "enter", "esc"], "{lines:#?}");
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("RESTARTED seq=") && l.contains("relaunch under account work-2")),
        "{lines:#?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

fn panel_torn() -> Vec<String> {
    let settled = panel_fx::screen(panel_fx::USAGE_SETTLED);
    let from = settled
        .iter()
        .rposition(|row| row.trim_start().starts_with("Resets "))
        .expect("a window's reset row");
    let mut torn = ended(PROBE_SAID);
    let frame = torn
        .iter()
        .position(|row| row.starts_with('─'))
        .expect("the composer's frame");
    torn.truncate(frame);
    torn.extend(settled[from..].iter().cloned());
    torn
}

#[test]
fn a_half_erased_panel_after_the_escape_gets_no_second_escape() {
    let torn = panel_torn();
    assert_eq!(aterm_phase::settings_panel(&torn), None);
    assert!(!has_composer_frame(&torn));
    assert!(torn.iter().any(|row| aterm_phase::panel_tail(row)));
    let run = |tag: &str, edit: &dyn Fn(&mut PanelWorld)| {
        let (dir, ledger) = ledger_at(tag);
        let host = ProbeHost::probing(true);
        let mut m = probe_world(
            &[panel_fx::USAGE_REFRESHING, panel_fx::USAGE_SETTLED],
            &host,
        );
        edit(world(&mut m));
        m.vanish_after = Some(200);
        let (lines, journal) = probe_run(tag, &mut m, &host, &ledger, probe_policy(120), |s| {
            s.set_survey_retry([Duration::from_millis(20); SURVEY_TRIES]);
        });
        let rows = probe_rows(&ledger);
        let bad = usage_probe_violations(&m, &host);
        let _ = std::fs::remove_dir_all(&dir);
        (m, host, lines, journal, rows, bad)
    };
    let stuck = |journal: &[String]| {
        journal
            .iter()
            .filter(|l| l.starts_with("WAITING ") && l.contains("the /usage panel is open"))
            .count()
    };

    let (mut m, host, lines, journal, rows, bad) =
        run("te-probe-torn", &|w| w.torn = Some(panel_torn()));
    assert_eq!(
        m.writes(),
        ["text:/usage", "enter", "esc", "text:keep going", "enter"],
        "{lines:#?}"
    );
    assert!(
        world(&mut m).torn.is_none() && world(&mut m).mends.is_none(),
        "the half-erased frame was served to a read, and the composer after it"
    );
    assert_eq!(host.said("probe"), ["probe:claimed", "probed:ok"]);
    assert_eq!(
        host.samples
            .lock()
            .unwrap()
            .iter()
            .map(|(sample, pan, _)| (sample.session_pct, sample.week_pct, *pan))
            .collect::<Vec<_>>(),
        [(Some(1), Some(10), 2)]
    );
    assert_eq!(stuck(&journal), 0, "{journal:#?}");
    assert!(other_skips(&journal).is_empty(), "{journal:#?}");
    assert_eq!(
        gate_words(&journal),
        ["escalated"],
        "the later point's alone: {journal:#?}"
    );
    assert_eq!(
        rows,
        ["typed /usage the accounts probe", "waiting /usage closed"]
    );
    assert_eq!(bad, Vec::<String>::new());

    let (m, _, lines, journal, _, bad) = run("te-probe-torn-control", &|w| w.esc_fails = 1);
    assert_eq!(probe_writes(&m), ["/usage", "esc", "esc"], "{lines:#?}");
    assert_eq!(stuck(&journal), 1, "{journal:#?}");
    assert!(
        journal
            .iter()
            .any(|l| l.starts_with("WAITING ") && l.contains("(1 tries)")),
        "{journal:#?}"
    );
    assert_eq!(bad, Vec::<String>::new());
}

#[test]
fn a_panel_closed_under_the_read_is_no_new_point_and_the_probes_go_on() {
    let run = |tag: &str, edit: &dyn Fn(&mut Mock)| {
        let (dir, ledger) = ledger_at(tag);
        let host = ProbeHost::probing(true);
        let mut m = probe_world(&[panel_fx::USAGE_SETTLED], &host);
        edit(&mut m);
        let (lines, journal) = probe_run(tag, &mut m, &host, &ledger, probe_policy(120), |_| {});
        let rows = probe_rows(&ledger);
        let bad = usage_probe_violations(&m, &host);
        let _ = std::fs::remove_dir_all(&dir);
        (m, host, lines, journal, rows, bad)
    };
    let changed = |journal: &[String]| {
        journal
            .iter()
            .filter(|l| l.contains("the point changed across the probe"))
            .count()
    };

    let (m, host, lines, journal, rows, bad) = run("te-probe-closed-mid-read", &|m| {
        let w = world(m);
        w.frames = vec![panel_fx::screen(panel_fx::USAGE_SETTLED), panel_torn()];
        w.leaves_after = Some(3);
    });
    assert_eq!(changed(&journal), 0, "{journal:#?}");
    assert_eq!(
        m.writes(),
        ["text:/usage", "enter", "text:keep going", "enter"],
        "no Escape of the loop's, and the act once: {lines:#?}"
    );
    assert_eq!(host.said("probe"), ["probe:claimed", "probed:closed"]);
    assert!(host.samples.lock().unwrap().is_empty());
    assert_eq!(
        gate_words(&journal),
        ["escalated"],
        "the later point's, and never probes-off: {journal:#?}"
    );
    assert_eq!(
        rows,
        ["typed /usage the accounts probe", "waiting /usage closed"]
    );
    assert_eq!(bad, Vec::<String>::new());

    let (m, _, lines, journal, rows, bad) = run("te-probe-closed-by-a-person", &|m| {
        let w = world(m);
        w.frames = vec![panel_fx::screen(panel_fx::USAGE_SETTLED), panel_torn()];
        w.leaves_after = Some(3);
        w.person_after = Some(2);
    });
    assert_eq!(changed(&journal), 0, "{journal:#?}");
    assert_eq!(m.writes(), ["text:/usage", "enter"], "{lines:#?}");
    assert!(
        journal
            .iter()
            .any(|l| l.starts_with("WAITING ") && l.ends_with("a person is typing")),
        "{journal:#?}"
    );
    assert_eq!(
        rows,
        ["typed /usage the accounts probe", "waiting /usage closed"]
    );
    assert_eq!(bad, Vec::<String>::new());

    let (_, _, lines, journal, _, bad) = run("te-probe-closed-mid-read-moved", &|m| {
        m.vanish_after = Some(200);
        let w = world(m);
        w.leaves_after = Some(1);
        w.after = Some(ended("Something else stands here now."));
    });
    assert_eq!(changed(&journal), 1, "{lines:#?}");
    assert!(
        !gate_words(&journal).is_empty() && gate_words(&journal).iter().all(|g| g == "probes-off"),
        "{journal:#?}"
    );
    assert_eq!(bad, Vec::<String>::new());
}

#[test]
fn a_panel_with_a_turn_running_under_it_gets_no_escape_and_no_sample() {
    let busy = panel_under_a_turn();
    assert!(aterm_phase::settings_panel(&busy).is_some());
    assert_eq!(worker_phase(&busy), Phase::Busy);
    let still = panel_fx::screen(panel_fx::USAGE_SETTLED);
    assert_ne!(worker_phase(&still), Phase::Busy);

    let (dir, ledger) = ledger_at("probe-turn-under");
    let host = ProbeHost::probing(true);
    let mut m = probe_world(&[panel_fx::USAGE_SETTLED], &host);
    world(&mut m).frames = vec![busy.clone()];
    let (lines, _) = probe_run_for(
        "te-probe-turn-under",
        &mut m,
        &host,
        &ledger,
        probe_policy(120),
        |_| {},
    );
    assert_eq!(m.writes(), ["text:/usage", "enter"], "{lines:#?}");
    assert_eq!(host.said("probe"), ["probe:claimed", "probed:unsettled"]);
    assert!(host.samples.lock().unwrap().is_empty());
    assert_eq!(world(&mut m).at, PanelAt::Open, "it stands, owed");
    assert_eq!(probe_rows(&ledger), ["typed /usage the accounts probe"]);
    assert_eq!(usage_probe_violations(&m, &host), Vec::<String>::new());
    let _ = std::fs::remove_dir_all(&dir);

    let (dir, ledger) = ledger_at("probe-turn-ended");
    let host = ProbeHost::probing(true);
    let mut m = probe_world(&[panel_fx::USAGE_SETTLED], &host);
    world(&mut m).frames = vec![busy, still];
    m.exit_when = Some(|m| probe_writes(m).contains(&"esc"));
    let (lines, _) = probe_run(
        "te-probe-turn-ended",
        &mut m,
        &host,
        &ledger,
        probe_policy(120),
        |_| {},
    );
    assert_eq!(probe_writes(&m), ["/usage", "esc"], "{lines:#?}");
    assert_eq!(host.said("probe"), ["probe:claimed", "probed:unsettled"]);
    assert!(host.samples.lock().unwrap().is_empty());
    assert_ne!(
        world(&mut m).at,
        PanelAt::Open,
        "closed once the turn ended"
    );
    assert_eq!(usage_probe_violations(&m, &host), Vec::<String>::new());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_usage_the_server_turns_away_or_skips_is_not_written_and_the_act_goes_on() {
    for (refusal, word) in [
        ("busy turn=1", "REFUSED "),
        ("halted by the owner", "HALTED "),
    ] {
        let (dir, ledger) = ledger_at("probe-send-refused");
        let host = ProbeHost::probing(true);
        let mut m = probe_world(&[panel_fx::USAGE_SETTLED], &host);
        m.send_replies.push_back(err(refusal));
        m.vanish_after = Some(200);
        let (lines, journal) = probe_run(
            "te-probe-send-refused",
            &mut m,
            &host,
            &ledger,
            probe_policy(120),
            |_| {},
        );
        assert_eq!(
            m.writes(),
            ["text:/usage", "text:keep going", "enter"],
            "{refusal}: {lines:#?}"
        );
        assert_eq!(probe_writes(&m), ["/usage"], "asked once, no Escape");
        assert_eq!(
            host.said("probe"),
            ["probe:claimed", "probed:not-written"],
            "{refusal}"
        );
        assert!(
            journal.iter().any(|l| l.starts_with(word)),
            "{refusal}: {journal:#?}"
        );
        assert!(
            probe_rows(&ledger).is_empty(),
            "nothing written: nothing owed"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    let (dir, ledger) = ledger_at("probe-send-skipped");
    let host = ProbeHost::probing(true);
    let mut m = probe_world(&[panel_fx::USAGE_SETTLED], &host);
    m.send_replies.push_back(ok("OK skipped seq=102\n"));
    let (lines, journal) = probe_run(
        "te-probe-send-skipped",
        &mut m,
        &host,
        &ledger,
        probe_policy(120),
        |_| {},
    );
    assert_eq!(
        m.writes(),
        ["text:/usage", "text:keep going", "enter"],
        "{lines:#?}"
    );
    assert_eq!(host.said("probe"), ["probe:claimed", "probed:not-written"]);
    assert!(
        journal
            .iter()
            .any(|l| l.starts_with("SKIPPED seq=102 rule=usage-probe@v1 ")),
        "{journal:#?}"
    );
    assert!(probe_rows(&ledger).is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn usage_left_typed_is_a_record_that_raises_nothing_and_the_panel_stays_owed() {
    let raised = |m: &Mock| -> Vec<String> {
        m.requests
            .iter()
            .filter(|r| r.starts_with("meta set attention"))
            .cloned()
            .collect()
    };
    let (dir, ledger) = ledger_at("probe-left-typed");
    let host = ProbeHost::probing(true);
    let mut m = probe_world(&[panel_fx::USAGE_SETTLED], &host);
    m.tick_at_key = 1;
    let (lines, journal) = probe_run_for(
        "te-probe-left-typed",
        &mut m,
        &host,
        &ledger,
        probe_policy(120),
        |_| {},
    );
    assert_eq!(m.writes(), ["text:/usage", "enter"], "{lines:#?}");
    assert_eq!(
        world(&mut m).at,
        PanelAt::Typed("/usage".to_string()),
        "it stands in the composer"
    );
    assert_eq!(host.said("probe"), ["probe:claimed", "probed:not-written"]);
    assert_eq!(host.said("inform").len(), 1, "told once: a record");
    assert_eq!(raised(&m), Vec::<String>::new(), "nothing is raised for it");
    assert!(
        !journal.iter().any(|l| l.starts_with("ESCALATED ")),
        "{journal:#?}"
    );
    let skipped: Vec<&String> = journal
        .iter()
        .filter(|l| l.contains("typed, not submitted"))
        .collect();
    assert!(
        skipped.len() == 1 && skipped[0].starts_with("SKIPPED seq=102 rule=usage-probe@v1 "),
        "its one journal line: {journal:#?}"
    );
    assert_eq!(
        probe_rows(&ledger),
        ["typed /usage the accounts probe"],
        "owed: no `waiting` row while the text stands"
    );
    assert!(crate::supervise::approvals::probe_owed(
        &std::fs::read_to_string(&ledger).unwrap_or_default(),
        None
    ));
    let _ = std::fs::remove_dir_all(&dir);

    let (dir, ledger) = ledger_at("probe-left-typed-other-rule");
    let host = ProbeHost::probing(false);
    let mut m = probe_world(&[panel_fx::USAGE_SETTLED], &host);
    m.tick_at_key = 1;
    let (lines, journal) = probe_run_for(
        "te-probe-left-typed-other-rule",
        &mut m,
        &host,
        &ledger,
        probe_policy(120),
        |_| {},
    );
    assert_eq!(m.writes(), ["text:keep going", "enter"], "{lines:#?}");
    assert_eq!(world(&mut m).at, PanelAt::Typed("keep going".to_string()));
    let raised = raised(&m);
    assert!(
        raised.len() == 1
            && raised[0].starts_with("meta set attention owner=supervisor claude idle: ")
            && raised[0].ends_with("`keep going`)"),
        "{:#?}",
        m.requests
    );
    assert_eq!(
        journal
            .iter()
            .filter(|l| l.starts_with("ESCALATED "))
            .count(),
        1,
        "{journal:#?}"
    );
    assert!(
        host.said("inform").is_empty(),
        "an escalation, not a record"
    );
    assert!(probe_rows(&ledger).is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn usage_left_typed_is_sent_as_the_probes_own_and_its_panel_is_closed() {
    let (dir, ledger) = ledger_at("probe-left-sent");
    let host = ProbeHost::probing(true);
    let mut m = probe_world(&[panel_fx::USAGE_SETTLED], &host);
    m.tick_at_key = 1;
    m.vanish_after = Some(200);
    let (lines, journal) = probe_run(
        "te-probe-left-sent",
        &mut m,
        &host,
        &ledger,
        probe_policy(0),
        |_| {},
    );
    assert_eq!(
        m.writes(),
        [
            "text:/usage",
            "enter",
            "enter",
            "esc",
            "text:keep going",
            "enter"
        ],
        "{lines:#?}\n{journal:#?}"
    );
    let keys: Vec<&String> = m
        .requests
        .iter()
        .filter(|r| verb_of(r) == "key" && (r.contains("/usage") || r.ends_with(" esc")))
        .collect();
    assert_eq!(keys.len(), 3, "{keys:#?}");
    assert_eq!(
        keys[0], "key if-human=0 if-gen=1.104 if=^❯\\x20/usage\\s*$ enter",
        "the probe's Enter, which the moved screen skipped"
    );
    assert!(
        keys[1].starts_with("key if-human=0 if-gen=1.")
            && keys[1].ends_with(" if=^❯\\x20/usage\\s*$ enter")
            && keys[1] != keys[0],
        "the Enter of the text left typed: fenced three ways on a later read: {keys:#?}"
    );
    assert_eq!(
        keys[2],
        "key if-human=0 if=^...Settings..Status...Config...Usage esc"
    );
    assert_eq!(count(&m, "turn"), 0, "never the unfenced fallback");
    assert_eq!(
        probe_writes(&m),
        ["/usage", "esc"],
        "one `/usage`, one Escape"
    );
    assert_eq!(
        host.said("probe"),
        ["probe:claimed", "probed:not-written"],
        "no second claim, and nothing sampled"
    );
    assert_eq!(host.said("inform").len(), 1, "told once: a record");
    assert!(
        !journal
            .iter()
            .any(|l| l.starts_with("CONTINUED ") && l.contains("/usage")),
        "`/usage` is no continuation: {journal:#?}"
    );
    let typed: Vec<&String> = journal.iter().filter(|l| l.starts_with("TYPED ")).collect();
    assert!(
        typed.len() == 1 && typed[0].starts_with("TYPED seq=106 rule=usage-probe@v1 "),
        "{journal:#?}"
    );
    assert_eq!(
        probe_rows(&ledger),
        ["typed /usage the accounts probe", "waiting /usage closed"]
    );
    let body = std::fs::read_to_string(&ledger).expect("the ledger");
    assert_eq!(
        body.lines()
            .filter(|l| l.contains("\"command\":\"/usage\""))
            .count(),
        2,
        "no row of `/usage` under the act's rule: {body}"
    );
    assert_eq!(
        crate::supervise::approvals::typed_at(&ledger, None).len(),
        1,
        "the budget's reader counts the continuation alone"
    );
    assert!(!crate::supervise::approvals::probe_owed(
        &std::fs::read_to_string(&ledger).unwrap_or_default(),
        None
    ));
    assert_eq!(usage_probe_violations(&m, &host), Vec::<String>::new());
    let _ = std::fs::remove_dir_all(&dir);

    let (dir, ledger) = ledger_at("probe-left-not-ours");
    let host = ProbeHost::probing(true);
    let mut m = probe_world(&[panel_fx::USAGE_SETTLED], &host);
    let rest = m.screens.last().cloned().expect("the point");
    *m.screens.last_mut().expect("the point") = with_draft(&rest, "/usage");
    m.cursor_col = Some(8);
    world(&mut m).at = PanelAt::Typed("/usage".to_string());
    world(&mut m).rest = Some(rest);
    let (lines, _) = probe_run_for(
        "te-probe-left-not-ours",
        &mut m,
        &host,
        &ledger,
        probe_policy(0),
        |_| {},
    );
    assert_eq!(m.writes(), ["enter"], "{lines:#?}");
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("CONTINUED ") && l.ends_with(" rule=continue@v1 /usage")),
        "the draft rule's act, as before: {lines:#?}"
    );
    assert_eq!(
        world(&mut m).at,
        PanelAt::Open,
        "not the harness's to close"
    );
    assert_eq!(probe_writes(&m), Vec::<&str>::new());
    assert!(host.said("probe").is_empty() && host.said("inform").is_empty());
    assert!(probe_rows(&ledger).is_empty());
    assert_eq!(
        crate::supervise::approvals::typed_at(&ledger, None).len(),
        1
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn usage_left_typed_over_its_own_menu_entry_is_still_the_probes_to_send() {
    let (dir, ledger) = ledger_at("probe-left-menu");
    let host = ProbeHost::probing(true);
    let mut m = probe_world(&[panel_fx::USAGE_SETTLED], &host);
    world(&mut m).menu = true;
    m.tick_at_key = 1;
    m.vanish_after = Some(700);
    let (lines, journal) = probe_run(
        "te-probe-left-menu",
        &mut m,
        &host,
        &ledger,
        probe_policy(0),
        |_| {},
    );
    assert_eq!(
        m.writes(),
        [
            "text:/usage",
            "enter",
            "enter",
            "esc",
            "text:keep going",
            "enter"
        ],
        "{lines:#?}\n{journal:#?}"
    );
    assert_eq!(
        journal
            .iter()
            .filter(|l| l.starts_with("TYPED "))
            .collect::<Vec<_>>(),
        [
            "TYPED seq=106 rule=usage-probe@v1 /usage left typed, its Enter written now: no act \
          of the policy's, and the panel it opens is closed unsampled"
        ],
        "{journal:#?}"
    );
    assert!(
        !journal
            .iter()
            .any(|l| l.contains(crate::supervise::policy::turn_end::MENU_DIVERTS)),
        "{journal:#?}"
    );
    assert_eq!(
        probe_rows(&ledger),
        ["typed /usage the accounts probe", "waiting /usage closed"]
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_enter_of_the_text_left_typed_that_does_not_take_is_written_again() {
    for (case, reply, written) in [
        ("not-taken", ok("OK seq=106\n"), 2),
        ("halted", err("halted reason=owner origin=local"), 1),
        ("busy", err("busy lease=s-9"), 1),
    ] {
        let tag = format!("probe-left-{case}");
        let (dir, ledger) = ledger_at(&tag);
        let host = ProbeHost::probing(true);
        let mut m = probe_world(&[panel_fx::USAGE_SETTLED], &host);
        m.key_replies
            .push_back(ok("OK skipped reason=changed seq=104\n"));
        m.key_replies.push_back(reply);
        if case == "halted" {
            m.hold = "1";
            m.hold_lifts_after = Some(1);
        }
        m.vanish_after = Some(200);
        let (lines, journal) = probe_run(
            &format!("te-{tag}"),
            &mut m,
            &host,
            &ledger,
            probe_policy(0),
            |s| {
                s.set_turn_end_timing(TurnEndTiming {
                    take_within: Duration::from_millis(100),
                    ..TurnEndTiming::default()
                });
            },
        );
        assert_eq!(
            m.writes(),
            [
                "text:/usage",
                "enter",
                "enter",
                "enter",
                "esc",
                "text:keep going",
                "enter"
            ],
            "{case}: {lines:#?}\n{journal:#?}"
        );
        assert_eq!(probe_writes(&m), ["/usage", "esc"], "{case}");
        assert_eq!(
            journal
                .iter()
                .filter(|l| l.starts_with("TYPED ")
                    && l.contains(" rule=usage-probe@v1 /usage left typed"))
                .count(),
            written,
            "{case}: {journal:#?}"
        );
        assert!(
            !journal
                .iter()
                .any(|l| l.starts_with("CONTINUED ") && l.contains("/usage")),
            "{case}: {journal:#?}"
        );
        assert_eq!(
            probe_rows(&ledger),
            ["typed /usage the accounts probe", "waiting /usage closed"],
            "{case}"
        );
        assert_eq!(
            crate::supervise::approvals::typed_at(&ledger, None).len(),
            1,
            "{case}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

fn with_draft(rows: &[String], text: &str) -> Vec<String> {
    let mut rows = rows.to_vec();
    let caret = rows.iter().rposition(|r| r == "❯").expect("a composer");
    rows[caret] = format!("❯ {text}");
    rows
}

#[test]
fn the_probe_is_not_typed_again_over_its_own_text_left_typed() {
    let second_turn_end = |tag: &str, cleared: bool| {
        let (dir, ledger) = ledger_at(tag);
        let host = ProbeHost::probing(true);
        let mut m = probe_world(
            &[panel_fx::USAGE_REFRESHING, panel_fx::USAGE_SETTLED],
            &host,
        );
        m.tick_at_key = 1;
        let _ = probe_run_for(
            &format!("te-{tag}"),
            &mut m,
            &host,
            &ledger,
            probe_policy(120),
            |_| {},
        );
        assert_eq!(probe_writes(&m), ["/usage"], "{tag}");
        assert_eq!(world(&mut m).at, PanelAt::Typed("/usage".to_string()));
        let rest = world(&mut m).rest.clone().expect("the composer");
        if cleared {
            world(&mut m).at = PanelAt::Rest;
            m.cursor_col = None;
            *m.screens.last_mut().expect("a screen") = rest;
        }
        let standing = m.screens.last().cloned().expect("a screen");
        let busy = if cleared {
            busy_screen()
        } else {
            with_draft(&busy_screen(), "/usage")
        };
        m.screens.push(busy);
        m.screens.push(standing);
        m.served = m.screens.len() - 2;
        m.stalls = 0;
        let next = ProbeHost::probing(true);
        let run_tag = format!("te-{tag}-next");
        let (lines, _) = if cleared {
            probe_run(&run_tag, &mut m, &next, &ledger, probe_policy(120), |_| {})
        } else {
            probe_run_for(&run_tag, &mut m, &next, &ledger, probe_policy(120), |_| {})
        };
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(
            next.said("").first().map(String::as_str),
            Some("turn-ran"),
            "{tag}: the turn was read live: {lines:#?}"
        );
        (probe_writes(&m), next.said(""), m.writes())
    };
    let (probes, said, writes) = second_turn_end("probe-left-again", false);
    assert_eq!(probes, ["/usage"], "the first loop's alone");
    assert_eq!(said, ["turn-ran"], "asked for no advice and no claim");
    assert_eq!(writes, ["text:/usage", "enter"], "nothing written over it");

    let (probes, said, _) = second_turn_end("probe-left-cleared", true);
    assert_eq!(probes, ["/usage", "/usage", "esc"]);
    assert_eq!(
        said[..4],
        ["turn-ran", "account:turn-end", "probe:claimed", "probed:ok"],
        "{said:#?}"
    );
}

#[test]
fn another_panel_ends_the_loops_probes() {
    let (wrote, told) = two_turn_ends("probe-inline-panel", &[panel_fx::USAGE_INLINE], 3);
    assert_eq!(wrote, ["/usage", "esc"]);
    assert_eq!(told, ["probe:claimed", "probed:renderer"]);

    let settled = panel_fx::screen(panel_fx::USAGE_SETTLED);
    let header = aterm_phase::settings_panel(&settled)
        .expect("the panel")
        .header;
    let mut other: Vec<String> = settled[..=header].to_vec();
    other.extend(
        ["", "   Version: 2.1.287", "", "   Esc to cancel"]
            .iter()
            .map(|r| r.to_string()),
    );
    let other = other.join("\n");
    let (wrote, told) = two_turn_ends("probe-other-tab", &[other.as_str()], 3);
    assert_eq!(wrote, ["/usage", "esc"]);
    assert_eq!(told, ["probe:claimed", "probed:renderer"]);
}

#[test]
fn a_loop_stopped_mid_probe_presses_nothing_and_the_next_loop_closes_the_panel() {
    let (dir, ledger) = ledger_at("probe-stopped");
    let host = ProbeHost::probing(true);
    let mut m = probe_world(&[panel_fx::USAGE_REFRESHING], &host);
    let stop = Arc::new(AtomicBool::new(false));
    world(&mut m).stop_at_read = Some((2, Arc::clone(&stop)));
    let opts = SuperviseOpts {
        idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
        ledger: Some(ledger.clone()),
        policy: probe_policy(120),
        ..SuperviseOpts::hosted_with(&owners_policy())
    };
    let mut out: Vec<u8> = Vec::new();
    {
        let mut s = session(&mut m, None);
        s.set_clock(PROBE_NOW, PDT);
        s.set_zone_lookup(test_zones);
        probe_waits(&mut s);
        assert_eq!(s.run_hosted(&opts, stop, &mut out), Ok(()));
    }
    assert_eq!(probe_writes(&m), ["/usage"], "{:#?}", m.requests);
    assert_eq!(world(&mut m).at, PanelAt::Open, "the panel stands");
    assert_eq!(host.said("probe"), ["probe:claimed", "probed:lost"]);
    assert_eq!(probe_rows(&ledger), ["typed /usage the accounts probe"]);
    assert!(crate::supervise::approvals::probe_owed(
        &std::fs::read_to_string(&ledger).unwrap_or_default(),
        None
    ));

    m.stalls = 0;
    let next = ProbeHost::probing(true);
    let (lines, _) = probe_run(
        "te-probe-stopped-next",
        &mut m,
        &next,
        &ledger,
        probe_policy(0),
        |_| {},
    );
    assert_eq!(probe_writes(&m), ["/usage", "esc"], "{lines:#?}");
    assert!(next.said("probe").is_empty());
    assert!(!crate::supervise::approvals::probe_owed(
        &std::fs::read_to_string(&ledger).unwrap_or_default(),
        None
    ));
    let _ = std::fs::remove_dir_all(&dir);
}

fn task_row(kind: &str, uuid: &str, id: &str, text: &str) -> String {
    let text = aterm_json::to_string(&aterm_json::Value::String(text.to_string())).expect("json");
    if kind == "user" {
        format!(
            r#"{{"parentUuid":null,"isSidechain":false,"type":"user","message":{{"role":"user","content":{text}}},"uuid":"{uuid}","sessionId":"0b2299fa","version":"2.1.299"}}"#
        )
    } else {
        format!(
            r#"{{"parentUuid":"p","isSidechain":false,"message":{{"id":"{id}","type":"message","role":"assistant","model":"claude-sonnet-5-5","content":[{{"type":"text","text":{text}}}],"stop_reason":"end_turn"}},"type":"assistant","uuid":"{uuid}","sessionId":"0b2299fa","version":"2.1.299"}}"#
        )
    }
}

fn transcript_of(rows: &[String]) -> String {
    let mut out = rows.join("\n");
    out.push('\n');
    out
}

/// A host whose agent's transcript is `path`, read as each of `texts` in
/// turn (the last repeats) — what a done edge snapshots ([`IdleHost::transcript`])
/// — and, with `ready`, the live upgrade: once a done edge has read the
/// transcript it asks for the next idle point and types its READY notice
/// there (a step that types: the next point is that turn's answer).
#[derive(Debug, Default)]
struct TaskHost {
    path: PathBuf,
    texts: std::sync::Mutex<VecDeque<String>>,
    stepped: Option<String>,
    sleep: std::sync::Mutex<Option<super::super::ClockJump>>,
    asks: std::sync::atomic::AtomicUsize,
    /// How many steps the host had taken at each read.
    read_at: std::sync::Mutex<Vec<usize>>,
    ready: bool,
    wanting: AtomicBool,
    steps: std::sync::atomic::AtomicUsize,
    /// How many points it wants and passes (no step) before it takes one.
    passes: std::sync::atomic::AtomicUsize,
}

impl TaskHost {
    fn reading(path: &Path, texts: &[&str]) -> Arc<Self> {
        Arc::new(Self {
            path: path.to_path_buf(),
            texts: std::sync::Mutex::new(texts.iter().map(|t| (*t).to_string()).collect()),
            ..Self::default()
        })
    }
}

impl IdleHost for TaskHost {
    fn wants(&self) -> bool {
        self.wanting.load(Ordering::SeqCst)
    }
    fn at_idle(&self) -> Option<HostStep> {
        if self
            .passes
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
            .is_ok()
        {
            return None;
        }
        self.wanting.store(false, Ordering::SeqCst);
        self.steps.fetch_add(1, Ordering::SeqCst);
        if let Some(clock) = self.sleep.lock().unwrap().take() {
            clock.slept(2 * 3600);
        }
        Some(HostStep {
            line: "upgrade step=notice".to_string(),
            moved: true,
            typed: Some(crate::supervise::policy::turn_end::HostTurn::Notice),
            repeat: false,
        })
    }
    fn owns_turn_end(&self) -> bool {
        false
    }
    fn transcript(&self) -> Option<(PathBuf, String)> {
        self.asks.fetch_add(1, Ordering::SeqCst);
        let steps = self.steps.load(Ordering::SeqCst);
        self.read_at.lock().unwrap().push(steps);
        if self.ready && steps == 0 {
            self.wanting.store(true, Ordering::SeqCst);
        }

        if let Some(after) = self.stepped.as_ref().filter(|_| steps > 0) {
            return Some((self.path.clone(), after.clone()));
        }
        let mut texts = self.texts.lock().unwrap();
        let text = if texts.len() > 1 {
            texts.pop_front()
        } else {
            texts.front().cloned()
        }?;
        Some((self.path.clone(), text))
    }
}

fn tasked_watch(
    m: &mut Mock,
    host: &Arc<TaskHost>,
    tasks: &Path,
    journal: Option<&Path>,
) -> Vec<String> {
    let opts = SuperviseOpts {
        policy: owners_policy(),
        journal: journal.map(Path::to_path_buf),
        idle_host: Some(Arc::clone(host) as Arc<dyn IdleHost>),
        tasks: Some(tasks.to_path_buf()),
        ..auto(30)
    };
    let mut out: Vec<u8> = Vec::new();
    let mut s = session(m, Some("@s-1".to_string()));
    s.set_clock(TEST_NOW, PDT);
    s.set_zone_lookup(test_zones);
    let _ = s.watch(&opts, &mut out);
    String::from_utf8(out)
        .expect("utf-8")
        .lines()
        .map(str::to_string)
        .collect()
}

fn working_task() -> Option<(u64, String)> {
    Some((
        1,
        "state=working from=s-0a1b since_ms=0 result_bytes=0".to_string(),
    ))
}

fn task_marks(m: &Mock) -> Vec<String> {
    m.task_requests
        .iter()
        .filter(|r| r.starts_with("task mark"))
        .cloned()
        .collect()
}

fn done_mark(why: &str, bytes: usize, path: &Path) -> String {
    format!(
        "task mark done why={why} result_bytes={bytes} result_path={}",
        aterm_codec::pct::encode(&path.to_string_lossy())
    )
}

const WORK: &str = "Fixed the parser; pushing it next.";

const LANDED: &str = "Fixed the parser and pushed it.\nNothing left: all of it is merged into \
                      the remote main branch.";

fn landed_reply() -> String {
    LANDED.to_string()
}

fn landed_result() -> String {
    format!("{WORK}\n\n{LANDED}")
}

fn landing_rows(reply: &str) -> Vec<String> {
    vec![
        task_row("user", "u-task", "", "Fix the parser."),
        task_row("assistant", "a-work", "msg_1", WORK),
        task_row("user", "u-land", "", LAND),
        task_row("assistant", "a-land", "msg_2", reply),
    ]
}

fn landing_screens(reply: &str) -> Vec<Vec<String>> {
    vec![busy_screen(), ended(WORK), busy_screen(), ended(reply)]
}

fn landing(m: &mut Mock) {
    m.autopilot = Some("land");
    m.turn_releases = Some(1);
    m.history_served();
}

#[test]
fn a_landed_workers_task_is_marked_once_with_its_defense() {
    let root = crate::supervise::ScratchDir::new("task", "landed");
    let text = transcript_of(&landing_rows(&landed_reply()));
    let run = |task: Option<(u64, String)>, tasks: &Path, word: &'static str| {
        let host = TaskHost::reading(&root.join("t.jsonl"), &[&text]);
        let journal = root.join(format!("journal-{word}-{}.jsonl", task.is_some()));
        let mut m = Mock::new(landing_screens(&landed_reply()));
        landing(&mut m);
        m.autopilot = Some(word);
        m.vanish_after = Some(4);
        m.task = task;
        let lines = tasked_watch(&mut m, &host, tasks, Some(&journal));
        let task_lines: Vec<String> = journal_records(&journal)
            .0
            .into_iter()
            .map(|r| r.line)
            .filter(|l| l.starts_with("TASK "))
            .collect();
        (m, lines, task_lines)
    };
    let tasks = root.join("tasks");
    let (m, lines, task_lines) = run(working_task(), &tasks, "land");
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with(&format!("CONTINUED seq=102 rule={RULE_LAND} "))),
        "{lines:#?}"
    );
    let path = tasks.join("s-1-1.result.md");
    assert_eq!(
        task_marks(&m),
        [done_mark("landed", landed_result().len(), &path)],
        "{lines:#?}"
    );
    assert_eq!(
        std::fs::read_to_string(&path).expect("the snapshot"),
        landed_result()
    );
    assert!(
        m.task
            .as_ref()
            .is_some_and(|(rev, f)| *rev == 2 && f.starts_with("state=done ")),
        "{:?}",
        m.task
    );
    assert_eq!(
        task_lines,
        [format!(
            "TASK seq=104 done why=landed rev=2 result_bytes={}",
            landed_result().len()
        )]
    );
    let cruise = root.join("cruise");
    let (m, lines, task_lines) = run(working_task(), &cruise, "cruise");
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with(&format!("CONTINUED seq=104 rule={RULE_DONE_CHECK} "))),
        "{lines:#?}"
    );
    assert!(task_marks(&m).is_empty(), "{:?}", m.task_requests);
    assert!(task_lines.is_empty(), "{task_lines:#?}");
    assert!(!cruise.exists(), "nothing written");
    let untasked = root.join("untasked");
    let (m, _, task_lines) = run(None, &untasked, "land");
    assert!(!m.task_requests.is_empty(), "the record read");
    assert!(task_marks(&m).is_empty(), "{:?}", m.task_requests);
    assert!(task_lines.is_empty(), "{task_lines:#?}");
    assert!(!untasked.exists(), "nothing written");
}

#[test]
fn a_screen_read_workers_landing_is_its_result() {
    use aterm_phase::gemini::fixtures as g;
    let root = crate::supervise::ScratchDir::new("task", "screen-result");
    let defense = "Nothing left: all of it is merged into the remote main branch.";
    let busy = aterm_phase::prompt::fixtures::screen(g::BUSY_THINKING);
    let host = TaskHost::reading(&root.join("none.jsonl"), &[]);
    let mut m = gemini_mock(vec![
        busy.clone(),
        gemini_ended(WORK),
        busy,
        gemini_ended(defense),
    ]);
    landing(&mut m);
    m.gen_fence = true;
    m.sends_gen = true;
    m.vanish_after = Some(6);
    m.stall_sleep = Some(Duration::from_millis(20));
    m.task = working_task();
    let tasks = root.join("tasks");
    let opts = SuperviseOpts {
        policy: owners_policy(),
        idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
        tasks: Some(tasks.clone()),
        ..auto(30)
    };
    let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
        s.set_turn_end_timing(TurnEndTiming {
            min_work: Duration::ZERO,
            short_backoff: Duration::from_millis(30),
            short_backoff_max: Duration::from_millis(60),
            ..TurnEndTiming::default()
        });
    });
    let marks = task_marks(&m);
    let [mark] = marks.as_slice() else {
        panic!("one done: {marks:?}: {lines:#?}");
    };
    let report = format!("{WORK}\n\n{defense}");
    let path = mark
        .strip_prefix(&format!(
            "task mark done why=landed result_bytes={} result_path=",
            report.len()
        ))
        .map(|p| PathBuf::from(aterm_codec::pct::decode(p)))
        .unwrap_or_else(|| panic!("{mark}"));
    assert!(path.starts_with(&tasks), "{mark}");
    assert_eq!(
        crate::harness::result::answer(&path, false),
        format!(
            "OK 3 source=screen program=gemini bytes={}\n{report}\n",
            report.len()
        )
    );
    let host = TaskHost::reading(&root.join("claude.jsonl"), &[]);
    let mut m = Mock::new(landing_screens(defense));
    landing(&mut m);
    m.vanish_after = Some(4);
    m.task = working_task();
    let tasks = root.join("tasks-claude");
    let lines = tasked_watch(&mut m, &host, &tasks, None);
    let path = tasks.join("s-1-1.result.md");
    assert_eq!(
        task_marks(&m),
        [done_mark("landed", 0, &path)],
        "{lines:#?}"
    );
    assert_eq!(
        crate::harness::result::answer(&path, false),
        "ERR result unavailable why=no-transcript\n"
    );
}

#[test]
fn a_defense_taller_than_the_screen_is_read_whole_before_it_lands() {
    let root = crate::supervise::ScratchDir::new("task", "tall");
    let tall = |need: bool| -> (Vec<String>, Vec<String>, String) {
        let mut said = vec!["Merged the parser, the lexer and their suites.".to_string()];
        if need {
            said.push("NEEDS YOU: the signing credential".to_string());
        }
        said.extend((0..45).map(|i| format!("- merged item {i} into the remote main branch")));
        said.push(String::new());
        said.push("Nothing left: all of it is merged into the remote main branch.".to_string());
        let mut drawn: Vec<String> = vec!["❯ Fix the parser.".to_string(), String::new()];
        drawn.extend(
            said.iter()
                .enumerate()
                .map(|(i, l)| match (i, l.is_empty()) {
                    (0, _) => format!("⏺ {l}"),
                    (_, true) => String::new(),
                    _ => format!("  {l}"),
                }),
        );
        drawn.extend(rows(&["", "✻ Worked for 12m 2s · done 4:24 PM", ""]));
        drawn.extend(composer("  ⏵⏵ bypass permissions on (shift+tab to cycle)"));
        let cut = drawn.len() - 34;
        let screen = drawn.split_off(cut);
        (screen, drawn, said.join("\n"))
    };
    let run = |need: bool, history: bool, tag: &str| {
        let (screen, above, text) = tall(need);
        assert!(
            !aterm_phase::said_whole(&screen),
            "the head is above the screen"
        );
        let transcript = transcript_of(&landing_rows(&text));
        let host = TaskHost::reading(&root.join(format!("{tag}.jsonl")), &[&transcript]);
        let mut m = Mock::new(vec![busy_screen(), ended(WORK), busy_screen(), screen]);
        landing(&mut m);
        m.scrollback = history.then_some(above);
        m.alt_screen = Some(false);
        m.vanish_after = Some(4);
        m.task = working_task();
        let lines = tasked_watch(&mut m, &host, &root.join(format!("tasks-{tag}")), None);
        (m, lines)
    };
    let (m, lines) = run(false, true, "clean");
    let marks = task_marks(&m);
    assert!(
        marks.len() == 1 && marks[0].starts_with("task mark done why=landed "),
        "{marks:?}: {lines:#?}"
    );
    let (m, lines) = run(true, true, "need");
    assert!(task_marks(&m).is_empty(), "{:?}", m.task_requests);
    assert!(
        lines.iter().any(|l| l.contains(&format!(
            "rule={} ",
            crate::supervise::policy::turn_end::RULE_LAND_NUDGE
        ))),
        "{lines:#?}"
    );
    let (m, _) = run(false, false, "unread");
    assert!(task_marks(&m).is_empty(), "{:?}", m.task_requests);
}

fn tall_on(
    end: &[String],
    reply_at: usize,
    said: &[String],
    head: &str,
) -> (Vec<String>, Vec<String>) {
    let mut drawn = end[..reply_at].to_vec();
    drawn.extend(
        said.iter()
            .enumerate()
            .map(|(i, l)| match (i, l.is_empty()) {
                (_, true) => String::new(),
                (0, _) => format!("{head}{l}"),
                _ => format!("  {l}"),
            }),
    );
    drawn.extend(end[reply_at + 1..].iter().cloned());
    let screen = drawn.split_off(drawn.len() - end.len());
    (screen, drawn)
}

fn tall_said(need: bool) -> Vec<String> {
    let mut said = vec!["Merged the parser, the lexer and their suites.".to_string()];
    if need {
        said.push("NEEDS YOU: the signing credential".to_string());
    }
    said.extend((0..55).map(|i| format!("- merged item {i} into the remote main branch")));
    said.push(String::new());
    said.push("Nothing left: all of it is merged into the remote main branch.".to_string());
    said
}

fn tall_landing(
    mock: fn(Vec<Vec<String>>) -> Mock,
    busy: &[String],
    work: &[String],
    tall: (Vec<String>, Vec<String>),
    history: bool,
) -> (Vec<String>, Vec<String>) {
    let (dir, journal) = journal_file("tall-landing");
    let (screen, above) = tall;
    let mut m = mock(vec![busy.to_vec(), work.to_vec(), busy.to_vec(), screen]);
    landing(&mut m);
    m.gen_fence = true;
    m.sends_gen = true;
    m.scrollback = history.then_some(above);
    m.alt_screen = Some(false);
    m.vanish_after = Some(6);
    m.stall_sleep = Some(Duration::from_millis(20));
    let opts = SuperviseOpts {
        policy: owners_policy(),
        journal: Some(journal.clone()),
        ..auto(30)
    };
    let (mut lines, _) = watch_lines_with(&mut m, &opts, |s| {
        s.set_turn_end_timing(TurnEndTiming {
            min_work: Duration::ZERO,
            short_backoff: Duration::from_millis(30),
            short_backoff_max: Duration::from_millis(60),
            ..TurnEndTiming::default()
        });
    });
    lines.extend(
        journal_records(&journal)
            .0
            .into_iter()
            .map(|r| r.line)
            .filter(|l| l.starts_with("DONE ")),
    );
    let _ = std::fs::remove_dir_all(&dir);
    (lines, m.needs_calls.clone())
}

#[test]
fn a_tall_land_report_on_every_harness_is_read_whole_before_it_lands() {
    use aterm_phase::prompt::fixtures::screen;
    use aterm_phase::{agy, gemini, muse};
    let gemini_tall = |need: bool| {
        let end = screen(gemini::fixtures::END_OF_TURN);
        let at = end
            .iter()
            .rposition(|r| r.starts_with("✦ "))
            .expect("the reply");
        tall_on(&end, at, &tall_said(need), "✦ ")
    };
    let muse_tall = |need: bool| {
        let end = screen(muse::fixtures::END_OF_TURN_1_4_10);
        let at = end
            .iter()
            .position(|r| r.starts_with("◆ Hi!"))
            .expect("the reply");
        tall_on(&end, at, &tall_said(need), "◆ ")
    };
    let agy_tall = |need: bool| {
        let end = screen(agy::fixtures::END_OF_TURN);
        let user = end
            .iter()
            .position(|r| r.starts_with("> Write about"))
            .expect("the message");
        let composer = (user..end.len())
            .find(|&i| end[i].starts_with('─'))
            .expect("the composer's rule");
        let mut drawn = end[..user].to_vec();
        drawn.extend([
            "> Land it all in the remote main branch. Put what only a person can give on a line \
             starting"
                .to_string(),
            "  NEEDS YOU: and carry on.".to_string(),
            String::new(),
        ]);
        drawn.extend(tall_said(need).iter().map(|l| {
            if l.is_empty() {
                String::new()
            } else {
                format!("  {l}")
            }
        }));
        drawn.push(String::new());
        drawn.extend(end[composer..].iter().cloned());
        let screen = drawn.split_off(drawn.len() - end.len());
        (screen, drawn)
    };
    type Tall = Box<dyn Fn(bool) -> (Vec<String>, Vec<String>)>;
    type Harness = (
        &'static str,
        fn(Vec<Vec<String>>) -> Mock,
        &'static str,
        &'static str,
        Tall,
    );
    let harnesses: [Harness; 3] = [
        (
            "gemini",
            gemini_mock,
            gemini::fixtures::BUSY_THINKING,
            gemini::fixtures::END_OF_TURN,
            Box::new(gemini_tall),
        ),
        (
            "agy",
            agy_mock,
            agy::fixtures::BUSY,
            agy::fixtures::END_OF_TURN,
            Box::new(agy_tall),
        ),
        (
            "muse",
            muse_mock,
            muse::fixtures::BUSY_1_4_10,
            muse::fixtures::END_OF_TURN_1_4_10,
            Box::new(muse_tall),
        ),
    ];
    let landed = |lines: &[String]| {
        lines
            .iter()
            .any(|l| l.starts_with("DONE ") && l.contains(&format!(" rule={RULE_LAND} ")))
    };
    for (program, mock, busy, work, tall) in harnesses {
        let (busy, work) = (screen(busy), screen(work));
        let (cut, _) = tall(true);
        assert!(
            !program_reader(program, &cut).said_whole(&cut),
            "{program}: the report's head is above the screen"
        );
        let (lines, needs) = tall_landing(mock, &busy, &work, tall(true), true);
        assert!(!landed(&lines), "{program}: {lines:#?}");
        assert!(
            lines.iter().any(|l| l.contains(&format!(
                " rule={} ",
                crate::supervise::policy::turn_end::RULE_LAND_NUDGE
            ))),
            "{program}: the nudge: {lines:#?}"
        );
        assert!(
            needs
                .first()
                .is_some_and(|c| c.starts_with("set attention owner=needs ")
                    && c.contains("the signing credential")),
            "{program}: the need raised: {needs:#?}\n{lines:#?}"
        );
        let (lines, needs) = tall_landing(mock, &busy, &work, tall(false), true);
        assert!(landed(&lines), "{program}: {lines:#?}");
        assert!(needs.is_empty(), "{program}: {needs:#?}");
        let (lines, _) = tall_landing(mock, &busy, &work, tall(false), false);
        assert!(!landed(&lines), "{program}: {lines:#?}");
    }
}

#[test]
fn a_need_said_earlier_in_a_turn_above_the_screen_is_read_before_it_lands() {
    let turn_of = |need: bool, calls: usize| -> (Vec<String>, Vec<String>) {
        let mut drawn: Vec<String> = vec![
            "❯ Land it all in the remote main branch.".to_string(),
            String::new(),
            "⏺ Merged the lexer and its suites.".to_string(),
        ];
        if need {
            drawn.push("  NEEDS YOU: the signing credential".to_string());
        }
        for call in 0..calls {
            drawn.extend([
                String::new(),
                format!("⏺ Bash(git log --oneline -1 branch-{call})"),
                format!("  ⎿  {call:04}abc merged branch {call}"),
            ]);
        }
        drawn.extend([String::new(), "⏺ Bash(git push origin main)".to_string()]);
        drawn.push("  ⎿  To github.com:a/parser.git".to_string());
        drawn.extend((0..40).map(|i| format!("     {i:02}a1b2c..{i:02}d3e4f  main -> main")));
        drawn.extend(rows(&[
            "",
            "⏺ Nothing left: all of it is merged into the remote main branch.",
            "",
            "✻ Worked for 12m 2s · done 4:24 PM",
            "",
        ]));
        drawn.extend(composer("  ⏵⏵ bypass permissions on (shift+tab to cycle)"));
        let screen = drawn.split_off(drawn.len() - 30);
        (screen, drawn)
    };
    let turn = |need: bool| turn_of(need, 0);
    let (screen, _) = turn(true);
    assert!(
        aterm_phase::said_whole(&screen) && !aterm_phase::turn_whole(&screen),
        "the last message on the screen, the turn's start above it"
    );
    let landed = |lines: &[String]| {
        lines
            .iter()
            .any(|l| l.starts_with("DONE ") && l.contains(&format!(" rule={RULE_LAND} ")))
    };
    let busy = busy_screen();
    let work = ended(WORK);
    let (lines, needs) = tall_landing(Mock::new, &busy, &work, turn(true), true);
    assert!(!landed(&lines), "{lines:#?}");
    assert!(
        lines.iter().any(|l| l.contains(&format!(
            " rule={} ",
            crate::supervise::policy::turn_end::RULE_LAND_NUDGE
        ))),
        "the nudge: {lines:#?}"
    );
    assert!(
        needs
            .first()
            .is_some_and(|c| c.starts_with("set attention owner=needs ")
                && c.contains("the signing credential")),
        "the need raised: {needs:#?}\n{lines:#?}"
    );
    let (lines, needs) = tall_landing(Mock::new, &busy, &work, turn(false), true);
    assert!(landed(&lines), "{lines:#?}");
    assert!(needs.is_empty(), "{needs:#?}");
    let nudged = |lines: &[String]| {
        lines.iter().any(|l| {
            l.contains(&format!(
                " rule={} ",
                crate::supervise::policy::turn_end::RULE_LAND_NUDGE
            ))
        })
    };
    for (why, read) in [
        ("no history", (turn(true), false)),
        ("140 calls", (turn_of(true, 140), true)),
        ("140 calls, no need", (turn_of(false, 140), true)),
    ] {
        let ((screen, above), history) = read;
        assert!(above.len() > 400 || !history, "{why}: {}", above.len());
        let (lines, _) = tall_landing(Mock::new, &busy, &work, (screen, above), history);
        assert!(!landed(&lines), "{why}: {lines:#?}");
        assert!(nudged(&lines), "{why}: the nudge: {lines:#?}");
    }
}

#[test]
fn a_need_said_before_a_tool_call_on_every_harness_is_read_before_it_lands() {
    use aterm_phase::prompt::fixtures::screen;
    use aterm_phase::{agy, gemini, muse};
    let before = |need: bool, head: &str| -> Vec<String> {
        if need {
            vec![
                format!("{head}NEEDS YOU: the signing credential"),
                "  I will push the branch now.".to_string(),
            ]
        } else {
            vec![format!("{head}I will push the branch now.")]
        }
    };
    let defense = "Nothing left: all of it is merged into the remote main branch.";
    let calls = |tall: bool| if tall { 60 } else { 1 };
    let split = |mut drawn: Vec<String>, height: usize, tall: bool| {
        if tall {
            let screen = drawn.split_off(drawn.len() - height);
            (screen, drawn)
        } else {
            (drawn, Vec::new())
        }
    };
    let gemini_turn = move |need: bool, tall: bool| {
        let end = screen(gemini::fixtures::END_OF_TURN);
        let at = end
            .iter()
            .rposition(|r| r.starts_with("✦ "))
            .expect("the reply");
        let mut drawn = end[..at].to_vec();
        drawn.extend(before(need, "✦ "));
        drawn.push(String::new());
        drawn.push("╭────────────────────────────────────────────╮".to_string());
        for call in 0..calls(tall) {
            drawn.push(format!(
                "│ ✓  Shell git push origin branch-{call:02}             │"
            ));
        }
        drawn.push("╰────────────────────────────────────────────╯".to_string());
        drawn.push(String::new());
        drawn.push(format!("✦ {defense}"));
        drawn.extend(end[at + 1..].iter().cloned());
        split(drawn, end.len(), tall)
    };
    let agy_turn = move |need: bool, tall: bool| {
        let end = screen(agy::fixtures::END_OF_TURN);
        let user = end
            .iter()
            .position(|r| r.starts_with("> Write about"))
            .expect("the message");
        let composer = (user..end.len())
            .find(|&i| end[i].starts_with('─'))
            .expect("the composer's rule");
        let mut drawn = end[..user].to_vec();
        drawn.push("> Land it all in the remote main branch.".to_string());
        drawn.push(String::new());
        drawn.extend(before(need, "  "));
        drawn.push(String::new());
        for call in 0..calls(tall) {
            drawn.push(format!(
                "● Ran git push origin branch-{call:02} (ctrl+o to expand)"
            ));
        }
        drawn.push(String::new());
        drawn.push(format!("  {defense}"));
        drawn.push(String::new());
        drawn.extend(end[composer..].iter().cloned());
        split(drawn, end.len(), tall)
    };
    let muse_turn = move |need: bool, tall: bool| {
        let end = screen(muse::fixtures::END_OF_TURN_1_4_10);
        let at = end
            .iter()
            .position(|r| r.starts_with("◆ Hi!"))
            .expect("the reply");
        let mut drawn = end[..at].to_vec();
        drawn.extend(before(need, "◆ "));
        drawn.push(String::new());
        for call in 0..calls(tall) {
            drawn.push(format!(
                "◆ Ran command · Push branch-{call:02} · ✓ · 2.3s · ctrl+o"
            ));
            drawn.push(String::new());
        }
        drawn.push(format!("◆ {defense}"));
        drawn.extend(end[at + 1..].iter().cloned());
        split(drawn, end.len().max(20), tall)
    };
    type Turn = Box<dyn Fn(bool, bool) -> (Vec<String>, Vec<String>)>;
    type Harness = (
        &'static str,
        fn(Vec<Vec<String>>) -> Mock,
        &'static str,
        &'static str,
        Turn,
    );
    let harnesses: [Harness; 3] = [
        (
            "gemini",
            gemini_mock,
            gemini::fixtures::BUSY_THINKING,
            gemini::fixtures::END_OF_TURN,
            Box::new(gemini_turn),
        ),
        (
            "agy",
            agy_mock,
            agy::fixtures::BUSY,
            agy::fixtures::END_OF_TURN,
            Box::new(agy_turn),
        ),
        (
            "muse",
            muse_mock,
            muse::fixtures::BUSY_1_4_10,
            muse::fixtures::END_OF_TURN_1_4_10,
            Box::new(muse_turn),
        ),
    ];
    let landed = |lines: &[String]| {
        lines
            .iter()
            .any(|l| l.starts_with("DONE ") && l.contains(&format!(" rule={RULE_LAND} ")))
    };
    let nudged = |lines: &[String]| {
        lines.iter().any(|l| {
            l.contains(&format!(
                " rule={} ",
                crate::supervise::policy::turn_end::RULE_LAND_NUDGE
            ))
        })
    };
    for (program, mock, busy, work, turn) in harnesses {
        let (busy, work) = (screen(busy), screen(work));
        for tall in [false, true] {
            let at = if tall {
                "above the screen"
            } else {
                "on the screen"
            };
            let (rows, _) = turn(true, tall);
            let reader = program_reader(program, &rows);
            assert_eq!(
                reader.said_tail(&rows).as_deref(),
                Some(defense),
                "{program} {at}: the last reply"
            );
            assert_eq!(
                reader.turn_whole(&rows),
                !tall,
                "{program} {at}: the turn's start"
            );
            let (lines, needs) = tall_landing(mock, &busy, &work, turn(true, tall), true);
            assert!(!landed(&lines), "{program} {at}: {lines:#?}");
            assert!(nudged(&lines), "{program} {at}: the nudge: {lines:#?}");
            assert!(
                needs
                    .first()
                    .is_some_and(|c| c.starts_with("set attention owner=needs ")
                        && c.contains("the signing credential")),
                "{program} {at}: the need raised: {needs:#?}\n{lines:#?}"
            );
            let (lines, needs) = tall_landing(mock, &busy, &work, turn(false, tall), true);
            assert!(landed(&lines), "{program} {at}: {lines:#?}");
            assert!(needs.is_empty(), "{program} {at}: {needs:#?}");
        }
        let (lines, _) = tall_landing(mock, &busy, &work, turn(false, true), false);
        assert!(!landed(&lines), "{program}: {lines:#?}");
        assert!(nudged(&lines), "{program}: the nudge: {lines:#?}");
    }
}

fn program_reader(program: &str, rows: &[String]) -> &'static dyn aterm_phase::ScreenReader {
    let name = if program == "gemini" { "node" } else { program };
    aterm_phase::identify(Some(name), rows)
}

#[test]
fn a_landing_is_published_once_and_a_restarted_loop_marks_nothing() {
    let root = crate::supervise::ScratchDir::new("task", "marker");
    let tasks = root.join("tasks");
    let text = transcript_of(&landing_rows(&landed_reply()));
    let host = TaskHost::reading(&root.join("t.jsonl"), &[&text]);
    let run_on = |screens: Vec<Vec<String>>, task: Option<(u64, String)>| {
        let held = screens.len() > 1;
        let mut m = Mock::new(screens);
        landing(&mut m);
        m.turn_releases = m.turn_releases.filter(|_| held);
        m.vanish_after = Some(4);
        m.task = task;
        let _ = tasked_watch(&mut m, &host, &tasks, None);
        m
    };
    let run = |task: Option<(u64, String)>| run_on(landing_screens(&landed_reply()), task);
    let m = run(working_task());
    let path = tasks.join("s-1-1.result.md");
    assert_eq!(
        task_marks(&m),
        [done_mark("landed", landed_result().len(), &path)]
    );
    assert_eq!(
        std::fs::read_to_string(&path).expect("snapshot"),
        landed_result()
    );
    let done = m.task.clone();
    std::fs::write(&path, "the snapshot as the first loop left it").expect("mark it");
    let again = run_on(vec![ended(&landed_reply())], done.clone());
    assert_eq!(again.task_requests, ["task"], "read, never marked");
    assert_eq!(again.turns, 0, "nothing typed into the landed worker");
    assert_eq!(again.task, done, "the record stands");
    assert_eq!(
        std::fs::read_to_string(&path).expect("snapshot"),
        "the snapshot as the first loop left it",
        "and the snapshot"
    );
    let unanswered = run(working_task());
    assert_eq!(
        task_marks(&unanswered),
        [done_mark("landed", landed_result().len(), &path)]
    );
    let undefended = run(done.clone());
    assert!(
        undefended
            .task_requests
            .iter()
            .any(|r| r.starts_with("task mark working ")),
        "reopened: {:?}",
        undefended.task_requests
    );
    assert_eq!(undefended.turns, 1, "Land's instruction");
    assert!(
        undefended
            .task_requests
            .last()
            .is_some_and(|r| r.starts_with("task mark done why=landed ")),
        "{:?}",
        undefended.task_requests
    );
    let stalled = |why: &str| {
        Some((
            2,
            format!("state=stalled from=s-0a1b since_ms=0 result_bytes=0 why={why}"),
        ))
    };
    let held = run(stalled("held"));
    assert_eq!(
        task_marks(&held),
        [done_mark(
            "landed",
            landed_result().len(),
            &tasks.join("s-1-2.result.md")
        )]
    );
    let shell = run(stalled("shell"));
    assert!(task_marks(&shell).is_empty(), "{:?}", shell.task_requests);
    let mut m = Mock::new(landing_screens(&landed_reply()));
    landing(&mut m);
    m.vanish_after = Some(4);
    m.task = working_task();
    let opts = SuperviseOpts {
        policy: owners_policy(),
        idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
        tasks: None,
        ..auto(30)
    };
    let mut s = session(&mut m, Some("@s-1".to_string()));
    s.set_clock(TEST_NOW, PDT);
    s.set_zone_lookup(test_zones);
    let mut out: Vec<u8> = Vec::new();
    let _ = s.watch(&opts, &mut out);
    drop(s);
    assert!(m.task_requests.is_empty(), "{:?}", m.task_requests);
    assert_eq!(m.task, working_task());
}

#[test]
fn a_landing_read_before_the_reply_was_written_waits_for_the_turns_end() {
    let root = crate::supervise::ScratchDir::new("task", "lag");
    let tasks = root.join("tasks");
    let mut rows = landing_rows(&landed_reply());
    let lagging = transcript_of(&rows[..rows.len() - 1]);
    rows.push(
        r#"{"parentUuid":"a-land","isSidechain":false,"type":"system","subtype":"turn_duration","durationMs":9000,"uuid":"td","sessionId":"0b2299fa","version":"2.1.301"}"#
            .to_string(),
    );
    let whole = transcript_of(&rows);
    let run = |transcript: &Path| {
        let host = TaskHost::reading(transcript, &[&lagging]);
        let mut m = Mock::new(landing_screens(&landed_reply()));
        landing(&mut m);
        m.vanish_after = Some(4);
        m.task = working_task();
        let _ = tasked_watch(&mut m, &host, &tasks, None);
        m
    };
    let on_disk = root.join("t.jsonl");
    std::fs::write(&on_disk, &whole).expect("the transcript as Claude Code finished it");
    let path = tasks.join("s-1-1.result.md");
    let m = run(&on_disk);
    assert_eq!(
        task_marks(&m),
        [done_mark("landed", landed_result().len(), &path)]
    );
    assert_eq!(
        std::fs::read_to_string(&path).expect("snapshot"),
        landed_result()
    );
    let m = run(&root.join("gone.jsonl"));
    assert_eq!(task_marks(&m), [done_mark("landed", 0, &path)]);
    assert_eq!(std::fs::read_to_string(&path).expect("snapshot"), "");
}

fn stamped(row: String, at: i64) -> String {
    let at = aterm_types::rfc3339::format_rfc3339(u64::try_from(at).expect("a stamp"));
    row.replacen('{', &format!(r#"{{"timestamp":"{at}","#), 1)
}

fn end_row_at(at: i64) -> String {
    stamped(
        r#"{"parentUuid":"p","isSidechain":false,"type":"system","subtype":"turn_duration","durationMs":9000,"uuid":"td","sessionId":"0b2299fa","version":"2.1.301"}"#
            .to_string(),
        at,
    )
}

fn notification_at(at: i64) -> String {
    stamped(
        task_row(
            "user",
            "u-woke",
            "",
            "<task-notification>\n<task-id>b1</task-id>\n<status>completed</status>\n\
             </task-notification>",
        ),
        at,
    )
}

#[test]
fn the_landing_points_showing_places_the_turn_snapshotted() {
    let root = crate::supervise::ScratchDir::new("task", "shown");
    let tasks = root.join("tasks");
    let run = |woke_at: i64| {
        let first = vec![
            stamped(
                task_row("user", "u-task", "", "Fix the parser."),
                TEST_NOW - 300,
            ),
            stamped(
                task_row("assistant", "a-work", "msg_1", WORK),
                TEST_NOW - 200,
            ),
            end_row_at(TEST_NOW - 200),
            stamped(task_row("user", "u-land", "", LAND), TEST_NOW - 150),
            stamped(
                task_row(
                    "assistant",
                    "a-held",
                    "msg_2",
                    "Started the review in the background.\nATERM-TASK-DONE",
                ),
                TEST_NOW - 110,
            ),
            end_row_at(TEST_NOW - 110),
            notification_at(woke_at),
        ];
        let mut whole = first.clone();
        whole.extend([
            stamped(
                task_row(
                    "assistant",
                    "a-wake",
                    "msg_3",
                    "The review found nothing more.\nATERM-TASK-DONE",
                ),
                woke_at + 5,
            ),
            end_row_at(woke_at + 5),
        ]);
        let on_disk = root.join(format!("t-{woke_at}.jsonl"));
        std::fs::write(&on_disk, transcript_of(&whole)).expect("the transcript on disk");
        let host = TaskHost::reading(&on_disk, &[&transcript_of(&first)]);
        let mut m = Mock::new(landing_screens(&landed_reply()));
        landing(&mut m);
        m.vanish_after = Some(4);
        m.task = working_task();
        let _ = tasked_watch(&mut m, &host, &tasks, None);
        let path = tasks.join("s-1-1.result.md");
        (
            task_marks(&m),
            std::fs::read_to_string(&path).expect("snapshot"),
            path,
        )
    };
    let (marks, snapshot, path) = run(TEST_NOW - 30);
    let report = format!("{WORK}\n\nThe review found nothing more.");
    assert_eq!(snapshot, report);
    assert_eq!(marks, [done_mark("landed", report.len(), &path)]);
    let (marks, snapshot, path) = run(TEST_NOW + 30);
    let report = format!("{WORK}\n\nStarted the review in the background.");
    assert_eq!(snapshot, report);
    assert_eq!(marks, [done_mark("landed", report.len(), &path)]);
}

#[test]
fn a_persons_own_exchange_marks_no_task() {
    let root = crate::supervise::ScratchDir::new("task", "left");
    let tasks = root.join("tasks");
    let said = format!("Hi! What would you like me to work on?\n{FINISHED}");
    let quick = |said: &str| {
        let mut r = rows(&[&format!("⏺ {said}"), ""]);
        r.extend(composer("  ⏵⏵ bypass permissions on (shift+tab to cycle)"));
        r
    };
    let greeting = transcript_of(&[
        task_row("user", "u-hi", "", "hi"),
        task_row("assistant", "a-hi", "msg_1", &said),
    ]);
    let landed = transcript_of(&landing_rows(&landed_reply()));
    let run = |person: bool| {
        let journal = root.join(format!("journal-{person}.jsonl"));
        let (screens, text) = if person {
            (vec![busy_screen(), quick(&said)], &greeting)
        } else {
            (
                vec![
                    busy_screen(),
                    quick(&said),
                    busy_screen(),
                    quick(&landed_reply()),
                ],
                &landed,
            )
        };
        let host = TaskHost::reading(&root.join("t.jsonl"), &[text]);
        let mut m = Mock::new(screens);
        landing(&mut m);
        m.vanish_after = Some(8);
        if person {
            m.human = vec!["2000"];
        }
        m.task = working_task();
        let mut out: Vec<u8> = Vec::new();
        let opts = SuperviseOpts {
            policy: SupervisorConfig {
                human_grace_s: 0,
                ..owners_policy()
            },
            journal: Some(journal.clone()),
            idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
            tasks: Some(tasks.clone()),
            ..auto(1)
        };
        let mut s = virtual_session(&mut m, Some("@s-1".to_string()));
        s.set_turn_end_timing(TurnEndTiming {
            short_backoff: Duration::from_millis(30),
            short_backoff_max: Duration::from_millis(60),
            ..TurnEndTiming::default()
        });
        let _ = s.watch(&opts, &mut out);
        drop(s);
        let left = journal_records(&journal)
            .0
            .into_iter()
            .filter(|r| r.line.starts_with("LEFT "))
            .count();
        (m, left)
    };
    let (m, left) = run(true);
    assert_eq!(left, 1, "the person's exchange");
    assert_eq!(m.task_requests, ["task"], "the loop's first read, no mark");
    let (m, left) = run(false);
    assert_eq!(left, 0);
    assert_eq!(task_marks(&m).len(), 1, "{:?}", m.task_requests);
}

#[test]
fn someone_elses_turn_opens_the_task_again_and_its_landing_is_published_again() {
    const HALF: &str = "Half of the README is in.";
    const AGAIN: &str = "Updated the README and pushed it.\nNothing left: the README is on the \
                         remote main branch.";
    let root = crate::supervise::ScratchDir::new("task", "reopen");
    let tasks = root.join("tasks");
    let first = landing_rows(&landed_reply());
    let one = transcript_of(&first);
    let first_mark = done_mark(
        "landed",
        landed_result().len(),
        &tasks.join("s-1-1.result.md"),
    );
    let host = TaskHost::reading(&root.join("t.jsonl"), &[&one]);
    let mut screens = landing_screens(&landed_reply());
    screens.extend([busy_screen(), ended(HALF)]);
    let mut m = Mock::new(screens);
    landing(&mut m);
    m.vanish_after = Some(4);
    m.task = working_task();
    let lines = tasked_watch(&mut m, &host, &tasks, None);
    assert_eq!(
        task_marks(&m),
        [first_mark.clone(), "task mark working why=turn".to_string()],
        "{lines:#?}"
    );
    let again_reply = AGAIN.to_string();
    let mut rows = first.clone();
    rows.extend([
        task_row("user", "u-more", "", "Also update the README."),
        task_row("assistant", "a-more", "msg_3", HALF),
        task_row("user", "u-nudge", "", LAND_NUDGE),
        task_row("assistant", "a-again", "msg_4", &again_reply),
    ]);
    let two = transcript_of(&rows);
    let relanded = |reopens: Option<usize>| {
        let host = TaskHost::reading(&root.join("t.jsonl"), &[&one, &two]);
        let mut screens = landing_screens(&landed_reply());
        screens.extend([
            busy_screen(),
            ended(HALF),
            busy_screen(),
            ended(&again_reply),
        ]);
        let mut m = Mock::new(screens);
        m.history_served();
        m.autopilot = Some("land");
        m.turn_gates = vec![1, 5];
        m.vanish_after = Some(4);
        m.task = working_task();
        m.task_reopens_at = reopens;
        let lines = tasked_watch(&mut m, &host, &tasks, None);
        (m, lines)
    };
    let second = tasks.join("s-1-3.result.md");
    let again_result = format!("{HALF}\n\n{AGAIN}");
    let (m, lines) = relanded(Some(5));
    assert_eq!(
        task_marks(&m),
        [
            first_mark.clone(),
            done_mark("landed", again_result.len(), &second)
        ],
        "{lines:#?}"
    );
    assert_eq!(
        std::fs::read_to_string(&second).expect("snapshot"),
        again_result
    );
    assert!(
        !tasks.join("s-1-1.result.md").exists(),
        "the new landing prunes the previous result pair"
    );
    let (m, lines) = relanded(None);
    assert_eq!(
        task_marks(&m),
        [
            first_mark,
            "task mark working why=turn".to_string(),
            done_mark("landed", again_result.len(), &second),
        ],
        "{lines:#?}"
    );
    let host = TaskHost::reading(&root.join("t.jsonl"), &[&one]);
    let mut m = Mock::new(landing_screens(&landed_reply()));
    landing(&mut m);
    m.vanish_after = Some(4);
    m.task = working_task();
    let _ = tasked_watch(&mut m, &host, &tasks, None);
    assert_eq!(
        m.task_requests.len(),
        3,
        "the loop's first read, the read at the landing and the mark: {:?}",
        m.task_requests
    );
}

#[test]
fn the_upgrades_ready_after_the_landing_leaves_the_snapshot_and_the_record() {
    let root = crate::supervise::ScratchDir::new("task", "ready");
    let tasks = root.join("tasks");
    let done = landing_rows(&landed_reply());
    let mut later = done.clone();
    later.extend([
        task_row(
            "user",
            "u-ready",
            "",
            "[aterm harness] A newer Claude Code is installed.",
        ),
        task_row("assistant", "a-ready", "msg_r", "READY"),
    ]);
    let run = |ready: bool| {
        let host = Arc::new(TaskHost {
            path: root.join("t.jsonl"),
            texts: std::sync::Mutex::new(
                [transcript_of(&done), transcript_of(&later)]
                    .into_iter()
                    .collect(),
            ),
            ready,
            ..TaskHost::default()
        });
        let mut screens = landing_screens(&landed_reply());
        screens.extend([
            // The point read again: where the host takes its step.
            ended(&landed_reply()),
            busy_screen(),
            ended("READY"),
        ]);
        let mut m = Mock::new(screens);
        landing(&mut m);
        m.vanish_after = Some(4);
        m.cursor_on_caret = true;
        m.task = working_task();
        let lines = tasked_watch(&mut m, &host, &tasks, None);
        (m, lines, host)
    };
    let path = tasks.join("s-1-1.result.md");
    let (m, lines, host) = run(true);
    assert_eq!(
        host.steps.load(Ordering::SeqCst),
        1,
        "the notice: {lines:#?}"
    );
    assert_eq!(
        task_marks(&m),
        [done_mark("landed", landed_result().len(), &path)],
        "{lines:#?}"
    );
    assert_eq!(
        m.task_requests.len(),
        3,
        "the loop's first read, the read at the landing and the mark: {:?}",
        m.task_requests
    );
    assert_eq!(
        host.asks.load(Ordering::SeqCst),
        1,
        "read once, at the landing"
    );
    assert_eq!(
        std::fs::read_to_string(&path).expect("snapshot"),
        landed_result()
    );
    let (m, lines, _) = run(false);
    assert_eq!(
        task_marks(&m),
        [
            done_mark("landed", landed_result().len(), &path),
            "task mark working why=turn".to_string(),
        ],
        "{lines:#?}"
    );
}

#[test]
fn the_landing_is_published_before_the_hosts_step_takes_its_point() {
    let root = crate::supervise::ScratchDir::new("task", "first");
    let tasks = root.join("tasks");
    let done = landing_rows(&landed_reply());
    let mut later = done.clone();
    later.extend([
        task_row(
            "user",
            "u-host",
            "",
            "[aterm harness] A newer Claude Code is installed.",
        ),
        task_row("assistant", "a-ready", "msg_r", "READY"),
    ]);
    let host = Arc::new(TaskHost {
        path: root.join("t.jsonl"),
        texts: std::sync::Mutex::new([transcript_of(&done)].into_iter().collect()),
        stepped: Some(transcript_of(&later)),
        wanting: AtomicBool::new(true),
        passes: std::sync::atomic::AtomicUsize::new(1),
        ..TaskHost::default()
    });
    let mut screens = landing_screens(&landed_reply());
    screens.extend([busy_screen(), ended("READY")]);
    let mut m = Mock::new(screens);
    landing(&mut m);
    m.vanish_after = Some(4);
    m.cursor_on_caret = true;
    m.task = working_task();
    let lines = tasked_watch(&mut m, &host, &tasks, None);
    assert_eq!(
        host.steps.load(Ordering::SeqCst),
        1,
        "the host's turn at the landing point: {lines:#?}"
    );
    assert_eq!(
        *host.read_at.lock().unwrap(),
        [0],
        "read once, before the step"
    );
    let path = tasks.join("s-1-1.result.md");
    assert_eq!(
        task_marks(&m),
        [done_mark("landed", landed_result().len(), &path)],
        "{lines:#?}"
    );
    assert_eq!(
        std::fs::read_to_string(&path).expect("snapshot"),
        landed_result()
    );
    assert_eq!(
        crate::harness::result::report(
            &transcript_of(&later),
            crate::harness::result::DoneBy::Turn
        )
        .map(|r| r.text),
        Ok("READY".to_string())
    );
}

#[test]
fn a_sleep_after_the_landing_marks_nothing_more_at_the_hosts_short_turn() {
    let root = crate::supervise::ScratchDir::new("task", "sleep");
    let tasks = root.join("tasks");
    let done = landing_rows(&landed_reply());
    let mut later = done.clone();
    later.extend([
        task_row(
            "user",
            "u-ready",
            "",
            "[aterm harness] A newer Claude Code is installed.",
        ),
        task_row("assistant", "a-ready", "msg_r", "READY"),
    ]);
    let host = Arc::new(TaskHost {
        path: root.join("t.jsonl"),
        texts: std::sync::Mutex::new(
            [transcript_of(&done), transcript_of(&later)]
                .into_iter()
                .collect(),
        ),
        ready: true,
        ..TaskHost::default()
    });
    let ready: Vec<String> = ended("READY")
        .into_iter()
        .map(|r| r.replace("Worked for 12m 2s", "Worked for 2s"))
        .collect();
    let mut screens = landing_screens(&landed_reply());
    screens.extend([ended(&landed_reply()), busy_screen(), ready]);
    let mut m = Mock::new(screens);
    landing(&mut m);
    m.vanish_after = Some(4);
    m.cursor_on_caret = true;
    m.task = working_task();
    let opts = SuperviseOpts {
        policy: owners_policy(),
        idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
        tasks: Some(tasks.clone()),
        ..auto(30)
    };
    let mut out: Vec<u8> = Vec::new();
    let mut s = session(&mut m, Some("@s-1".to_string()));
    s.set_clock(TEST_NOW, PDT);
    s.set_zone_lookup(test_zones);
    *host.sleep.lock().unwrap() = Some(s.clock_jump_handle());
    let _ = s.watch(&opts, &mut out);
    let woke = s.wake_settle.is_some();
    drop(s);
    let lines = String::from_utf8(out).expect("utf-8");
    assert!(woke, "the Mac slept after the landing: {lines}");
    assert_eq!(host.steps.load(Ordering::SeqCst), 1, "the notice: {lines}");
    let path = tasks.join("s-1-1.result.md");
    assert_eq!(
        task_marks(&m),
        [done_mark("landed", landed_result().len(), &path)],
        "{lines}"
    );
    assert_eq!(m.turns, 1, "Land's instruction alone: {lines}");
    assert_eq!(
        std::fs::read_to_string(&path).expect("snapshot"),
        landed_result()
    );
}

#[test]
fn an_older_builds_done_record_is_opened_again_and_the_landing_published() {
    let root = crate::supervise::ScratchDir::new("task", "older-done");
    let tasks = root.join("tasks");
    let text = transcript_of(&landing_rows(&landed_reply()));
    let host = TaskHost::reading(&root.join("t.jsonl"), &[&text]);
    let run = |task: Option<(u64, String)>| {
        let mut m = Mock::new(landing_screens(&landed_reply()));
        landing(&mut m);
        m.vanish_after = Some(4);
        m.task = task;
        let lines = tasked_watch(&mut m, &host, &tasks, None);
        (task_marks(&m), lines)
    };
    for why in ["marker", "done-check"] {
        let older = Some((
            2,
            format!("state=done from=ctl since_ms=0 result_bytes=17 why={why}"),
        ));
        let (marks, lines) = run(older);
        let path = tasks.join("s-1-3.result.md");
        assert_eq!(
            marks.last(),
            Some(&done_mark("landed", landed_result().len(), &path)),
            "why={why}: {marks:#?} {lines:#?}"
        );
        assert!(
            marks.len() >= 2 && marks[0].starts_with("task mark working"),
            "why={why}: opened again first: {marks:#?}"
        );
    }
    let working = Some((
        2,
        "state=working from=s-0a1b since_ms=0 result_bytes=0".to_string(),
    ));
    let (marks, lines) = run(working);
    let path = tasks.join("s-1-2.result.md");
    assert_eq!(
        marks,
        [done_mark("landed", landed_result().len(), &path)],
        "{lines:#?}"
    );
}

#[test]
fn a_loop_started_at_a_published_landing_types_nothing() {
    let root = crate::supervise::ScratchDir::new("task", "held-check");
    let text = transcript_of(&landing_rows(&landed_reply()));
    let host = TaskHost::reading(&root.join("t.jsonl"), &[&text]);
    let record = |why: &str| {
        Some((
            2,
            format!(
                "state=done from=ctl since_ms=0 result_bytes={} why={why} result_path={}",
                landed_result().len(),
                aterm_codec::pct::encode(&root.join("tasks/s-1-1.result.md").to_string_lossy())
            ),
        ))
    };
    let published = record("landed");
    let run = |task: Option<(u64, String)>, word: &'static str| {
        let journal = root.join(format!(
            "journal-{word}-{}.jsonl",
            task.as_ref().map_or(0, |t| t.0)
        ));
        let mut m = Mock::new(vec![ended(&landed_reply())]);
        m.history_served();
        m.autopilot = Some(word);
        m.vanish_after = Some(6);
        m.task = task;
        let opts = SuperviseOpts {
            policy: owners_policy(),
            journal: Some(journal.clone()),
            idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
            tasks: Some(root.join("tasks")),
            ..auto(30)
        };
        let mut out: Vec<u8> = Vec::new();
        let mut s = session(&mut m, Some("@s-1".to_string()));
        s.set_clock(TEST_NOW, PDT);
        s.set_zone_lookup(test_zones);
        s.set_turn_end_timing(TurnEndTiming {
            short_backoff: Duration::from_millis(30),
            short_backoff_max: Duration::from_millis(60),
            ..TurnEndTiming::default()
        });
        let _ = s.watch(&opts, &mut out);
        drop(s);
        let mut lines: Vec<String> = String::from_utf8(out)
            .expect("utf-8")
            .lines()
            .map(str::to_string)
            .collect();
        lines.extend(journal_records(&journal).0.into_iter().map(|r| r.line));
        (m, lines)
    };
    let said = |lines: &[String], kind: &str| {
        lines
            .iter()
            .filter(|l| l.starts_with(kind))
            .collect::<std::collections::BTreeSet<_>>()
            .len()
    };
    let typed = |lines: &[String], rule: &str| {
        lines
            .iter()
            .filter(|l| l.starts_with("CONTINUED ") && l.contains(&format!("rule={rule} ")))
            .collect::<std::collections::BTreeSet<_>>()
            .len()
    };
    let (m, lines) = run(published.clone(), "land");
    assert!(m.stalls >= 6, "the run stayed past the read: {}", m.stalls);
    assert_eq!(said(&lines, "CONTINUED "), 0, "{lines:#?}");
    assert_eq!(said(&lines, "SKIPPED "), 0, "never decided: {lines:#?}");
    assert_eq!(
        said(&lines, "DONE "),
        0,
        "said by the loop before: {lines:#?}"
    );
    assert_eq!(m.task_requests, ["task"], "read once: {lines:#?}");
    assert_eq!(m.task, published, "the record stands");
    let (m, lines) = run(working_task(), "land");
    assert_eq!(typed(&lines, RULE_LAND), 1, "{lines:#?}");
    assert!(task_marks(&m).is_empty(), "{:?}", m.task_requests);
    for why in ["marker", "done-check"] {
        let (_, lines) = run(record(why), "land");
        assert_eq!(typed(&lines, RULE_LAND), 1, "why={why}: {lines:#?}");
    }
    let (m, lines) = run(published, "cruise");
    assert!(typed(&lines, RULE_DONE_CHECK) >= 1, "{lines:#?}");
    assert_eq!(said(&lines, "DONE "), 0, "{lines:#?}");
    assert!(
        !task_marks(&m)
            .iter()
            .any(|r| r.starts_with("task mark done")),
        "{:?}",
        m.task_requests
    );
}

#[test]
fn a_retrys_wall_whose_notice_left_withholds_the_hosts_step() {
    use aterm_phase::prompt::fixtures::{END_529, screen};
    let walled = screen(END_529);
    let left = |rows: &[String]| -> Vec<String> {
        rows.iter()
            .filter(|r| !r.contains("API Error"))
            .cloned()
            .collect()
    };
    let refused: Vec<String> = walled
        .iter()
        .map(|r| {
            if r.contains("API Error") {
                "  ⎿  API Error: 400 due to tool use concurrency issues. Run /rewind to recover \
                 the conversation."
                    .to_string()
            } else {
                r.clone()
            }
        })
        .collect();
    for (label, screens, held) in [
        (
            "the 529's notice left",
            vec![busy_screen(), walled.clone(), left(&walled)],
            true,
        ),
        (
            "work between",
            vec![busy_screen(), walled.clone(), busy_screen(), left(&walled)],
            false,
        ),
        (
            "a refused 400's notice left",
            vec![busy_screen(), refused.clone(), left(&refused)],
            false,
        ),
    ] {
        let host = Arc::new(PointHost {
            wants: true,
            ..PointHost::default()
        });
        let mut m = Mock::new(screens);
        m.vanish_after = Some(3);
        m.stall_sleep = Some(Duration::from_millis(5));
        m.exit_when = Some(|m| m.turns >= 1);
        let opts = SuperviseOpts {
            idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
            ..hosted(30)
        };
        let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
            s.set_turn_end_timing(TurnEndTiming {
                short_backoff: Duration::ZERO,
                ..TurnEndTiming::default()
            });
        });
        let withheld = host.withheld.lock().unwrap().clone();
        let why = format!("{label}: {withheld:?}\n{lines:#?}");
        assert_eq!(
            host.idle.load(Ordering::SeqCst),
            usize::from(!held),
            "{why}"
        );
        if held {
            assert_eq!(m.turns, 0, "{why}");
            assert!(withheld.contains(&crate::supervise::Guard::Wall), "{why}");
        }
    }
}

fn watchdog_after_keep_going(fixture: &str, tick: &str) -> Vec<String> {
    aterm_phase::prompt::fixtures::screen(fixture)
        .into_iter()
        .map(|r| {
            let r = r
                .replace("❯ carry on with the task", "❯ keep going")
                .replace("❯ toolstart", "❯ keep going")
                .replace(" · esc to interrupt", "");
            match r.split_once(" · Retrying in ") {
                Some((head, rest)) => match rest.split_once(" (") {
                    Some((_, clock)) => format!("{head} · Retrying in {tick} ({clock}"),
                    None => r,
                },
                None => r,
            }
        })
        .collect()
}

fn retry_read_as_done(rows: &[String]) -> Vec<String> {
    rows.iter()
        .map(|r| r.replace("attempt 1/300", "attempt one of 300"))
        .collect()
}

fn watchdog_run(
    tag: &str,
    screens: Vec<Vec<String>>,
    grace: u32,
) -> (Mock, Vec<String>, Vec<String>) {
    let (dir, path) = journal_file(tag);
    let mut m = Mock::new(screens);
    m.turn_releases = Some(1);
    m.vanish_after = Some(8);
    m.stall_sleep = Some(Duration::from_millis(20));
    let mut opts = SuperviseOpts {
        journal: Some(path.clone()),
        ..hosted(30)
    };
    opts.policy.human_grace_s = grace;
    let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
        s.set_turn_end_timing(TurnEndTiming {
            take_within: Duration::from_millis(1),
            short_backoff: Duration::ZERO,
            min_work: Duration::ZERO,
            ..TurnEndTiming::default()
        });
    });
    let (records, _) = journal_records(&path);
    let waits = records
        .iter()
        .filter(|r| r.kind == "waiting")
        .map(|r| {
            r.summary
                .split_once(' ')
                .map_or(r.summary.clone(), |(_, why)| why.to_string())
        })
        .collect();
    let _ = std::fs::remove_dir_all(&dir);
    (m, lines, waits)
}

fn typed_acts(lines: &[String]) -> Vec<String> {
    lines
        .iter()
        .filter(|l| l.starts_with("CONTINUED") || l.starts_with("TYPED"))
        .map(|l| l.split(" rule=").nth(1).unwrap_or_default().to_string())
        .collect()
}

#[test]
fn the_watchdogs_retry_row_is_a_turn_in_progress_and_nothing_is_typed_into_it() {
    use aterm_phase::prompt::fixtures::{
        WALL_WATCHDOG_FULLSCREEN_2_1_301, WALL_WATCHDOG_MIDTURN_2_1_301,
        WALL_WATCHDOG_SESSION_STATUSLINE_2_1_300,
    };
    for (name, fixture) in [
        ("inline", WALL_WATCHDOG_SESSION_STATUSLINE_2_1_300),
        ("fullscreen", WALL_WATCHDOG_FULLSCREEN_2_1_301),
        ("mid-turn", WALL_WATCHDOG_MIDTURN_2_1_301),
    ] {
        let ticking: Vec<Vec<String>> = (40..46)
            .map(|s| watchdog_after_keep_going(fixture, &format!("59m {s}s")))
            .collect();
        let run = |tag: &str, retry: Vec<Vec<String>>| {
            let mut screens = vec![
                busy_screen(),
                ended("Fixed the parser; the suite is green."),
            ];
            screens.extend(retry);
            watchdog_run(tag, screens, 120)
        };
        let (m, lines, waits) = run("te-watchdog-busy", ticking.clone());
        assert_eq!(
            typed_acts(&lines),
            ["continue@v1 keep going"],
            "{name}: the first continuation only: {lines:#?}"
        );
        assert_eq!(m.turns, 1, "{name}: {lines:#?}\n{:#?}", m.requests);
        assert!(
            !lines
                .iter()
                .any(|l| l.starts_with("EVENT limited") || l.starts_with("LIMITED")),
            "{name}: no limit: {lines:#?}"
        );
        assert!(
            waits
                .iter()
                .all(|w| w.contains("to take the continue@v1 act")),
            "{name}: no wait but the act's own window: {waits:#?}"
        );
        let old: Vec<Vec<String>> = ticking.iter().map(|r| retry_read_as_done(r)).collect();
        let (m, lines, _) = run("te-watchdog-done-row", old);
        assert!(m.turns >= 2, "{name}: typed into the retry: {lines:#?}");
    }
}

#[test]
fn a_box_the_loop_reads_makes_the_next_esc_a_new_one() {
    use aterm_phase::prompt::fixtures::{BOX_BASH_TOUCH, screen};
    let run = |screens: Vec<Vec<String>>| {
        let mut m = Mock::new(screens);
        m.vanish_after = Some(3);
        m.stall_sleep = Some(Duration::from_millis(5));
        let opts = hosted(30);
        let mut out: Vec<u8> = Vec::new();
        let mut s = session(&mut m, None);
        s.set_clock(TEST_NOW, PDT);
        s.set_zone_lookup(test_zones);
        let _ = s.watch(&opts, &mut out);
        (
            s.turn_end.esc_read(),
            String::from_utf8(out).expect("utf-8"),
        )
    };
    let mut stopped = rows(&[
        "⏺ Working.",
        "  ⎿  Interrupted · What should Claude do instead?",
        "",
    ]);
    stopped.extend(composer("  ⏵⏵ bypass permissions on (shift+tab to cycle)"));
    let (read, out) = run(vec![busy_screen(), stopped.clone()]);
    assert!(read.is_some(), "the control: the Esc read: {out}");
    let (read, out) = run(vec![busy_screen(), stopped, screen(BOX_BASH_TOUCH)]);
    assert!(out.contains("EVENT prompt seq="), "the box was read: {out}");
    assert_eq!(read, None, "the box read since: {out}");
}

#[test]
fn esc_on_the_vendors_retry_is_an_ordinary_turn_a_person_ended() {
    use aterm_phase::prompt::fixtures::{
        WALL_WATCHDOG_FULLSCREEN_2_1_301, WALL_WATCHDOG_FULLSCREEN_ESC_2_1_301,
        WALL_WATCHDOG_MIDTURN_2_1_301, WALL_WATCHDOG_MIDTURN_ESC_2_1_301,
        WALL_WATCHDOG_MIDTURN_SERVED_2_1_301,
    };
    let spinning = |rows: Vec<String>| -> Vec<String> {
        rows.into_iter()
            .map(
                |r| match r.split_once(" Session limit reached · Retrying in ") {
                    Some((glyph, _)) => format!("{glyph} Cooking… (2m 4s · ↓ 1.1k tokens)"),
                    None => r,
                },
            )
            .collect()
    };
    for (name, standing, after, held) in [
        (
            "mid-turn Esc",
            WALL_WATCHDOG_MIDTURN_2_1_301,
            WALL_WATCHDOG_MIDTURN_ESC_2_1_301,
            Some("a person stopped the turn with Esc"),
        ),
        (
            "first-request Esc",
            WALL_WATCHDOG_FULLSCREEN_2_1_301,
            WALL_WATCHDOG_FULLSCREEN_ESC_2_1_301,
            Some("a person is typing"),
        ),
        (
            "served",
            WALL_WATCHDOG_MIDTURN_2_1_301,
            WALL_WATCHDOG_MIDTURN_SERVED_2_1_301,
            None,
        ),
    ] {
        let screens = |turn: &dyn Fn(Vec<String>) -> Vec<String>| -> Vec<Vec<String>> {
            let mut screens = vec![
                busy_screen(),
                ended("Fixed the parser; the suite is green."),
            ];
            screens.extend(
                (40..46).map(|s| turn(watchdog_after_keep_going(standing, &format!("59m {s}s")))),
            );
            screens.push(watchdog_after_keep_going(after, ""));
            screens
        };
        for grace in [120, 0] {
            let (_, retry, retry_waits) = watchdog_run("te-watchdog-esc", screens(&|r| r), grace);
            let (_, plain, plain_waits) =
                watchdog_run("te-watchdog-esc-plain", screens(&spinning), grace);
            assert_eq!(
                (typed_acts(&retry), &retry_waits),
                (typed_acts(&plain), &plain_waits),
                "{name}, grace {grace}: as the ordinary turn: {retry:#?}\n{plain:#?}"
            );
            assert!(
                !retry
                    .iter()
                    .any(|l| l.starts_with("EVENT limited") || l.starts_with("LIMITED")),
                "{name}: no limit: {retry:#?}"
            );
            assert!(
                retry_waits
                    .iter()
                    .all(|w| !w.contains("resets") && !w.contains("by itself")),
                "{name}: no wait on a reset: {retry_waits:#?}"
            );
            if let Some(held) = held.filter(|_| grace > 0) {
                assert!(
                    retry_waits.iter().any(|w| w.ends_with(held)),
                    "{name}: the person's grace: {retry_waits:#?}\n{retry:#?}"
                );
            }
        }
        let old = screens(&|r| retry_read_as_done(&r));
        let (_, old, _) = watchdog_run("te-watchdog-esc-old", old, 0);
        let (_, plain, _) = watchdog_run("te-watchdog-esc-plain", screens(&spinning), 0);
        assert_ne!(typed_acts(&old), typed_acts(&plain), "{name}: {old:#?}");
    }
}

#[test]
fn the_stalled_streams_retry_in_a_narrow_pane_is_typed_nothing() {
    let rule = "─".repeat(36);
    let after_keep_going = |status: &[String]| {
        let mut r = rows(&["❯ keep going", ""]);
        r.extend(status.iter().cloned());
        r.extend(rows(&[
            "",
            &rule,
            "❯",
            &rule,
            "  ⏵⏵ bypass permissions on (shift+tab",
        ]));
        r
    };
    let run = |tag: &str, retry: Vec<Vec<String>>| {
        let mut screens = vec![
            busy_screen(),
            ended("Fixed the parser; the suite is green."),
        ];
        screens.extend(retry);
        watchdog_run(tag, screens, 120)
    };
    let ticking: Vec<Vec<String>> = (30..36)
        .rev()
        .map(|s| {
            after_keep_going(&rows(&[
                "✻",
                &format!("Waiting for API · will retry in {s}s"),
                "response       · check your network",
            ]))
        })
        .collect();
    let (m, lines, _) = run("te-stalled-retry", ticking);
    assert_eq!(
        typed_acts(&lines),
        ["continue@v1 keep going"],
        "the first continuation only: {lines:#?}"
    );
    assert_eq!(m.turns, 1, "{lines:#?}");
    let done = vec![after_keep_going(&rows(&["✻ Worked for 3m 2s · done 4:31 PM"])); 6];
    let (m, lines, _) = run("te-stalled-retry-done", done);
    assert!(m.turns >= 2, "the done row is an idle point: {lines:#?}");
}

/// A host that keeps what the loop tells it of the account's usage.
#[derive(Debug, Default)]
struct UsageHost {
    told: std::sync::Mutex<Vec<UsageSeen>>,
}

impl IdleHost for UsageHost {
    fn wants(&self) -> bool {
        false
    }
    fn at_idle(&self) -> Option<HostStep> {
        None
    }
    fn owns_turn_end(&self) -> bool {
        false
    }
    fn usage_seen(&self, seen: &UsageSeen) {
        self.told.lock().unwrap().push(seen.clone());
    }
}

#[test]
fn what_the_screen_says_of_usage_is_told_to_the_host_once_per_change() {
    use aterm_phase::prompt::fixtures::{CREDITS_IN_USE_2_1_300, CREDITS_STARTED_2_1_300};
    let run = |screens: Vec<Vec<String>>| {
        let host = Arc::new(UsageHost::default());
        let mut m = Mock::new(screens);
        m.vanish_after = Some(3);
        m.stall_sleep = Some(Duration::from_millis(5));
        let opts = SuperviseOpts {
            idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
            ..hosted(30)
        };
        let (lines, _) = watch_lines(&mut m, &opts);
        let told = host.told.lock().unwrap().clone();
        (lines, told)
    };
    let busy_on_credits = {
        let mut r = busy_screen();
        let at = r.len() - 1;
        r[at] = format!("{:<95}Now using usage credits", r[at]);
        r
    };
    let started = aterm_phase::prompt::fixtures::screen(CREDITS_STARTED_2_1_300);
    let lasting = aterm_phase::prompt::fixtures::screen(CREDITS_IN_USE_2_1_300);
    let (lines, told) = run(vec![
        busy_screen(),
        busy_on_credits,
        started.clone(),
        lasting.clone(),
        lasting,
    ]);
    let credits: Vec<Option<bool>> = told
        .iter()
        .map(|t| t.credits.as_ref().map(|c| c.started.is_some()))
        .collect();
    assert_eq!(
        credits,
        [None, Some(false), Some(true), Some(false)],
        "{told:#?}\n{lines:#?}"
    );
    assert!(
        lines.iter().any(|l| l.starts_with("EVENT idle"))
            && !lines
                .iter()
                .any(|l| l.starts_with("EVENT limited") || l.starts_with("LIMITED")),
        "an ordinary turn end, no wall: {lines:#?}"
    );
    let (_, told) = run(vec![
        busy_screen(),
        ended("Fixed the parser; the suite is green."),
    ]);
    assert_eq!(told, [UsageSeen::default()], "{told:#?}");
}

#[test]
fn an_off_session_has_the_harnesss_usage_panel_closed_before_the_park() {
    let (dir, ledger) = ledger_at("probe-off");
    let mut m = panel_left_owed(&ledger);
    assert_eq!(world(&mut m).at, PanelAt::Open);
    let before = m.writes();

    m.autopilot = Some("off");
    m.exit_when = Some(|m| m.metas >= 3);
    let next = ProbeHost::probing(true);
    let started = Instant::now();
    let (lines, journal) = probe_run(
        "te-probe-off-next",
        &mut m,
        &next,
        &ledger,
        probe_policy(1),
        |_| {},
    );
    assert_eq!(probe_writes(&m), ["/usage", "esc"], "{lines:#?}");
    assert_eq!(world(&mut m).at, PanelAt::Rest, "{lines:#?}");
    let esc = world(&mut m).probes.last().expect("the Escape").at;
    assert!(
        esc.duration_since(started) >= Duration::from_secs(1),
        "one grace in the loop's own sight: {:?}",
        esc.duration_since(started)
    );
    assert!(
        journal.iter().any(|l| l.starts_with("PARKED seq=")),
        "{journal:#?}"
    );
    let escaped = m
        .requests
        .iter()
        .rposition(|r| verb_of(r) == "key" && r.ends_with(" esc"))
        .unwrap_or_else(|| panic!("the Escape: {:#?}", m.requests));
    let step = format!("timeout {}", OFF_STEP.as_millis());
    let parked = m
        .requests
        .iter()
        .position(|r| r.starts_with("await seq ") && r.ends_with(&step))
        .unwrap_or_else(|| panic!("the park's step: {:#?}", m.requests));
    assert!(escaped < parked, "{:#?}", m.requests);
    assert_eq!(
        m.writes()[before.len()..],
        ["esc"],
        "the Escape alone: {:#?}",
        m.requests
    );
    assert!(next.said("probe").is_empty(), "no probe while parked");
    let _ = std::fs::remove_dir_all(&dir);
}

fn panel_left_owed(ledger: &Path) -> Mock {
    let host = ProbeHost::probing(true);
    let mut m = probe_world(&[panel_fx::USAGE_REFRESHING], &host);
    let stop = Arc::new(AtomicBool::new(false));
    world(&mut m).stop_at_read = Some((2, Arc::clone(&stop)));
    let opts = SuperviseOpts {
        idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
        ledger: Some(ledger.to_path_buf()),
        policy: probe_policy(120),
        ..SuperviseOpts::hosted_with(&owners_policy())
    };
    let mut out: Vec<u8> = Vec::new();
    {
        let mut s = session(&mut m, None);
        s.set_clock(PROBE_NOW, PDT);
        s.set_zone_lookup(test_zones);
        probe_waits(&mut s);
        assert_eq!(s.run_hosted(&opts, stop, &mut out), Ok(()));
    }
    assert_eq!(world(&mut m).at, PanelAt::Open, "the panel stands");
    m.stalls = 0;
    m.metas = 0;
    m.status_reads = 0;
    m
}

struct OffPanelRun {
    m: Mock,
    /// The next loop's requests.
    requests: Vec<String>,
    /// The next loop's writes of the probe's ([`probe_writes`]).
    writes: Vec<&'static str>,
    journal: Vec<String>,
    /// The ledger still owes the panel's close.
    owed: bool,
}

fn off_panel_run(
    tag: &str,
    word: &'static str,
    suspect: bool,
    input: &'static str,
    esc: fn(&mut PanelWorld),
    exit: fn(&Mock) -> bool,
) -> OffPanelRun {
    let (dir, ledger) = ledger_at(tag);
    let mut m = panel_left_owed(&ledger);
    let (requests_before, writes_before) = (m.requests.len(), probe_writes(&m).len());
    m.autopilot = Some(word);
    m.input = input;
    m.exit_when = Some(exit);
    esc(world(&mut m));
    let next = ProbeHost::probing(true);
    next.relaunches.store(true, Ordering::SeqCst);
    let policy = SupervisorConfig {
        stall_term: true,
        stall_term_after_s: 60,
        ..probe_policy(0)
    };
    let (_, journal) = probe_run(tag, &mut m, &next, &ledger, policy, |s| {
        s.stall_suspect = suspect;
        if suspect {
            s.seeds_learned_state("a stall suspected before the run");
        }
    });
    let owed = crate::supervise::approvals::probe_owed(
        &std::fs::read_to_string(&ledger).unwrap_or_default(),
        None,
    );
    let _ = std::fs::remove_dir_all(&dir);
    let requests = m.requests[requests_before..].to_vec();
    let writes = probe_writes(&m)[writes_before..].to_vec();
    OffPanelRun {
        m,
        requests,
        writes,
        journal,
        owed,
    }
}

fn esc_takes(_: &mut PanelWorld) {}

fn esc_refused_unread(w: &mut PanelWorld) {
    w.esc_replies = [err(
        "busy input-unread bytes=1 wait_ms=1500 input=pending (the program has not read input \
         queued 1s ago; retry in a moment)",
    )]
    .into();
}

fn esc_halted(w: &mut PanelWorld) {
    w.esc_replies = [err("halted by the owner")].into();
}

fn esc_fails_once(w: &mut PanelWorld) {
    w.esc_fails = 1;
}

fn sent(requests: &[String], verb: &str) -> bool {
    requests.iter().any(|r| verb_of(r) == verb)
}

#[test]
fn an_off_session_with_a_stall_suspected_parks_and_leaves_the_panel_owed() {
    let off = off_panel_run(
        "probe-off-stall",
        "off",
        true,
        INPUT_STALLED,
        esc_takes,
        |m| m.metas >= 3,
    );
    assert!(
        off.journal.iter().any(|l| l.starts_with("PARKED seq=")),
        "{:#?}",
        off.journal
    );
    assert!(
        !off.journal.iter().any(|l| l.starts_with("FROZEN")),
        "{:#?}",
        off.journal
    );
    assert!(!sent(&off.requests, "restart"), "{:#?}", off.requests);
    assert!(off.writes.is_empty(), "no Escape: {:#?}", off.requests);
    let mut m = off.m;
    assert_eq!(world(&mut m).at, PanelAt::Open, "the panel stands");
    assert!(off.owed, "owed until the word turns");

    let cruise = off_panel_run(
        "probe-cruise-stall",
        "cruise",
        true,
        INPUT_STALLED,
        esc_takes,
        |m| m.requests.iter().any(|r| verb_of(r) == "restart"),
    );
    assert!(
        cruise.journal.iter().any(|l| l.starts_with("FROZEN seq=")),
        "{:#?}",
        cruise.journal
    );
    assert!(sent(&cruise.requests, "restart"), "{:#?}", cruise.requests);
    assert!(
        !cruise.journal.iter().any(|l| l.starts_with("PARKED")),
        "{:#?}",
        cruise.journal
    );
}

#[test]
fn an_off_sessions_refused_escape_is_its_one_close() {
    let off = off_panel_run(
        "probe-off-refused",
        "off",
        false,
        INPUT_CLEAR,
        esc_refused_unread,
        |m| m.metas >= 3,
    );
    assert_eq!(off.writes, ["esc"], "one Escape: {:#?}", off.requests);
    assert!(
        off.journal
            .iter()
            .any(|l| l.starts_with("REFUSED") && l.contains("ERR busy input-unread")),
        "{:#?}",
        off.journal
    );
    assert!(
        off.journal.iter().any(|l| l.starts_with("PARKED seq=")),
        "{:#?}",
        off.journal
    );
    assert!(
        !off.journal.iter().any(|l| l.starts_with("FROZEN")),
        "{:#?}",
        off.journal
    );
    let mut m = off.m;
    assert_eq!(world(&mut m).at, PanelAt::Open, "the panel stands");
    assert!(off.owed, "owed until the word turns");

    let cruise = off_panel_run(
        "probe-cruise-refused",
        "cruise",
        false,
        INPUT_CLEAR,
        esc_refused_unread,
        |_| false,
    );
    assert_eq!(cruise.writes, ["esc", "esc"], "{:#?}", cruise.requests);
    let mut m = cruise.m;
    assert_ne!(world(&mut m).at, PanelAt::Open, "closed");
    assert!(!cruise.owed);
}

#[test]
fn an_off_sessions_one_close_promises_no_second_escape_and_waits_on_no_hold() {
    let parked = |run: &OffPanelRun| run.journal.iter().any(|l| l.starts_with("PARKED seq="));
    let off = off_panel_run(
        "probe-off-failed",
        "off",
        false,
        INPUT_CLEAR,
        esc_fails_once,
        |m| m.metas >= 3,
    );
    assert_eq!(off.writes, ["esc"], "one Escape: {:#?}", off.requests);
    let waiting: Vec<&String> = off
        .journal
        .iter()
        .filter(|l| l.starts_with("WAITING seq="))
        .collect();
    assert_eq!(waiting.len(), 1, "{:#?}", off.journal);
    assert!(
        waiting[0].ends_with(
            "panel is open (1 tries): owed until the word turns (autopilot off: no Escape again)"
        ),
        "{}",
        waiting[0]
    );
    assert!(parked(&off), "{:#?}", off.journal);
    assert!(off.owed, "owed until the word turns");

    let off = off_panel_run(
        "probe-off-halted",
        "off",
        false,
        INPUT_CLEAR,
        esc_halted,
        |m| m.metas >= 3,
    );
    assert_eq!(off.writes, ["esc"], "one Escape: {:#?}", off.requests);
    assert!(
        off.journal.iter().any(|l| l.starts_with("HALTED seq=")
            && l.contains("halted by the owner")
            && l.contains("owed until the word turns")),
        "{:#?}",
        off.journal
    );
    assert!(
        !off.journal.iter().any(|l| l.starts_with("UNHALTED")),
        "no wait on the hold: {:#?}",
        off.journal
    );
    assert!(parked(&off), "{:#?}", off.journal);
    assert!(off.owed, "owed until the word turns");

    let cruise = off_panel_run(
        "probe-cruise-failed",
        "cruise",
        false,
        INPUT_CLEAR,
        esc_fails_once,
        |m| {
            m.requests
                .iter()
                .rposition(|r| verb_of(r) == "key" && r.ends_with(" esc"))
                .is_some_and(|at| {
                    m.requests[at..]
                        .iter()
                        .filter(|r| r.starts_with("await gone "))
                        .count()
                        >= 2
                })
        },
    );
    assert!(
        cruise
            .journal
            .iter()
            .any(|l| l.starts_with("WAITING seq=") && l.ends_with("(1 tries): Escape again in 5 s")),
        "{:#?}",
        cruise.journal
    );
    let cruise = off_panel_run(
        "probe-cruise-halted",
        "cruise",
        false,
        INPUT_CLEAR,
        esc_halted,
        |_| false,
    );
    assert!(
        cruise.journal.iter().any(|l| l.starts_with("UNHALTED")),
        "{:#?}",
        cruise.journal
    );
    assert_eq!(cruise.writes, ["esc", "esc"], "{:#?}", cruise.requests);
    assert!(!cruise.owed);
}

#[test]
fn a_landings_mark_turned_away_is_asked_for_again() {
    let root = crate::supervise::ScratchDir::new("task", "busy");
    let text = transcript_of(&landing_rows(&landed_reply()));
    let run = |busy: usize, tag: &str| {
        let host = TaskHost::reading(&root.join(format!("t-{tag}.jsonl")), &[&text]);
        let tasks = root.join(format!("tasks-{tag}"));
        let journal = root.join(format!("journal-{tag}.jsonl"));
        let mut m = Mock::new(landing_screens(&landed_reply()));
        landing(&mut m);
        m.vanish_after = Some(4);
        m.task = working_task();
        m.task_busy = busy;
        let _ = tasked_watch(&mut m, &host, &tasks, Some(&journal));
        let dones = journal_records(&journal)
            .0
            .into_iter()
            .filter(|r| r.line.starts_with("DONE "))
            .count();
        let mark = done_mark(
            "landed",
            landed_result().len(),
            &tasks.join("s-1-1.result.md"),
        );
        (m, mark, dones)
    };
    let (m, mark, dones) = run(1, "refused");
    assert_eq!(
        task_marks(&m),
        [mark.clone(), mark],
        "{:?}",
        m.task_requests
    );
    assert!(
        m.task
            .as_ref()
            .is_some_and(|(_, f)| f.starts_with("state=done ")),
        "{:?}",
        m.task
    );
    assert_eq!(dones, 1);
    let (m, mark, _) = run(0, "served");
    assert_eq!(task_marks(&m), [mark], "{:?}", m.task_requests);
}

#[test]
fn a_loop_started_on_a_done_record_at_a_wall_still_types_the_retry() {
    let root = crate::supervise::ScratchDir::new("task", "seed-wall");
    let published = Some((
        2,
        "state=done from=ctl since_ms=0 result_bytes=17 why=landed".to_string(),
    ));
    let run = |screens: Vec<Vec<String>>, screens_held: bool, tag: &str| {
        let host = TaskHost::reading(&root.join("t.jsonl"), &[""]);
        let mut m = Mock::new(screens);
        m.history_served();
        m.turn_releases = (screens_held).then_some(0);
        m.exit_when = Some(|m| m.turns > 0 || m.stalls >= 3);
        m.vanish_after = Some(3);
        m.autopilot = Some("land");
        m.task = published.clone();
        let opts = SuperviseOpts {
            policy: owners_policy(),
            idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
            tasks: Some(root.join(format!("tasks-{tag}"))),
            ..auto(30)
        };
        let mut out: Vec<u8> = Vec::new();
        {
            let mut s = session(&mut m, Some("@s-1".to_string()));
            s.set_clock(TEST_NOW, PDT);
            s.set_zone_lookup(test_zones);
            s.set_turn_end_timing(TurnEndTiming {
                retry_backoff: vec![Duration::from_millis(50); 3],
                ..TurnEndTiming::default()
            });
            let _ = s.watch(&opts, &mut out);
        }
        let lines: Vec<String> = String::from_utf8(out)
            .expect("utf-8")
            .lines()
            .map(str::to_string)
            .collect();
        (m, lines)
    };
    let end_529 = aterm_phase::prompt::fixtures::screen(aterm_phase::prompt::fixtures::END_529);
    let (m, lines) = run(
        vec![end_529, busy_screen(), ended("Stage 2 landed.")],
        true,
        "529",
    );
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("CONTINUED ") && l.contains("rule=api-retry@v1")),
        "{lines:#?}"
    );
    assert_eq!(
        task_marks(&m),
        ["task mark working why=turn"],
        "{:?}",
        m.task_requests
    );
    let (m, lines) = run(
        vec![ended(
            "Yes, it's all done.\nNothing left: it is merged into the remote main branch.",
        )],
        false,
        "plain",
    );
    assert!(
        !lines.iter().any(|l| l.starts_with("CONTINUED ")),
        "{lines:#?}"
    );
    assert!(task_marks(&m).is_empty(), "{:?}", m.task_requests);
}

#[test]
fn a_seed_read_served_after_the_loop_acted_reads_nothing_into_the_point() {
    let root = crate::supervise::ScratchDir::new("task", "seed-owed");
    let published = Some((
        2,
        "state=done from=ctl since_ms=0 result_bytes=17 why=landed".to_string(),
    ));
    let host = TaskHost::reading(&root.join("t.jsonl"), &[""]);
    let opts = SuperviseOpts {
        policy: owners_policy(),
        idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
        tasks: Some(root.join("tasks")),
        ..auto(30)
    };
    let wall = aterm_phase::prompt::fixtures::screen(aterm_phase::prompt::fixtures::END_529);
    let run = |served_at_once: bool| {
        let mut m = Mock::new(vec![wall.clone()]);
        m.task = published.clone();
        m.task_lost = if served_at_once { 0 } else { usize::MAX };
        {
            let mut s = session(&mut m, Some("@s-1".to_string()));
            s.set_clock(TEST_NOW, PDT);
            s.set_zone_lookup(test_zones);
            s.set_turn_end_timing(TurnEndTiming {
                retry_backoff: vec![Duration::ZERO; 3],
                ..TurnEndTiming::default()
            });
            let point = point_at(&s, wall.clone(), 101);
            let action = s
                .turn_end_at_point(&point, None, &opts)
                .expect("a decision");
            s.task_edge(&point.screen, &opts, &mut Alone)
                .expect("the edge");
            s.turn_end_execute(&point, action, &opts, &[], &mut Alone)
                .expect("carried out");
            assert_eq!(s.ctl.turns, 1, "the retry typed");
            assert!(s.turn_end.act_in_flight(), "and not taken yet");
            s.ctl.task_lost = 0;
            s.turn_end_now(&point, &opts, &[], &mut Alone)
                .expect("decided again");
        }
        m
    };
    let m = run(false);
    assert!(
        m.task_requests.iter().filter(|r| *r == "task").count() >= 2,
        "turned away, then served: {:?}",
        m.task_requests
    );
    assert!(task_marks(&m).is_empty(), "{:?}", m.task_requests);
    assert_eq!(m.task, published, "the record stands");
    let m = run(true);
    assert_eq!(
        task_marks(&m),
        ["task mark working why=turn"],
        "{:?}",
        m.task_requests
    );
}

#[test]
fn a_workers_done_check_goes_without_the_back_off() {
    let root = crate::supervise::ScratchDir::new("task", "backoff");
    let text = transcript_of(&[
        task_row("user", "u-task", "", "Fix the parser."),
        task_row("assistant", "a-1", "msg_1", "Fixed the parser; all done."),
    ]);
    let run = |task: Option<(u64, String)>| {
        let host = TaskHost::reading(&root.join("t.jsonl"), &[&text]);
        let journal = root.join(format!("journal-{}.jsonl", task.is_some()));
        let mut m = Mock::new(vec![ended("Fixed the parser; all done.")]);
        m.exit_when = Some(|m| m.turns > 0 || m.stalls >= 3);
        m.vanish_after = Some(3);
        m.task = task;
        let mut lines = tasked_watch(&mut m, &host, &root.join("tasks"), Some(&journal));
        lines.extend(journal_records(&journal).0.into_iter().map(|r| r.line));
        (m, lines)
    };
    let (m, lines) = run(working_task());
    assert!(
        lines.iter().any(|l| l.starts_with("CONTINUED ")
            && l.ends_with(&format!("rule={RULE_DONE_CHECK} {}", clip(DONE_CHECK)))),
        "{lines:#?}"
    );
    assert!(!lines.iter().any(|l| l.contains("back-off")), "{lines:#?}");
    assert_eq!(m.turns, 1);
    let (m, lines) = run(None);
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("WAITING ") && l.contains("1 short turn(s) in a row")),
        "{lines:#?}"
    );
    assert_eq!(m.turns, 0, "{lines:#?}");
}

#[test]
fn a_carried_waits_break_reads_the_owners_off_before_the_host_step() {
    let carried = waiting_on(&[
        "⏺ Monitor event: \"lander\"",
        "",
        "⏺ Still in its crates-tests lane. I restarted the watch.",
        "",
    ]);
    for (label, screen, tail, word, offered) in [
        (
            "carried",
            &carried,
            Some(transcript_closed(1)),
            "off",
            false,
        ),
        (
            "carried",
            &carried,
            Some(transcript_closed(1)),
            "cruise",
            true,
        ),
        ("drawn", &woken(&[]), None, "off", false),
    ] {
        let host = Arc::new(CarriedHost {
            launched: tail.as_ref().map(|_| transcript_at("20:00:00")),
            tail,
            ..CarriedHost::default()
        });
        let mut m = Mock::new(vec![busy_screen(), screen.clone()]);
        m.autopilot = Some("cruise");
        m.autopilot_on.insert(1, Some(word));
        m.vanish_after = Some(2);
        let opts = SuperviseOpts {
            idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
            ..hosted(30)
        };
        let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
            s.set_background_settle(Duration::ZERO);
        });
        assert_eq!(
            host.backgrounds.load(Ordering::SeqCst) > 0,
            offered,
            "{label} {word}: {lines:#?}\n{:#?}",
            m.requests
        );
        assert!(m.metas > 1, "{label} {word}: {:#?}", m.requests);
    }
}

fn stamp_rows(ledger: &Path, seen: &mut usize, books: &mut Vec<String>, now_ms: i64) {
    let body = std::fs::read_to_string(ledger).unwrap_or_default();
    for (i, line) in body.lines().enumerate().skip(*seen) {
        let ms = now_ms + i64::try_from(i - *seen).unwrap_or(0);
        let stamped = match line.strip_prefix("{\"ts\":") {
            Some(rest) => {
                let digits = rest.chars().take_while(char::is_ascii_digit).count();
                format!("{{\"ts\":{ms}{}", &rest[digits..])
            }
            None => line.to_string(),
        };
        books.push(stamped);
    }
    *seen = body.lines().count();
}

#[test]
fn a_tries_own_short_turn_ending_on_its_limit_is_where_its_next_rung_counts_from() {
    use crate::supervise::approvals::{Ledger, open_wall, usage_attempt};
    use crate::supervise::policy::turn_end::{RULE_LIMIT_RESUME, decide_turn_end};
    let opts = SuperviseOpts {
        policy: SupervisorConfig {
            model_fallback: None,
            usage_credits: true,
            ..owners_policy()
        },
        ..auto(30)
    };
    let spend = {
        let mut r = rows(&[
            "⏺ Running the suite.",
            "  ⎿  You've hit your monthly spend limit.",
            "",
            "✻ Worked for 3m 2s · done 4:51 PM",
            "",
        ]);
        r.extend(composer("  ? for shortcuts"));
        r
    };
    let live = {
        let mut r = rows(&[
            "❯ keep going",
            "",
            "⏺ Working.",
            "",
            "✻ Synthesizing… (18s)",
            "",
        ]);
        r.extend(composer("  esc to interrupt"));
        r
    };
    type Decided = Option<(i64, Option<&'static str>)>;
    let decided = |s: &mut Session<'_, Mock>,
                   rows: &[String],
                   seq: u64,
                   worked: Option<Duration>|
     -> Decided {
        let point = point_at(s, rows.to_vec(), seq);
        match s
            .turn_end_at_point(&point, worked, &opts)
            .expect("a decision")
        {
            TurnEndAction::WaitUntil { until, .. } => {
                let r = s.turn_end_reading(&point.screen, None, &opts);
                Some((
                    s.unix_of(until),
                    decide_turn_end(&s.turn_end, &r, &opts.policy, until).rule_id(),
                ))
            }
            other => Some((s.now_unix(), other.rule_id())),
        }
    };
    let t = TEST_NOW;
    let mut wrong = Vec::new();
    for (n, (work_s, after)) in [
        (240_i64, true),
        (240, false),
        (100, true),
        (100, false),
        (360, true),
    ]
    .into_iter()
    .enumerate()
    {
        let case = format!(
            "the try's turn works {work_s} s, the successor attached {}",
            if after { "after it" } else { "into it" }
        );
        let (dir, ledger) = ledger_at(&format!("short-try-turn-{n}"));
        let books_path = dir.join("books.jsonl");
        let (mut seen, mut books) = (0, Vec::new());
        let mut m = Mock::new(vec![spend.clone()]);
        let mut s = session(&mut m, Some("@s-1".to_string()));
        s.set_zone_lookup(test_zones);
        s.ledger = Ledger::open(Some(&ledger), Some("@s-1"), &mut Vec::new());
        s.set_turn_end_timing(TurnEndTiming {
            take_within: Duration::ZERO,
            ..TurnEndTiming::default()
        });
        s.set_clock(t, PDT);
        let wait = decided(&mut s, &spend, 100, Some(Duration::from_secs(3 * 60)));
        stamp_rows(&ledger, &mut seen, &mut books, t * 1000);
        let try_at = wait.map_or(t, |(until, _)| until);
        s.set_clock(try_at, PDT);
        let p = point_at(&s, spend.clone(), 101);
        let a = s.turn_end_at_point(&p, None, &opts).expect("the try");
        assert_eq!(a.rule_id(), Some(RULE_LIMIT_RESUME), "{case}");
        s.pass_wake_settle();
        s.turn_end_execute(&p, a, &opts, &[], &mut Alone)
            .expect("typed");
        stamp_rows(&ledger, &mut seen, &mut books, try_at * 1000);
        s.set_clock(try_at + 20, PDT);
        let reader = s.reader(&live);
        s.end_memory_on_live_turn(reader, &live, 102);
        s.turn_end.took();
        stamp_rows(&ledger, &mut seen, &mut books, (try_at + 20) * 1000);
        let end_at = try_at + 20 + work_s;
        let attach = if after {
            end_at + 30
        } else {
            try_at + 20 + work_s / 2
        };
        if !after {
            std::fs::write(&books_path, books.join("\n") + "\n").expect("the books");
        }
        s.set_clock(end_at, PDT);
        let first = decided(
            &mut s,
            &spend,
            103,
            Some(Duration::from_secs(work_s.unsigned_abs())),
        );
        stamp_rows(&ledger, &mut seen, &mut books, end_at * 1000);
        if after {
            std::fs::write(&books_path, books.join("\n") + "\n").expect("the books");
        }
        drop(s);
        let seed = usage_attempt(&books_path, Some("@s-1"));
        let mut m2 = Mock::new(vec![spend.clone()]);
        let mut s2 = session(&mut m2, Some("@s-1".to_string()));
        s2.set_clock(attach, PDT);
        s2.set_zone_lookup(test_zones);
        s2.ledger = Ledger::open(Some(&books_path), Some("@s-1"), &mut Vec::new());
        seed_usage_act(
            &mut s2.turn_end,
            &books_path,
            s2.setup.sid.as_deref(),
            attach * 1000,
            Instant::now(),
        );
        s2.seed_wall_memory(open_wall(&books_path, Some("@s-1")));
        let next = if after {
            decided(&mut s2, &spend, 101, None)
        } else {
            s2.set_clock(end_at, PDT);
            decided(
                &mut s2,
                &spend,
                101,
                Some(Duration::from_secs((work_s / 2).unsigned_abs())),
            )
        };
        drop(s2);
        let _ = std::fs::remove_dir_all(&dir);
        let due = |d: Decided| d.filter(|&(_, rule)| rule == Some(RULE_LIMIT_RESUME));
        let (Some((mine, _)), Some((theirs, _))) = (due(first), due(next)) else {
            wrong.push(format!("{case}: first loop {first:?}, successor {next:?}"));
            continue;
        };
        let restarted = work_s >= 5 * 60;
        let ok = if restarted {
            seed.is_none() && theirs >= mine
        } else {
            mine == end_at + 120 * 60 && theirs == mine
        };
        if !ok {
            wrong.push(format!(
                "{case}: the first loop at try+{}s, the successor (seed {seed:?}) at try+{}s",
                mine - try_at,
                theirs - try_at
            ));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

#[test]
fn a_try_inside_its_take_window_is_awaited_by_a_loop_that_follows() {
    use crate::supervise::approvals::{Ledger, open_wall};
    use crate::supervise::policy::turn_end::{RULE_LIMIT_RESUME, decide_turn_end};
    let opts = SuperviseOpts {
        policy: SupervisorConfig {
            model_fallback: None,
            usage_credits: true,
            ..owners_policy()
        },
        ..auto(30)
    };
    let spend = {
        let mut r = rows(&[
            "⏺ Running the suite.",
            "  ⎿  You've hit your monthly spend limit.",
            "",
            "✻ Worked for 3m 2s · done 4:51 PM",
            "",
        ]);
        r.extend(composer("  ? for shortcuts"));
        r
    };
    let live = |prompt: &str| {
        let mut r = rows(&[
            &format!("❯ {prompt}"),
            "",
            "⏺ Working.",
            "",
            "✻ Synthesizing… (18s)",
            "",
        ]);
        r.extend(composer("  esc to interrupt"));
        r
    };
    type Decided = Option<(i64, Option<&'static str>)>;
    let decided = |s: &mut Session<'_, Mock>,
                   rows: &[String],
                   seq: u64,
                   worked: Option<Duration>|
     -> Decided {
        let point = point_at(s, rows.to_vec(), seq);
        match s
            .turn_end_at_point(&point, worked, &opts)
            .expect("a decision")
        {
            TurnEndAction::WaitUntil { until, .. } => {
                let r = s.turn_end_reading(&point.screen, None, &opts);
                Some((
                    s.unix_of(until),
                    decide_turn_end(&s.turn_end, &r, &opts.policy, until).rule_id(),
                ))
            }
            other => Some((s.now_unix(), other.rule_id())),
        }
    };
    let busy = |s: &mut Session<'_, Mock>, rows: &[String], seq: u64| {
        let reader = s.reader(rows);
        s.end_memory_on_live_turn(reader, rows, seq);
        s.turn_end.took();
    };
    let t = TEST_NOW;
    let mut wrong = Vec::new();
    for (n, (case, attach_s, work)) in [
        (
            "1 s after the try, its own 3 h turn",
            1_i64,
            Some(3 * 3600_i64),
        ),
        ("6 s after the try, its own 3 h turn", 6, Some(3 * 3600)),
        (
            "19 s after the try, its first look the try's turn",
            19,
            Some(3 * 3600),
        ),
        (
            "control: 1 s after the try, its own 90 min turn",
            1,
            Some(90 * 60),
        ),
        (
            "control: past the window, the try met, a person's turn",
            45,
            None,
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let (dir, ledger) = ledger_at(&format!("try-in-window-{n}"));
        let books_path = dir.join("books.jsonl");
        let (mut seen, mut books) = (0, Vec::new());
        let mut m = Mock::new(vec![spend.clone()]);
        let mut s = session(&mut m, Some("@s-1".to_string()));
        s.set_zone_lookup(test_zones);
        s.ledger = Ledger::open(Some(&ledger), Some("@s-1"), &mut Vec::new());
        let window = if work.is_none() {
            Duration::from_millis(50)
        } else {
            TurnEndTiming::default().take_within
        };
        s.set_turn_end_timing(TurnEndTiming {
            take_within: window,
            ..TurnEndTiming::default()
        });
        s.set_clock(t, PDT);
        let wait = decided(&mut s, &spend, 100, Some(Duration::from_secs(3 * 60)));
        stamp_rows(&ledger, &mut seen, &mut books, t * 1000);
        let try_at = wait.map_or(t, |(until, _)| until);
        s.set_clock(try_at, PDT);
        let p = point_at(&s, spend.clone(), 101);
        let a = s.turn_end_at_point(&p, None, &opts).expect("the try");
        assert_eq!(a.rule_id(), Some(RULE_LIMIT_RESUME), "{case}");
        s.pass_wake_settle();
        s.turn_end_execute(&p, a, &opts, &[], &mut Alone)
            .expect("typed");
        stamp_rows(&ledger, &mut seen, &mut books, try_at * 1000);
        let snapshot = |books: &[String], at: i64| {
            if at == attach_s {
                std::fs::write(&books_path, books.join("\n") + "\n").expect("the books");
            }
        };
        snapshot(&books, 1);
        s.set_clock(try_at + 5, PDT);
        let _ = decided(&mut s, &spend, 102, None);
        stamp_rows(&ledger, &mut seen, &mut books, (try_at + 5) * 1000);
        snapshot(&books, 6);
        snapshot(&books, 19);
        let mut events: Vec<(i64, Vec<String>, bool, Option<Duration>)> = Vec::new();
        let (first, end_at) = match work {
            Some(work) => {
                s.set_clock(try_at + 20, PDT);
                busy(&mut s, &live("keep going"), 103);
                stamp_rows(&ledger, &mut seen, &mut books, (try_at + 20) * 1000);
                let end_at = try_at + 20 + work;
                s.set_clock(end_at, PDT);
                let first = decided(
                    &mut s,
                    &spend,
                    104,
                    Some(Duration::from_secs(work.unsigned_abs())),
                );
                events.push((try_at + 5, spend.clone(), false, None));
                events.push((try_at + 20, live("keep going"), true, None));
                events.push((
                    end_at,
                    spend.clone(),
                    false,
                    Some(Duration::from_secs(work.unsigned_abs())),
                ));
                (first, end_at)
            }
            None => {
                std::thread::sleep(window * 2);
                s.set_clock(try_at + 40, PDT);
                let _ = decided(&mut s, &spend, 103, None);
                stamp_rows(&ledger, &mut seen, &mut books, (try_at + 40) * 1000);
                snapshot(&books, 45);
                s.set_clock(try_at + 20 * 60, PDT);
                busy(&mut s, &live("please look at the schema"), 104);
                stamp_rows(&ledger, &mut seen, &mut books, (try_at + 20 * 60) * 1000);
                let end_at = try_at + 40 * 60;
                s.set_clock(end_at, PDT);
                let first = decided(&mut s, &spend, 105, Some(Duration::from_secs(20 * 60)));
                events.push((
                    try_at + 20 * 60,
                    live("please look at the schema"),
                    true,
                    None,
                ));
                events.push((
                    end_at,
                    spend.clone(),
                    false,
                    Some(Duration::from_secs(20 * 60)),
                ));
                (first, end_at)
            }
        };
        drop(s);
        let attach = try_at + attach_s;
        let mut m2 = Mock::new(vec![spend.clone()]);
        let mut s2 = session(&mut m2, Some("@s-1".to_string()));
        s2.set_clock(attach, PDT);
        s2.set_zone_lookup(test_zones);
        s2.ledger = Ledger::open(Some(&books_path), Some("@s-1"), &mut Vec::new());
        seed_usage_act(
            &mut s2.turn_end,
            &books_path,
            s2.setup.sid.as_deref(),
            attach * 1000,
            Instant::now(),
        );
        s2.seed_wall_memory(open_wall(&books_path, Some("@s-1")));
        let mut next = None;
        if attach_s != 19 {
            next = decided(&mut s2, &spend, 201, None);
        }
        for (seq, (at, rows, is_busy, worked)) in
            (202..).zip(events.into_iter().filter(|e| e.0 > attach))
        {
            s2.set_clock(at, PDT);
            if is_busy {
                busy(&mut s2, &rows, seq);
            } else {
                next = decided(
                    &mut s2,
                    &rows,
                    seq,
                    worked.map(|w| w.saturating_sub(Duration::from_secs(20))),
                );
            }
        }
        drop(s2);
        let taken = std::fs::read_to_string(&books_path)
            .unwrap_or_default()
            .lines()
            .filter(|l| l.contains(crate::supervise::approvals::WALL_TAKEN))
            .count();
        let _ = std::fs::remove_dir_all(&dir);
        let due = |d: Decided| d.filter(|&(_, rule)| rule == Some(RULE_LIMIT_RESUME));
        let (Some((mine, _)), Some((theirs, _))) = (due(first), due(next)) else {
            wrong.push(format!("{case}: first loop {first:?}, successor {next:?}"));
            continue;
        };
        let ok = if work.is_some() {
            mine == end_at + 30 * 60 && theirs == mine && taken == 1
        } else {
            mine == try_at + 40 + 120 * 60 && theirs + 40 >= mine && theirs <= mine
        };
        if !ok {
            wrong.push(format!(
                "{case}: the first loop at try+{}s, the successor at try+{}s, {taken} take row(s)",
                mine - try_at,
                theirs - try_at
            ));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

#[test]
fn a_restart_of_another_kind_begins_a_run_of_its_own() {
    use crate::supervise::approvals::{Ledger, open_model_switch};
    use crate::supervise::policy::turn_end::{RULE_CONTINUE, Restart};
    use aterm_phase::prompt::fixtures as fx;
    const RESET: i64 = 1_792_279_200;
    let now = RESET - 2 * 86_400;
    let armed = fx::screen(fx::WALL_SONNET_BUCKET_AUTO_CONTINUE_ARMED_2_1_300);
    let redrawn: Vec<String> = armed
        .iter()
        .filter(|r| {
            ![
                "Usage limit reached",
                "Continuing automatically",
                "/rate-limit-options",
                "Claude Code will continue automatically",
                "to cancel the wait",
            ]
            .iter()
            .any(|g| r.contains(g))
        })
        .map(|r| r.replace("Sonnet 5.5 · Claude API", "Opus 5.5 · Claude API"))
        .collect();
    let notice = "You've hit your Sonnet limit · resets Oct 17 at 4:20pm (America/Los_Angeles)";
    let tail = |prompt: &str, at: i64| -> String {
        let stamp = crate::harness::usage::rfc3339_utc(at);
        include_str!("fixtures/claude-2.1.294-transcript-session-limit.jsonl")
            .lines()
            .skip(1)
            .filter(|l| !l.contains("\"subtype\":\"informational\""))
            .map(|l| {
                let mut row = l
                    .replace("seventh try", prompt)
                    .replace(
                        "You've hit your session limit · resets 11:33am (America/Los_Angeles)",
                        notice,
                    )
                    .replace("\"five_hour\"", "\"seven_day_sonnet\"");
                for ms in ["921", "951", "952", "953"] {
                    row = row.replace(
                        &format!("\"2026-10-08T16:34:58.{ms}Z\""),
                        &format!("\"{stamp}\""),
                    );
                }
                row
            })
            .collect::<Vec<_>>()
            .join("\n")
            + "\n"
    };
    let met = tail("please run the integration tests", now - 300);
    let served = {
        let mut r = rows(&[
            "❯ please run the integration tests",
            "",
            "⏺ Ran them: all green.",
            "",
            "✻ Worked for 4m 0s · done 8:40 AM",
            "",
        ]);
        r.extend(composer("  ? for shortcuts"));
        r
    };
    let mut wrong = Vec::new();
    for (n, before) in [
        "a memory restart's run",
        "a fallback's run another decision ended",
        "none",
    ]
    .into_iter()
    .enumerate()
    {
        let memory_first = before == "a memory restart's run";
        let fallback_first = before.starts_with("a fallback's");
        let answers: &[&'static str] = if before == "none" {
            &["relaunched"]
        } else {
            &["wait:held", "relaunched"]
        };
        let host = AccountHost::scripted(&[None], answers);
        *host.tail.lock().unwrap() = Some(met.clone());
        let opts = SuperviseOpts {
            idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
            policy: owners_policy(),
            ..auto(30)
        };
        let (dir, ledger) = ledger_at(&format!("restart-kinds-{n}"));
        let mut m = Mock::new(vec![armed.clone()]);
        let mut s = session(&mut m, Some("@s-1".to_string()));
        s.set_zone_lookup(test_zones);
        s.ledger = Ledger::open(Some(&ledger), Some("@s-1"), &mut Vec::new());
        s.set_turn_end_timing(TurnEndTiming {
            take_within: Duration::ZERO,
            ..TurnEndTiming::default()
        });
        if memory_first {
            s.set_clock(now - 900, PDT);
            let p = point_at(&s, fx::screen(fx::MEMORY_BANNER_IDLE), 90);
            let a = s
                .turn_end_at_point(&p, None, &opts)
                .expect("the memory restart");
            assert!(
                matches!(
                    &a,
                    TurnEndAction::Restart {
                        why: Restart::Memory,
                        ..
                    }
                ),
                "{a:?}"
            );
            s.pass_wake_settle();
            s.turn_end_execute(&p, a, &opts, &[], &mut Alone)
                .expect("waits");
        }
        if fallback_first {
            s.set_clock(now - 900, PDT);
            let p = point_at(&s, armed.clone(), 90);
            let a = s
                .turn_end_at_point(&p, Some(Duration::from_secs(60)), &opts)
                .expect("the fallback");
            assert!(
                matches!(
                    &a,
                    TurnEndAction::Restart {
                        why: Restart::Model { .. },
                        ..
                    }
                ),
                "{a:?}"
            );
            s.pass_wake_settle();
            s.turn_end_execute(&p, a, &opts, &[], &mut Alone)
                .expect("waits");
            s.set_clock(now - 600, PDT);
            let p = point_at(&s, served.clone(), 91);
            let a = s
                .turn_end_at_point(&p, Some(Duration::from_secs(240)), &opts)
                .expect("the served turn");
            assert!(!matches!(&a, TurnEndAction::Restart { .. }), "{a:?}");
            s.pass_wake_settle();
            s.turn_end_execute(&p, a, &opts, &[], &mut Alone)
                .expect("decided");
        }
        s.set_clock(now, PDT);
        let p = point_at(&s, armed.clone(), 100);
        let a = s
            .turn_end_at_point(&p, Some(Duration::from_secs(60)), &opts)
            .expect("the fallback");
        assert!(
            matches!(
                &a,
                TurnEndAction::Restart {
                    why: Restart::Model { .. },
                    ..
                }
            ),
            "{before}: {a:?}"
        );
        s.pass_wake_settle();
        s.turn_end_execute(&p, a, &opts, &[], &mut Alone)
            .expect("relaunched");
        let made = open_model_switch(&ledger, Some("@s-1")).map(|o| o.made_unix);
        s.set_clock(now + 30, PDT);
        s.pass_wake_settle();
        let point = point_at(&s, redrawn.clone(), 102);
        let redraw = s
            .turn_end_at_point(&point, None, &opts)
            .expect("a decision");
        let redraw = match redraw {
            TurnEndAction::WaitUntil { until, .. } => {
                let r = s.turn_end_reading(&point.screen, None, &opts);
                (
                    s.unix_of(until),
                    crate::supervise::policy::turn_end::decide_turn_end(
                        &s.turn_end,
                        &r,
                        &opts.policy,
                        until,
                    )
                    .rule_id(),
                )
            }
            other => (s.now_unix(), other.rule_id()),
        };
        drop(s);
        let _ = std::fs::remove_dir_all(&dir);
        if made != Some(now) || redraw.1 != Some(RULE_CONTINUE) || redraw.0 > now + 30 + 120 {
            wrong.push(format!(
                "{before} before: made={:?}, the redraw decides {:?} (now{:+}s)",
                made.map(|m| m - now),
                redraw.1,
                redraw.0 - now
            ));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

#[test]
fn a_fallbacks_relaunch_in_flight_when_its_loop_ends_is_made_at_its_first_ask() {
    use crate::supervise::approvals::{Ledger, open_model_switch, open_wall, relaunch_waiting};
    use crate::supervise::policy::turn_end::{RULE_CONTINUE, Restart, decide_turn_end};
    use aterm_phase::prompt::fixtures as fx;
    const RESET: i64 = 1_792_279_200;
    let now = RESET - 2 * 86_400;
    let armed = fx::screen(fx::WALL_SONNET_BUCKET_AUTO_CONTINUE_ARMED_2_1_300);
    let redrawn: Vec<String> = armed
        .iter()
        .filter(|r| {
            ![
                "Usage limit reached",
                "Continuing automatically",
                "/rate-limit-options",
                "Claude Code will continue automatically",
                "to cancel the wait",
            ]
            .iter()
            .any(|g| r.contains(g))
        })
        .map(|r| r.replace("Sonnet 5.5 · Claude API", "Opus 5.5 · Claude API"))
        .collect();
    let notice = "You've hit your Sonnet limit · resets Oct 17 at 4:20pm (America/Los_Angeles)";
    let met = {
        let mut rows = redrawn.clone();
        let top = rows
            .iter()
            .position(|r| r.starts_with('─'))
            .expect("the composer's frame");
        rows.splice(
            top..top,
            [
                "❯ please run the integration tests".to_string(),
                format!("  ⎿  {notice}"),
                String::new(),
                "✻ Churned for 0s · done 8:49 AM".to_string(),
                String::new(),
            ],
        );
        rows
    };
    let tail = |prompt: &str, at: i64| -> String {
        let stamp = crate::harness::usage::rfc3339_utc(at);
        include_str!("fixtures/claude-2.1.294-transcript-session-limit.jsonl")
            .lines()
            .skip(1)
            .filter(|l| !l.contains("\"subtype\":\"informational\""))
            .map(|l| {
                let mut row = l
                    .replace("seventh try", prompt)
                    .replace(
                        "You've hit your session limit · resets 11:33am (America/Los_Angeles)",
                        notice,
                    )
                    .replace("\"five_hour\"", "\"seven_day_sonnet\"");
                for ms in ["921", "951", "952", "953"] {
                    row = row.replace(
                        &format!("\"2026-10-08T16:34:58.{ms}Z\""),
                        &format!("\"{stamp}\""),
                    );
                }
                row
            })
            .collect::<Vec<_>>()
            .join("\n")
            + "\n"
    };
    let before = tail("carry on with the task", now - 600);
    let hosted = |transcript: &str, answers: &[&'static str]| {
        let host = AccountHost::scripted(&[None], answers);
        *host.tail.lock().unwrap() = Some(transcript.to_string());
        let opts = SuperviseOpts {
            idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
            policy: owners_policy(),
            ..auto(30)
        };
        (host, opts)
    };
    let mut wrong = Vec::new();
    for (n, answered) in [true, false].into_iter().enumerate() {
        let (dir, ledger) = ledger_at(&format!("relaunch-in-flight-{n}"));
        let (host, opts) = hosted(&before, &["wait:resume"]);
        let mut m = Mock::new(vec![armed.clone()]);
        let mut s = session(&mut m, Some("@s-1".to_string()));
        s.set_zone_lookup(test_zones);
        s.ledger = Ledger::open(Some(&ledger), Some("@s-1"), &mut Vec::new());
        s.set_turn_end_timing(TurnEndTiming {
            take_within: Duration::ZERO,
            ..TurnEndTiming::default()
        });
        s.set_clock(now, PDT);
        let p = point_at(&s, armed.clone(), 100);
        let a = s.turn_end_at_point(&p, None, &opts).expect("the fallback");
        assert!(
            matches!(
                &a,
                TurnEndAction::Restart {
                    why: Restart::Model { .. },
                    ..
                }
            ),
            "{a:?}"
        );
        s.turn_end_execute(&p, a, &opts, &[], &mut Alone)
            .expect("waits");
        assert_eq!(host.said("restart"), ["restart:model"]);
        drop(s);
        let run = relaunch_waiting(&ledger, Some("@s-1"));
        let (screen, transcript) = if answered {
            (
                met.clone(),
                tail("please run the integration tests", now + 55),
            )
        } else {
            (redrawn.clone(), before.clone())
        };
        let (_host2, opts2) = hosted(&transcript, &["relaunched"]);
        let mut m2 = Mock::new(vec![screen.clone()]);
        let mut s2 = session(&mut m2, Some("@s-1".to_string()));
        s2.set_clock(now + 61, PDT);
        s2.set_zone_lookup(test_zones);
        s2.ledger = Ledger::open(Some(&ledger), Some("@s-1"), &mut Vec::new());
        s2.set_turn_end_timing(TurnEndTiming {
            take_within: Duration::ZERO,
            ..TurnEndTiming::default()
        });
        s2.seed_model_switch(&ledger);
        s2.seed_wall_memory(open_wall(&ledger, Some("@s-1")));
        let p = point_at(&s2, screen.clone(), 101);
        let a = s2
            .turn_end_at_point(&p, None, &opts2)
            .expect("the relaunch");
        let asked = matches!(
            &a,
            TurnEndAction::Restart {
                why: Restart::Model { .. },
                ..
            }
        );
        s2.pass_wake_settle();
        s2.turn_end_execute(&p, a, &opts2, &[], &mut Alone)
            .expect("relaunched");
        let made = open_model_switch(&ledger, Some("@s-1")).map(|o| o.made_unix - now);
        let ended = relaunch_waiting(&ledger, Some("@s-1")).is_none();
        s2.set_clock(now + 70, PDT);
        s2.pass_wake_settle();
        let point = point_at(&s2, screen, 102);
        let decided = match s2
            .turn_end_at_point(&point, None, &opts2)
            .expect("a decision")
        {
            TurnEndAction::WaitUntil { until, .. } => {
                let r = s2.turn_end_reading(&point.screen, None, &opts2);
                (
                    s2.unix_of(until),
                    decide_turn_end(&s2.turn_end, &r, &opts2.policy, until).rule_id(),
                )
            }
            other => (s2.now_unix(), other.rule_id()),
        };
        drop(s2);
        let _ = std::fs::remove_dir_all(&dir);
        let ok = run == Some(now)
            && asked
            && made == Some(0)
            && ended
            && if answered {
                decided.0 >= RESET
            } else {
                decided.1 == Some(RULE_CONTINUE) && decided.0 < RESET
            };
        if !ok {
            wrong.push(format!(
                "{}: the run's row {:?}, the successor asked the relaunch {asked}, made=now{:+?} \
                 (the run ended {ended}), then {:?} at now{:+}s",
                if answered {
                    "a person's prompt answered"
                } else {
                    "control: the redraw"
                },
                run.map(|r| r - now),
                made,
                decided.1,
                decided.0 - now
            ));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// A host that withholds the continuations while `on` ([`IdleHost::withholds_turn_end`]).
#[derive(Debug, Default)]
struct Withholds {
    on: AtomicBool,
}

impl IdleHost for Withholds {
    fn wants(&self) -> bool {
        false
    }
    fn at_idle(&self) -> Option<HostStep> {
        None
    }
    fn owns_turn_end(&self) -> bool {
        false
    }
    fn withholds_turn_end(&self) -> bool {
        self.on.load(Ordering::SeqCst)
    }
}

#[test]
fn a_host_that_withholds_the_continuations_types_nothing() {
    let run = |on: bool| {
        let host = Arc::new(Withholds {
            on: AtomicBool::new(on),
        });
        let mut m = Mock::new(vec![
            busy_screen(),
            ended("Fixed the parser; the suite is green."),
        ]);
        m.vanish_after = Some(2);
        let opts = SuperviseOpts {
            idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
            ..hosted(30)
        };
        let (lines, _) = watch_lines(&mut m, &opts);
        (count(&m, "turn"), lines)
    };
    let (typed, lines) = run(true);
    assert_eq!(typed, 0, "{lines:#?}");
    assert!(
        !lines.iter().any(|l| l.starts_with("CONTINUED")),
        "{lines:#?}"
    );
    let (typed, lines) = run(false);
    assert_eq!(typed, 1, "{lines:#?}");
}

fn paste_person_run(text: &str, shown: &str, setup: impl FnOnce(&mut Mock)) -> (Mock, Vec<String>) {
    const SAID: &str = "Fixed the parser; the suite is green.";
    let mut m = Mock::new(vec![busy_screen(), ended(SAID), typed_in(SAID, shown)]);
    m.gen_fence = true;
    m.sends_gen = true;
    m.human_seq = Some(0);
    m.human = vec!["null"];
    m.help = FENCED_HELP.to_string();
    m.screen_cols.insert(2, 2 + shown.chars().count());
    m.vanish_after = Some(2);
    m.exit_when = Some(|m| {
        !m.person_skipped.is_empty()
            || m.requests.iter().any(|r| {
                (r.starts_with("key ") && r.ends_with(" enter"))
                    || r.starts_with("meta set attention")
            })
    });
    setup(&mut m);
    let opts = SuperviseOpts {
        policy: SupervisorConfig {
            continue_text: text.to_string(),
            ..owners_policy()
        },
        ..auto(30)
    };
    let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
        s.set_turn_end_timing(TurnEndTiming {
            min_work: Duration::ZERO,
            ..TurnEndTiming::default()
        });
    });
    (m, lines)
}

#[test]
fn a_fenced_paste_cannot_submit_after_its_original_person_count_is_lost() {
    let long = "carry on";
    for text in ["carry on", "keep going"] {
        for human in [0, 7] {
            let (m, lines) = paste_person_run(text, text, |m| {
                m.human_seq = Some(human);
            });
            assert!(
                m.requests
                    .iter()
                    .any(|r| r.starts_with("send ") && r.ends_with(text)),
                "the admitted whole write, count {human}: {lines:#?}\n{:#?}",
                m.requests
            );
            assert!(
                m.requests
                    .iter()
                    .any(|r| r.starts_with("key ") && r.ends_with(" enter"))
                    && lines.iter().any(|l| l.starts_with("CONTINUED ")),
                "the admitted Enter, count {human}: {lines:#?}\n{:#?}",
                m.requests
            );
            assert_eq!(m.human_seq, Some(human));
            assert_eq!(count(&m, "turn"), 0, "no legacy paste");
        }
    }

    let mut failures = Vec::new();
    fn rejected(
        failures: &mut Vec<String>,
        label: &str,
        m: &Mock,
        lines: &[String],
        no_enter_request: bool,
    ) {
        let entered = m
            .requests
            .iter()
            .any(|r| r.starts_with("key ") && r.ends_with(" enter"));
        let recorded = lines.iter().any(|l| l.starts_with("CONTINUED "));
        if recorded || (no_enter_request && entered) || count(m, "turn") != 0 {
            failures.push(format!("{label}: {lines:#?}\n{:#?}", m.requests));
        }
    }

    let (m, lines) = paste_person_run(long, long, |m| {
        m.human_seq = None;
        m.screens.truncate(2);
    });
    if m.requests.iter().any(|r| r.starts_with("send ")) {
        failures.push(format!(
            "the initial missing count wrote: {lines:#?}\n{:#?}",
            m.requests
        ));
    }
    rejected(&mut failures, "missing initial count", &m, &lines, true);

    let (m, lines) = paste_person_run(long, long, |m| {
        m.human_seq = Some(7);
        m.human_seq_on.insert(2, None);
    });
    assert!(
        m.requests
            .iter()
            .any(|r| r.starts_with("send ") && r.ends_with(long))
    );
    assert!(
        m.reads.iter().any(|(i, _)| *i == 2),
        "the missing-count read-back was reached"
    );
    rejected(
        &mut failures,
        "count omitted from read-back",
        &m,
        &lines,
        true,
    );

    let (m, lines) = paste_person_run(long, long, |m| {
        m.human_seq = Some(7);
        m.person_at_send = 1;
        m.screens.truncate(2);
    });
    assert_eq!(m.person_at_send, 0, "the gesture before send occurred");
    assert_eq!(m.human_seq, Some(8));
    if m.person_skipped.len() != 1 {
        failures.push(format!(
            "the send was not skipped for the person: {lines:#?}\n{:#?}",
            m.requests
        ));
    }
    rejected(&mut failures, "person before send", &m, &lines, true);

    let (m, lines) = paste_person_run(long, long, |m| {
        m.human_seq = Some(7);
        m.person_at_key = 1;
    });
    assert_eq!(m.person_at_key, 0, "the gesture before Enter occurred");
    assert_eq!(m.human_seq, Some(8));
    assert!(
        m.requests
            .iter()
            .any(|r| r.starts_with("key ") && r.ends_with(" enter"))
    );
    if m.person_skipped.len() != 1 {
        failures.push(format!(
            "Enter was not skipped for the person: {lines:#?}\n{:#?}",
            m.requests
        ));
    }
    rejected(&mut failures, "person before Enter", &m, &lines, false);
    assert!(
        failures.is_empty(),
        "original paste person fence: {failures:#?}"
    );
}

#[test]
fn a_fenced_paste_names_its_original_person_in_both_writes() {
    for human in [0, 7] {
        let (m, lines) = paste_person_run("carry on", "carry on", |m| {
            m.human_seq = Some(human);
        });
        let writes: Vec<&String> = m
            .requests
            .iter()
            .filter(|r| r.starts_with("send ") || r.starts_with("key "))
            .collect();
        assert_eq!(writes.len(), 2, "{lines:#?}\n{:#?}", m.requests);
        for request in writes {
            let words: Vec<&str> = request.split(' ').collect();
            assert!(
                words.contains(&format!("if-human={human}").as_str()),
                "{request}"
            );
            assert!(words.iter().any(|w| w.starts_with("if-gen=")), "{request}");
            assert!(words.iter().any(|w| w.starts_with("if=")), "{request}");
        }
    }
}

#[test]
fn a_refused_paste_person_fence_never_becomes_a_legacy_turn() {
    let said = "Fixed the parser; the suite is green.";
    let (m, lines) = paste_person_run("carry on", "carry on", |m| {
        m.human_seq = Some(7);
        m.screens = vec![busy_screen(), ended(said), busy_screen(), ended(said)];
        m.send_replies = VecDeque::from([
            err("usage: send [if-gen=<g>] [if=<re>] [--] <text>"),
            err("usage: send [if-gen=<g>] [if=<re>] [--] <text>"),
        ]);
        m.exit_when = Some(|m| {
            m.requests.iter().filter(|r| r.starts_with("send ")).count() >= 2 || m.turns > 0
        });
    });
    let sends: Vec<&String> = m
        .requests
        .iter()
        .filter(|r| r.starts_with("send "))
        .collect();
    assert_eq!(
        sends.len(),
        2,
        "both decisions reached the strict send: {lines:#?}\n{:#?}",
        m.requests
    );
    assert!(
        sends
            .iter()
            .all(|r| r.starts_with("send if-human=7 if-gen=")),
        "{sends:#?}"
    );
    assert_eq!(count(&m, "turn"), 0, "{lines:#?}\n{:#?}", m.requests);
    assert!(
        !m.requests.iter().any(|r| r.starts_with("key ")),
        "{:#?}",
        m.requests
    );
    assert!(
        !lines.iter().any(|l| l.starts_with("CONTINUED ")),
        "{lines:#?}"
    );
}

const LATCH_KEY: &str = "default";

/// A host whose tabs share one login ([`LoginLatches`]), as a window's tabs
/// do: this session is tab `sid`, and everything the loop asked of the latch
/// is kept, in order.
#[derive(Debug)]
struct LatchHost {
    latches: Arc<LoginLatches>,
    sid: &'static str,
    peer: Option<&'static str>,
    blind: bool,
    /// The login comes back in this tab right after this one takes its
    /// sign-in: the owner signed in elsewhere.
    back_in: Option<&'static str>,
    said: std::sync::Mutex<Vec<String>>,
}

impl LatchHost {
    fn tab(latches: &Arc<LoginLatches>, sid: &'static str) -> Self {
        Self {
            latches: Arc::clone(latches),
            sid,
            peer: None,
            blind: false,
            back_in: None,
            said: std::sync::Mutex::new(Vec::new()),
        }
    }

    fn said(&self) -> Vec<String> {
        self.said.lock().unwrap().clone()
    }
}

impl IdleHost for LatchHost {
    fn wants(&self) -> bool {
        false
    }
    fn at_idle(&self) -> Option<HostStep> {
        None
    }
    fn owns_turn_end(&self) -> bool {
        false
    }
    fn login_shared(&self) -> Option<LoginShared> {
        let shared = (!self.blind)
            .then(|| self.latches.shared(LATCH_KEY, self.sid))
            .flatten();
        self.said.lock().unwrap().push(format!("shared:{shared:?}"));
        shared
    }
    fn login_carry(&self) -> Result<(), String> {
        let carried = self.latches.carry(LATCH_KEY, self.sid);
        self.said.lock().unwrap().push(format!("carry:{carried:?}"));
        if carried.is_ok()
            && let Some(peer) = self.peer
        {
            let read = self.latches.shared(LATCH_KEY, peer);
            self.said.lock().unwrap().push(format!("{peer}:{read:?}"));
        }
        if carried.is_ok()
            && let Some(by) = self.back_in
        {
            self.latches.back(LATCH_KEY, by);
            self.said.lock().unwrap().push(format!("back in {by}"));
        }
        carried
    }
    fn login_abandon(&self) {
        self.said.lock().unwrap().push("abandon".to_string());
        self.latches.gone(self.sid);
    }
    fn login_back(&self) {
        self.said.lock().unwrap().push("back".to_string());
        self.latches.back(LATCH_KEY, self.sid);
    }
}

#[test]
fn one_sign_in_per_login_however_many_tabs_share_it() {
    let mut lost = rows(&[
        "⏺ Pushing the branch.",
        "  ⎿  Not logged in · Please run /login",
        "",
        "✻ Worked for 3m 2s · done 4:51 PM",
        "",
    ]);
    lost.extend(composer("  ? for shortcuts"));
    let dialog = rows(&[
        " Browser didn't open? Use the url below to sign in (⌘/ctrl + click):",
        " https://claude.ai/oauth/authorize?code=true",
        "",
        " Paste code here if prompted >",
    ]);
    let run = |host: &Arc<LatchHost>, screens: Vec<Vec<String>>| {
        let (dir, path) = journal_file(&format!("te-latch-{}", host.sid.trim_start_matches('@')));
        let mut m = Mock::new(screens);
        m.turn_releases = Some(1);
        m.vanish_after = Some(2);
        let mut opts = SuperviseOpts {
            idle_host: Some(Arc::clone(host) as Arc<dyn IdleHost>),
            journal: Some(path.clone()),
            ..hosted(30)
        };
        opts.policy.continue_policy = false;
        let (lines, _) = watch_lines(&mut m, &opts);
        let (records, _) = journal_records(&path);
        let _ = std::fs::remove_dir_all(&dir);
        let notes: Vec<String> = records
            .into_iter()
            .map(|r| format!("{} {}", r.kind, r.summary))
            .collect();
        (lines, notes)
    };
    let acts = |lines: &[String]| -> Vec<String> {
        lines
            .iter()
            .filter(|l| l.starts_with("TYPED ") || l.starts_with("CONTINUED "))
            .cloned()
            .collect()
    };
    let typed = |lines: &[String], rule: &str| {
        acts(lines)
            .iter()
            .filter(|l| l.contains(&format!(" rule={rule} ")))
            .count()
    };
    let latches = Arc::new(LoginLatches::default());
    let a = Arc::new(LatchHost {
        peer: Some("@2"),
        ..LatchHost::tab(&latches, "@1")
    });
    let (lines, _) = run(
        &a,
        vec![
            busy_screen(),
            lost.clone(),
            dialog.clone(),
            busy_screen(),
            ended("Pushed."),
        ],
    );
    assert_eq!(typed(&lines, RULE_LOGIN), 1, "{lines:#?}");
    assert_eq!(
        typed(&lines, RULE_LOGIN_BACK),
        0,
        "back where it was carried: {lines:#?}"
    );
    let said = a.said();
    let carried_by_1 = Some(LoginShared::Carried {
        by: "@1".to_string(),
    });
    assert_eq!(
        said.iter()
            .filter(|s| s.starts_with("carry:") || s.starts_with("@2:") || *s == "back")
            .cloned()
            .collect::<Vec<_>>(),
        [
            "carry:Ok(())".to_string(),
            format!("@2:{carried_by_1:?}"),
            "back".to_string(),
        ],
        "{said:#?}\n{lines:#?}"
    );
    let b = Arc::new(LatchHost::tab(&latches, "@2"));
    let (lines, _) = run(
        &b,
        vec![busy_screen(), lost.clone(), busy_screen(), ended("Pushed.")],
    );
    assert_eq!(typed(&lines, RULE_LOGIN_BACK), 1, "{lines:#?}");
    assert_eq!(
        typed(&lines, RULE_LOGIN),
        0,
        "no sign-in of its own: {lines:#?}"
    );
    let said = b.said();
    assert!(
        said.iter().all(|s| !s.starts_with("carry:")) && said.last().is_some_and(|s| s == "back"),
        "{said:#?}"
    );
    assert_eq!(
        latches.shared(LATCH_KEY, "@9"),
        None,
        "nothing left of the sign-in"
    );
    let latches = Arc::new(LoginLatches::default());
    assert_eq!(latches.carry(LATCH_KEY, "@1"), Ok(()));
    let c = Arc::new(LatchHost::tab(&latches, "@3"));
    let (lines, notes) = run(&c, vec![busy_screen(), lost.clone()]);
    assert!(acts(&lines).is_empty(), "nothing typed: {lines:#?}");
    assert!(
        notes
            .iter()
            .any(|n| n.starts_with("waiting ") && n.contains("@1")),
        "waits on @1's sign-in: {notes:#?}"
    );
    assert!(
        c.said().iter().all(|s| !s.starts_with("carry:")),
        "{:#?}",
        c.said()
    );
    let d = Arc::new(LatchHost {
        blind: true,
        ..LatchHost::tab(&latches, "@4")
    });
    let (lines, notes) = run(&d, vec![busy_screen(), lost.clone()]);
    assert!(acts(&lines).is_empty(), "nothing typed: {lines:#?}");
    assert!(
        notes
            .iter()
            .any(|n| n.starts_with("skipped ") && n.contains("@1")),
        "{notes:#?}"
    );
    assert!(
        d.said().iter().any(|s| s == "carry:Err(\"@1\")"),
        "{:#?}",
        d.said()
    );
    for lost_reply in [false, true] {
        let latches = Arc::new(LoginLatches::default());
        let host = Arc::new(LatchHost::tab(&latches, "@1"));
        let mut m = fenced(vec![lost.clone()]);
        m.help_send = Some(PROBE_HELP_SEND.to_string());
        if lost_reply {
            m.send_replies.push_back(CtlReply {
                stdout: String::new(),
                stderr: "server closed the connection without responding".to_string(),
                code: 1,
            });
        } else {
            m.human_seq = None;
        }
        let opts = SuperviseOpts {
            idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
            ..hosted(30)
        };
        let mut heard = Heard::default();
        let result = {
            let mut s = session(&mut m, None);
            s.set_clock(TEST_NOW, PDT);
            s.set_zone_lookup(test_zones);
            s.program = Some("claude".to_string());
            let read = s.screen().unwrap();
            let mut point = point_at(&s, read.rows.clone(), read.seq);
            point.screen = read;
            s.turn_end_execute(
                &point,
                TurnEndAction::TypeCommand {
                    command: "/login".to_string(),
                    rule_id: RULE_LOGIN,
                    then: crate::supervise::policy::turn_end::Then::Nothing,
                },
                &opts,
                &[],
                &mut heard,
            )
        };
        assert_eq!(result.is_err(), lost_reply, "{:?}", host.said());
        assert_eq!(
            m.requests.iter().filter(|r| verb_of(r) == "send").count(),
            usize::from(lost_reply),
            "{:#?}",
            m.requests
        );
        assert!(m.requests.iter().all(|r| verb_of(r) != "turn"));
        assert!(host.said().iter().any(|line| line == "carry:Ok(())"));
        assert!(host.said().iter().any(|line| line == "abandon"));
        assert_eq!(latches.shared(LATCH_KEY, "@2"), None);
        assert_eq!(latches.carry(LATCH_KEY, "@2"), Ok(()));
    }
}

#[test]
fn the_carrier_delivers_its_turn_again_where_the_login_came_back_elsewhere() {
    let wall_row = |r: &String| r.trim_end() == "❯ /login";
    let dialog = handoff();
    let at = dialog.iter().position(wall_row).expect("the /login row");
    let mut lost: Vec<String> = dialog[..at].to_vec();
    while lost.last().is_some_and(|r| r.trim().is_empty()) {
        lost.pop();
    }
    lost.push(String::new());
    lost.extend(composer("  ? for shortcuts"));
    let left = login_left_from(&dialog);
    let run = |back_in: Option<&'static str>| {
        let latches = Arc::new(LoginLatches::default());
        let host = Arc::new(LatchHost {
            back_in,
            ..LatchHost::tab(&latches, "@1")
        });
        let tag = if back_in.is_some() { "back" } else { "none" };
        let (dir, path) = journal_file(&format!("te-carrier-{tag}"));
        let mut m = Mock::new(vec![
            busy_screen(),
            lost.clone(),
            dialog.clone(),
            dialog.clone(),
            left.clone(),
            busy_screen(),
            ended("Pushed."),
        ]);
        m.turn_releases = Some(1);
        m.vanish_after = Some(2);
        let mut opts = SuperviseOpts {
            idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
            journal: Some(path.clone()),
            ..hosted(30)
        };
        opts.policy.continue_policy = false;
        let (lines, _) = watch_lines(&mut m, &opts);
        let (records, _) = journal_records(&path);
        let _ = std::fs::remove_dir_all(&dir);
        let notices: Vec<String> = records
            .iter()
            .filter(|r| r.kind == "exception")
            .filter_map(|r| {
                r.summary
                    .split_whitespace()
                    .find_map(|w| w.strip_prefix("notice="))
                    .map(str::to_string)
            })
            .collect();
        let typed = |rule: &str| {
            lines
                .iter()
                .filter(|l| {
                    (l.starts_with("TYPED ") || l.starts_with("CONTINUED "))
                        && l.contains(&format!(" rule={rule}"))
                })
                .count()
        };
        let counts = (typed(RULE_LOGIN), typed(RULE_LOGIN_BACK));
        let log = format!("{lines:#?}\n{records:#?}\n{:#?}", host.said());
        (notices, counts, host.said(), latches, log)
    };
    let (notices, (logins, backs), said, latches, log) = run(Some("@2"));
    assert_eq!(logins, 1, "`/login` once: {log}");
    assert_eq!(backs, 1, "the turn delivered again, once: {log}");
    assert_eq!(
        notices,
        ["login-back-dialog"],
        "the dialog to close, no sign-in, none cancelled: {log}"
    );
    assert_eq!(said.last().map(String::as_str), Some("back"), "{log}");
    assert_eq!(
        latches.shared(LATCH_KEY, "@1"),
        None,
        "its loss over: {log}"
    );
    let (notices, (logins, backs), _, _, log) = run(None);
    assert_eq!(logins, 1, "{log}");
    assert_eq!(backs, 0, "nothing typed into the left sign-in: {log}");
    assert_eq!(notices, ["login-sign-in", "login-cancelled"], "{log}");
}

fn login_left_from(dialog: &[String]) -> Vec<String> {
    let typed = dialog
        .iter()
        .position(|r| r.trim_end() == "❯ /login")
        .expect("the /login row");
    let mut r = dialog[..=typed].to_vec();
    r.push("  ⎿  Login interrupted".to_string());
    r.push(String::new());
    r.extend(composer("  ? for shortcuts"));
    r
}

fn at_no_wall(rows: &[String]) -> Vec<String> {
    rows.iter()
        .map(|r| {
            if r.contains("Not logged in · Please run /login") {
                "  ⎿  Interrupted · What should Claude do instead?".to_string()
            } else {
                r.clone()
            }
        })
        .collect()
}

#[test]
fn a_loop_started_inside_the_login_a_loop_before_typed_carries_it_on() {
    use crate::supervise::approvals::{Outcome, Row};
    use aterm_phase::prompt::fixtures::{LOGIN_PICKER_WARNED, screen};
    let picker = screen(LOGIN_PICKER_WARNED);
    let typed_before = || {
        let now_ms = crate::supervise::journal::unix_ms();
        let point = Row {
            rule_id: crate::supervise::approvals::RULE_POINT,
            outcome: Outcome::Waiting,
            command: "",
            reason: "the turn-end policy judged the point (short turns in a row: 0)",
            box_seq: 90,
        }
        .to_json(now_ms - 61_000, None);
        let login = Row {
            rule_id: RULE_LOGIN,
            outcome: Outcome::Typed,
            command: "/login",
            reason: "the turn-end policy",
            box_seq: 90,
        }
        .to_json(now_ms - 60_000, None);
        point + "\n" + &login + "\n"
    };
    let run = |tag: &str, screens: Vec<Vec<String>>| {
        let (dir, ledger) = ledger_at(tag);
        std::fs::write(&ledger, typed_before()).expect("the ledger");
        let (jdir, journal) = journal_file(&format!("te-{tag}"));
        let latches = Arc::new(LoginLatches::default());
        let host = Arc::new(LatchHost::tab(&latches, "@1"));
        let mut m = Mock::new(screens);
        m.vanish_after = Some(2);
        let opts = SuperviseOpts {
            idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
            journal: Some(journal.clone()),
            ..hosted(30)
        };
        let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
            s.set_approval_ledger(Some(ledger.clone()));
        });
        let (records, _) = journal_records(&journal);
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&jdir);
        let notices: Vec<String> = records
            .iter()
            .filter(|r| r.kind == "exception")
            .filter_map(|r| {
                r.summary
                    .split_whitespace()
                    .find_map(|w| w.strip_prefix("notice="))
                    .map(str::to_string)
            })
            .collect();
        let acts: Vec<String> = lines
            .iter()
            .filter(|l| l.starts_with("TYPED ") || l.starts_with("CONTINUED "))
            .cloned()
            .collect();
        let log = format!(
            "{lines:#?}\n{records:#?}\n{:#?}\n{:#?}",
            host.said(),
            m.requests
        );
        (notices, acts, host.said(), log)
    };
    let carried = |said: &[String]| said.iter().any(|s| s == "carry:Ok(())");

    let (notices, acts, said, log) = run(
        "successor-handoff",
        vec![handoff(), handoff(), login_left_from(&handoff())],
    );
    assert_eq!(notices, ["login-sign-in", "login-cancelled"], "{log}");
    assert!(acts.is_empty(), "nothing typed: {log}");
    assert!(carried(&said), "the sign-in is this tab's again: {log}");
    let (notices, acts, said, log) = run(
        "successor-picker",
        vec![picker.clone(), picker.clone(), login_left_from(&picker)],
    );
    assert_eq!(notices, ["login-choose", "login-cancelled"], "{log}");
    assert!(acts.is_empty(), "nothing typed: {log}");
    assert!(carried(&said), "{log}");
    let (notices, acts, said, log) = run(
        "successor-left",
        vec![login_left_from(&handoff()), login_left_from(&handoff())],
    );
    assert_eq!(notices, ["login-cancelled"], "{log}");
    assert!(acts.is_empty(), "nothing typed: {log}");
    assert!(!carried(&said), "a sign-in left is nobody's: {log}");

    for (tag, screens) in [
        (
            "successor-none-handoff",
            vec![
                at_no_wall(&handoff()),
                at_no_wall(&handoff()),
                at_no_wall(&login_left_from(&handoff())),
            ],
        ),
        (
            "successor-none-picker",
            vec![
                at_no_wall(&picker),
                at_no_wall(&picker),
                at_no_wall(&login_left_from(&picker)),
            ],
        ),
    ] {
        let (notices, acts, said, log) = run(tag, screens);
        assert!(notices.is_empty(), "no login notice: {log}");
        assert!(
            !acts.iter().any(|a| a.ends_with(" /login")),
            "no /login: {log}"
        );
        assert!(!carried(&said), "{log}");
    }
}

#[test]
fn the_loop_holds_its_own_turns_on_usage_credits_until_the_notices_reset() {
    use crate::supervise::policy::turn_end::{CREDITS_HOLD_TOLD, RULE_CREDITS_HOLD};
    use aterm_phase::prompt::fixtures::{CREDITS_IN_USE_2_1_300, CREDITS_STARTED_2_1_300, screen};
    let started = screen(CREDITS_STARTED_2_1_300);
    let lasting = screen(CREDITS_IN_USE_2_1_300);
    let run = |first: Vec<String>, usage_credits: bool, points: u64| {
        let mut m = Mock::new(vec![first]);
        let mut opts = hosted(30);
        opts.policy.usage_credits = usage_credits;
        let mut heard = Heard::default();
        let mut s = session(&mut m, Some("@s-1".to_string()));
        s.set_clock(TEST_NOW, PDT);
        s.set_zone_lookup(test_zones);
        let _ = s.screen().expect("the read before the point");
        let mut actions = Vec::new();
        for seq in 0..points {
            let point = point_at(&s, lasting.clone(), 102 + seq);
            let action = s
                .turn_end_at_point(&point, Some(Duration::from_secs(180)), &opts)
                .expect("a decision");
            s.turn_end_execute(&point, action.clone(), &opts, &[], &mut heard)
                .expect("carried out");
            actions.push(action);
        }
        let kept = s.credits_kept.clone().and_then(|k| k.reset);
        drop(s);
        let typed = count(&m, "@s-1 turn") + count(&m, "@s-1 send");
        (heard_lines(&heard), actions, typed, kept)
    };
    let nine_43 = TEST_NOW + 48 * 60;
    let (lines, actions, typed, kept) = run(started.clone(), false, 1);
    assert_eq!(typed, 0, "{lines:#?}");
    assert_eq!(kept.and_then(|k| k.unix), Some(nine_43));
    assert!(
        matches!(&actions[..], [TurnEndAction::WaitUntil { why, .. }] if why.contains(RULE_CREDITS_HOLD)),
        "{actions:#?}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("WAITING seq=102") && l.contains(RULE_CREDITS_HOLD)),
        "{lines:#?}"
    );
    let (lines, actions, typed, _) = run(lasting.clone(), false, 2);
    assert_eq!(typed, 0, "{lines:#?}");
    assert!(
        matches!(&actions[..], [TurnEndAction::Escalate { reason }, TurnEndAction::WaitUntil { why, .. }]
            if reason == CREDITS_HOLD_TOLD && why.contains(RULE_CREDITS_HOLD)),
        "{actions:#?}"
    );
    let (lines, _, typed, _) = run(started, true, 1);
    assert!(continued(&lines, RULE_CONTINUE), "{lines:#?}");
    assert_eq!(typed, 1, "{lines:#?}");
}

#[test]
fn the_loop_keeps_a_reset_ahead_and_a_models_credits_where_the_screen_draws_none() {
    use crate::supervise::policy::turn_end::{
        CREDITS_HOLD_MODEL_TOLD, CREDITS_HOLD_TOLD, RULE_CREDITS_HOLD,
    };
    use aterm_phase::prompt::fixtures::{CREDITS_IN_USE_2_1_300, CREDITS_STARTED_2_1_300, screen};
    let started = screen(CREDITS_STARTED_2_1_300);
    let lasting = screen(CREDITS_IN_USE_2_1_300);
    let bound = TurnEndTiming::default().credits_bound;
    let drawn = |notice: &str| {
        let mut rows = lasting.clone();
        let mode = rows
            .iter()
            .position(|r| r.contains("auto mode on"))
            .expect("the mode row");
        let width = rows[mode].trim_end().chars().count();
        let lead = "  ⏵⏵ auto mode on (shift+tab to cycle) · ← for agents";
        let room = width - lead.chars().count();
        assert!(notice.is_empty() || notice.chars().count() + 2 <= room);
        rows[mode] = format!("{lead}{notice:>room$}");
        rows
    };
    let quiet = drawn("");
    let nine_43 = TEST_NOW + 48 * 60;
    let run = |reads: Vec<Vec<String>>, passed: bool, fallback: bool, bound: Duration| {
        let mut m = Mock::new(reads.clone());
        let mut opts = hosted(30);
        if !fallback {
            opts.policy.model_fallback = None;
        }
        let mut heard = Heard::default();
        let mut s = session(&mut m, Some("@s-1".to_string()));
        s.set_clock(TEST_NOW, PDT);
        s.set_zone_lookup(test_zones);
        s.turn_end.timing.credits_bound = bound;
        let _ = s.screen().expect("the first read");
        if passed && let Some(r) = s.credits_kept.as_mut().and_then(|k| k.reset.as_mut()) {
            r.unix = Some(TEST_NOW - 60);
        }
        for _ in 1..reads.len() {
            let _ = s.screen().expect("a read");
        }
        let kept = s.credits_kept.clone();
        let point = point_at(&s, quiet.clone(), 102);
        let action = s
            .turn_end_at_point(&point, Some(Duration::from_secs(180)), &opts)
            .expect("a decision");
        s.turn_end_execute(&point, action.clone(), &opts, &[], &mut heard)
            .expect("carried out");
        drop(s);
        let typed = count(&m, "@s-1 turn") + count(&m, "@s-1 send");
        (heard_lines(&heard), action, typed, kept)
    };
    let (lines, action, typed, kept) =
        run(vec![started.clone(), quiet.clone()], false, true, bound);
    assert_eq!(typed, 0, "{lines:#?}");
    assert_eq!(
        kept.and_then(|k| k.reset).and_then(|r| r.unix),
        Some(nine_43)
    );
    assert!(
        matches!(&action, TurnEndAction::WaitUntil { why, .. } if why.contains(RULE_CREDITS_HOLD)),
        "{action:#?}"
    );
    let (lines, _, typed, kept) = run(vec![started, quiet.clone()], true, true, bound);
    assert_eq!(kept, None);
    assert!(continued(&lines, RULE_CONTINUE), "{lines:#?}");
    assert_eq!(typed, 1, "{lines:#?}");
    for (notice, reset_text) in [
        ("You're now using usage credits", None),
        (
            "You're now using usage credits · resets 9am tomorrow",
            Some("9am tomorrow"),
        ),
    ] {
        let rows = drawn(notice);
        let (lines, action, typed, kept) =
            run(vec![rows.clone(), quiet.clone()], false, true, bound);
        assert_eq!(typed, 0, "{notice}: {lines:#?}");
        let kept = kept.expect("the account's known credits");
        assert_eq!(kept.model, None);
        assert_eq!(kept.reset.as_ref().map(|r| r.text.as_str()), reset_text);
        assert_eq!(kept.reset.and_then(|r| r.unix), None);
        assert!(
            matches!(&action, TurnEndAction::Escalate { reason } if reason == CREDITS_HOLD_TOLD),
            "{notice}: {action:#?}"
        );
        let (lines, _, typed, kept) = run(vec![rows, quiet.clone()], false, true, Duration::ZERO);
        assert_eq!(kept, None, "{notice}");
        assert!(continued(&lines, RULE_CONTINUE), "{notice}: {lines:#?}");
        assert_eq!(typed, 1, "{notice}: {lines:#?}");
    }
    let model = drawn("Fable 5.1 is drawing from usage credits");
    let (lines, action, typed, kept) = run(vec![model.clone(), quiet.clone()], false, false, bound);
    assert_eq!(typed, 0, "{lines:#?}");
    assert_eq!(kept.and_then(|k| k.model).as_deref(), Some("fable"));
    assert!(
        matches!(&action, TurnEndAction::Escalate { reason } if reason == CREDITS_HOLD_MODEL_TOLD),
        "{action:#?}"
    );
    let mut m = Mock::new(vec![model, quiet]);
    let mut s = session(&mut m, Some("@s-1".to_string()));
    s.set_clock(TEST_NOW, PDT);
    let _ = s.screen().expect("the notice");
    let host = Arc::new(AccountHost::default());
    *host.started.lock().unwrap() = Some(TEST_NOW + 30);
    s.stall_host = Some(Arc::clone(&host) as Arc<dyn IdleHost>);
    let _ = s.screen().expect("the quiet read");
    assert_eq!(s.credits_kept, None);
}

#[test]
fn the_hosts_background_step_is_withheld_on_usage_credits() {
    let bg = |credits: bool| {
        let mut r = rows(&[
            "⏺ Waiting for the build.",
            "",
            "✻ Crunched for 9m 27s · done 11:07 AM · 1 shell still running",
            "",
        ]);
        r.extend(composer("  ⏵⏵ auto mode on · 1 shell · ← for agents"));
        if credits {
            let at = r.len() - 1;
            r[at] = format!("{:<95}Now using usage credits", r[at]);
        }
        r
    };
    let run = |credits: bool, usage_credits: bool| {
        let host = Arc::new(BackgroundHost::default());
        let mut m = Mock::new(vec![bg(credits)]);
        let mut s = session(&mut m, Some("@s-1".to_string()));
        s.set_clock(TEST_NOW, PDT);
        s.stall_host = Some(Arc::clone(&host) as Arc<dyn IdleHost>);
        s.setup.background_settle = Duration::ZERO;
        let screen = s.screen().expect("the busy read");
        let cfg = SupervisorConfig {
            usage_credits,
            ..SupervisorConfig::default()
        };
        let mut since = Some(Instant::now());
        s.host_steps_in_background(&screen, &mut since, Some(&cfg), &mut Alone)
            .expect("the break's step");
        let offered = host.backgrounds.load(Ordering::SeqCst);
        let withheld: Vec<&str> = host
            .withheld
            .lock()
            .unwrap()
            .iter()
            .map(|g| g.word())
            .collect();
        (offered, withheld)
    };
    assert_eq!(run(true, false), (0, vec!["credits"]));
    assert_eq!(run(true, true), (1, vec![]));
    assert_eq!(run(false, false), (1, vec![]));
}

#[test]
fn a_need_said_early_in_a_tall_turn_that_claims_no_done_is_raised() {
    let fit = |mut drawn: Vec<String>, rows: usize| {
        if drawn.len() > rows {
            let screen = drawn.split_off(drawn.len() - rows);
            (screen, drawn)
        } else {
            (drawn, Vec::new())
        }
    };
    let claude_turn = |calls: usize| {
        let mut drawn: Vec<String> = vec![
            "❯ Fix the parser and land it.".to_string(),
            String::new(),
            "⏺ Merged the lexer and its suites.".to_string(),
            "  NEEDS YOU: the signing credential".to_string(),
        ];
        for call in 0..calls {
            drawn.extend([
                String::new(),
                format!("⏺ Bash(git log --oneline -1 branch-{call})"),
                format!("  ⎿  {call:04}abc merged branch {call}"),
            ]);
        }
        drawn.extend(rows(&[
            "",
            "⏺ Fixed the parser; pushing it next.",
            "",
            "✻ Worked for 12m 2s · done 4:24 PM",
            "",
        ]));
        drawn.extend(composer("  ⏵⏵ bypass permissions on (shift+tab to cycle)"));
        fit(drawn, 30)
    };
    let claude_message = |lines: usize| {
        let mut drawn: Vec<String> = vec![
            "❯ Fix the parser.".to_string(),
            String::new(),
            "⏺ NEEDS YOU: the signing credential".to_string(),
        ];
        for i in 0..lines {
            drawn.push(format!("  - merged branch {i:02} and ran its suite"));
        }
        drawn.push("  Pushing the parser next.".to_string());
        drawn.extend(rows(&["", "✻ Worked for 12m 2s · done 4:24 PM", ""]));
        drawn.extend(composer("  ⏵⏵ bypass permissions on (shift+tab to cycle)"));
        fit(drawn, 30)
    };
    let gemini_turn = |tall: bool| {
        use aterm_phase::gemini;
        use aterm_phase::prompt::fixtures::screen;
        let end = screen(gemini::fixtures::END_OF_TURN);
        let at = end
            .iter()
            .rposition(|r| r.starts_with("✦ "))
            .expect("the reply");
        let mut drawn = end[..at].to_vec();
        drawn.push("✦ NEEDS YOU: the signing credential".to_string());
        drawn.push("  I will push the branch now.".to_string());
        drawn.push(String::new());
        drawn.push("╭────────────────────────────────────────────╮".to_string());
        for call in 0..(if tall { 60 } else { 1 }) {
            drawn.push(format!(
                "│ ✓  Shell git push origin branch-{call:02}             │"
            ));
        }
        drawn.push("╰────────────────────────────────────────────╯".to_string());
        drawn.push(String::new());
        drawn.push("✦ I will push the next branch now.".to_string());
        drawn.extend(end[at + 1..].iter().cloned());
        if tall {
            fit(drawn, end.len())
        } else {
            (drawn, Vec::new())
        }
    };
    let run = |mock: fn(Vec<Vec<String>>) -> Mock,
               busy: Vec<String>,
               work: Vec<String>,
               word: &'static str,
               (screen, above): (Vec<String>, Vec<String>)| {
        let (dir, journal) = journal_file("tall-need");
        let mut m = mock(vec![busy.clone(), work, busy, screen]);
        landing(&mut m);
        m.autopilot = Some(word);
        m.gen_fence = true;
        m.sends_gen = true;
        m.scrollback = Some(above);
        m.scrollback_from = Some(3);
        m.alt_screen = Some(false);
        m.vanish_after = Some(6);
        m.stall_sleep = Some(Duration::from_millis(20));
        let opts = SuperviseOpts {
            policy: owners_policy(),
            journal: Some(journal.clone()),
            ..auto(30)
        };
        let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
            s.set_turn_end_timing(TurnEndTiming {
                min_work: Duration::ZERO,
                short_backoff: Duration::from_millis(30),
                short_backoff_max: Duration::from_millis(60),
                ..TurnEndTiming::default()
            });
        });
        let _ = std::fs::remove_dir_all(&dir);
        let raised = m.needs_calls.iter().any(|c| {
            c.starts_with("set attention owner=needs ") && c.contains("the signing credential")
        });
        let before: Vec<&String> = lines
            .iter()
            .filter(|l| l.starts_with("CONTINUED seq=102 ") && l.contains("NEEDS YOU"))
            .collect();
        (raised, before.is_empty(), lines)
    };
    for word in ["cruise", "land"] {
        for (why, turn) in [
            ("an earlier message", claude_turn(20)),
            ("a tall message's head", claude_message(40)),
            ("control: on the screen", claude_turn(1)),
        ] {
            let tall = !turn.1.is_empty();
            let (raised, clean_before, lines) =
                run(Mock::new, busy_screen(), ended(WORK), word, turn);
            assert!(raised, "{word} {why} (tall={tall}): {lines:#?}");
            assert!(clean_before, "{word} {why}: {lines:#?}");
        }
    }
    for tall in [true, false] {
        use aterm_phase::gemini;
        use aterm_phase::prompt::fixtures::screen;
        let turn = gemini_turn(tall);
        assert_eq!(!turn.1.is_empty(), tall);
        let (raised, clean_before, lines) = run(
            gemini_mock,
            screen(gemini::fixtures::BUSY_THINKING),
            screen(gemini::fixtures::END_OF_TURN),
            "cruise",
            turn,
        );
        assert!(raised, "gemini tall={tall}: {lines:#?}");
        assert!(clean_before, "gemini tall={tall}: {lines:#?}");
    }
}

fn answered_turn(prompt: &str, said: &str, done: &str) -> Vec<String> {
    let mut r = rows(&[
        &format!("❯ {prompt}"),
        "",
        &format!("⏺ {said}"),
        "",
        done,
        "",
    ]);
    r.extend(composer("  ⏵⏵ bypass permissions on (shift+tab to cycle)"));
    r
}

#[test]
fn a_turn_the_verdict_never_saw_busy_is_read_and_judged_within_a_recheck() {
    const PROMPT: &str = "write the parser for this repository";
    const ASK: &str = "what stands now?";
    const REPLY: &str = "Here is where it stands: the parser module is written and tested.";
    const FIRST: &str = "Wrote the parser module.";
    let under = |top: &str, prompt: &str, said: &str, done: &str| {
        let mut r = vec![format!("⏺ {top}")];
        r.extend((2..=10).map(|i| format!("⏺ Earlier: step {i} of the plan is in.")));
        r.push(String::new());
        r.extend(answered_turn(prompt, said, done));
        r
    };
    const TOP: &str = "Earlier: step 1 of the plan is in.";
    let first = under(TOP, PROMPT, FIRST, "✻ Worked for 3s · done 4:24 PM");
    let run = |tag: &str, next: Vec<String>| {
        let mut m = Mock::new(vec![busy_screen(), first.clone(), next]);
        m.sends_gen = true;
        m.agent_pushes = true;
        m.stall_sleep = Some(Duration::from_secs(20));
        m.vanish_after = Some(40);
        let (dir, path) = journal_file(tag);
        let opts = SuperviseOpts {
            journal: Some(path.clone()),
            ..hosted(3600)
        };
        let _ = watch_lines(&mut m, &opts);
        let (records, _) = journal_records(&path);
        let _ = std::fs::remove_dir_all(&dir);
        let lines: Vec<String> = records.into_iter().map(|r| r.line).collect();
        (m, lines)
    };
    let dirty_checks = |m: &Mock| {
        m.requests
            .iter()
            .filter(|r| r.starts_with("await seq ") && r.ends_with(" timeout 0"))
            .count()
    };
    let (m, lines) = run(
        "p4-unseen-turn",
        under(TOP, ASK, REPLY, "✻ Worked for 0s · done 4:25 PM"),
    );
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("EVENT idle seq=") && l.contains("Here is where it stands")),
        "the person's turn is a point: {lines:#?}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("WAITING ") && l.contains("2 short turn(s) in a row")),
        "judged as a turn of someone else's, backed off from there: {lines:#?}"
    );
    assert_eq!(
        m.turns, 0,
        "nothing typed over it: {lines:#?}\n{:#?}",
        m.requests
    );
    assert!(dirty_checks(&m) >= 1, "{:#?}", m.requests);
    let (m, lines) = run(
        "p4-redrawn-above",
        under(
            "Earlier: the first step of the plan, redrawn.",
            PROMPT,
            FIRST,
            "✻ Worked for 3s · done 4:24 PM",
        ),
    );
    let points = lines
        .iter()
        .filter(|l| l.starts_with("EVENT idle seq="))
        .count();
    assert_eq!(points, 1, "no new point: {lines:#?}");
    assert!(
        !lines.iter().any(|l| l.contains("2 short turn(s)")),
        "{lines:#?}"
    );
    assert_eq!(
        m.turns, 1,
        "the first point's act, once: {lines:#?}\n{:#?}",
        m.requests
    );
    assert!(dirty_checks(&m) >= 1, "{:#?}", m.requests);
}

#[test]
fn a_turn_the_points_key_cannot_tell_is_judged_by_its_submission() {
    const PROMPT: &str = "write the parser for this repository";
    const FIRST: &str = "Wrote the parser module.";
    const DONE: &str = "✻ Worked for 3s · done 4:24 PM";
    let under = |top: &str| {
        let mut r = vec![format!("⏺ {top}")];
        r.extend((2..=10).map(|i| format!("⏺ Earlier: step {i} of the plan is in.")));
        r.push(String::new());
        r.extend(answered_turn(PROMPT, FIRST, DONE));
        r
    };
    let first = under("Earlier: step 1 of the plan is in.");
    let run = |tag: &str, next: Vec<String>, submits: [u64; 2]| {
        let mut m = Mock::new(vec![busy_screen(), first.clone(), next]);
        m.submits_on = [(1, submits[0]), (2, submits[1])].into_iter().collect();
        m.sends_gen = true;
        m.agent_pushes = true;
        m.stall_sleep = Some(Duration::from_secs(20));
        m.vanish_after = Some(40);
        let (dir, path) = journal_file(tag);
        let opts = SuperviseOpts {
            journal: Some(path.clone()),
            ..hosted(3600)
        };
        let _ = watch_lines(&mut m, &opts);
        let (records, _) = journal_records(&path);
        let _ = std::fs::remove_dir_all(&dir);
        let lines: Vec<String> = records.into_iter().map(|r| r.line).collect();
        let points = lines
            .iter()
            .filter(|l| l.starts_with("EVENT idle seq="))
            .count();
        (m, lines, points)
    };
    let scrolled = under("Earlier: the plan's first step, scrolled by the turn below.");
    let (m, lines, points) = run("same-words-submitted", scrolled.clone(), [1, 2]);
    assert_eq!(points, 2, "the person's turn is a point: {lines:#?}");
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("WAITING ") && l.contains("2 short turn(s) in a row")),
        "judged as a turn of someone else's, backed off from there: {lines:#?}"
    );
    assert_eq!(
        m.turns, 0,
        "nothing typed over it: {lines:#?}\n{:#?}",
        m.requests
    );
    for (tag, next, submits) in [
        ("same-words-unsubmitted", scrolled.clone(), [1, 1]),
        ("submitted-undrawn", first.clone(), [1, 2]),
    ] {
        let (m, lines, points) = run(tag, next, submits);
        assert_eq!(points, 1, "{tag}: no new point: {lines:#?}");
        assert!(
            !lines.iter().any(|l| l.contains("2 short turn(s)")),
            "{tag}: {lines:#?}"
        );
        assert_eq!(
            m.turns, 1,
            "{tag}: the first point's act, once: {lines:#?}\n{:#?}",
            m.requests
        );
    }
}

/// A host that saw the agent LAUNCHED at `launch` (unix ms; `None`: it saw
/// no launch, as for a supervisor attaching to an agent it did not start, or
/// one a person keyed into since) and started this loop as the session's Off
/// lifted where `lifted`; it wants no point and types nothing.
#[derive(Debug, Default)]
struct LaunchHost {
    launch: Option<i64>,
    lifted: bool,
    asked: std::sync::atomic::AtomicUsize,
}

impl LaunchHost {
    fn saw(launch: Option<i64>) -> Arc<Self> {
        Arc::new(Self {
            launch,
            ..Self::default()
        })
    }
}

impl IdleHost for LaunchHost {
    fn wants(&self) -> bool {
        false
    }
    fn at_idle(&self) -> Option<HostStep> {
        None
    }
    fn owns_turn_end(&self) -> bool {
        false
    }
    fn lifted_off(&self) -> bool {
        self.lifted
    }
    fn launch(&self) -> Option<i64> {
        self.asked.fetch_add(1, Ordering::SeqCst);
        self.launch
    }
}

const LAUNCHED_MS: i64 = 1_792_520_389_950;

fn launched_run(
    tag: &str,
    m: &mut Mock,
    host: &Arc<LaunchHost>,
    books: Option<&str>,
) -> (Vec<String>, Vec<String>) {
    let (dir, ledger) = ledger_at(tag);
    if let Some(books) = books {
        std::fs::write(&ledger, books).expect("the ledger");
    }
    let journal = dir.join("journal.jsonl");
    m.vanish_after = Some(3);
    let opts = SuperviseOpts {
        idle_host: Some(Arc::clone(host) as Arc<dyn IdleHost>),
        journal: Some(journal.clone()),
        ..hosted(60)
    };
    let _ = watch_lines_with(m, &opts, |s| {
        s.set_approval_ledger(books.map(|_| ledger.clone()));
    });
    let lines = journal_records(&journal)
        .0
        .into_iter()
        .map(|r| r.line)
        .collect();
    let rows = std::fs::read_to_string(&ledger)
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect();
    let _ = std::fs::remove_dir_all(&dir);
    (lines, rows)
}

fn point_rows(rows: &[String]) -> Vec<(u64, u32)> {
    rows.iter()
        .filter(|r| r.contains(&format!("\"rule_id\":\"{}\"", approvals::RULE_POINT)))
        .map(|r| {
            let num = |key: &str| -> u64 {
                let at = r.find(key).unwrap_or_else(|| panic!("{key} in {r}")) + key.len();
                r[at..]
                    .chars()
                    .take_while(char::is_ascii_digit)
                    .collect::<String>()
                    .parse()
                    .unwrap_or_else(|_| panic!("{key} in {r}"))
            };
            let streak = u32::try_from(num(": ")).unwrap_or(u32::MAX);
            (num("\"box_seq\":"), streak)
        })
        .collect()
}

fn back_offs(lines: &[String]) -> Vec<String> {
    lines
        .iter()
        .filter(|l| l.starts_with("WAITING ") && l.contains("back-off"))
        .map(|l| l.split_once("Z ").map_or(l.clone(), |(_, t)| t.to_string()))
        .collect()
}

const LAB_REPLY: &str = "Implemented the parser module and its unit tests in this turn.";

type LaunchCase = (&'static str, fn() -> Mock, u64, u64);

fn launches() -> Vec<LaunchCase> {
    use aterm_phase::prompt::fixtures::screen;
    fn gemini() -> Mock {
        use aterm_phase::gemini::fixtures as g;
        gemini_mock(vec![
            screen(g::PRE_BANNER),
            screen(g::BUSY_THINKING),
            gemini_ended(LAB_REPLY),
        ])
    }
    fn muse() -> Mock {
        use aterm_phase::muse::fixtures as u;
        let blank = vec![String::new(); 50];
        muse_mock(vec![
            blank.clone(),
            blank.clone(),
            blank.clone(),
            blank,
            screen(u::BUSY),
            screen(u::END_OF_TURN),
        ])
    }
    vec![("gemini", gemini, 101, 103), ("muse", muse, 104, 106)]
}

#[test]
fn a_launchs_frame_before_any_prompt_is_no_short_turn() {
    for (agent, mock, first, reply) in launches() {
        let host = LaunchHost::saw(Some(LAUNCHED_MS));
        let (lines, rows) = launched_run(&format!("p3-{agent}"), &mut mock(), &host, Some(""));
        assert_eq!(point_rows(&rows), [(reply, 1)], "{agent}: {rows:#?}");
        assert_eq!(
            back_offs(&lines),
            ["1 short turn(s) in a row: 2 min back-off"],
            "{agent}: {lines:#?}"
        );
        assert!(
            lines.contains(&format!("EVENT idle seq={first} -")),
            "{agent}: the frame was read at a point: {lines:#?}"
        );
        assert!(host.asked.load(Ordering::SeqCst) >= 1, "{agent}");
        let host = LaunchHost::saw(None);
        let (lines, rows) =
            launched_run(&format!("p3-{agent}-attach"), &mut mock(), &host, Some(""));
        assert_eq!(
            point_rows(&rows),
            [(first, 1), (reply, 2)],
            "{agent}: {rows:#?}"
        );
        assert_eq!(
            back_offs(&lines).last().map(String::as_str),
            Some("2 short turn(s) in a row: 4 min back-off"),
            "{agent}: {lines:#?}"
        );
        let host = LaunchHost::saw(Some(LAUNCHED_MS));
        let (lines, _) = launched_run(&format!("p3-{agent}-unbooked"), &mut mock(), &host, None);
        assert_eq!(
            back_offs(&lines).last().map(String::as_str),
            Some("2 short turn(s) in a row: 4 min back-off"),
            "{agent}: {lines:#?}"
        );
    }
}

#[test]
fn a_launch_with_a_task_counts_its_first_reply() {
    use aterm_phase::gemini::fixtures as g;
    use aterm_phase::prompt::fixtures::screen;
    for (label, screens, at) in [
        (
            "the frame first",
            vec![
                screen(g::PRE_BANNER),
                screen(g::BUSY_THINKING),
                gemini_ended(LAB_REPLY),
            ],
            103,
        ),
        (
            "its turn running first",
            vec![screen(g::BUSY_THINKING), gemini_ended(LAB_REPLY)],
            102,
        ),
        ("its reply first", vec![gemini_ended(LAB_REPLY)], 101),
    ] {
        let host = LaunchHost::saw(Some(LAUNCHED_MS));
        let mut m = gemini_mock(screens);
        let (lines, rows) = launched_run("p3-tasked", &mut m, &host, Some(""));
        assert_eq!(point_rows(&rows), [(at, 1)], "{label}: {rows:#?}");
        assert_eq!(
            back_offs(&lines),
            ["1 short turn(s) in a row: 2 min back-off"],
            "{label}: {lines:#?}"
        );
    }
    let mut m = gemini_mock(vec![
        screen(g::PRE_BANNER),
        screen(g::BUSY_THINKING),
        gemini_ended(LAB_REPLY),
    ]);
    let (_, rows) = launched_run("p3-tasked-attach", &mut m, &LaunchHost::saw(None), Some(""));
    assert_eq!(point_rows(&rows), [(101, 1), (103, 2)], "{rows:#?}");
}

#[test]
fn a_successors_first_point_keeps_its_seeds() {
    use aterm_phase::gemini::fixtures as g;
    use aterm_phase::prompt::fixtures::screen;
    let screens = || {
        vec![
            screen(g::PRE_BANNER),
            screen(g::BUSY_THINKING),
            gemini_ended(LAB_REPLY),
        ]
    };
    let host = Arc::new(LaunchHost {
        launch: Some(LAUNCHED_MS),
        lifted: true,
        ..LaunchHost::default()
    });
    let (_, rows) = launched_run("p3-lifted", &mut gemini_mock(screens()), &host, Some(""));
    assert_eq!(point_rows(&rows), [(101, 1), (103, 2)], "{rows:#?}");
    let (_, books) = launched_run(
        "p3-books",
        &mut gemini_mock(vec![screen(g::PRE_BANNER), screen(g::BUSY_THINKING)]),
        &LaunchHost::saw(None),
        Some(""),
    );
    assert_eq!(point_rows(&books), [(101, 1)], "the books: {books:#?}");
    assert!(
        books
            .last()
            .is_some_and(|r| r.contains(approvals::RULE_TURN_RAN)),
        "the books: {books:#?}"
    );
    let books = books.join("\n") + "\n";
    let (_, rows) = launched_run(
        "p3-successor",
        &mut gemini_mock(screens()),
        &LaunchHost::saw(Some(LAUNCHED_MS)),
        Some(&books),
    );
    let after: Vec<(u64, u32)> = point_rows(&rows).into_iter().skip(1).collect();
    assert_eq!(after, [(101, 2), (103, 3)], "{rows:#?}");
    let act = approvals::Row {
        rule_id: RULE_LAND,
        outcome: approvals::Outcome::Typed,
        command: LAND,
        reason: "the turn-end policy (short turns in a row: 1)",
        box_seq: 90,
    }
    .to_json(crate::supervise::journal::unix_ms() - 10_000, None);
    let mut m = gemini_mock(screens());
    m.autopilot = Some("land");
    let (lines, rows) = launched_run(
        "p3-land-act",
        &mut m,
        &LaunchHost::saw(Some(LAUNCHED_MS)),
        Some(&format!("{act}\n")),
    );
    assert_eq!(
        point_rows(&rows).first(),
        Some(&(101, 2)),
        "{rows:#?}\n{lines:#?}"
    );
    assert!(!lines.iter().any(|l| l.starts_with("DONE ")), "{lines:#?}");
}

#[test]
fn an_agent_started_again_since_its_books_is_a_launchs() {
    use aterm_phase::gemini::fixtures as g;
    use aterm_phase::prompt::fixtures::screen;
    let screens = || {
        vec![
            screen(g::PRE_BANNER),
            screen(g::BUSY_THINKING),
            gemini_ended(LAB_REPLY),
        ]
    };
    let (_, books) = launched_run(
        "p3b-books",
        &mut gemini_mock(vec![screen(g::PRE_BANNER), screen(g::BUSY_THINKING)]),
        &LaunchHost::saw(None),
        Some(""),
    );
    assert_eq!(point_rows(&books), [(101, 1)], "the books: {books:#?}");
    let books = books.join("\n") + "\n";
    let last = approvals::last_row_ms(&books).expect("the books' last row");
    let host = LaunchHost::saw(Some(last + 1_000));
    let (lines, rows) = launched_run(
        "p3b-started-again",
        &mut gemini_mock(screens()),
        &host,
        Some(&books),
    );
    let after: Vec<(u64, u32)> = point_rows(&rows).into_iter().skip(1).collect();
    assert_eq!(after, [(103, 2)], "{rows:#?}\n{lines:#?}");
    assert_eq!(
        back_offs(&lines),
        ["2 short turn(s) in a row: 4 min back-off"],
        "{lines:#?}"
    );
    assert!(
        lines.contains(&"EVENT idle seq=101 -".to_string()),
        "the frame was read at a point: {lines:#?}"
    );
    assert!(host.asked.load(Ordering::SeqCst) >= 1);
    for (tag, launch) in [
        ("p3b-same-process", Some(last - 1_000)),
        ("p3b-unseen", None),
    ] {
        let (lines, rows) = launched_run(
            tag,
            &mut gemini_mock(screens()),
            &LaunchHost::saw(launch),
            Some(&books),
        );
        let after: Vec<(u64, u32)> = point_rows(&rows).into_iter().skip(1).collect();
        assert_eq!(after, [(101, 2), (103, 3)], "{tag}: {rows:#?}\n{lines:#?}");
        assert_eq!(
            back_offs(&lines).last().map(String::as_str),
            Some("3 short turn(s) in a row: 8 min back-off"),
            "{tag}: {lines:#?}"
        );
    }
}

#[test]
fn a_screen_cleared_after_the_first_point_still_owes_its_act() {
    use aterm_phase::gemini::fixtures as g;
    use aterm_phase::prompt::fixtures::screen;
    let host = LaunchHost::saw(Some(LAUNCHED_MS));
    let mut m = gemini_mock(vec![
        screen(g::PRE_BANNER),
        screen(g::BUSY_THINKING),
        gemini_ended(LAB_REPLY),
        screen(g::PRE_BANNER),
    ]);
    let (lines, rows) = launched_run("p3-cleared", &mut m, &host, Some(""));
    assert!(
        lines.contains(&"EVENT idle seq=104 -".to_string()),
        "{lines:#?}"
    );
    assert!(
        lines.iter().any(|l| l.starts_with("WAITING seq=104 ")
            && l.ends_with("1 short turn(s) in a row: 2 min back-off")),
        "the back-off stands at the cleared screen: {lines:#?}"
    );
    assert_eq!(point_rows(&rows), [(103, 1)], "{rows:#?}");
    assert_eq!(host.asked.load(Ordering::SeqCst), 1, "{lines:#?}");
    let host = LaunchHost::saw(Some(LAUNCHED_MS));
    let mut m = gemini_mock(vec![
        screen(g::PRE_BANNER),
        screen(g::BUSY_THINKING),
        gemini_ended(LAB_REPLY),
        screen(g::BUSY_THINKING),
        screen(g::PRE_BANNER),
    ]);
    let (lines, rows) = launched_run("p3-cleared-ran", &mut m, &host, Some(""));
    assert_eq!(point_rows(&rows), [(103, 1), (105, 2)], "{rows:#?}");
    assert_eq!(
        back_offs(&lines).last().map(String::as_str),
        Some("2 short turn(s) in a row: 4 min back-off"),
        "{lines:#?}"
    );
}

fn gemini_tool_only_end() -> Vec<String> {
    let mut rows =
        aterm_phase::prompt::fixtures::screen(aterm_phase::gemini::fixtures::YOLO_TOOL_RAN);
    let at = rows
        .iter()
        .rposition(|r| r.starts_with("✦ Tool call done."))
        .expect("the reply");
    rows[at] = String::new();
    rows
}

fn policy_reading(program: &str, rows: &[String]) -> TurnEndReading {
    let reading = aterm_phase::read(Some(program), rows, None);
    TurnEndReading::of(
        &reading,
        rows,
        crate::supervise::policy::turn_end::ComposerRead {
            typed: Some(false),
            menu_fg: None,
        },
        None,
        None,
        None,
    )
}

#[test]
fn a_turn_ended_with_no_words_still_holds_a_conversation() {
    use aterm_phase::prompt::fixtures::screen;
    let rows = gemini_tool_only_end();
    assert!(
        rows.iter().filter(|row| row.starts_with(" > ")).count() >= 2,
        "two messages on the screen"
    );
    let r = policy_reading("gemini", &rows);
    assert_eq!((r.said_tail.as_deref(), r.opened_by), (None, None));
    assert!(r.message_drawn, "{r:?}");
    assert!(!r.holds_no_conversation(), "{r:?}");
    let measured = policy_reading(
        "gemini",
        &screen(aterm_phase::gemini::fixtures::YOLO_TOOL_RAN),
    );
    assert!(!measured.holds_no_conversation(), "{measured:?}");
    let pre = policy_reading("gemini", &screen(aterm_phase::gemini::fixtures::PRE_BANNER));
    assert!(!pre.message_drawn, "{pre:?}");
    assert!(pre.holds_no_conversation(), "{pre:?}");
    let blank = vec![String::new(); 50];
    let muse = policy_reading("muse", &blank);
    assert!(!muse.message_drawn, "{muse:?}");
}

#[test]
fn a_first_turn_ended_with_no_words_after_a_launch_is_judged() {
    use aterm_phase::gemini::fixtures as g;
    use aterm_phase::prompt::fixtures::screen;
    let screens = || {
        vec![
            screen(g::PRE_BANNER),
            gemini_tool_only_end(),
            gemini_tool_only_end(),
            gemini_tool_only_end(),
        ]
    };
    let host = LaunchHost::saw(Some(LAUNCHED_MS));
    let mut m = gemini_mock(screens());
    let (lines, rows) = launched_run("rv2-launch", &mut m, &host, Some(""));
    assert_eq!(point_rows(&rows).len(), 1, "the turn's point: {rows:#?}");
    assert_eq!(
        back_offs(&lines),
        ["1 short turn(s) in a row: 2 min back-off"],
        "{lines:#?}"
    );
    let host = LaunchHost::saw(None);
    let mut m = gemini_mock(screens());
    let (_, rows_attach) = launched_run("rv2-attach", &mut m, &host, Some(""));
    assert_eq!(point_rows(&rows_attach), [(101, 1)], "{rows_attach:#?}");
}

#[test]
fn a_first_turn_ended_on_muses_read_row_after_a_launch_is_judged() {
    use aterm_phase::muse::fixtures as u;
    use aterm_phase::prompt::fixtures::screen;
    let ended = screen(u::END_OF_TURN_READ_ROW);
    let run = |tag: &str, end: &[String]| {
        let screens = vec![
            screen(u::IDLE_1_4_10),
            end.to_vec(),
            end.to_vec(),
            end.to_vec(),
        ];
        let host = LaunchHost::saw(Some(LAUNCHED_MS));
        let mut m = muse_mock(screens);
        launched_run(tag, &mut m, &host, Some(""))
    };
    let (lines, rows) = run("f3-muse-read", &ended);
    assert_eq!(point_rows(&rows).len(), 1, "the turn's point: {rows:#?}");
    assert_eq!(
        back_offs(&lines),
        ["1 short turn(s) in a row: 2 min back-off"],
        "{lines:#?}"
    );
    let mut glued = ended.clone();
    let at = glued
        .iter()
        .position(|r| r == "  Read README.md")
        .expect("the read row");
    glued[at - 1] = "  and every test it needs".to_string();
    let (lines, _) = run("f3-muse-glued", &glued);
    assert!(back_offs(&lines).is_empty(), "{lines:#?}");
}

#[test]
fn a_top_reveal_keeps_the_point_key_and_brings_the_opener_into_sight() {
    const ACT: &str = "keep going";
    let mut body: Vec<String> = vec![
        format!("❯ {ACT}"),
        String::new(),
        "⏺ Bash(cargo test)".to_string(),
        "  ⎿  test result: ok. 41 passed".to_string(),
        String::new(),
        "⏺ All done. Nothing left to do. The report:".to_string(),
    ];
    body.extend((1..=12).map(|i| format!("  - item {i} of the parser is written and tested")));
    body.extend([
        String::new(),
        "✻ Worked for 10m 2s · done 4:24 PM".to_string(),
        String::new(),
    ]);
    let refs: Vec<&str> = body.iter().map(String::as_str).collect();
    let mut revealed = rows(&refs);
    revealed.extend(composer("  ? for shortcuts"));
    let cut: Vec<String> = revealed[3..].to_vec();
    let turn = |rows: &Vec<String>| Turn {
        phase: Phase::Idle,
        screen: Screen {
            rows: rows.clone(),
            ..Screen::default()
        },
        timed_out: false,
    };
    let reader = aterm_phase::Program::Claude.reader();
    assert_eq!(
        super::super::point_key(reader, &turn(&cut)),
        super::super::point_key(reader, &turn(&revealed)),
        "the same point by its review key"
    );
    let (a, b) = (
        policy_reading("claude", &cut),
        policy_reading("claude", &revealed),
    );
    assert_eq!(a.answered, None, "the act's row above the screen");
    assert_eq!(
        b.answered.map(|s| s.prompt),
        Some(crate::supervise::approvals::words_hash(ACT)),
        "the act's row revealed"
    );
    assert_eq!((a.phase, a.said_tail), (b.phase, b.said_tail));
}

#[test]
fn a_rewrapped_long_token_hashes_as_another_opener_over_the_same_words() {
    const URL: &str = "https://example.com/org/repository/pull/12345/files#diff-0123456789abcdef";
    let at = |width: usize| {
        let room = width - 2;
        let mut body = vec!["❯ review".to_string()];
        let chars: Vec<char> = URL.chars().collect();
        for chunk in chars.chunks(room) {
            body.push(format!("  {}", chunk.iter().collect::<String>()));
        }
        body.extend([
            String::new(),
            "⏺ Reviewed: the change is sound.".to_string(),
            String::new(),
            "✻ Worked for 3s · done 4:24 PM".to_string(),
            String::new(),
        ]);
        let refs: Vec<&str> = body.iter().map(String::as_str).collect();
        let mut r = rows(&refs);
        r.extend(composer("  ? for shortcuts"));
        let read = policy_reading("claude", &r);
        (
            read.answered.map(|s| s.prompt),
            crate::supervise::policy::turn_end::said_mark(read.said_tail.as_deref()),
        )
    };
    let ((narrow, said_narrow), (wide, said_wide)) = (at(40), at(60));
    assert!(narrow.is_some() && wide.is_some());
    assert_ne!(narrow, wide, "one prompt, two openers");
    assert_eq!(said_narrow, said_wide, "the same words");
}

#[test]
fn a_turn_the_verdict_never_saw_busy_is_rechecked_under_a_short_settle() {
    const PROMPT: &str = "write the parser for this repository";
    const ASK: &str = "what stands now?";
    const REPLY: &str = "Here is where it stands: the parser module is written and tested.";
    const FIRST: &str = "Wrote the parser module.";
    const TOP: &str = "Earlier: step 1 of the plan is in.";
    let under = |top: &str, prompt: &str, said: &str, done: &str| {
        let mut r = vec![format!("⏺ {top}")];
        r.extend((2..=10).map(|i| format!("⏺ Earlier: step {i} of the plan is in.")));
        r.push(String::new());
        r.extend(answered_turn(prompt, said, done));
        r
    };
    let first = under(TOP, PROMPT, FIRST, "✻ Worked for 3s · done 4:24 PM");
    let run = |tag: &str, next: Vec<String>| {
        let mut m = Mock::new(vec![busy_screen(), first.clone(), next]);
        m.sends_gen = true;
        m.agent_pushes = true;
        m.stall_sleep = Some(Duration::from_secs(20));
        m.vanish_after = Some(100);
        let (dir, path) = journal_file(tag);
        let opts = SuperviseOpts {
            journal: Some(path.clone()),
            ..hosted(3600)
        };
        let _ = watch_lines_with(&mut m, &opts, |s| {
            s.setup.wait_step = Duration::from_secs(2);
        });
        let (records, _) = journal_records(&path);
        let _ = std::fs::remove_dir_all(&dir);
        let lines: Vec<String> = records.into_iter().map(|r| r.line).collect();
        (m, lines)
    };
    let dirty_checks = |m: &Mock| {
        m.requests
            .iter()
            .filter(|r| r.starts_with("await seq ") && r.ends_with(" timeout 0"))
            .count()
    };
    let (m, lines) = run(
        "p4-short-settle",
        under(TOP, ASK, REPLY, "✻ Worked for 0s · done 4:25 PM"),
    );
    assert!(
        m.requests.iter().any(|r| r.ends_with(" timeout 2000")),
        "the setup's 2 s step: {:#?}",
        m.requests
    );
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("EVENT idle seq=") && l.contains("Here is where it stands")),
        "the person's turn is a point: {lines:#?}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("WAITING ") && l.contains("2 short turn(s) in a row")),
        "judged, backed off from there: {lines:#?}"
    );
    assert_eq!(m.turns, 0, "nothing typed over it: {lines:#?}");
    assert!(dirty_checks(&m) >= 1, "{:#?}", m.requests);
    let (_, lines) = run(
        "p4-short-settle-redrawn",
        under(
            "Earlier: the first step of the plan, redrawn.",
            PROMPT,
            FIRST,
            "✻ Worked for 3s · done 4:24 PM",
        ),
    );
    assert_eq!(
        lines
            .iter()
            .filter(|l| l.starts_with("EVENT idle seq="))
            .count(),
        1,
        "no new point: {lines:#?}"
    );
}

#[test]
fn gemini_clis_usage_limit_stopped_before_any_point_decided_it_is_read_as_its_limit() {
    use aterm_phase::gemini::fixtures as g;
    use aterm_phase::prompt::fixtures::screen;
    let run = |before: Vec<String>| {
        let (dir, journal) = journal_file("te-gemini-unsettled-stop");
        let mut m = gemini_mock(vec![
            screen(g::BUSY_THINKING),
            before,
            screen(g::LIMIT_STOPPED),
            screen(g::BUSY_THINKING),
            gemini_ended("Carried on."),
        ]);
        m.turn_gates = vec![2];
        m.attention_gates = vec![(2, 0)];
        m.stall_sleep = Some(Duration::from_secs(3600));
        m.vanish_after = Some(8);
        m.exit_when = Some(|m| {
            m.requests
                .iter()
                .any(|r| r.starts_with("meta unset attention owner=supervisor"))
        });
        let opts = SuperviseOpts {
            policy: owners_policy(),
            journal: Some(journal.clone()),
            ..auto(3 * 3600)
        };
        let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
            s.set_turn_end_timing(TurnEndTiming {
                min_work: Duration::ZERO,
                ..TurnEndTiming::default()
            });
            s.set_box_settle(Duration::from_secs(1));
            s.set_clock_running();
        });
        let (records, _) = journal_records(&journal);
        let _ = std::fs::remove_dir_all(&dir);
        let lines: Vec<String> = records
            .iter()
            .map(|r| r.line.clone())
            .chain(lines)
            .collect();
        (m, lines)
    };
    let (m, lines) = run(screen(g::LIMIT_DIALOG));
    let said = |needle: &str| lines.iter().any(|l| l.contains(needle));
    assert!(
        !said("rule=limit-retry@v1"),
        "PRECONDITION: no point decided the dialog: {lines:#?}"
    );
    let raised: Vec<&String> = m
        .requests
        .iter()
        .filter(|r| r.starts_with("meta set attention owner=supervisor"))
        .collect();
    assert_eq!(raised.len(), 1, "{lines:#?}\n{:#?}", m.requests);
    assert!(
        raised[0].contains(
            "(Gemini CLI's usage limit names no reset; after Stop, resumed on a back-off ladder"
        ),
        "the badge's reason is the limit, not the error: {}",
        raised[0]
    );
    let turns: Vec<usize> = m
        .requests
        .iter()
        .enumerate()
        .filter(|(_, r)| verb_of(r) == "turn")
        .map(|(i, _)| i)
        .collect();
    assert!(
        lines
            .iter()
            .find(|l| l.starts_with("CONTINUED "))
            .is_some_and(|l| l.contains("rule=usage-resume@v1")),
        "the first turn typed is the limit's resume: {lines:#?}"
    );
    let wait = lines
        .iter()
        .find(|l| l.starts_with("WAITING ") && l.contains("usage-session resets"))
        .unwrap_or_else(|| panic!("the error waited on: {lines:#?}"));
    let until = wait
        .split_whitespace()
        .find_map(|w| w.strip_prefix("until="))
        .and_then(aterm_types::rfc3339::parse_utc)
        .expect("an until");
    assert!(
        (TEST_NOW + 1800..TEST_NOW + 1830).contains(&until),
        "the ladder's first rung from the dialog's read: {wait}"
    );
    let unset = m
        .requests
        .iter()
        .position(|r| r.starts_with("meta unset attention owner=supervisor"))
        .expect("the badge taken down");
    assert!(
        unset > turns[0],
        "after the try got through: {:#?}",
        m.requests
    );

    let (m, lines) = run(screen(g::BOX_SHELL));
    assert!(
        !m.requests
            .iter()
            .any(|r| verb_of(r) == "turn" && r.contains("keep going")),
        "the control: {lines:#?}\n{:#?}",
        m.requests
    );
    let raised: Vec<&String> = m
        .requests
        .iter()
        .filter(|r| r.starts_with("meta set attention owner=supervisor"))
        .collect();
    assert!(
        raised
            .iter()
            .any(|r| r.contains("An unknown error occurred."))
            && !raised.iter().any(|r| r.contains("after Stop")),
        "the control: the error handed over as itself: {lines:#?}\n{:#?}",
        m.requests
    );
}

/// A host whose step at an idle point, asked once `wanting`, MOVED the session
/// where `moved` (the live upgrade's relaunch under Land: the agent ended and
/// its line typed at the shell, no carry-on) or moved nothing, typing no turn
/// either way.
#[derive(Debug)]
struct RelaunchHost {
    moved: bool,
    wanting: AtomicBool,
}

impl IdleHost for RelaunchHost {
    fn wants(&self) -> bool {
        self.wanting.load(Ordering::SeqCst)
    }
    fn at_idle(&self) -> Option<HostStep> {
        self.wanting.store(false, Ordering::SeqCst);
        Some(HostStep {
            line: "upgrade step=relaunched".to_string(),
            moved: self.moved,
            typed: None,
            repeat: false,
        })
    }
    fn owns_turn_end(&self) -> bool {
        false
    }
}

#[test]
fn a_host_step_that_moved_the_session_spends_the_count_on_the_harness() {
    let run = |moved: bool| {
        let host = Arc::new(RelaunchHost {
            moved,
            wanting: AtomicBool::new(false),
        });
        let mut m = Mock::new(vec![idle_screen()]);
        m.cursor_on_caret = true;
        m.submits_on = [(0, 2)].into_iter().collect();
        m.inputs_on = [(0, (Some(5), 5))].into_iter().collect();
        let opts = SuperviseOpts {
            idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
            ..hosted(30)
        };
        let mut s = session(&mut m, Some("@s-1".to_string()));
        s.set_clock(TEST_NOW, PDT);
        let screen = s.screen().expect("the point");
        assert_eq!(screen.submits, Some(2), "the host's count");
        assert_eq!(
            (screen.inputs_before, screen.inputs_after),
            (Some(5), Some(5)),
            "the host's input writes"
        );
        let point = s.turn_of(screen);
        let _ = s
            .turn_end_at_point(&point, Some(Duration::from_secs(6)), &opts)
            .expect("the decision");
        let counted = s.turn_end.redrawn_fresh(Some(2), Some(5));
        host.wanting.store(true, Ordering::SeqCst);
        let stepped = s
            .host_steps_here(&point, &opts, &mut false, &mut Alone)
            .expect("the host's step");
        (counted, stepped, s.turn_end.redrawn_fresh(Some(2), Some(5)))
    };
    assert_eq!(
        run(true),
        (true, true, false),
        "the relaunch's keys spend the count"
    );
    assert_eq!(run(false), (true, true, true), "a step that moved nothing");
}

#[test]
fn a_restart_whose_process_was_taken_reads_the_count_again() {
    let run = |answer: &'static str, counts: bool| {
        let host = AccountHost::scripted(&[None], &[answer]);
        let mut m = Mock::new(vec![idle_screen(), idle_screen()]);
        m.cursor_on_caret = true;
        if counts {
            m.submits_on = [(0, 2), (1, 3)].into_iter().collect();
        }
        let opts = SuperviseOpts {
            idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
            ..hosted(30)
        };
        let (judged, after) = {
            let mut s = session(&mut m, Some("@s-1".to_string()));
            s.set_clock(TEST_NOW, PDT);
            let screen = s.screen().expect("the point");
            let point = s.turn_of(screen);
            let _ = s
                .turn_end_at_point(&point, Some(Duration::from_secs(6)), &opts)
                .expect("the decision");
            let judged = s.turn_end.counted_from();
            s.restart_agent(
                &point,
                &Restart::Memory,
                "memory-restart@v1",
                TurnEndAction::Nothing,
                &opts,
                &[],
                &mut Alone,
            )
            .expect("the restart");
            (judged, s.turn_end.counted_from())
        };
        assert_eq!(host.said("restart"), ["restart:memory"], "{answer}");
        let reads = m.requests.iter().filter(|r| verb_of(r) == "text").count();
        (judged.0, after.0, after.1.is_some(), reads)
    };
    assert_eq!(
        run("adopted", true),
        (Some(2), Some(3), true, 2),
        "read again once its process was taken"
    );
    assert_eq!(
        run("relaunched", true),
        (Some(2), None, false, 1),
        "no process found yet: nothing read, the baseline gone"
    );
    assert_eq!(
        run("adopted", false),
        (None, None, false, 1),
        "a host that counts none"
    );
}

#[test]
fn a_restarts_count_baseline_is_marked_as_a_reading_marks_its_conversation() {
    let answered = answered_turn(
        "fix the failing parser test",
        "Fixed the parser test.",
        "✻ Worked for 3s · done 4:24 PM",
    );
    let run = |next: Vec<String>| {
        let host = AccountHost::scripted(&[None], &["adopted"]);
        let mut m = Mock::new(vec![idle_screen(), idle_screen(), next]);
        m.cursor_on_caret = true;
        m.submits_on = [(0, 2), (1, 3), (2, 4)].into_iter().collect();
        let opts = SuperviseOpts {
            idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
            ..hosted(30)
        };
        let mut s = session(&mut m, Some("@s-1".to_string()));
        s.set_clock(TEST_NOW, PDT);
        let screen = s.screen().expect("the point");
        let point = s.turn_of(screen);
        let _ = s
            .turn_end_at_point(&point, Some(Duration::from_secs(6)), &opts)
            .expect("the decision");
        s.restart_agent(
            &point,
            &Restart::Memory,
            "memory-restart@v1",
            TurnEndAction::Nothing,
            &opts,
            &[],
            &mut Alone,
        )
        .expect("the restart");
        let rebased = s.turn_end.counted_from();
        let before = s.turn_end.point_number();
        let next = s.screen().expect("the next read");
        let p = s.turn_of(next);
        let a = s
            .turn_end_at_point(&p, None, &opts)
            .expect("the next decision");
        (rebased, before, s.turn_end.point_number(), a)
    };
    let (rebased, before, after, a) = run(idle_screen());
    assert_eq!(rebased.0, Some(3), "the count read after `adopted`");
    assert_eq!(
        after, before,
        "a submission counted over the rows the baseline stood on folds no turn: {a:?}"
    );
    assert!(
        matches!(&a, TurnEndAction::WaitUntil { why, .. } if why.contains("1 short turn(s)")),
        "held, the point's own back-off: {a:?}"
    );
    let (_, before, after, a) = run(answered);
    assert_eq!(
        after.map(|n| n - 1),
        before,
        "a person's prompt answered since the restart is folded as a turn: {a:?}"
    );
}

#[test]
fn a_rewrapped_band_read_as_a_launch_card_keeps_the_points_back_off() {
    let msg = " > FOREVERLAB-SAME-WORDS. write the parser for this repository under \
               lab/parser-module-parser-module-parser.rs and land it";
    let said = "✦ Implemented the parser module and its unit tests in this turn.";
    let banner = || {
        [
            "",
            " ▝▜▄     Gemini CLI v0.46.0",
            "   ▝▜▄",
            "  ▗▟▀    Authenticated with gemini-api-key /auth",
            " ▝▀",
            "",
            "",
            "Tips for getting started:",
            "1. Create GEMINI.md files to customize your interactions",
            "2. /help for more information",
            "3. Ask coding questions, edit code or run commands",
            "4. Be specific for the best results",
        ]
        .iter()
        .map(|r| (*r).to_string())
        .collect::<Vec<_>>()
    };
    let footer = |cols: usize, gap: usize, blank: usize| {
        let mut r = vec![String::new(); blank];
        r.extend([
            format!("{}? for shortcuts", " ".repeat(cols - 16)),
            "─".repeat(cols),
            " Shift+Tab to accept edits".to_string(),
            "▄".repeat(cols),
            " >   Type your message or @path/to/file".to_string(),
            "▀".repeat(cols),
            format!(
                " workspace (/directory){}branch{}sandbox{}/model",
                " ".repeat(gap),
                " ".repeat(14),
                " ".repeat(18)
            ),
            format!(
                " /private/tmp/at.8Vr8/proj/same-words{}main{}no sandbox{}Auto",
                " ".repeat(gap),
                " ".repeat(16),
                " ".repeat(17)
            ),
        ]);
        r
    };
    let mut whole = banner();
    whole.extend([
        "▄".repeat(144),
        msg.to_string(),
        "▀".repeat(144),
        said.to_string(),
    ]);
    whole.extend(footer(144, 42, 2));
    whole.extend(std::iter::repeat_n(String::new(), 24));
    let mut rewrapped = banner();
    rewrapped.extend([
        "▄".repeat(104),
        "▄".repeat(40),
        msg[..104].to_string(),
        msg[104..].to_string(),
        "▀".repeat(104),
        "▀".repeat(40),
        said.to_string(),
    ]);
    rewrapped.extend(footer(104, 29, 5));
    rewrapped.extend([
        String::new(),
        format!("ox{}/model", " ".repeat(31)),
        format!(
            " /private/tmp/at.8Vr8/proj/same-words{}main{}no sa",
            " ".repeat(28),
            " ".repeat(30)
        ),
        format!("ndbox{}Auto", " ".repeat(30)),
    ]);
    rewrapped.extend(std::iter::repeat_n(String::new(), 26));
    assert!(
        !aterm_phase::gemini::fresh(&whole) && aterm_phase::gemini::fresh(&rewrapped),
        "the reader takes the re-wrapped frame for a launch card"
    );
    type Inputs = (Option<u64>, u64);
    let read =
        |rows: &[String], cursor_row: usize, seq: u64, submits: u64, inputs: Inputs| Screen {
            rows: rows.to_vec(),
            cursor_row,
            cursor_col: 3,
            seq,
            submits: Some(submits),
            inputs_before: inputs.0,
            inputs_after: Some(inputs.1),
            ..Screen::default()
        };
    let run = |cleared: bool, first_inputs: Inputs, again_inputs: Inputs| {
        let mut m = Mock::new(vec![whole.clone()]);
        m.program = "gemini";
        let opts = hosted(30);
        let mut s = session(&mut m, Some("@s-1".to_string()));
        s.set_clock(TEST_NOW, PDT);
        let first = s.turn_of(read(&whole, 22, 3614, 2, first_inputs));
        let a = s
            .turn_end_at_point(&first, Some(Duration::from_secs(6)), &opts)
            .expect("the first decision");
        assert!(
            matches!(&a, TurnEndAction::WaitUntil { why, .. } if why.contains("1 short turn(s)")),
            "the first point's back-off: {a:?}"
        );
        let again = s.turn_of(read(
            &rewrapped,
            28,
            4306,
            if cleared { 3 } else { 2 },
            again_inputs,
        ));
        s.turn_end_at_point(&again, None, &opts)
            .expect("the re-wrapped point's decision")
    };
    let settled = (Some(40), 40);
    let a = run(false, settled, settled);
    assert!(
        matches!(&a, TurnEndAction::WaitUntil { why, .. } if why.contains("1 short turn(s)")),
        "the first point's back-off stands: {a:?}"
    );
    let keyed = (Some(43), 43);
    assert_eq!(
        run(true, settled, keyed),
        TurnEndAction::Nothing,
        "a /clear: no task"
    );
    assert_eq!(
        run(false, settled, keyed),
        TurnEndAction::Nothing,
        "keys with nothing submitted: no task"
    );
    assert_eq!(
        run(false, settled, (Some(40), 41)),
        TurnEndAction::Nothing,
        "a key that landed while the re-wrapped rows were read: no task"
    );
    assert_eq!(
        run(false, (Some(40), 41), (Some(41), 41)),
        TurnEndAction::Nothing,
        "a key that landed while the point's rows were read: no task"
    );
    assert_eq!(
        run(false, (None, 41), (Some(41), 41)),
        TurnEndAction::Nothing,
        "a write in flight at the point's read: no task"
    );
}
