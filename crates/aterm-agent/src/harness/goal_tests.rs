// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Unit tests for the active-goal readers.

use super::*;

/// One `goal_status` row as Claude Code 2.1.310 writes it.
fn row(attachment: &str) -> String {
    format!(
        r#"{{"parentUuid":"p","isSidechain":false,"type":"attachment","uuid":"u","timestamp":"2026-10-22T06:05:54.082Z","attachment":{attachment},"userType":"external","cwd":"/w","sessionId":"s"}}"#
    )
}

/// The goal `rows` leave, folded through the transcript projection as the usage fold does.
fn fold(rows: &[String]) -> Option<String> {
    let mut goal = ClaudeGoal::default();
    for line in rows {
        if ClaudeGoal::may_be(line)
            && let Ok(value) = super::super::transcript::goal(line)
            && let Some(map) = value.as_object()
        {
            goal.row(map);
        }
    }
    goal.active().map(str::to_owned)
}

#[test]
fn a_set_goal_is_active_through_its_checks_until_met_or_cleared() {
    let set = row(
        r#"{"type":"goal_status","met":false,"sentinel":true,"condition":"implement the aos aterm vision"}"#,
    );
    let check = row(
        r#"{"type":"goal_status","met":false,"condition":"implement the aos aterm vision","reason":"The vision is not implemented: M6 remains"}"#,
    );
    let met = row(
        r#"{"type":"goal_status","met":true,"condition":"implement the aos aterm vision","iterations":4,"durationMs":9,"tokens":3}"#,
    );
    let cleared = row(
        r#"{"type":"goal_status","met":true,"sentinel":true,"condition":"implement the aos aterm vision"}"#,
    );
    let vision = Some("implement the aos aterm vision".to_owned());
    assert_eq!(fold(std::slice::from_ref(&set)), vision, "/goal sets it");
    assert_eq!(
        fold(&[set.clone(), check.clone()]),
        vision,
        "a failed check keeps it"
    );
    assert_eq!(
        fold(&[set.clone(), check.clone(), met]),
        None,
        "a met check ends it"
    );
    assert_eq!(
        fold(&[set.clone(), cleared.clone()]),
        None,
        "/goal clear ends it"
    );
    assert_eq!(
        fold(&[set.clone(), cleared, set]),
        vision,
        "a goal set again after a clear is the one in force"
    );
}

#[test]
fn a_failed_goal_is_no_goal_and_a_newer_goal_replaces_an_older_one() {
    let first = row(r#"{"type":"goal_status","met":false,"sentinel":true,"condition":"first"}"#);
    let failed = row(r#"{"type":"goal_status","met":false,"failed":true,"condition":"first"}"#);
    let second = row(r#"{"type":"goal_status","met":false,"sentinel":true,"condition":"second"}"#);
    assert_eq!(fold(&[first.clone(), failed]), None);
    assert_eq!(fold(&[first, second]), Some("second".to_owned()));
}

#[test]
fn only_the_main_conversations_goal_rows_count() {
    let set = row(r#"{"type":"goal_status","met":false,"sentinel":true,"condition":"ship it"}"#);
    let side = set.replace(r#""isSidechain":false"#, r#""isSidechain":true"#);
    let cleared_aside =
        row(r#"{"type":"goal_status","met":true,"sentinel":true,"condition":"ship it"}"#)
            .replace(r#""isSidechain":false"#, r#""isSidechain":true"#);
    assert_eq!(
        fold(std::slice::from_ref(&side)),
        None,
        "a sidechain's row sets nothing"
    );
    assert_eq!(
        fold(&[set, cleared_aside]),
        Some("ship it".to_owned()),
        "a sidechain's row clears nothing"
    );
    let quoted = r#"{"type":"user","message":{"role":"user","content":"a row reads {\"type\":\"goal_status\",\"met\":false,\"condition\":\"x\"}"}}"#;
    let other = row(r#"{"type":"queued_command","prompt":"Goal set: goal_status"}"#);
    assert_eq!(
        fold(&[quoted.to_owned(), other]),
        None,
        "a row that only names goal_status in its words, or another attachment, is no goal row"
    );
}

#[test]
fn a_goal_is_shown_as_one_bounded_line() {
    let set = row(
        r#"{"type":"goal_status","met":false,"sentinel":true,"condition":"  land the fix\n\tthen   push  "}"#,
    );
    assert_eq!(fold(&[set]), Some("land the fix then push".to_owned()));
    let blank = row(r#"{"type":"goal_status","met":false,"sentinel":true,"condition":"  \n "}"#);
    assert_eq!(fold(&[blank]), None, "a blank condition is no goal");
    let long = "word ".repeat(GOAL_BYTES);
    let shown = shown(&long).expect("a long goal is shown");
    assert!(
        shown.len() <= GOAL_BYTES,
        "cut to {GOAL_BYTES} bytes: {}",
        shown.len()
    );
    assert!(shown.starts_with("word word"));
}

#[test]
fn the_usage_fold_keeps_the_goal_beside_its_spend() {
    let mut usage = super::super::usage::TranscriptUsage::new();
    usage.fold_line(&row(
        r#"{"type":"goal_status","met":false,"sentinel":true,"condition":"finish the parser"}"#,
    ));
    usage.fold_line(
        r#"{"type":"assistant","timestamp":"2026-10-22T06:06:00Z","message":{"id":"m1","model":"claude-opus-5-5","usage":{"input_tokens":3,"output_tokens":4}}}"#,
    );
    assert_eq!(usage.goal(), Some("finish the parser"));
    assert_eq!(usage.assistant_rows, 1, "the spend still folds");
    usage.fold_line(&row(
        r#"{"type":"goal_status","met":true,"sentinel":true,"condition":"finish the parser"}"#,
    ));
    assert_eq!(usage.goal(), None);
}

#[test]
fn a_store_is_asked_only_for_a_uuids_active_goal_and_its_hex_is_decoded() {
    let thread = "019a1b2c-3d4e-7f60-8a9b-0c1d2e3f4a5b";
    let codex = Store::Codex.query(thread).expect("a uuid is asked for");
    assert!(
        codex.contains("FROM thread_goals WHERE thread_id = '019a1b2c-"),
        "{codex}"
    );
    assert!(codex.contains("status = 'active'"), "{codex}");
    let muse = Store::Muse.query(thread).expect("a uuid is asked for");
    assert!(
        muse.contains("FROM goals WHERE session_id = '019a1b2c-"),
        "{muse}"
    );
    for bad in ["x' OR 1=1 --", "019a1b2c3d4e7f608a9b0c1d2e3f4a5b", ""] {
        assert_eq!(Store::Codex.query(bad), None, "{bad:?} is no uuid");
    }
    let hex = |s: &str| s.bytes().map(|b| format!("{b:02X}")).collect::<String>();
    assert_eq!(
        objective_of(&format!("{}\n", hex("port the lexer\nto Rust"))),
        Some("port the lexer to Rust".to_owned())
    );
    assert_eq!(objective_of(""), None, "no row");
    assert_eq!(objective_of("\n"), None, "an empty objective");
    assert_eq!(objective_of("ZZ"), None, "not hex");
    let cut = &hex("caf\u{e9}")[..8];
    assert_eq!(
        objective_of(cut),
        Some("caf".to_owned()),
        "a split last character is dropped"
    );
}

/// A database laid as `schema` and `rows` say, with Apple's `/usr/bin/sqlite3`, WAL mode
/// checkpointed so it stands closed as an agent leaves it.
#[cfg(target_os = "macos")]
fn store_db(dir: &std::path::Path, name: &str, schema: &str, rows: &str) -> std::path::PathBuf {
    let db = dir.join(name);
    let made = aterm_uds::deadline::output_until(
        aterm_uds::spawnfd::command("/usr/bin/sqlite3")
            .arg(&db)
            .arg(format!(
                "PRAGMA journal_mode=WAL; {schema} {rows} PRAGMA wal_checkpoint(TRUNCATE);"
            )),
        std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN,
    )
    .unwrap_or_else(|e| panic!("sqlite3 within the hang detector: {e}"));
    assert!(made.status.success(), "{made:?}");
    db
}

/// codex-alab's `thread_goals`, as its `goals_1.sqlite` lays it (0.164).
#[cfg(target_os = "macos")]
const CODEX_SCHEMA: &str = "CREATE TABLE thread_goals (thread_id TEXT PRIMARY KEY NOT NULL, \
    goal_id TEXT NOT NULL, objective TEXT NOT NULL, status TEXT NOT NULL CHECK(status IN \
    ('active','paused','blocked','usage_limited','budget_limited','complete')), token_budget \
    INTEGER, tokens_used INTEGER NOT NULL DEFAULT 0, time_used_seconds INTEGER NOT NULL DEFAULT \
    0, created_at_ms INTEGER NOT NULL, updated_at_ms INTEGER NOT NULL);";

/// Muse Code's `goals`, as its session's `goals.db` lays it (1.4.13).
#[cfg(target_os = "macos")]
const MUSE_SCHEMA: &str = "CREATE TABLE goals (session_id TEXT PRIMARY KEY, goal_id TEXT NOT \
    NULL, revision TEXT NOT NULL, objective TEXT NOT NULL, status TEXT NOT NULL, \
    percent_complete INTEGER NOT NULL, current_work TEXT, next_work TEXT, token_budget TEXT, \
    tokens_used TEXT NOT NULL DEFAULT '0', created_at_ms INTEGER NOT NULL, updated_at_ms INTEGER \
    NOT NULL, last_progress_at_ms INTEGER, last_progress_reminder_fired_at_ms INTEGER, \
    llm_steps_used TEXT NOT NULL DEFAULT '0', last_progress_at_steps TEXT, \
    last_progress_reminder_fired_at_steps TEXT);";

#[cfg(target_os = "macos")]
#[test]
fn codex_alab_and_muse_code_stores_name_only_an_active_goal() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("aterm-goal-store-")
        .tempdir()
        .expect("scratch");
    let dir = std::fs::canonicalize(dir.path()).expect("canonical");
    let (a, b, c) = (
        "019a0000-0000-7000-8000-00000000000a",
        "019a0000-0000-7000-8000-00000000000b",
        "019a0000-0000-7000-8000-00000000000c",
    );
    let codex = store_db(
        &dir,
        CODEX_GOALS,
        CODEX_SCHEMA,
        &format!(
            "INSERT INTO thread_goals VALUES ('{a}','g1','ship the release | then rest','active',NULL,0,0,1,2); \
             INSERT INTO thread_goals VALUES ('{b}','g2','paused work','paused',NULL,0,0,1,2); \
             INSERT INTO thread_goals VALUES ('{c}','g3','done work','complete',NULL,0,0,1,2);"
        ),
    );
    assert_eq!(
        read_store(Store::Codex, &codex, a),
        Ok(Some("ship the release | then rest".to_owned()))
    );
    assert_eq!(
        read_store(Store::Codex, &codex, b),
        Ok(None),
        "a paused goal is not pursued"
    );
    assert_eq!(
        read_store(Store::Codex, &codex, c),
        Ok(None),
        "a complete goal is over"
    );
    let missing = "019a0000-0000-7000-8000-0000000000ff";
    assert_eq!(
        read_store(Store::Codex, &codex, missing),
        Ok(None),
        "a cleared goal has no row"
    );
    let muse = store_db(
        &dir,
        MUSE_GOALS,
        MUSE_SCHEMA,
        &format!(
            "INSERT INTO goals (session_id,goal_id,revision,objective,status,percent_complete,created_at_ms,updated_at_ms) \
             VALUES ('{a}','g1','r1','tidy the parser','active',40,1,2); \
             INSERT INTO goals (session_id,goal_id,revision,objective,status,percent_complete,created_at_ms,updated_at_ms) \
             VALUES ('{b}','g2','r1','blocked work','blocked',10,1,2);"
        ),
    );
    assert_eq!(
        read_store(Store::Muse, &muse, a),
        Ok(Some("tidy the parser".to_owned()))
    );
    assert_eq!(read_store(Store::Muse, &muse, b), Ok(None));
    assert_eq!(
        read_store(Store::Codex, &dir.join("absent.sqlite"), a),
        Err(aterm_uds::foreign_db::Unread::Missing)
    );

    let mut held = StoreGoal::default();
    assert_eq!(
        held.read(Store::Codex, &codex, a),
        Some("ship the release | then rest")
    );
    assert_eq!(
        held.read(Store::Codex, &codex, b),
        None,
        "another thread's read is its own"
    );
    assert_eq!(
        held.read(Store::Codex, &dir.join("absent.sqlite"), a),
        None,
        "no store, no goal"
    );
    assert_eq!(
        held.read(Store::Codex, &codex, a),
        Some("ship the release | then rest")
    );
    let made = aterm_uds::deadline::output_until(
        aterm_uds::spawnfd::command("/usr/bin/sqlite3")
            .arg(&codex)
            .arg(format!(
                "UPDATE thread_goals SET status = 'complete' WHERE thread_id = '{a}'; \
             PRAGMA wal_checkpoint(TRUNCATE);"
            )),
        std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN,
    )
    .unwrap_or_else(|e| panic!("sqlite3 within the hang detector: {e}"));
    assert!(made.status.success(), "{made:?}");
    assert_eq!(
        held.read(Store::Codex, &codex, a),
        None,
        "the store moved: read again"
    );
}
