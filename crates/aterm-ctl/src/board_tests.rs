// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn scratch() -> (aterm_tempfile::TempDir, Board) {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let board = Board::in_state(dir.path());
    (dir, board)
}

/// An hour ago, plus `ms`: a row started then is fresh, never pruned.
fn at(ms: u64) -> u64 {
    now_ms() - 3_600_000 + ms
}

fn worker(sid: &str, label: &str, started_ms: u64) -> Row {
    Row {
        label: Some(label.to_owned()),
        node: Some("0123456789abcdef0123456789abcdef".to_owned()),
        host: Some("m100".to_owned()),
        program: Some("claude".to_owned()),
        cwd: Some("/Users//a b/repo".to_owned()),
        ..Row::started(sid, started_ms)
    }
}

#[test]
fn a_row_reads_back_as_it_was_written_its_values_escaped() {
    let mut row = worker("s-0a1", "EW-24", 1_000);
    row.why = Some("tests red: 3 of 40".to_owned());
    row.checked = Some("0123456789ab@origin/main".to_owned());
    row.accept_ms = Some(9);
    row.owed = vec!["kind=task a b\n".to_owned()];
    row.paying = vec!["kind=task c\n".to_owned(), "kind=task d=e\n".to_owned()];
    row.was_told = Some("ok.exit".to_owned());
    let line = row.line();
    assert!(
        !line.contains("a b"),
        "a space in a value is escaped: {line}"
    );
    assert_eq!(line.split_whitespace().count(), KEYS.len(), "{line}");
    assert_eq!(Row::parse(&line).expect("parse"), row);
}

#[test]
fn a_row_keeps_a_later_aterms_key_and_state_and_refuses_a_bad_row() {
    let line = format!("{} after_three=x%20y", worker("s-0a1", "L", 1).line());
    let mut row = Row::parse(&line).expect("a later key");
    assert_eq!(row.extra, [("after_three".to_owned(), "x%20y".to_owned())]);
    assert_eq!(row.line(), line, "written back as it was read");
    assert!(row.merge("@s-0a1 rev=2 state=done conclusion=finished", 5));
    assert!(
        row.line().ends_with(" after_three=x%20y"),
        "a merge keeps it"
    );
    let later = Row::parse("sid=s-0a1 state=starting").expect("a later state");
    assert!(later.is_open(), "a state this aterm does not know is open");
    for bad in [
        "sid=s-0a1 state=working sid=s-0a2",
        "sid=nope state=working",
        "sid=s-0a1 state=a/b",
        "sid=s-0a1",
        "sid=s-0a1 state=working rev=x",
        "sid=s-0a1 state=working loose",
        "sid=s-0a1 state=working a/b=c",
    ] {
        assert!(Row::parse(bad).is_err(), "{bad} is refused");
    }
}

#[test]
fn a_live_answer_moves_a_row_forward_never_back() {
    let mut row = worker("s-0a1", "L", 1);
    assert!(row.merge(
        "@s-0a1 rev=3 state=working agent=claude attention=- result_bytes=0",
        10
    ));
    assert_eq!((row.rev, row.heard_ms), (3, Some(10)));
    assert!(
        !row.merge("@s-0a1 rev=2 state=done agent=claude result_bytes=9", 11),
        "an older revision changes nothing"
    );
    assert_eq!(row.state, "working");
    assert!(row.merge(
        "@s-0a1 rev=5 state=done conclusion=finished checked=0123456789ab@origin/main",
        12
    ));
    assert_eq!(row.conclusion.as_deref(), Some("finished"));
    assert_eq!(row.landed.as_deref(), Some("0123456789ab"));
    assert!(
        !row.merge(
            "@s-0a1 rev=5 state=done conclusion=finished checked=0123456789ab@origin/main",
            13
        ),
        "the same answer again changes nothing but when it was heard"
    );
    assert_eq!(row.heard_ms, Some(13));
}

#[test]
fn a_closed_answer_always_lands_and_keeps_what_the_board_knew() {
    let mut row = worker("s-0a1", "L", 1);
    row.merge(
        "@s-0a1 rev=5 state=done conclusion=finished checked=0123456789ab@origin/main",
        2,
    );
    assert!(row.merge("@s-0a1 rev=0 state=closed agent=- result_bytes=0", 3));
    assert_eq!(row.state, "closed");
    assert_eq!(row.rev, 5, "a closed row's revision never goes back");
    assert_eq!(row.conclusion.as_deref(), Some("finished"));
    assert_eq!(row.checked.as_deref(), Some("0123456789ab@origin/main"));
    assert_eq!(row.landed.as_deref(), Some("0123456789ab"));
    assert!(
        !row.merge("@s-0a1 rev=4 state=working", 4),
        "an older live row does not reopen it"
    );
    assert!(
        row.merge("@s-0a1 rev=5 state=working why=restored", 5),
        "a worker read as gone that answers again (its tab restored) is open again"
    );
    assert_eq!(row.state, "working");
    assert_eq!(
        row.landed.as_deref(),
        Some("0123456789ab"),
        "its landing stands"
    );

    let mut gone = worker("s-0a2", "L", 1);
    gone.merge("@s-0a2 rev=0 state=closed", 2);
    assert_eq!(
        gone.conclusion.as_deref(),
        Some("failed"),
        "a tab closed with no conclusion is failed, as await task reads it"
    );
}

#[test]
#[cfg(unix)]
fn required_board_accept_waits_for_its_native_writer() {
    let (_dir, board) = scratch();
    let row = worker("s-0a1", "queued-accept", at(1));
    board.record(&row).unwrap();
    board
        .record(&worker("s-0b2", "queued-accept", at(2)))
        .unwrap();
    let bound = board.bind(true).unwrap();
    let accepted_at = at(3);
    for (who, sid) in [("@s-0a1", "s-0a1"), ("queued-accept", "s-0b2")] {
        let mut expected = board.row(sid).unwrap();
        expected.accept = Some("passed".to_owned());
        expected.accept_ms = Some(accepted_at);
        let accepted = bound
            .accept_started(
                who,
                "passed",
                accepted_at,
                std::time::Instant::now() - Duration::from_secs(11),
            )
            .unwrap();
        assert_eq!(accepted, expected);
        assert_eq!(board.row(sid).unwrap(), expected);
    }
    let held = bound.lock().unwrap();
    let before = board.scan().rows;
    let expired = std::time::Instant::now() - Duration::from_secs(61);
    assert_eq!(
        bound
            .accept_started("queued-accept", "failed", at(4), expired)
            .unwrap_err()
            .kind(),
        io::ErrorKind::WouldBlock
    );
    assert_eq!(
        bound
            .accept_started("queued-accept", "bad word", at(4), expired)
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidInput
    );
    assert_eq!(board.scan().rows, before);
    drop(held);
    let fresh = board.accept("queued-accept", "fresh", at(5)).unwrap();
    assert_eq!(fresh.sid, "s-0b2");
    assert_eq!(fresh.accept.as_deref(), Some("fresh"));
    assert_eq!(board.row("s-0b2").unwrap(), fresh);
    assert_eq!(
        board.accept("@s-0ff", "passed", at(6)).unwrap_err().kind(),
        io::ErrorKind::NotFound
    );
}

#[test]
fn a_board_records_reads_merges_and_accepts_its_rows() {
    let (_dir, board) = scratch();
    assert_eq!(board.scan(), Scan::default(), "no folder is an empty board");
    board
        .record(&worker("s-0b2", "EW-19", at(2_000)))
        .expect("record");
    board
        .record(&worker("s-0a1", "EW-24", at(1_000)))
        .expect("record");
    let scan = board.scan();
    let sids: Vec<&str> = scan.rows.iter().map(|r| r.sid.as_str()).collect();
    assert_eq!(sids, ["s-0a1", "s-0b2"], "oldest first");

    let seen = vec![
        "@s-0a1 rev=4 state=done conclusion=finished checked=0123456789ab@origin/main".to_owned(),
        "@s-0ff rev=1 state=working".to_owned(),
    ];
    assert_eq!(board.apply(&seen, 5_000).expect("apply"), 1);
    assert!(
        board.row("s-0ff").is_none(),
        "a worker the board did not start is not added"
    );
    let row = board.row("s-0a1").expect("row");
    assert_eq!(row.landed.as_deref(), Some("0123456789ab"));

    board
        .record(&worker("s-0c3", "EW-24", at(3_000)))
        .expect("record");
    let accepted = board.accept("EW-24", "passed", 6_000).expect("accept");
    assert_eq!(accepted.sid, "s-0c3", "a label names its newest row");
    assert_eq!(
        board.row("s-0c3").expect("row").accept.as_deref(),
        Some("passed")
    );
    assert_eq!(
        board.accept("@s-0a1", "failed", 7_000).expect("by sid").sid,
        "s-0a1"
    );
    assert!(board.accept("@s-0ff", "passed", 1).is_err(), "no such row");
    assert!(board.accept("EW-24", "pass ed", 1).is_err(), "no such word");
}

#[cfg(unix)]
#[test]
fn a_board_never_reads_a_loose_file_and_counts_it() {
    use std::os::unix::fs::PermissionsExt;
    let (_dir, board) = scratch();
    board.record(&worker("s-0a1", "L", at(1))).expect("record");
    let loose = board.dir().join("s-0b2");
    std::fs::write(&loose, worker("s-0b2", "L", at(2)).line()).expect("write");
    std::fs::set_permissions(&loose, std::fs::Permissions::from_mode(0o644)).expect("chmod");
    std::fs::write(board.dir().join("s-0c3"), "sid=s-0d4 state=working\n").expect("write");
    let scan = board.scan();
    assert_eq!(scan.rows.len(), 1);
    assert_eq!(
        scan.unread, 2,
        "a file open to others, and one naming another sid"
    );
}

#[test]
fn a_board_prunes_rows_closed_a_week_or_unheard_a_month() {
    let (_dir, board) = scratch();
    let day = 24 * 3_600_000;
    let now = now_ms();
    let mut closed = worker("s-0a1", "L", now - 9 * day);
    closed.state = "closed".to_owned();
    closed.heard_ms = Some(now - 8 * day);
    let mut fresh_closed = worker("s-0a2", "L", now - 9 * day);
    fresh_closed.state = "closed".to_owned();
    fresh_closed.heard_ms = Some(now - day);
    let unheard = worker("s-0a3", "L", now - 31 * day);
    let open = worker("s-0a4", "L", now - 20 * day);
    for row in [&closed, &fresh_closed, &unheard, &open] {
        board.record(row).expect("record");
    }
    board.prune();
    let sids: Vec<String> = board.scan().rows.into_iter().map(|r| r.sid).collect();
    assert_eq!(sids, ["s-0a4", "s-0a2"], "oldest first");
}

#[cfg(unix)]
#[test]
fn one_keeper_holds_a_board() {
    let (_dir, board) = scratch();
    let first = board.keeper_lock().expect("the first keeper");
    assert!(board.keeper_lock().is_none(), "a second keeper waits");
    drop(first);
    assert!(
        board.keeper_lock().is_some(),
        "and takes it once the first is gone"
    );
}

/// A keeper that found the board kept waits for its lock and takes it when the first lets go:
/// the first lets go only once the waiter showed it found the lock held.
/// NEGATIVE CONTROL: `wait_keeper_lock` as one try (`self.keeper_lock().ok_or(..)`) refuses the
/// second while the first holds, never reporting it held, and "the waiter found the lock held"
/// fails.
#[cfg(unix)]
#[test]
fn a_waiting_keeper_takes_the_board_when_the_first_lets_go() {
    let (_dir, board) = scratch();
    let first = board.keeper_lock().expect("the first keeper");
    let (held, found_held) = std::sync::mpsc::channel();
    let waiter = {
        let board = board.clone();
        std::thread::spawn(move || {
            board.wait_keeper_lock(move || {
                let _ = held.send(());
            })
        })
    };
    found_held
        .recv_timeout(Duration::from_secs(60))
        .expect("the waiter found the lock held");
    drop(first);
    let second = waiter
        .join()
        .expect("the waiter")
        .expect("the second takes it once the first lets go");
    assert!(board.keeper_lock().is_none(), "the second keeps it");
    drop(second);
}

/// A keeper with nothing to ask is due at its next prune, a closed row's next ask, a Mac's
/// backoff's end, or its next settle of the task rows its board owes its log, whichever is
/// first, and at nothing sooner: an idle board wakes it hourly. A backoff already over is no
/// deadline: a Mac whose rows are all out of asking does not keep the keeper due in the past.
/// NEGATIVE CONTROL: `next_due` as `at + KEEP_IDLE` (the read every 5 s) fails "an idle board
/// is due at its prune"; without its `> at` filter it fails "a backoff over is no deadline";
/// without the settle it fails "a board that owes its log is due at its next settle".
#[cfg(unix)]
#[test]
fn an_idle_keeper_is_due_at_its_next_prune_ask_or_backoff() {
    let at = std::time::Instant::now();
    let mut closed_asked = std::collections::HashMap::new();
    let mut backoff = Backoff::default();
    assert_eq!(
        next_due(at, Some(at), &closed_asked, &backoff, None),
        at + KEEP_PRUNE_EVERY,
        "an idle board is due at its prune"
    );
    closed_asked.insert("s-0b1".to_owned(), at + CLOSED_ASKED_EVERY);
    assert_eq!(
        next_due(at, Some(at), &closed_asked, &backoff, None),
        at + CLOSED_ASKED_EVERY
    );
    let wait = backoff.unanswered("m1".to_owned(), at);
    assert!(wait < CLOSED_ASKED_EVERY, "PRECONDITION: {wait:?}");
    assert_eq!(
        next_due(at, Some(at), &closed_asked, &backoff, None),
        at + wait
    );
    assert_eq!(
        next_due(at, None, &closed_asked, &backoff, None),
        at,
        "never pruned"
    );
    let later = at + wait + Duration::from_secs(1);
    assert_eq!(
        next_due(
            later,
            Some(at),
            &std::collections::HashMap::new(),
            &backoff,
            None
        ),
        at + KEEP_PRUNE_EVERY,
        "a backoff over is no deadline"
    );
    let settle = at + OWED_SETTLED_EVERY;
    assert_eq!(
        next_due(
            at,
            Some(at),
            &std::collections::HashMap::new(),
            &Backoff::default(),
            Some(settle)
        ),
        settle,
        "a board that owes its log is due at its next settle"
    );
}

/// A keeper settles what its board owes its log a period after it finds it owing, and every
/// period after while it owes, never sooner: a settle's own write to the board wakes the keeper,
/// which settles nothing then. A board that owes nothing has no settle due.
/// NEGATIVE CONTROL: settling as soon as a row is found owing fails "found owing", and settling
/// whenever the keeper wakes fails "not yet".
#[cfg(unix)]
#[test]
fn a_keeper_settles_what_its_board_owes_a_period_after_and_every_period_after() {
    let at = std::time::Instant::now();
    let due = at + OWED_SETTLED_EVERY;
    assert_eq!(
        settle_step(false, Some(due), due),
        (false, None),
        "nothing owed"
    );
    assert_eq!(
        settle_step(true, None, at),
        (false, Some(due)),
        "found owing"
    );
    assert_eq!(
        settle_step(true, Some(due), at + Duration::from_secs(1)),
        (false, Some(due)),
        "not yet"
    );
    assert_eq!(
        settle_step(true, Some(due), due),
        (true, Some(due + OWED_SETTLED_EVERY)),
        "due: it settles, and is due a period on"
    );
}

/// A keeper at rest wakes when a row is written to its board, by whatever process, well before
/// its next due an hour away; the write is caught even when it lands before the rest begins.
/// NEGATIVE CONTROL: `Rest::until` sleeping to `due` without its watch hangs past the
/// one-minute hang detector, and "the write ended the rest" fails.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_resting_keeper_wakes_when_its_board_is_written() {
    let (_dir, board) = scratch();
    let mut row = worker("s-0c1", "L", now_ms());
    board.record(&row).expect("the initial row and lock exist");
    let lock = aterm_uds::file_watch::FileStamp::of(&board.dir().join(LOCK));
    let mut rest = Rest::new(board.dir());
    row.label = Some("rewritten while resting".into());
    // Publish independently of the writer lock. On Linux even closing a lock opened writable
    // can emit CLOSE_WRITE, so calling record here could hide a mistaken lock-only watch.
    aterm_uds::atomic_file::write(
        &board.dir().join(&row.sid),
        format!("{}\n", row.line()).as_bytes(),
        0o600,
    )
    .expect("replace the existing row without touching the lock");
    assert_eq!(
        aterm_uds::file_watch::FileStamp::of(&board.dir().join(LOCK)),
        lock,
        "updating a row does not change the lock entry"
    );
    let (done, ended) = std::sync::mpsc::channel();
    let waiting = std::thread::spawn(move || {
        let changed = rest.until(std::time::Instant::now() + Duration::from_secs(3600));
        let _ = done.send(changed);
    });
    assert_eq!(
        ended.recv_timeout(Duration::from_secs(60)),
        Ok(true),
        "the write ended the rest"
    );
    waiting.join().expect("the rest finished");
    assert_eq!(board.scan().rows, [row]);
}

/// A new registration must return before waiting for a later write or deadline. The minute
/// detects a hang, not a latency budget; a failed test's directory and process retire the watch.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn rearmed_without_another_write(mut rest: Rest, why: &str) -> Rest {
    let (done, ended) = std::sync::mpsc::channel();
    let waiting = std::thread::spawn(move || {
        let invalidated = rest.until(std::time::Instant::now() + Duration::from_secs(3600));
        let _ = done.send((rest, invalidated));
    });
    let (rest, invalidated) = ended.recv_timeout(Duration::from_secs(60)).expect(why);
    waiting.join().expect("the rest finished");
    assert!(invalidated, "{why}");
    rest
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_rearmed_board_watch_requires_a_fresh_snapshot_after_creation() {
    let (_dir, board) = scratch();
    let mut rest = Rest::new(board.dir());
    assert!(board.scan().rows.is_empty());
    assert!(!rest.until(std::time::Instant::now()), "no folder to watch");

    let row = worker("s-0d1", "L", now_ms());
    board.record(&row).expect("write before the watch exists");
    let mut rest = rearmed_without_another_write(
        rest,
        "a new watch invalidates the snapshot taken before registration",
    );
    assert_eq!(board.scan().rows, [row]);
    assert!(
        !rest.until(std::time::Instant::now()),
        "the retained watch does not invalidate an unchanged snapshot"
    );
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_rearmed_board_watch_requires_a_fresh_snapshot_after_replacement() {
    let (dir, board) = scratch();
    board.ensure().expect("the original board folder");
    let mut rest = Rest::new(board.dir());
    std::fs::rename(board.dir(), dir.path().join("previous-board"))
        .expect("move the watched directory away");
    assert!(rest.until(std::time::Instant::now()), "the watch is gone");
    assert!(board.scan().rows.is_empty());

    let row = worker("s-0d2", "L", now_ms());
    board
        .record(&row)
        .expect("write before the replacement watch");
    let mut rest = rearmed_without_another_write(
        rest,
        "a replacement watch invalidates the snapshot taken before registration",
    );
    assert_eq!(board.scan().rows, [row]);
    assert!(
        !rest.until(std::time::Instant::now()),
        "the replacement watch stays armed without another invalidation"
    );
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_board_watch_hears_replacement_of_its_state_directory() {
    let root = aterm_tempfile::tempdir().expect("scratch");
    let state = root.path().join("state");
    let board = Board::in_state(&state);
    board.ensure().expect("the original board");
    let original = aterm_uds::directory_identity::read(board.dir()).unwrap();
    let rest = Rest::new(board.dir());
    assert!(board.scan().rows.is_empty());

    let previous = root.path().join("previous-state");
    std::fs::rename(&state, &previous).expect("move the state ancestor");
    let row = worker("s-0e1", "L", now_ms());
    board
        .record(&row)
        .expect("write into the replacement board");
    assert_eq!(
        aterm_uds::directory_identity::read(&previous.join(DIR)).unwrap(),
        original
    );
    assert_ne!(
        aterm_uds::directory_identity::read(board.dir()).unwrap(),
        original
    );
    let mut rest = rearmed_without_another_write(
        rest,
        "a moved state directory invalidates the old board snapshot",
    );
    assert_eq!(board.scan().rows.as_slice(), std::slice::from_ref(&row));
    assert!(
        !rest.until(std::time::Instant::now()),
        "the new path is armed"
    );

    let later = worker("s-0e2", "L", now_ms());
    board.record(&later).expect("write after recovery");
    let _rest = rearmed_without_another_write(rest, "a later write at the new path is heard");
    let mut rows = board.scan().rows;
    rows.sort_by(|a, b| a.sid.cmp(&b.sid));
    assert_eq!(rows, [row, later]);
}

#[test]
fn a_label_is_taken_off_a_spawn_before_it_leaves_this_mac() {
    let words = |w: &[&str]| w.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
    let (kept, label) =
        take_label(&words(&["agent=claude", "label=EW-24", "model=opus"])).expect("take");
    assert_eq!(kept, words(&["agent=claude", "model=opus"]));
    assert_eq!(label.as_deref(), Some("EW-24"));
    let (kept, label) = take_label(&words(&["agent=claude"])).expect("none");
    assert_eq!((kept.len(), label), (1, None));
    for bad in [
        &["agent=claude", "label=a", "label=b"][..],
        &["agent=claude", "label="],
        &["agent=claude", "label=a/b"],
    ] {
        assert!(take_label(&words(bad)).is_err(), "{bad:?}");
    }
    let long = format!("label={}", "x".repeat(WORD_MAX + 1));
    assert!(take_label(&[long]).is_err());
}

#[test]
fn a_spawned_row_keeps_the_brief_by_digest_never_its_text() {
    let task = b"secret brief text";
    let args: Vec<String> = ["agent=claude", "model=opus", "cwd=/w"]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
    let row = spawned(
        "s-0a1",
        Placed {
            node: Some("ab".repeat(16)),
            host: Some("m100".to_owned()),
            account: Some("m22".to_owned()),
        },
        Some("EW-24".to_owned()),
        (&args, task),
        Some("s-0d1"),
        42,
    );
    let line = row.line();
    assert!(!line.contains("secret"), "{line}");
    assert_eq!(row.brief, Some(brief_digest(task)));
    assert_eq!(row.brief_bytes, task.len() as u64);
    assert_eq!(row.program.as_deref(), Some("claude"));
    assert_eq!(row.model.as_deref(), Some("opus"));
    assert_eq!(row.autopilot.as_deref(), Some("land"), "a worker's default");
    assert_eq!(row.from.as_deref(), Some("s-0d1"));
    assert_eq!((row.state.as_str(), row.rev), ("working", 0));
}

/// A PROGRAM WORKER'S ROW NAMES ITS PROGRAM AND NO AUTOPILOT: none reads it. CONTROL: an agent's
/// row above defaults to Land.
#[test]
fn a_program_workers_row_names_its_program_and_no_autopilot() {
    let args: Vec<String> = ["program=/opt/my tools/run", "arg=-x", "cwd=/w"]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
    let row = spawned("s-0a2", Placed::default(), None, (&args, b"t"), None, 42);
    assert_eq!(row.program.as_deref(), Some("/opt/my tools/run"));
    assert_eq!(row.autopilot, None);
    assert_eq!(row.model, None);
    let back = Row::parse(&row.line()).expect("a row");
    assert_eq!(
        back.program.as_deref(),
        Some("/opt/my tools/run"),
        "one field, read back"
    );
}

#[test]
fn brief_digest_is_sha256() {
    assert_eq!(
        brief_digest(b"abc"),
        "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}

#[test]
fn a_board_reply_reads_back_as_rows_lines_and_json() {
    let (_dir, board) = scratch();
    board
        .record(&worker("s-0a1", "EW-24", at(1)))
        .expect("record");
    let mut closed = worker("s-0b2", "EW-19", at(2));
    closed.state = "closed".to_owned();
    board.record(&closed).expect("record");
    let text = reply(&board.scan(), Some("m14 mini"));
    let mut lines = text.lines();
    assert_eq!(lines.next(), Some("OK 3"));
    let rows: Vec<String> = lines.map(str::to_owned).collect();
    assert_eq!(
        rows[0],
        "board host=m14%20mini rows=2 shown=2 open=1 unread=0"
    );
    let read = parse_reply(&rows).expect("parse");
    assert_eq!(read.host(), Some("m14 mini"));
    assert_eq!(read.rows.len(), 2);
    assert_eq!(
        lines_of(&read),
        rows.iter().map(|r| format!("{r}\n")).collect::<String>()
    );
    let json = json_of(&read);
    assert!(
        json.starts_with("{\"board\":{\"host\":\"m14 mini\""),
        "{json}"
    );
    assert!(json.contains("\"cwd\":\"/Users//a b/repo\""), "{json}");
    assert!(json.contains("\"label\":\"EW-19\""), "{json}");
    assert!(parse_reply(&["worker sid=s-0a1 state=working".to_owned()]).is_err());
    assert!(parse_reply(&["board rows=0".to_owned(), "nope".to_owned()]).is_err());
}

#[test]
fn a_wait_row_of_the_board_says_it_was_read_off_the_board() {
    let mut row = worker("s-0a1", "EW-24", 1);
    row.rev = 4;
    row.state = "done".to_owned();
    row.conclusion = Some("finished".to_owned());
    assert_eq!(
        row.await_row(),
        "@s-0a1 rev=4 state=done agent=- program_state=- attention=- result_bytes=0 \
         conclusion=finished read=board"
    );
    assert_eq!(row.wait_tail(Some("m14")), " label=EW-24 host=m100");
    row.host = None;
    assert_eq!(row.wait_tail(Some("m14")), " label=EW-24 host=m14");
}

#[test]
fn a_read_shows_every_open_row_before_any_closed_one() {
    let mut rows: Vec<Row> = (0..SHOWN_MAX as u64 + 50)
        .map(|n| {
            let mut row = worker(&format!("s-{:x}", 0x1000 + n), "L", 1_000 + n);
            row.state = "closed".to_owned();
            row
        })
        .collect();
    rows.insert(0, worker("s-0a1", "oldest-open", 1));
    let scan = Scan { rows, unread: 0 };
    let shown = shown(&scan);
    assert_eq!(shown.len(), SHOWN_MAX);
    assert_eq!(
        shown[0].sid, "s-0a1",
        "the oldest row, open, is shown, oldest first"
    );
    assert!(
        shown.windows(2).all(|w| w[0].started_ms <= w[1].started_ms),
        "oldest first"
    );
}

#[test]
fn a_filter_takes_a_dispatchers_rows_and_a_labels() {
    let words = |w: &[&str]| w.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
    let (kept, filter) =
        Filter::take(&words(&["--json", "from=@s-0d1", "label=EW-*"])).expect("take");
    assert_eq!(kept, words(&["--json"]));
    assert_eq!(filter.from.as_deref(), Some("s-0d1"));
    let mut row = worker("s-0a1", "EW-24", 1);
    row.from = Some("s-0d1".to_owned());
    assert!(row.matches(&filter));
    assert!(row.matches(&Filter::default()));
    let exact = Filter {
        label: Some("EW-2".to_owned()),
        ..Filter::default()
    };
    assert!(!row.matches(&exact), "a label without `*` names one label");
    row.from = Some("s-0d2".to_owned());
    assert!(!row.matches(&filter), "another dispatcher's");
    for bad in [
        &["from=s-0d1", "from=s-0d2"][..],
        &["from=nope"],
        &["label=a/b"],
        &["label=a", "label=b"],
    ] {
        assert!(Filter::take(&words(bad)).is_err(), "{bad:?}");
    }
}

/// A closed row's `heard_ms` is when it closed, and its rev the board's own: a worker read as
/// gone again moves neither, so the keeper stops asking and the row is pruned on time, and an
/// asker's edge never outranks the worker's own rev when it answers again.
#[test]
fn a_closed_row_keeps_when_it_closed_and_its_own_rev() {
    let mut row = worker("s-0a1", "L", 1);
    assert!(row.merge("@s-0a1 rev=3 state=working", 10));
    assert!(row.merge("@s-0a1 rev=10 state=closed agent=- result_bytes=0", 20));
    assert_eq!(
        (row.rev, row.heard_ms),
        (3, Some(20)),
        "closed at 20, rev 3"
    );
    assert!(
        !row.merge("@s-0a1 rev=10 state=closed agent=- result_bytes=0", 30),
        "gone again is no news"
    );
    assert_eq!(row.heard_ms, Some(20), "and not hearing from it");
    assert!(
        row.merge("@s-0a1 rev=3 state=working why=restored", 40),
        "its tab restored at its own rev reopens it"
    );
    assert_eq!((row.state.as_str(), row.heard_ms), ("working", Some(40)));
}

/// A close the worker's Mac answered (its row ends `task_news=`) raises the board's rev, so a
/// live row read before it and merged after it does not reopen the row and drop its
/// conclusion: the regression the board's own rev on every closed row was.
#[test]
fn a_closed_row_its_mac_answered_is_not_reopened_by_an_older_read() {
    let mut row = worker("s-0a1", "L", 1);
    assert!(row.merge("@s-0a1 rev=5 state=working task_news=0", 10));
    assert!(row.merge(
        "@s-0a1 rev=6 state=closed agent=- program_state=- attention=- result_bytes=0 \
         conclusion=finished checked=0123456789ab@origin/main task_news=1",
        20
    ));
    assert_eq!(row.rev, 6);
    assert!(
        !row.merge("@s-0a1 rev=5 state=working task_news=0", 30),
        "a read from before the close changes nothing"
    );
    assert_eq!(
        (row.state.as_str(), row.conclusion.as_deref()),
        ("closed", Some("finished"))
    );
}

/// The regression: a wait held a worker's `done` row across a dropped leg while the board heard
/// it fail, and once its tab went the wait closed it from that older row, whose `finished` and
/// receipt replaced the board's newer `failed`, for good (every later close carries no words).
/// A close below the board's rev keeps the board's words. NEGATIVE CONTROL: a close at the
/// board's rev carries its own.
#[test]
fn a_close_built_from_an_older_row_keeps_the_boards_newer_words() {
    let mut row = worker("s-0a1", "L", 1);
    assert!(row.merge(
        "@s-0a1 rev=5 state=done conclusion=finished why=landed \
         checked=0123456789ab@origin/main task_news=1",
        10
    ));
    assert!(row.merge(
        "@s-0a1 rev=7 state=failed conclusion=failed why=could-not-finish task_news=1",
        20
    ));
    assert!(row.merge(
        "@s-0a1 rev=5 state=closed agent=- program_state=- attention=- result_bytes=0 \
         conclusion=finished why=landed checked=0123456789ab@origin/main task_news=1",
        30
    ));
    assert_eq!(
        (
            row.state.as_str(),
            row.rev,
            row.conclusion.as_deref(),
            row.why.as_deref(),
            row.checked.as_deref()
        ),
        ("closed", 7, Some("failed"), Some("could-not-finish"), None)
    );
    assert_eq!(
        row.landed.as_deref(),
        Some("0123456789ab"),
        "a landing read once stands"
    );

    let mut row = worker("s-0a2", "L", 1);
    assert!(row.merge(
        "@s-0a2 rev=7 state=failed conclusion=failed why=could-not-finish task_news=1",
        10
    ));
    assert!(row.merge(
        "@s-0a2 rev=7 state=closed agent=- program_state=- attention=- result_bytes=0 \
         conclusion=failed why=exit task_news=1",
        20
    ));
    assert_eq!(
        (row.state.as_str(), row.why.as_deref()),
        ("closed", Some("exit"))
    );
}

/// The regression: the keeper re-asked a closed row every ten minutes and each `gone` answer
/// moved `heard_ms`, so the row was asked forever and never pruned.
#[test]
fn a_board_prunes_a_closed_row_a_keeper_keeps_reading_as_gone() {
    let (_dir, board) = scratch();
    let day = 24 * 3_600_000;
    let now = now_ms();
    let mut row = worker("s-0a1", "L", now - 9 * day);
    row.state = "closed".to_owned();
    row.heard_ms = Some(now - 6 * day);
    row.conclusion = Some("failed".to_owned());
    board.record(&row).expect("record");
    // Two days on, the keeper reads it gone again.
    let later = now + 2 * day;
    let gone = ["@s-0a1 rev=0 state=closed agent=- result_bytes=0".to_owned()];
    assert_eq!(board.apply(&gone, later).expect("apply"), 0);
    assert_eq!(
        board.row("s-0a1").expect("still there").heard_ms,
        Some(now - 6 * day),
        "a re-ask that reads it gone leaves when it closed"
    );
    let bound = board.bind(false).expect("bound board");
    let _lock = bound.lock().expect("the board's lock");
    bound.prune_locked(later);
    assert!(board.row("s-0a1").is_none(), "closed over a week: pruned");
}

#[cfg(unix)]
#[test]
fn a_mac_that_keeps_not_answering_is_asked_less_often_until_it_answers() {
    let now = std::time::Instant::now();
    let mut backoff = Backoff::default();
    assert!(backoff.asks("n1", now));
    let waits: Vec<u64> = (0..7)
        .map(|_| backoff.unanswered("n1".to_owned(), now).as_secs())
        .collect();
    assert_eq!(
        waits,
        [40, 80, 160, 320, 600, 600, 600],
        "to the most, not reset"
    );
    assert!(!backoff.asks("n1", now + Duration::from_secs(599)));
    assert!(
        backoff.asks("n1", now + KEEP_BACKOFF_MAX),
        "once its wait is over"
    );
    assert_eq!(
        backoff.unanswered("n1".to_owned(), now).as_secs(),
        600,
        "a wait that ran out keeps the count"
    );
    backoff.answered("n1");
    assert_eq!(backoff.unanswered("n1".to_owned(), now).as_secs(), 40);
    backoff.keep(|_| false);
    assert!(backoff.asks("n1", now), "a Mac no row names is forgotten");
}

#[test]
fn a_lone_dash_is_no_label_and_no_accept_word() {
    let words = |w: &[&str]| w.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
    assert!(take_label(&words(&["agent=claude", "label=-"])).is_err());
    assert!(take_label(&words(&["agent=claude", "label=a-"])).is_ok());
    let (_dir, board) = scratch();
    board.record(&worker("s-0a1", "L", at(1))).expect("record");
    assert!(board.accept("@s-0a1", "-", at(2)).is_err());
    assert!(board.accept("-", "passed", at(2)).is_err());
    assert!(Filter::take(&words(&["label=-"])).is_err());
}

#[test]
fn no_byte_another_mac_sends_reaches_a_terminal_as_sent() {
    let sent = "sid=s-0a1 state=working later=\u{1b}]0;x\u{7}";
    let row = Row::parse(sent).expect("a later key");
    assert!(
        !row.line().contains(['\u{1b}', '\u{7}']),
        "an unknown key's value is kept encoded: {}",
        row.line()
    );
    let read = parse_reply(&[
        "board host=m1 \u{1b}[2J=1 rows=1".to_owned(),
        format!("worker {sent}"),
    ])
    .expect("a reply");
    assert!(
        read.header.iter().all(|(key, _)| valid_word(key)),
        "a header key that is no word is dropped: {:?}",
        read.header
    );
    let shown = lines_of(&read);
    assert!(!shown.contains(['\u{1b}', '\u{7}']), "{shown}");
}

/// A wait writes only the rows of the Mac that answered it: one over this Mac's socket the
/// rows of its own workers, one a person aimed at a Mac nothing, one that asked each worker
/// where its row says it runs every row it answered.
#[test]
fn a_wait_writes_only_rows_of_the_mac_that_answered() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let _here = HereGuard::at(dir.path());
    let board = Board::at(dir.path());
    let mut local = worker("s-0a1", "L", at(1));
    local.node = None;
    board.record(&local).expect("record");
    board.record(&worker("s-0a2", "R", at(2))).expect("record");
    let seen = [
        "@s-0a1 rev=4 state=done conclusion=finished".to_owned(),
        "@s-0a2 rev=4 state=closed conclusion=finished checked=0123456789ab@origin/main".to_owned(),
    ];
    let states = || ["s-0a1", "s-0a2"].map(|sid| board.row(sid).expect("a row").state);
    apply_seen(&seen, Writes::Nothing);
    assert_eq!(states(), ["working", "working"]);
    apply_seen(&seen, Writes::Here);
    assert_eq!(
        states(),
        ["done", "working"],
        "this Mac's socket answers for its own workers alone"
    );
    apply_seen(&seen, Writes::Rows);
    assert_eq!(states(), ["done", "closed"]);
}

/// Old round replies and pruning remain on the board whose keeper lock is held, even
/// when the same pathname now names new rows with the same worker ids.
#[cfg(unix)]
#[test]
fn a_keeper_lease_binds_reads_merges_and_pruning_to_its_locked_directory() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let state = scratch.path().join("state");
    let retired = scratch.path().join("retired");
    let board = Board::in_state(&state);
    let original = worker("s-0a1", "original", at(1));
    board.record(&original).unwrap();
    let held = board.keeper_lock().unwrap();
    std::fs::rename(&state, &retired).unwrap();
    let replacement = worker("s-0a1", "replacement", at(2));
    board.record(&replacement).unwrap();
    let new_keeper = board.keeper_lock().unwrap();
    assert!(!held.board.is_current());
    assert!(new_keeper.board.is_current());
    assert_eq!(
        held.board.scan().rows.as_slice(),
        std::slice::from_ref(&original)
    );
    let read = held.board.row("s-0a1").unwrap();
    assert_eq!(read, original);
    assert_eq!(
        held.board
            .apply(
                &["@s-0a1 rev=3 state=done conclusion=finished".to_owned()],
                now_ms()
            )
            .unwrap(),
        1
    );
    let old_board = Board::in_state(&retired);
    let updated = old_board.row("s-0a1").unwrap();
    assert_eq!(updated.rev, 3);
    assert_eq!(updated.label, original.label);
    assert_eq!(updated.started_ms, original.started_ms);
    assert_eq!(board.row("s-0a1").unwrap(), replacement);
    assert!(
        old_board.keeper_lock().is_none(),
        "the old lease still owns the retired board"
    );
    let _writer = held.board.lock().unwrap();
    held.board.prune_locked(now_ms() + millis(UNHEARD_KEPT));
    assert!(old_board.row("s-0a1").is_none());
    assert_eq!(board.row("s-0a1").unwrap(), replacement);
    drop((held, new_keeper));
    assert!(old_board.keeper_lock().is_some());
}

/// A waiter already asleep on the retired inode must discard it after handoff, then
/// acquire the directory currently named by the board. The callback proves pre-opening;
/// the minute bounds a hang, not a promised handoff latency.
#[cfg(unix)]
#[test]
fn a_preopened_keeper_waiter_rebinds_after_directory_replacement() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let state = scratch.path().join("state");
    let retired = scratch.path().join("retired");
    let board = Board::in_state(&state);
    let first = board.keeper_lock().unwrap();
    let (held, found_held) = std::sync::mpsc::channel();
    let (finished, result) = std::sync::mpsc::channel();
    let waiter = {
        let board = board.clone();
        std::thread::spawn(move || {
            let binding = board.wait_keeper_lock(|| {
                held.send(()).unwrap();
            });
            finished
                .send(binding)
                .unwrap_or_else(|_| panic!("waiter result receiver gone"));
        })
    };
    found_held
        .recv_timeout(Duration::from_secs(60))
        .expect("waiter pre-opened the old lock");
    std::fs::rename(&state, &retired).unwrap();
    let replacement = worker("s-0a1", "replacement", at(1));
    board.record(&replacement).unwrap();
    drop(first);
    let second = result
        .recv_timeout(Duration::from_secs(60))
        .expect("keeper handoff did not finish")
        .unwrap();
    waiter.join().unwrap();
    assert!(second.board.is_current());
    assert_eq!(second.board.scan().rows, [replacement]);
    assert!(
        board.keeper_lock().is_none(),
        "the returned lease owns the current board"
    );
    assert!(
        Board::in_state(&retired).keeper_lock().is_some(),
        "the retired lock was released"
    );
}

#[cfg(unix)]
#[test]
fn a_writer_transaction_keeps_its_directory_when_the_state_path_is_replaced() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let state = scratch.path().join("state");
    let retired = scratch.path().join("retired");
    let board = Board::in_state(&state);
    let original = worker("s-0a1", "original", at(1));
    board.record(&original).unwrap();
    use std::os::unix::fs::MetadataExt as _;
    let transaction = board.bind(false).unwrap();
    let old_writer = transaction.lock().unwrap();
    std::fs::rename(&state, &retired).unwrap();
    let metadata = std::fs::metadata(retired.join(DIR).join(LOCK)).unwrap();
    let replacement = worker("s-0a1", "replacement", at(2));
    board.record(&replacement).unwrap();
    let (events, first_event) = std::sync::mpsc::channel();
    let writer = std::thread::spawn(move || {
        let mut contended = Some(events.clone());
        let result = aterm_uds::file_lock::with_lock_wait_observer(
            move |file| {
                if let Some(events) = contended.take() {
                    let metadata = file.metadata().unwrap();
                    events.send(Some((metadata.dev(), metadata.ino()))).unwrap();
                }
            },
            || transaction.accept("@s-0a1", "passed", now_ms()),
        );
        // A wrong-directory lock would complete instead of contending: report that too,
        // so the failure is observed without using absence or a short timing threshold.
        let _ = events.send(None);
        result
    });
    let first = first_event
        .recv_timeout(Duration::from_secs(60))
        .expect("writer progress");
    drop(old_writer);
    let accepted = writer.join().unwrap().unwrap();
    assert_eq!(
        first,
        Some((metadata.dev(), metadata.ino())),
        "the transaction locks the retired directory it bound"
    );
    assert_eq!(accepted.label, original.label);
    assert_eq!(
        Board::in_state(&retired)
            .row("s-0a1")
            .unwrap()
            .accept
            .as_deref(),
        Some("passed")
    );
    assert_eq!(board.row("s-0a1").unwrap(), replacement);
}
