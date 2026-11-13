// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_messages::{
    LOG_CAP, LogRecord, LogState, Message, MessageId, Origin, Retired, Severity, WallStamp, tags,
};

/// `lines` shelved as aterm's own.
fn host(lines: &[LogLine]) -> Vec<(LogLine, Shelf)> {
    lines.iter().map(|l| (l.clone(), Shelf::Host)).collect()
}

/// A record the wire posted.
fn wire_posted(id: u64, title: &str) -> LogLine {
    let mut msg = Message::new(tags::SYSTEM, Severity::Warn, title);
    msg.origin = Origin::Wire;
    LogLine::Posted(LogRecord::from_posted(
        MessageId::from_raw(id).unwrap(),
        WallStamp { unix_ms: id },
        &msg,
    ))
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "aterm-messages-store-{}-{name}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

fn posted(id: u64, title: &str) -> LogLine {
    LogLine::Posted(LogRecord::from_posted(
        MessageId::from_raw(id).unwrap(),
        WallStamp { unix_ms: id },
        &Message::new(tags::SYSTEM, Severity::Info, title),
    ))
}

fn retired(id: u64) -> LogLine {
    LogLine::Retired {
        id: MessageId::from_raw(id).unwrap(),
        how: Retired::Folded,
        unix_ms: id + 1,
        title: format!("m{id}"),
        detail: Vec::new(),
        repeats: 1,
        mark: None,
    }
}

fn write_lines(path: &Path, lines: &[String]) {
    use std::io::Write as _;
    let mut f = std::fs::File::create(path).unwrap();
    for l in lines {
        f.write_all(l.as_bytes()).unwrap();
    }
}

fn read_lines(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect()
}

#[test]
fn load_reads_only_the_tail_and_skips_junk_lines() {
    let dir = scratch("tail");
    let path = dir.join(FILE_NAME);
    let mut lines: Vec<String> = (1..=40)
        .map(|i| encoded(&posted(i, &format!("m{i}"))))
        .collect();
    lines.insert(20, "this is not a record\n".into());
    lines.insert(30, "m1\tkind=posted\tid=99\n".into());
    write_lines(&path, &lines);
    let whole = load_tail(&path, u64::MAX);
    assert_eq!(whole.log.len(), 40, "every record from an uncut file");
    assert_eq!(whole.skipped, 2, "the two junk lines");
    assert_eq!(
        whole.log.next_id(),
        MessageId::from_raw(41).unwrap(),
        "ids continue past the loaded ones"
    );
    let last_ten: usize = lines[lines.len() - 10..].iter().map(String::len).sum();
    let cut = load_tail(&path, last_ten as u64 + 7);
    assert_eq!(cut.log.len(), 10, "the partial first line is not a record");
    let ids: Vec<u64> = cut.log.records().map(|r| r.id.raw()).collect();
    assert_eq!(ids, (31..=40).collect::<Vec<_>>());
    assert!(load_tail(&dir.join("absent"), TAIL_BYTES).log.is_empty());
    assert!(
        load_tail(&dir, TAIL_BYTES).log.is_empty(),
        "no-follow, no dirs"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn a_symlinked_log_is_refused() {
    let dir = scratch("symlink");
    let real = dir.join("real.log");
    write_lines(&real, &[encoded(&posted(1, "m1"))]);
    let link = dir.join(FILE_NAME);
    std::os::unix::fs::symlink(&real, &link).unwrap();
    assert!(load_tail(&link, TAIL_BYTES).log.is_empty());
    assert_eq!(load_tail(&real, TAIL_BYTES).log.len(), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn append_never_drops_and_keeps_the_engines_order_past_the_channels_depth() {
    let dir = scratch("append");
    let path = dir.join(FILE_NAME);
    let (writer, rx) = Writer::unattended(&path, 2);
    drop(rx);
    let lines: Vec<LogLine> = (1..=5).map(|i| posted(i, &format!("m{i}"))).collect();
    writer.append(&host(&lines));
    let on_disk = read_lines(&path);
    assert_eq!(on_disk.len(), 5, "written by the caller for a dead thread");
    for (line, id) in on_disk.iter().zip(1..=5u64) {
        assert_eq!(
            LogLine::decode(line.trim_end())
                .unwrap()
                .id()
                .unwrap()
                .raw(),
            id
        );
    }
    drop(writer);
    let path = dir.join("threaded.log");
    let writer = Writer::spawn(&path).expect("a writer over a temp file");
    let many: Vec<LogLine> = (1..=(QUEUE_CAP as u64 + 50))
        .map(|i| posted(i, &format!("m{i}")))
        .collect();
    writer.append(&host(&many));
    writer.flush();
    let flushed = read_lines(&path);
    assert_eq!(flushed.len(), QUEUE_CAP + 50, "flushed before the drop");
    drop(writer);
    let on_disk = read_lines(&path);
    assert_eq!(on_disk.len(), QUEUE_CAP + 50, "nothing dropped");
    let ids: Vec<u64> = on_disk
        .iter()
        .map(|l| LogLine::decode(l.trim_end()).unwrap().id().unwrap().raw())
        .collect();
    assert_eq!(
        ids,
        (1..=(QUEUE_CAP as u64 + 50)).collect::<Vec<_>>(),
        "the file keeps the engine's order"
    );
    let reloaded = load_tail(&path, TAIL_BYTES);
    assert_eq!(reloaded.skipped, 0);
    assert_eq!(reloaded.log.len(), LOG_CAP.min(QUEUE_CAP + 50));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "created owner-only");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// A WRITER THAT FALLS BEHIND NEVER HOLDS THE CALLER: with its thread
/// paused mid-write (the files lock held, as a disk that stopped
/// answering would), four queues' worth of lines are appended at once and
/// a bounded flush answers `false` at its deadline; once the thread runs
/// again every line lands, none lost and in the order appended.
#[test]
fn a_paused_writer_holds_no_caller_and_lands_every_line_in_order_after() {
    let dir = scratch("paused");
    let path = dir.join(FILE_NAME);
    let writer = Writer::spawn(&path).expect("a writer over a temp file");
    let n = 2000u64;
    let lines: Vec<LogLine> = (1..=n).map(|i| posted(i, &format!("m{i}"))).collect();
    let mut paused = Some(writer.files.lock().unwrap());
    let (writer_ref, lines_ref) = (&writer, &lines);
    std::thread::scope(|scope| {
        let (appended_tx, appended) = std::sync::mpsc::channel();
        scope.spawn(move || {
            writer_ref.append(&host(lines_ref));
            let _ = appended_tx.send(());
        });
        if appended
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .is_err()
        {
            drop(paused.take());
            panic!("the append held its caller while the thread was paused");
        }
    });
    assert!(
        !writer.flush_within(std::time::Duration::from_millis(20)),
        "a thread that cannot write misses the bound"
    );
    drop(paused);
    writer.flush();
    let ids: Vec<u64> = read_lines(&path)
        .iter()
        .map(|l| LogLine::decode(l).unwrap().id().unwrap().raw())
        .collect();
    assert_eq!(ids, (1..=n).collect::<Vec<_>>(), "every line, in order");
    drop(writer);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Once a paused disk resumes, the writer lands every accepted line even
/// if the UI has gone idle.
#[test]
fn a_recovered_writer_drains_overflow_without_another_ui_call() {
    let dir = aterm_tempfile::tempdir().expect("a temp dir");
    let path = dir.path().join(FILE_NAME);
    let writer = Writer::spawn(&path).expect("a writer over a temp file");
    let n = (QUEUE_CAP * 4) as u64;
    let lines: Vec<LogLine> = (1..=n).map(|id| posted(id, &format!("m{id}"))).collect();
    let bytes: u64 = lines.iter().map(|line| encoded(line).len() as u64).sum();
    let paused = writer.files.lock().unwrap();
    writer.append(&host(&lines));
    assert!(
        writer.overflowed.load(Ordering::Relaxed) > 0,
        "the real writer could not consume the overflowing burst"
    );
    drop(paused);

    let deadline = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    loop {
        let landed = std::fs::metadata(&path).expect("the actual log").len();
        if landed >= bytes {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the writer stranded accepted lines after recovery: {landed}/{bytes} bytes"
        );
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    let ids: Vec<u64> = read_lines(&path)
        .iter()
        .map(|line| LogLine::decode(line).unwrap().id().unwrap().raw())
        .collect();
    assert_eq!(ids, (1..=n).collect::<Vec<_>>(), "every line, in order");
}

/// A budget small enough to rotate in a test: 4 KiB files, a look after
/// every write.
const TINY: crate::logging::RotationBudget = crate::logging::RotationBudget {
    rotate_at: 4096,
    check_bytes: 0,
    check_every: std::time::Duration::ZERO,
};

/// Three budgets' worth of appends leave the current file and one older copy, each
/// within the budget plus one look and one line.
#[test]
fn the_log_rotates_while_running_and_stays_bounded() {
    let dir = scratch("rotate");
    let path = dir.join(FILE_NAME);
    let writer = Writer::spawn_with(&path, TINY, TINY).expect("the writer");
    let mut id = 0;
    while (id as usize) * 60 < 3 * TINY.rotate_at as usize {
        id += 1;
        writer.append(&host(&[posted(id, &format!("m{id}"))]));
    }
    writer.flush();
    let bound = TINY.rotate_at + TINY.check_bytes + MAX_LINE_BYTES as u64;
    let current = std::fs::metadata(&path).unwrap().len();
    let older = std::fs::metadata(crate::logging::rotated_path(&path))
        .expect("rotated once at least")
        .len();
    assert!(current <= bound && older <= bound, "{current} / {older}");
    let mut second = crate::logging::rotated_path(&path).into_os_string();
    second.push(".1");
    assert!(!Path::new(&second).exists(), "one older copy, never two");
    let reloaded = load_tail(&path, TAIL_BYTES);
    assert_eq!(reloaded.skipped, 0);
    drop(writer);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The older copy's tail is read first: a `Posted` in `.1` meets its `Retired` in
/// the current file, and ids rise past the newest.
#[test]
fn a_launch_reads_across_the_rotation() {
    let dir = scratch("across");
    let path = dir.join(FILE_NAME);
    let n = 5;
    write_lines(
        &crate::logging::rotated_path(&path),
        &(1..=n)
            .map(|i| encoded(&posted(i, &format!("m{i}"))))
            .collect::<Vec<_>>(),
    );
    write_lines(
        &path,
        &[encoded(&retired(n)), encoded(&posted(n + 1, "new"))],
    );
    let loaded = load_tail(&path, TAIL_BYTES);
    assert_eq!(loaded.log.len(), usize::try_from(n + 1).unwrap());
    assert_eq!(loaded.skipped, 0);
    let closed = loaded.log.get(MessageId::from_raw(n).unwrap()).unwrap();
    assert_eq!(
        closed.state,
        LogState::Retired(Retired::Folded),
        "its Retired, from the current file"
    );
    assert_eq!(loaded.log.next_id(), MessageId::from_raw(n + 2).unwrap());
    let current_len = std::fs::metadata(&path).unwrap().len();
    let only_current = load_tail(&path, current_len);
    assert_eq!(only_current.log.len(), 1, "the orphan Retired is ignored");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Posted/Retired pairs pushed past the channel's depth all read back `Folded`: a
/// `Retired` written ahead of its `Posted` would replay as `Stale`.
#[test]
fn a_full_channel_never_puts_a_retired_line_ahead_of_its_posted_line() {
    let dir = scratch("reorder");
    let path = dir.join(FILE_NAME);
    let writer = Writer::spawn(&path).expect("a writer over a temp file");
    let n = QUEUE_CAP as u64 + 20;
    let lines: Vec<LogLine> = (1..=n).flat_map(|i| [posted(i, "m"), retired(i)]).collect();
    writer.append(&host(&lines));
    drop(writer);
    let loaded = load_tail(&path, TAIL_BYTES);
    assert_eq!(loaded.skipped, 0, "every line decodes");
    for i in (n - LOG_CAP as u64 + 1)..=n {
        let rec = loaded.log.get(MessageId::from_raw(i).unwrap()).unwrap();
        assert_eq!(
            rec.state,
            LogState::Retired(Retired::Folded),
            "record {i} must read back as Folded, not as a dead process's open row"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Wire lines rotate in `messages.wire.log`, bounded; `messages.log` keeps its one
/// host record unrotated, and a launch reads it back beside the wire's newest.
#[test]
fn a_wire_flood_never_rotates_aterms_own_record_off_disk() {
    let dir = scratch("wire-shelf");
    let path = dir.join(FILE_NAME);
    let writer = Writer::spawn_with(&path, BUDGET, TINY).expect("the writer");
    assert!(
        !wire_path(&path).exists(),
        "no wire file until the wire writes"
    );
    writer.append(&[
        (posted(1, "crash record"), Shelf::Host),
        (retired(1), Shelf::Host),
    ]);
    let mut id = 1;
    while (id as usize) * 60 < 3 * TINY.rotate_at as usize {
        id += 1;
        writer.append(&[
            (wire_posted(id, &format!("w{id}")), Shelf::Wire),
            (retired(id), Shelf::Wire),
        ]);
    }
    writer.flush();
    let host_lines = read_lines(&path);
    assert_eq!(host_lines.len(), 2, "{host_lines:?}");
    assert!(
        !crate::logging::rotated_path(&path).exists(),
        "messages.log never rotated"
    );
    let wire = wire_path(&path);
    let bound = TINY.rotate_at + TINY.check_bytes + MAX_LINE_BYTES as u64;
    let current = std::fs::metadata(&wire).unwrap().len();
    let older = std::fs::metadata(crate::logging::rotated_path(&wire))
        .expect("the wire's file rotated")
        .len();
    assert!(current <= bound && older <= bound, "{current} / {older}");
    drop(writer);
    let loaded = load_tail(&path, TAIL_BYTES);
    assert_eq!(loaded.skipped, 0);
    let crash = loaded.log.get(MessageId::from_raw(1).unwrap());
    assert_eq!(
        crash.map(|r| &r.state),
        Some(&LogState::Retired(Retired::Folded)),
        "the host record reads back"
    );
    let newest = loaded.log.get(MessageId::from_raw(id).unwrap()).unwrap();
    assert!(newest.wire_owned());
    assert_eq!(newest.state, LogState::Retired(Retired::Folded));
    let _ = std::fs::remove_dir_all(&dir);
}

/// Records interleaved across the two files load oldest-first by id, each meeting
/// its own `Retired`; ids continue past the newest; the wire's tail is read at most
/// [`WIRE_TAIL_BYTES`].
#[test]
fn a_launch_merges_the_two_files_by_id() {
    let dir = scratch("wire-merge");
    let path = dir.join(FILE_NAME);
    write_lines(
        &path,
        &[
            encoded(&posted(1, "h1")),
            encoded(&posted(3, "h3")),
            encoded(&retired(1)),
            encoded(&posted(6, "h6")),
        ],
    );
    write_lines(
        &wire_path(&path),
        &[
            encoded(&wire_posted(2, "w2")),
            encoded(&wire_posted(4, "w4")),
            encoded(&retired(2)),
            encoded(&wire_posted(5, "w5")),
        ],
    );
    let loaded = load_tail(&path, TAIL_BYTES);
    let ids: Vec<u64> = loaded.log.records().map(|r| r.id.raw()).collect();
    assert_eq!(ids, vec![1, 2, 3, 4, 5, 6], "oldest first, by id");
    for (id, how) in [
        (1, Retired::Folded),
        (2, Retired::Folded),
        (4, Retired::Stale),
    ] {
        let rec = loaded.log.get(MessageId::from_raw(id).unwrap()).unwrap();
        assert_eq!(rec.state, LogState::Retired(how), "record {id}");
    }
    assert_eq!(loaded.log.next_id(), MessageId::from_raw(7).unwrap());
    let big: Vec<String> = (10..10 + 2 * WIRE_TAIL_BYTES / 64)
        .map(|i| encoded(&wire_posted(i, &format!("w{i}"))))
        .collect();
    write_lines(&wire_path(&path), &big);
    let loaded = load_tail(&path, TAIL_BYTES);
    let wire_bytes: u64 = loaded
        .log
        .records()
        .filter(|r| r.wire_owned())
        .map(|r| encoded(&LogLine::Posted(r.clone())).len() as u64)
        .sum();
    assert!(wire_bytes <= WIRE_TAIL_BYTES, "{wire_bytes}");
    assert!(loaded.log.get(MessageId::from_raw(6).unwrap()).is_some());
    let _ = std::fs::remove_dir_all(&dir);
}
