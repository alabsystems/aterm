// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The schema (round trip, checksum, privacy, the shell's status), the log (append, rotation,
//! a crash mid-write, the reader's window) and the writer (a lost row is said, not hidden).

use std::io::Write as _;
use std::time::Duration;

use crate::facts::HostFacts;
use crate::log::{self, Limits, Log, file_path};
use crate::row::{self, Digest, Fig, Kind, Line, Meter, Outcome, Row, Stop, parse_line};
use crate::sink::Sink;
use crate::summary;

fn facts() -> HostFacts {
    HostFacts {
        host: "m14".to_owned(),
        model: "Mac16,6".to_owned(),
        cpu: "Apple M4 Max".to_owned(),
        cores: 16,
        mem_mib: 65_536,
        os: "macOS 27.0.1".to_owned(),
        aterm: "0.153.0+gabc".to_owned(),
    }
}

fn row(at_ms: u64, kind: Kind, wall: u64) -> Row {
    Row {
        at_ms,
        kind: kind.clone(),
        key: Some(Digest::key(&kind, "targo test -p x", Some("/w/aterm"))),
        dir: Some(Digest::dir("/w/aterm")),
        label: row::label("/usr/local/bin/targo"),
        sid: Some("s-1a2b".to_owned()),
        on: None,
        host: "m14".to_owned(),
        facts: facts().digest(),
        wall_ms: Fig::Known(wall),
        cpu_ms: Fig::Known(wall / 2),
        peak_bytes: Fig::unknown("jobs"),
        wait_ms: Fig::unknown("aterm-capacity"),
        cache: Fig::unknown("targo"),
        outcome: Outcome::Ok,
        stop: Stop::Exit,
        code: Some(0),
        sig: None,
        meter: Meter::Exact,
    }
}

#[test]
fn a_row_reads_back_as_written() {
    let mut r = row(1_793_826_807_000, Kind::Command, 1234);
    assert_eq!(parse_line(&r.line()), Some(Line::Row(r.clone())));
    r.cache = Fig::Na;
    r.peak_bytes = Fig::Known(1 << 30);
    r.outcome = Outcome::Fail;
    r.stop = Stop::Signal;
    r.code = Some(130);
    r.sig = Some(2);
    r.dir = None;
    r.sid = None;
    r.kind = Kind::AgentCall;
    r.meter = Meter::Sampled;
    assert_eq!(parse_line(&r.line()), Some(Line::Row(r.clone())));
    r.key = None;
    assert!(r.line().contains(" key=- "), "{}", r.line());
    assert_eq!(parse_line(&r.line()), Some(Line::Row(r)));
}

#[test]
fn a_fleet_row_names_the_mac_it_ran_on_and_a_reader_before_it_keeps_the_row() {
    for kind in [Kind::Build, Kind::Test, Kind::Task] {
        let mut r = row(9, kind.clone(), 3);
        assert_eq!(
            parse_line(&r.line()),
            Some(Line::Row(r.clone())),
            "{kind:?}"
        );
        assert!(!r.line().contains(" on="), "no on= unless known");
        r.on = Some("0123456789abcdef0123456789abcdef".to_owned());
        let line = r.line();
        assert!(
            line.contains(" on=0123456789abcdef0123456789abcdef host=m14 "),
            "{line}"
        );
        assert_eq!(parse_line(&line), Some(Line::Row(r.clone())));
    }
    // What a reader that predates a kind or a key does with them, as one before TM-1 did with
    // `build`, `test`, `task` and `on=`: it keeps the row under the kind's word and skips the key.
    let newer = row(9, Kind::Other("probe".to_owned()), 3);
    let body = newer.body().replace(" host=", " later=word host=");
    assert_eq!(
        parse_line(&crate::row::seal(body)),
        Some(Line::Row(newer)),
        "a newer writer's row reads as its kind's word, its unknown key skipped"
    );
    let mut bad = row(9, Kind::Task, 3).body();
    bad = bad.replace(" host=", " on=a/b host=");
    assert_eq!(
        parse_line(&crate::row::seal(bad)),
        None,
        "an on= no word spells"
    );
}

#[test]
fn the_line_says_schema_1_and_names_every_field() {
    let line = row(5, Kind::Spawn, 7).line();
    assert!(line.starts_with("schema=1 at=5 kind=spawn "), "{line}");
    for key in [
        "key=",
        "dir=",
        "label=targo ",
        "sid=",
        "host=m14 ",
        "facts=",
        "wall_ms=7 ",
        "cpu_ms=3 ",
        "peak_bytes=?jobs ",
        "wait_ms=?aterm-capacity ",
        "cache=?targo ",
        "outcome=ok ",
        "stop=exit ",
        "code=0 ",
        "sig=- ",
        "meter=exact ",
        " sum=",
    ] {
        assert!(line.contains(key), "{key} missing: {line}");
    }
    assert!(line.ends_with('\n'));
}

#[test]
fn a_changed_or_cut_line_is_torn() {
    let line = row(5, Kind::Command, 7).line();
    assert!(parse_line(&line.replace("wall_ms=7", "wall_ms=8")).is_none());
    assert!(parse_line(&line[..line.len() / 2]).is_none());
    assert!(parse_line(&line.replace("schema=1", "schema=2")).is_none());
}

#[test]
fn a_row_holds_no_command_line_only_its_digest() {
    let secret = format!("curl -H '{}: {} {}'", "Authorization", "Bearer", "SECRET");
    let mut r = row(9, Kind::Command, 1);
    r.key = Some(Digest::key(
        &Kind::Command,
        &secret,
        Some("/Users//me/private"),
    ));
    r.label = row::label("curl");
    let line = r.line();
    assert!(!line.contains("SECRET") && !line.contains("Bearer") && !line.contains("private"));
    assert_eq!(row::label("/bin/my prog;rm -rf"), "my_prog_rm_-rf");
    assert_eq!(row::label(&"x".repeat(80)).len(), row::LABEL_MAX);
    assert_eq!(row::label(""), "-");
}

#[test]
fn the_key_is_stable_and_separates_what_and_where() {
    let a = Digest::key(&Kind::Command, "make", Some("/a"));
    assert_eq!(a, Digest::key(&Kind::Command, "make", Some("/a")));
    assert_ne!(a, Digest::key(&Kind::Command, "make", Some("/b")));
    assert_ne!(a, Digest::key(&Kind::AgentCall, "make", Some("/a")));
    // Length prefixes: moving a byte between parts changes the key.
    assert_ne!(
        Digest::key(&Kind::Command, "ab", Some("c")),
        Digest::key(&Kind::Command, "a", Some("bc"))
    );
    assert_eq!(Digest::parse(&a.hex()), Some(a));
    assert_eq!(Digest::parse("XYZ"), None);
}

#[test]
fn the_key_never_hashes_an_assignments_value() {
    let dir = Some("/w");
    let key = |line: &str| Digest::key(&Kind::Command, line, dir);
    // A guess at the value cannot be confirmed from the key: every value keys the same.
    assert_eq!(key("SECRET_TOKEN=hunter2 sleep 3"), key("sleep 3"));
    assert_eq!(
        key("SECRET_TOKEN=hunter2 sleep 3"),
        key("SECRET_TOKEN=correct-horse sleep 3")
    );
    assert_eq!(key(r#"A="two words" B='x y' make  -j8"#), key("make -j8"));
    assert_eq!(key(r"A=one\ two make"), key("make"));
    assert_eq!(key("export API_KEY=abc"), key("export"));
    // A word that is no assignment stays: the key still tells commands apart.
    assert_ne!(key("make -j8"), key("make -j4"));
    assert_ne!(key("ls --color=auto"), key("ls"));
    assert_ne!(key("ls 9X=1"), key("ls"));
    assert_eq!(
        row::key_words("  a   'b  c'  D=1 \"e f\""),
        "a 'b  c' \"e f\""
    );
}

#[test]
fn the_shells_status_names_the_stop() {
    assert_eq!(Stop::of_shell_status(Some(0)), (Stop::Exit, Some(0), None));
    assert_eq!(Stop::of_shell_status(Some(1)), (Stop::Exit, Some(1), None));
    assert_eq!(
        Stop::of_shell_status(Some(128)),
        (Stop::Exit, Some(128), None)
    );
    assert_eq!(
        Stop::of_shell_status(Some(130)),
        (Stop::Signal, Some(130), Some(2))
    );
    assert_eq!(
        Stop::of_shell_status(Some(137)),
        (Stop::Signal, Some(137), Some(9))
    );
    assert_eq!(Stop::of_shell_status(None), (Stop::Unknown, None, None));
}

#[test]
fn the_host_line_spells_the_facts_its_digest_names() {
    let f = facts();
    let Some(Line::Host(host)) = parse_line(&f.line(77)) else {
        panic!("no host line");
    };
    assert_eq!(host.facts, f.digest());
    assert_eq!(host.spelled.cpu, "Apple_M4_Max");
    let forged = f.line(77).replace("cores=16", "cores=8");
    // Re-sealed so only the digest, not the checksum, can catch it.
    let body = forged.rsplit_once(" sum=").expect("sealed").0.to_owned();
    assert!(parse_line(&row::seal(body)).is_none());
}

fn log_in(dir: &std::path::Path, limits: Limits) -> Log {
    Log::new(dir.join("telemetry"), limits, facts())
}

#[test]
fn appends_read_back_in_order_under_one_host_line() {
    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let log = log_in(tmp.path(), Limits::DEFAULT);
    for at in 1..=3 {
        let done = log
            .append(&[row(at, Kind::Command, at * 10).line()])
            .expect("append");
        assert_eq!((done.lines, done.rotated, done.mended), (1, false, false));
    }
    let read = log::read(log.dir(), 4, None).expect("read");
    assert_eq!(
        read.rows.iter().map(|r| r.at_ms).collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    assert_eq!(read.hosts.len(), 1);
    assert_eq!((read.torn, read.dropped, read.files), (0, 0, 1));
    let since = log::read(log.dir(), 4, Some(2)).expect("read");
    assert_eq!(since.rows.len(), 2);
}

#[cfg(unix)]
#[test]
fn the_log_is_the_accounts_own() {
    use std::os::unix::fs::PermissionsExt as _;
    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let log = log_in(tmp.path(), Limits::DEFAULT);
    log.append(&[row(1, Kind::Command, 10).line()])
        .expect("append");
    let mode =
        |p: &std::path::Path| std::fs::metadata(p).expect("metadata").permissions().mode() & 0o777;
    assert_eq!(mode(log.dir()), 0o700);
    assert_eq!(mode(&log::file_path(log.dir(), 0)), 0o600);
    assert_eq!(mode(&log.dir().join(".lock")), 0o600);
}

#[test]
fn rotation_keeps_the_log_bounded_and_the_newest_rows() {
    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let limits = Limits {
        file_bytes: 2_000,
        files: 3,
    };
    let log = log_in(tmp.path(), limits);
    let line_len = row(1, Kind::Command, 1).line().len() as u64;
    for at in 1..=100 {
        log.append(&[row(at, Kind::Command, at).line()])
            .expect("append");
    }
    assert!(!file_path(log.dir(), 3).exists(), "a fourth file");
    for n in 0..3 {
        let path = file_path(log.dir(), n);
        let len = std::fs::metadata(&path).expect("kept").len();
        assert!(
            len <= limits.file_bytes + line_len,
            "{path:?} is {len} bytes"
        );
        let text = std::fs::read_to_string(&path).expect("read");
        assert!(
            matches!(
                parse_line(text.lines().next().expect("a line")),
                Some(Line::Host(_))
            ),
            "{path:?} does not open with the host's facts"
        );
    }
    let read = log::read(log.dir(), limits.files, None).expect("read");
    assert_eq!(read.torn, 0);
    let ats: Vec<u64> = read.rows.iter().map(|r| r.at_ms).collect();
    assert_eq!(ats.last(), Some(&100), "the newest row is kept");
    assert!(
        ats.windows(2).all(|w| w[0] + 1 == w[1]),
        "in order, none lost between files"
    );
    assert!(ats[0] > 1, "the oldest rows were rotated away");
}

#[test]
fn a_crash_mid_write_costs_one_torn_line_and_the_next_row_is_whole() {
    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let log = log_in(tmp.path(), Limits::DEFAULT);
    log.append(&[row(1, Kind::Command, 1).line()])
        .expect("append");
    // A writer killed half-way through its line: no line feed, no checksum.
    let whole = row(2, Kind::Command, 2).line();
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(file_path(log.dir(), 0))
        .expect("open");
    f.write_all(&whole.as_bytes()[..whole.len() / 2])
        .expect("write");
    drop(f);
    let done = log
        .append(&[row(3, Kind::Command, 3).line()])
        .expect("append");
    assert!(done.mended, "the torn tail was ended before the append");
    let read = log::read(log.dir(), 4, None).expect("read");
    assert_eq!(
        read.rows.iter().map(|r| r.at_ms).collect::<Vec<_>>(),
        vec![1, 3]
    );
    assert_eq!(read.torn, 1);
}

#[test]
fn the_writer_appends_without_blocking_and_says_what_it_lost() {
    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let dir = tmp.path().join("telemetry");
    // Its folder cannot be made (a file is in the way): the first batch is lost and counted.
    std::fs::write(&dir, b"in the way").expect("write");
    let sink = Sink::start(Log::new(dir.clone(), Limits::DEFAULT, facts())).expect("start");
    sink.emit(row(1, Kind::Spawn, 1));
    assert!(sink.flush(Duration::from_secs(60)), "the writer answers");
    std::fs::remove_file(&dir).expect("remove");
    sink.emit(row(2, Kind::Spawn, 2));
    sink.emit(row(3, Kind::Spawn, 3));
    assert!(sink.flush(Duration::from_secs(60)), "the writer answers");
    let read = log::read(&dir, 4, None).expect("read");
    assert_eq!(
        read.rows.iter().map(|r| r.at_ms).collect::<Vec<_>>(),
        vec![2, 3]
    );
    assert_eq!(read.dropped, 1, "the lost row is said in a gap line");
    let counts = sink.counts();
    use std::sync::atomic::Ordering::Relaxed;
    assert_eq!(
        (
            counts.emitted.load(Relaxed),
            counts.written.load(Relaxed),
            counts.dropped.load(Relaxed)
        ),
        (3, 2, 1)
    );
}

/// A batch cut short after the window's gap line (a disk that filled mid-write) never has the
/// line's rows said again: the next gap line says only the row the batch lost.
#[test]
fn a_batch_cut_short_after_its_gap_line_never_has_the_line_said_twice() {
    use std::sync::atomic::{AtomicU64, Ordering::SeqCst};
    const HANG: Duration = Duration::from_secs(60);
    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let dir = tmp.path().join("telemetry");
    let left = std::sync::Arc::new(AtomicU64::new(u64::MAX));
    let log = Log::new(dir.clone(), Limits::DEFAULT, facts()).cut_after(left.clone());
    let sink = Sink::start(log).expect("start");
    sink.emit(row(1, Kind::Spawn, 1));
    assert!(sink.flush(HANG), "the writer answers");
    // Two rows the queue lost, then a batch the disk fills during, just past its gap line.
    sink.counts().dropped.fetch_add(2, SeqCst);
    let gap = row::gap_line(crate::now_ms(), 2).len() as u64;
    left.store(gap + 10, SeqCst);
    sink.emit(row(2, Kind::Spawn, 2));
    assert!(sink.flush(HANG), "the writer answers");
    left.store(u64::MAX, SeqCst);
    sink.emit(row(3, Kind::Spawn, 3));
    assert!(sink.flush(HANG), "the writer answers");
    let read = log::read(&dir, 4, None).expect("read");
    assert_eq!(
        read.rows.iter().map(|r| r.at_ms).collect::<Vec<_>>(),
        vec![1, 3]
    );
    assert_eq!(
        (read.dropped, read.torn),
        (3, 0),
        "the two and the cut row, each said once, and none of the cut row left"
    );
    let counts = sink.counts();
    assert_eq!(
        (counts.written.load(SeqCst), counts.dropped.load(SeqCst)),
        (2, 3)
    );
}

/// A gap line cut only of its line feed would read as whole: the write is cut back, so the line
/// the append reports unwritten is not in the log, and its rows are said once, by the next.
#[test]
fn a_gap_line_cut_before_its_line_feed_is_not_said_twice() {
    use std::sync::atomic::{AtomicU64, Ordering::SeqCst};
    const HANG: Duration = Duration::from_secs(60);
    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let dir = tmp.path().join("telemetry");
    let left = std::sync::Arc::new(AtomicU64::new(u64::MAX));
    let log = Log::new(dir.clone(), Limits::DEFAULT, facts()).cut_after(left.clone());
    let sink = Sink::start(log).expect("start");
    sink.emit(row(1, Kind::Spawn, 1));
    assert!(sink.flush(HANG), "the writer answers");
    sink.counts().dropped.fetch_add(2, SeqCst);
    let gap = row::gap_line(crate::now_ms(), 2).len() as u64;
    left.store(gap - 1, SeqCst);
    sink.emit(row(2, Kind::Spawn, 2));
    assert!(sink.flush(HANG), "the writer answers");
    left.store(u64::MAX, SeqCst);
    sink.emit(row(3, Kind::Spawn, 3));
    assert!(sink.flush(HANG), "the writer answers");
    let read = log::read(&dir, 4, None).expect("read");
    assert_eq!(
        read.rows.iter().map(|r| r.at_ms).collect::<Vec<_>>(),
        vec![1, 3]
    );
    assert_eq!(
        (read.dropped, read.torn),
        (3, 0),
        "the two and the row behind the cut line, each said once"
    );
}

#[test]
fn a_stalled_writer_never_holds_up_an_emit_and_says_every_row_it_lost() {
    use std::sync::atomic::Ordering::Relaxed;
    const HANG: Duration = Duration::from_secs(60);
    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let dir = tmp.path().join("telemetry");
    std::fs::create_dir(&dir).expect("dir");
    // Another writer holds the lock, and this one would wait past the hang detector for it.
    let held = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.join(".lock"))
        .expect("lock file");
    aterm_uds::file_lock::lock_exclusive_until(
        &held,
        std::time::Instant::now() + HANG,
        "the test holds the telemetry lock",
    )
    .expect("hold the lock");
    let log = Log::new(dir.clone(), Limits::DEFAULT, facts()).waiting(HANG * 10);
    let sink = std::sync::Arc::new(Sink::start(log).expect("start"));
    let n = crate::sink::QUEUE as u64 + 400;
    let (done, emitted) = std::sync::mpsc::channel();
    let emitter = std::sync::Arc::clone(&sink);
    std::thread::spawn(move || {
        for at in 1..=n {
            emitter.emit(row(at, Kind::Command, 1));
        }
        let _ = done.send(());
    });
    emitted
        .recv_timeout(HANG)
        .expect("an emit waited on the stalled writer");
    let lost = sink.counts().dropped.load(Relaxed);
    assert!(
        lost >= 400 - 256,
        "a full queue drops what it cannot hold: {lost} of {n} dropped"
    );
    drop(held);
    assert!(sink.flush(HANG), "the writer answers once the lock is free");
    // The gap line goes before the writer's next batch: one more row carries it.
    sink.emit(row(n + 1, Kind::Command, 1));
    assert!(sink.flush(HANG), "the writer answers");
    let counts = sink.counts();
    let (emitted, written, dropped) = (
        counts.emitted.load(Relaxed),
        counts.written.load(Relaxed),
        counts.dropped.load(Relaxed),
    );
    assert_eq!(emitted, n + 1);
    assert_eq!(
        written + dropped,
        emitted,
        "every row is written or counted"
    );
    let read = log::read(&dir, 4, None).expect("read");
    assert_eq!(read.rows.len() as u64, written);
    assert_eq!(read.dropped, dropped, "the log says every row it lost");
}

#[test]
fn the_summary_is_one_line_a_kind_with_nearest_rank_percentiles() {
    assert_eq!(summary::rank(&[], 50), None);
    assert_eq!(summary::rank(&[7], 90), Some(7));
    let v: Vec<u64> = (1..=10).collect();
    assert_eq!(
        (summary::rank(&v, 50), summary::rank(&v, 90)),
        (Some(5), Some(9))
    );
    let mut rows: Vec<Row> = (1..=10).map(|i| row(i, Kind::Command, i * 100)).collect();
    let mut failed = row(11, Kind::AgentCall, 5);
    failed.outcome = Outcome::Fail;
    failed.cpu_ms = Fig::unknown("jobs");
    rows.push(failed);
    let lines = summary::by_kind(&rows);
    assert_eq!(lines.len(), 2);
    let agent = &lines[0];
    assert_eq!(
        (agent.kind.as_str(), agent.n, agent.failed),
        ("agent-call", 1, 1)
    );
    assert_eq!((agent.cpu.n, agent.cpu.unknown), (0, 1));
    let cmd = &lines[1];
    assert_eq!((cmd.kind.as_str(), cmd.n), ("command", 10));
    assert_eq!((cmd.wall.p50, cmd.wall.p90), (Some(500), Some(900)));
    assert_eq!((cmd.peak.n, cmd.peak.unknown), (0, 10));
    let reading = log::Reading {
        rows,
        ..log::Reading::default()
    };
    let text = summary::render(&reading, None, "/t");
    assert!(text.starts_with("telemetry schema=1 rows=11 "), "{text}");
    assert!(
        text.contains("\nkind=command n=10 failed=0 stalled=0 wall_p50_ms=500 wall_p90_ms=900")
    );
    let json = summary::render_json(&reading, None, "/t", false);
    assert!(json.contains(r#""kind":"command""#), "{json}");
}

#[test]
fn since_reads_relative_spans_and_absolute_times() {
    let now = 10_000_000;
    assert_eq!(summary::parse_since("90s", now), Ok(now - 90_000));
    assert_eq!(summary::parse_since("2h", now), Ok(now - 7_200_000));
    assert_eq!(summary::parse_since("1d", 100), Ok(0));
    assert_eq!(
        summary::parse_since("1793826807000", now),
        Ok(1_793_826_807_000)
    );
    assert!(summary::parse_since("yesterday", now).is_err());
    assert!(summary::parse_since("5w", now).is_err());
}
