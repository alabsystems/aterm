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
fn a_store_validates_uuids_and_formats_bounded_objectives() {
    let session = "019a1b2c-3d4e-7f60-8a9b-0c1d2e3f4a5b";
    assert!(is_uuid(session));
    for bad in ["x' OR 1=1 --", "019a1b2c3d4e7f608a9b0c1d2e3f4a5b", ""] {
        assert!(!is_uuid(bad), "{bad:?} is no uuid");
    }
    assert_eq!(
        objective_of("port the lexer\nto Rust"),
        Some("port the lexer to Rust".to_owned())
    );
    assert_eq!(objective_of(""), None, "no row");
    assert_eq!(objective_of("  \n\t "), None, "an empty objective");
}

/// A database laid as `schema` and `rows` say, with Apple's `/usr/bin/sqlite3`, WAL mode
/// checkpointed so it stands closed as an agent leaves it.
#[cfg(target_os = "macos")]
fn store_db(dir: &std::path::Path, name: &str, schema: &str, rows: &str) -> std::path::PathBuf {
    let db = dir.join(name);
    let made = aterm_uds::deadline::output_until(
        aterm_uds::spawnfd::command(crate::test_program::kept("/usr/bin/sqlite3"))
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
fn muse_code_store_names_only_an_active_goal() {
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
    let muse = store_db(
        &dir,
        MUSE_GOALS,
        MUSE_SCHEMA,
        &format!(
            "INSERT INTO goals (session_id,goal_id,revision,objective,status,percent_complete,created_at_ms,updated_at_ms) \
             VALUES ('{a}','g1','r1','ship the release | then rest','active',40,1,2); \
             INSERT INTO goals (session_id,goal_id,revision,objective,status,percent_complete,created_at_ms,updated_at_ms) \
             VALUES ('{b}','g2','r1','blocked work','blocked',10,1,2); \
             INSERT INTO goals (session_id,goal_id,revision,objective,status,percent_complete,created_at_ms,updated_at_ms) \
             VALUES ('{c}','g3','r1','done work','complete',100,1,2);"
        ),
    );
    assert_eq!(
        read_store(Store::Muse, &muse, a),
        Ok(Some("ship the release | then rest".to_owned()))
    );
    assert_eq!(read_store(Store::Muse, &muse, b), Ok(None));
    assert_eq!(read_store(Store::Muse, &muse, c), Ok(None));
    let missing = "019a0000-0000-7000-8000-0000000000ff";
    assert_eq!(
        read_store(Store::Muse, &muse, missing),
        Ok(None),
        "a cleared goal has no row"
    );
    assert_eq!(
        read_store(Store::Muse, &muse, "not-a-uuid"),
        Err(aterm_uds::foreign_db::Unread::Failed)
    );
    assert_eq!(
        read_store(Store::Muse, &dir.join("absent.sqlite"), a),
        Err(aterm_uds::foreign_db::Unread::Missing)
    );

    let mut held = StoreGoal::default();
    assert_eq!(
        held.read(Store::Muse, &muse, a),
        Some("ship the release | then rest")
    );
    assert_eq!(
        held.read(Store::Muse, &muse, b),
        None,
        "another session's read is its own"
    );
    assert_eq!(
        held.read(Store::Muse, &dir.join("absent.sqlite"), a),
        None,
        "no store, no goal"
    );
    assert_eq!(
        held.read(Store::Muse, &muse, a),
        Some("ship the release | then rest")
    );
    let made = aterm_uds::deadline::output_until(
        aterm_uds::spawnfd::command(crate::test_program::kept("/usr/bin/sqlite3"))
            .arg(&muse)
            .arg(format!(
                "UPDATE goals SET status = 'complete' WHERE session_id = '{a}'; \
             PRAGMA wal_checkpoint(TRUNCATE);"
            )),
        std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN,
    )
    .unwrap_or_else(|e| panic!("sqlite3 within the hang detector: {e}"));
    assert!(made.status.success(), "{made:?}");
    assert_eq!(
        held.read(Store::Muse, &muse, a),
        None,
        "the store moved: read again"
    );

    // An empty table missing a required column fails the read (`Unread::Failed`), so `StoreGoal`
    // retains its previously cached goal rather than mistaking a schema mismatch for a cleared goal.
    let active_db = store_db(
        &dir,
        "migrating-goals.db",
        MUSE_SCHEMA,
        &format!(
            "INSERT INTO goals (session_id,goal_id,revision,objective,status,percent_complete,created_at_ms,updated_at_ms) \
             VALUES ('{a}','g1','r1','keep across empty schema mismatch','active',50,1,2);"
        ),
    );
    let mut kept = StoreGoal::default();
    assert_eq!(
        kept.read(Store::Muse, &active_db, a),
        Some("keep across empty schema mismatch")
    );
    let broken = aterm_uds::deadline::output_until(
        aterm_uds::spawnfd::command(crate::test_program::kept("/usr/bin/sqlite3"))
            .arg(&active_db)
            .arg(
                "DROP TABLE goals; \
                 CREATE TABLE goals (session_id TEXT PRIMARY KEY, status TEXT NOT NULL); \
                 PRAGMA wal_checkpoint(TRUNCATE);",
            ),
        std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN,
    )
    .unwrap_or_else(|e| panic!("sqlite3 within the hang detector: {e}"));
    assert!(broken.status.success(), "{broken:?}");
    assert_eq!(
        read_store(Store::Muse, &active_db, a),
        Err(aterm_uds::foreign_db::Unread::Failed)
    );
    assert_eq!(
        kept.read(Store::Muse, &active_db, a),
        Some("keep across empty schema mismatch"),
        "failed schema read keeps last known goal"
    );

    // An active rollback `-journal` (`Unread::Writing`) must not freeze the `[db, db-wal]` stamp:
    // once `-journal` is removed without touching `db` or `-wal`, the next `StoreGoal::read`
    // immediately observes the committed goal.
    let journaled_db = store_db(
        &dir,
        "journaled-goals.db",
        MUSE_SCHEMA,
        &format!(
            "INSERT INTO goals (session_id,goal_id,revision,objective,status,percent_complete,created_at_ms,updated_at_ms) \
             VALUES ('{a}','g1','r1','visible once rollback journal clears','active',60,1,2);"
        ),
    );
    let journal_path = dir.join("journaled-goals.db-journal");
    std::fs::write(&journal_path, b"in-flight rollback journal").expect("write journal");
    let mut after_write = StoreGoal::default();
    assert_eq!(
        after_write.read(Store::Muse, &journaled_db, a),
        None,
        "in-flight rollback journal defers initial read"
    );
    std::fs::remove_file(&journal_path).expect("remove journal");
    assert_eq!(
        after_write.read(Store::Muse, &journaled_db, a),
        Some("visible once rollback journal clears"),
        "clearing -journal without modifying db/-wal retries instead of freezing a stale stamp"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn a_live_muse_sessions_goal_is_the_one_in_the_store_beside_the_lock_it_holds() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("aterm-goal-lock-")
        .tempdir()
        .expect("scratch");
    let home = std::fs::canonicalize(dir.path()).expect("canonical");
    let sid = "019a0000-0000-7000-8000-00000000000a";
    let session = home.join(".local/share/muse/sessions/2026/10/24").join(sid);
    std::fs::create_dir_all(&session).expect("session dir");
    std::fs::write(session.join(".session.lock"), b"pid=4242\nhost=mac\n").expect("lock");
    std::fs::write(session.join("session.jsonl"), b"{}\n").expect("log");
    store_db(
        &session,
        MUSE_GOALS,
        MUSE_SCHEMA,
        &format!(
            "INSERT INTO goals (session_id,goal_id,revision,objective,status,percent_complete,created_at_ms,updated_at_ms) \
             VALUES ('{sid}','g1','r1','ship it','active',40,1,2);"
        ),
    );
    let lock = std::fs::canonicalize(session.join(".session.lock")).expect("canonical lock");
    let goal = |argv: &[&str], files: &[std::path::PathBuf]| {
        let argv: Vec<String> = argv.iter().map(|a| (*a).to_owned()).collect();
        crate::harness::profile::muse_goal_for(
            4242,
            &argv,
            &[],
            &home,
            Some(files),
            &mut StoreGoal::default(),
        )
    };
    assert_eq!(
        goal(&["muse"], std::slice::from_ref(&lock)).as_deref(),
        Some("ship it")
    );
    assert_eq!(
        goal(&["muse", "resume", sid], &[]).as_deref(),
        Some("ship it"),
        "a session its launch names, found by its folder, holds the store beside its lock there"
    );
}

/// Claude Code 2.1.323 clearing a goal after a Cloudflare 403 it read as a lost login (m2,
/// 2026-11-13): the measured rows, their ids, session and paths synthetic.
const CLEARED_BY_ERROR: &str =
    include_str!("testdata/goal/claude-2.1.323-transcript-goal-cleared-by-error.jsonl");

/// The same goal dropped by a person's `/goal clear`: the command row and the same clear row, no
/// notice after it.
const CLEARED_BY_PERSON: &str =
    include_str!("testdata/goal/claude-2.1.323-transcript-goal-cleared-by-person.jsonl");

const MEASURED_GOAL: &str = "the account registry and automated account switching just works";

fn folded(lines: &str) -> ClaudeGoal {
    let mut goal = ClaudeGoal::default();
    for line in lines.lines() {
        goal.line(line);
    }
    goal
}

/// T1: THE MEASURED ERROR CLEAR folds to a goal cleared by the error, its condition and cause
/// kept, served once a real answer came after it (never by the error's own synthetic answer).
/// Negative control: a person's `/goal clear` folds to a goal cleared by them, which the harness
/// reads as no goal.
#[test]
fn an_error_clear_is_told_from_a_persons_goal_clear() {
    let goal = folded(CLEARED_BY_ERROR);
    let ended = goal.ended().expect("an ended goal");
    assert_eq!(ended.condition, MEASURED_GOAL);
    assert_eq!(
        ended.end,
        GoalEnd::ByError {
            label: "authentication failed".to_owned()
        }
    );
    assert_eq!(goal.active(), None);
    let GoalRead::Lost(lost) = goal.read() else {
        panic!("not lost: {:?}", goal.read());
    };
    assert_eq!(
        (lost.condition.as_str(), lost.clear.as_str(), lost.served),
        (MEASURED_GOAL, "00000000-0000-4000-8000-000000000005", true)
    );
    assert_eq!(
        lost.line.as_deref(),
        Ok(MEASURED_GOAL),
        "typed back as held"
    );
    let to_the_error: String = CLEARED_BY_ERROR
        .lines()
        .take(7)
        .map(|l| format!("{l}\n"))
        .collect();
    assert!(
        matches!(
            folded(&to_the_error).read(),
            GoalRead::Lost(Lost { served: false, .. })
        ),
        "the error's own synthetic answer is no served one"
    );

    let person = folded(CLEARED_BY_PERSON);
    assert_eq!(person.ended().map(|e| &e.end), Some(&GoalEnd::Cleared));
    assert_eq!(person.read(), GoalRead::Off);
}

/// The notice names the error only as the clear row's own child. Negative controls: a notice
/// under another row, one in a sidechain, and one before any clear leave the clear a person's;
/// a goal set after the error clear is the goal in force.
#[test]
fn only_the_clears_own_notice_makes_it_an_error_clear() {
    let lines: Vec<&str> = CLEARED_BY_ERROR.lines().collect();
    let notice = lines[5];
    let without = |swap: &dyn Fn(&str) -> String| {
        let mut rows: Vec<String> = lines.iter().map(|l| (*l).to_owned()).collect();
        rows[5] = swap(notice);
        folded(&rows.join("\n")).read()
    };
    let elsewhere = without(&|n| {
        n.replace(
            r#""parentUuid":"00000000-0000-4000-8000-000000000005""#,
            r#""parentUuid":"00000000-0000-4000-8000-000000000004""#,
        )
    });
    assert_eq!(elsewhere, GoalRead::Off, "a notice under another row");
    let aside = without(&|n| n.replace(r#""isSidechain":false"#, r#""isSidechain":true"#));
    assert_eq!(aside, GoalRead::Off, "a sidechain's notice");
    let mut early = folded(notice);
    for line in &lines[..5] {
        early.line(line);
    }
    assert_eq!(early.read(), GoalRead::Off, "a notice before the clear");
    let mut again = folded(CLEARED_BY_ERROR);
    again.line(lines[1]);
    assert_eq!(again.read(), GoalRead::Active(MEASURED_GOAL.to_owned()));
}

/// A goal record reads the whole transcript once, then only what was written since: a line still
/// being written waits for its end, and a shorter file, or another, starts it over. Negative
/// control: a transcript that cannot be opened reads nothing.
#[test]
fn a_goal_record_reads_on_from_where_it_stopped() {
    use std::io::Write as _;
    let dir = aterm_tempfile::Builder::new()
        .prefix("aterm-goal-record-")
        .tempdir()
        .expect("scratch");
    let path = dir.path().join("t.jsonl");
    let lines: Vec<&str> = CLEARED_BY_ERROR.lines().collect();
    let mut record = GoalRecord::default();
    assert_eq!(record.read(&path), GoalRead::Unread, "no file");
    let mut f = std::fs::File::create(&path).expect("transcript");
    for line in &lines[..3] {
        writeln!(f, "{line}").expect("write");
    }
    assert_eq!(
        record.read(&path),
        GoalRead::Active(MEASURED_GOAL.to_owned())
    );
    for line in &lines[3..7] {
        writeln!(f, "{line}").expect("write");
    }
    let (head, tail) = lines[9].split_at(lines[9].len() / 2);
    for line in &lines[7..9] {
        writeln!(f, "{line}").expect("write");
    }
    write!(f, "{head}").expect("write");
    assert!(
        matches!(
            record.read(&path),
            GoalRead::Lost(Lost { served: false, .. })
        ),
        "the half-written answer is not folded"
    );
    writeln!(f, "{tail}").expect("write");
    assert!(matches!(
        record.read(&path),
        GoalRead::Lost(Lost { served: true, .. })
    ));
    drop(f);
    std::fs::write(&path, format!("{}\n", lines[1])).expect("shorter");
    assert_eq!(
        record.read(&path),
        GoalRead::Active(MEASURED_GOAL.to_owned()),
        "a shorter file is read over"
    );
    let other = dir.path().join("u.jsonl");
    std::fs::write(&other, CLEARED_BY_PERSON).expect("other");
    assert_eq!(
        record.read(&other),
        GoalRead::Off,
        "another file is read over"
    );
}

/// A person's `/goal clear` typed once Claude Code had cleared the goal after an error: Claude Code
/// holds no goal then, so it answers `No goal set` and writes no goal row, only the command row
/// and its output (the rows' form as 2.1.323 writes a `/goal` command; ids synthetic).
const PERSON_CLEARS_AFTER: &str =
    include_str!("testdata/goal/claude-2.1.323-goal-clear-with-no-goal-set.jsonl");

/// The command row a `/goal` typed with `args` writes (2.1.323), on the main chain or a sidechain.
fn goal_command(args: &str, sidechain: bool) -> String {
    format!(
        r#"{{"parentUuid":"00000000-0000-4000-8000-000000000009","isSidechain":{sidechain},"type":"user","message":{{"role":"user","content":"<command-name>/goal</command-name>\n            <command-message>goal</command-message>\n            <command-args>{args}</command-args>"}},"uuid":"00000000-0000-4000-8000-000000000012","timestamp":"2026-11-13T12:00:00.000Z","userType":"external","cwd":"/w","sessionId":"s","version":"2.1.323"}}"#
    )
}

/// A PERSON'S `/goal` AFTER AN ERROR CLEAR TAKES THE GOAL UP, though it writes no goal row: the
/// measured error clear and then `/goal clear` answered `No goal set` folds to no goal, so the
/// harness never sets back a goal the person dropped. So do `/goal off` typed while a turn ran (a
/// `local_command` system row), a condition Claude Code refused and a bare `/goal`. Negative
/// controls: the error clear alone stays lost; the harness's own restore line (the very
/// condition) with no set row after it, a sidechain's `/goal clear` and a `/goal` command whose
/// output alone names it change nothing.
#[test]
fn a_persons_goal_after_an_error_clear_takes_the_goal_up() {
    let cleared = folded(&format!("{CLEARED_BY_ERROR}{PERSON_CLEARS_AFTER}"));
    assert_eq!(cleared.read(), GoalRead::Off, "the person's /goal clear");
    assert!(cleared.ended().is_some_and(|e| e.taken));
    let after = |line: &str| folded(&format!("{CLEARED_BY_ERROR}{line}\n")).read();
    let local = r#"{"parentUuid":"00000000-0000-4000-8000-000000000009","isSidechain":false,"type":"system","subtype":"local_command","content":"<command-name>/goal</command-name>\n            <command-message>goal</command-message>\n            <command-args>off</command-args>","uuid":"00000000-0000-4000-8000-000000000013","timestamp":"2026-11-13T12:00:00.000Z","sessionId":"s","version":"2.1.323"}"#;
    assert_eq!(
        after(local),
        GoalRead::Off,
        "/goal off typed while a turn ran"
    );
    let refused = format!("{} more", "x".repeat(CONDITION_UNITS));
    assert_eq!(
        after(&goal_command(&refused, false)),
        GoalRead::Off,
        "a condition Claude Code refused"
    );
    assert_eq!(
        after(&goal_command("", false)),
        GoalRead::Off,
        "a bare /goal"
    );

    assert!(matches!(folded(CLEARED_BY_ERROR).read(), GoalRead::Lost(_)));
    assert!(
        matches!(
            after(&goal_command(MEASURED_GOAL, false)),
            GoalRead::Lost(_)
        ),
        "the restore's own line"
    );
    assert!(
        matches!(after(&goal_command("clear", true)), GoalRead::Lost(_)),
        "a sidechain's /goal clear"
    );
    let stdout = PERSON_CLEARS_AFTER.lines().nth(1).expect("the output row");
    assert!(
        matches!(after(stdout), GoalRead::Lost(_)),
        "the output row alone names no command"
    );
}

/// THE CONDITION IS TYPED BACK AS CLAUDE CODE HELD IT, never cut or re-flowed: a 3000-character
/// goal and one of 700 CJK characters (2100 bytes, past aterm's shown bound) are kept whole, and a
/// goal read lost keeps its whole line though its shown form is cut. Negative controls: a goal
/// with a line break or a tab, one longer than Claude Code's own limit, and an empty one are
/// untypable, never typed changed.
#[test]
fn a_cleared_condition_is_typed_back_whole_or_not_at_all() {
    let long = "a".repeat(3000);
    assert_eq!(typable(&long).as_deref(), Ok(long.as_str()));
    let cjk = "目".repeat(700);
    assert!(cjk.len() > GOAL_BYTES, "PRECONDITION: past the shown bound");
    assert_eq!(typable(&format!("  {cjk} ")).as_deref(), Ok(cjk.as_str()));
    let astral = "😀".repeat(CONDITION_UNITS / 2);
    assert_eq!(
        typable(&astral).as_deref(),
        Ok(astral.as_str()),
        "4000 units"
    );
    assert_eq!(typable(&format!("{astral}a")), Err(Untypable::Long));
    assert_eq!(
        typable(&"a".repeat(CONDITION_UNITS + 1)),
        Err(Untypable::Long)
    );
    assert_eq!(typable("land it\nthen push"), Err(Untypable::Breaks));
    assert_eq!(typable("land\tit"), Err(Untypable::Breaks));
    assert_eq!(typable("land\u{2028}it"), Err(Untypable::Breaks));
    assert_eq!(typable(" \n "), Err(Untypable::Empty));

    let at_clear = |condition: &str| {
        let swap = |l: &str| l.replace(MEASURED_GOAL, condition);
        let lines: String = CLEARED_BY_ERROR
            .lines()
            .map(|l| format!("{}\n", swap(l)))
            .collect();
        folded(&lines).read()
    };
    let GoalRead::Lost(lost) = at_clear(&cjk) else {
        panic!("not lost");
    };
    assert_eq!(lost.line.as_deref(), Ok(cjk.as_str()), "whole");
    assert!(lost.condition.len() <= GOAL_BYTES, "shown cut");
    let GoalRead::Lost(lost) = at_clear("land it\\nthen push") else {
        panic!("not lost");
    };
    assert_eq!(
        lost.line,
        Err(Untypable::Breaks),
        "never re-flowed onto one line"
    );
    let mut set = folded(CLEARED_BY_ERROR);
    set.line(
        &CLEARED_BY_ERROR
            .lines()
            .nth(1)
            .expect("set")
            .replace(MEASURED_GOAL, &cjk),
    );
    assert_eq!(set.read(), GoalRead::Active(cjk.clone()), "held whole");
    assert!(
        set.active().is_some_and(|a| a.len() <= GOAL_BYTES),
        "shown cut"
    );
}
