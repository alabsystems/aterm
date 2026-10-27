// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Unit tests for incremental transcript usage folding, rate-limit walls, and footer text.

use std::fs;
use std::io::Write as _;

use super::*;

struct TestDir(aterm_tempfile::TempDir);

impl TestDir {
    fn new(label: &str) -> Self {
        Self(
            aterm_tempfile::Builder::new()
                .prefix(&format!("aterm-session-usage-{label}-"))
                .tempdir()
                .expect("create the scratch root"),
        )
    }

    fn join(&self, name: &str) -> PathBuf {
        self.0.path().join(name)
    }
}

fn row(id: &str, model: &str, input: u64, output: u64) -> String {
    format!(
        "{{\"type\":\"assistant\",\"isSidechain\":false,\"message\":{{\"id\":\"{id}\",\"model\":\"{model}\",\"role\":\"assistant\",\"usage\":{{\"input_tokens\":{input},\"output_tokens\":{output},\"cache_creation_input_tokens\":10,\"cache_read_input_tokens\":1000}}}}}}\n"
    )
}

fn append(path: &Path, text: &str) {
    let mut f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .expect("open for append");
    f.write_all(text.as_bytes()).expect("append");
}

fn output_of(fold: &TranscriptUsage, model: &str) -> u64 {
    fold.per_model().get(model).map_or(0, |s| s.output)
}

const BIG: u64 = u64::MAX;

#[test]
fn a_reread_reads_only_the_appended_bytes() {
    let dir = TestDir::new("appended");
    let path = dir.join("s.jsonl");
    let first = format!(
        "{}{}",
        row("m1", "claude-opus-5-5", 100, 20),
        row("m2", "claude-opus-5-5", 5, 3)
    );
    append(&path, &first);
    let mut cursor = TranscriptCursor::new(&path);
    let a = cursor.advance(BIG).expect("reads");
    assert_eq!(a.read, first.len() as u64, "the first read is the file");
    assert!(a.at_end && !a.restarted);
    assert_eq!(output_of(cursor.fold(), "claude-opus-5-5"), 23);

    let more = row("m3", "claude-opus-5-5", 7, 11);
    append(&path, &more);
    let a = cursor.advance(BIG).expect("reads");
    assert_eq!(
        a.read,
        more.len() as u64 + 1,
        "the appended row and the boundary byte, nothing before them"
    );
    assert_eq!(output_of(cursor.fold(), "claude-opus-5-5"), 34);

    let a = cursor.advance(BIG).expect("reads");
    assert_eq!(a.read, 0, "an unchanged file is not read");
    assert!(a.at_end);
    assert_eq!(
        cursor.bytes_read(),
        first.len() as u64 + more.len() as u64 + 1,
        "the meter counts every byte read, over the cursor's life"
    );
    assert_eq!(cursor.offset(), (first.len() + more.len()) as u64);
}

#[cfg(unix)]
#[test]
fn an_unchanged_file_is_not_opened() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = TestDir::new("unopened");
    let path = dir.join("s.jsonl");
    append(&path, &row("m1", "claude-opus-5-5", 1, 5));
    let mut cursor = TranscriptCursor::new(&path);
    cursor.advance(BIG).expect("reads");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).expect("chmod");
    let unchanged = cursor.advance(BIG);
    append_keeping_mode(&path, &row("m2", "claude-opus-5-5", 1, 7));
    let changed = cursor.advance(BIG);
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("chmod");
    assert_eq!(unchanged.expect("not opened, so not refused").read, 0);
    assert!(changed.is_err(), "control: a changed file is opened");
}

#[cfg(unix)]
fn append_keeping_mode(path: &Path, text: &str) {
    use std::os::unix::fs::PermissionsExt as _;
    let mode = fs::metadata(path).expect("meta").permissions().mode();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).expect("chmod");
    append(path, text);
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).expect("chmod");
}

#[test]
fn no_line_buffer_is_kept_between_reads() {
    let dir = TestDir::new("buffer");
    let path = dir.join("s.jsonl");
    let long = format!(
        "{{\"type\":\"user\",\"text\":\"{}\"}}\n",
        "x".repeat(3 * READ_CHUNK)
    );
    append(
        &path,
        &format!("{long}{}", row("m1", "claude-opus-5-5", 1, 5)),
    );
    let mut cursor = TranscriptCursor::new(&path);
    cursor.advance(BIG).expect("reads");
    assert_eq!(output_of(cursor.fold(), "claude-opus-5-5"), 5);
    assert_eq!(cursor.pending.capacity(), 0, "nothing held after the read");
    let torn = "{\"type\":\"assistant\"";
    append(&path, torn);
    cursor.advance(BIG).expect("reads");
    assert_eq!(cursor.pending.len(), torn.len());
    assert!(
        cursor.pending.capacity() < 2 * torn.len() + 16,
        "a torn tail keeps its own bytes: {}",
        cursor.pending.capacity()
    );
}

#[test]
fn a_truncated_file_starts_the_fold_over() {
    let dir = TestDir::new("truncated");
    let path = dir.join("s.jsonl");
    append(
        &path,
        &format!(
            "{}{}{}",
            row("m1", "claude-opus-5-5", 1, 100),
            row("m2", "claude-opus-5-5", 1, 100),
            row("m3", "claude-opus-5-5", 1, 100)
        ),
    );
    let mut cursor = TranscriptCursor::new(&path);
    cursor.advance(BIG).expect("reads");
    assert_eq!(output_of(cursor.fold(), "claude-opus-5-5"), 300);
    fs::write(&path, row("m9", "claude-haiku-4-5", 1, 7)).expect("truncate and rewrite");
    let a = cursor.advance(BIG).expect("reads");
    assert!(a.restarted, "a shorter file is not the one folded");
    assert_eq!(cursor.restarts(), 1);
    assert_eq!(output_of(cursor.fold(), "claude-opus-5-5"), 0);
    assert_eq!(output_of(cursor.fold(), "claude-haiku-4-5"), 7);
}

#[cfg(unix)]
#[test]
fn a_rotated_file_starts_the_fold_over() {
    let dir = TestDir::new("rotated");
    let path = dir.join("s.jsonl");
    let old = row("m1", "claude-opus-5-5", 1, 5);
    append(&path, &old);
    let mut cursor = TranscriptCursor::new(&path);
    cursor.advance(BIG).expect("reads");
    let next = dir.join("next.jsonl");
    let first = row("n1", "claude-opus-5-5", 1, 7);
    assert_eq!(first.len(), old.len(), "the same boundary byte");
    append(
        &next,
        &format!("{first}{}", row("n2", "claude-opus-5-5", 1, 700)),
    );
    fs::rename(&next, &path).expect("rotate");
    let a = cursor.advance(BIG).expect("reads");
    assert!(a.restarted);
    assert_eq!(
        output_of(cursor.fold(), "claude-opus-5-5"),
        707,
        "the new file's rows, and none of the old one's"
    );
}

#[test]
fn a_file_rewritten_in_place_is_caught_by_the_boundary_byte() {
    let dir = TestDir::new("rewritten");
    let path = dir.join("s.jsonl");
    let first = row("m1", "claude-opus-5-5", 1, 5);
    append(&path, &first);
    let mut cursor = TranscriptCursor::new(&path);
    cursor.advance(BIG).expect("reads");
    let rewrite = format!(
        "{}{}",
        row("r1", "claude-sonnet-5", 1_000_000, 9),
        row("r2", "claude-sonnet-5", 1, 1)
    );
    assert_ne!(rewrite.as_bytes()[first.len() - 1], b'\n');
    let mut f = fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .expect("open in place");
    f.write_all(rewrite.as_bytes()).expect("overwrite");
    drop(f);
    let a = cursor.advance(BIG).expect("reads");
    assert!(a.restarted, "the byte the fold stopped after moved");
    assert_eq!(output_of(cursor.fold(), "claude-opus-5-5"), 0);
    assert_eq!(output_of(cursor.fold(), "claude-sonnet-5"), 10);
}

#[test]
fn a_budget_bounds_one_read_and_the_next_carries_on() {
    let dir = TestDir::new("budget");
    let path = dir.join("s.jsonl");
    let text: String = (0..20)
        .map(|i| row(&format!("m{i}"), "claude-opus-5-5", 3, i))
        .collect();
    append(&path, &text);
    let mut cursor = TranscriptCursor::new(&path);
    let a = cursor.advance(100).expect("reads");
    assert_eq!(a.read, 100);
    assert!(!a.at_end);
    let mut steps = 1;
    while !cursor.advance(100).expect("reads").at_end {
        steps += 1;
        assert!(steps < 1000, "it gets there");
    }
    assert_eq!(output_of(cursor.fold(), "claude-opus-5-5"), (0..20).sum());
    assert_eq!(cursor.fold().rows, 20);
}

#[test]
fn any_split_of_the_appends_folds_to_the_one_pass_result() {
    let dir = TestDir::new("splits");
    let text: String = (0..15)
        .map(|i| {
            let model = ["claude-opus-5-5", "claude-haiku-4-5", "claude-fable-5-1"][i % 3];
            let id = if i % 5 == 4 { i - 1 } else { i };
            let r = row(&format!("m{id}"), model, i as u64, (i * 7) as u64);
            if i % 7 == 0 {
                format!("{{\"type\":\"user\",\"text\":\"\u{00E9}\u{6F22}\u{1F600}\"}}\n{r}")
            } else {
                r
            }
        })
        .collect();
    let mut whole = TranscriptUsage::new();
    for line in text.split_terminator('\n') {
        whole.fold_line(line);
    }
    for step in [1usize, 3, 17, 64, 333, 4096] {
        let path = dir.join(&format!("s{step}.jsonl"));
        fs::write(&path, b"").expect("create");
        let mut cursor = TranscriptCursor::new(&path);
        let mut f = fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .expect("open");
        for chunk in text.as_bytes().chunks(step) {
            f.write_all(chunk).expect("append");
            let a = cursor.advance(BIG).expect("reads");
            assert!(!a.restarted, "step {step}: an append is never a restart");
        }
        assert_eq!(cursor.fold().per_model(), whole.per_model(), "step {step}");
        assert_eq!(cursor.fold().duplicates, whole.duplicates, "step {step}");
        assert_eq!(cursor.fold().rows, whole.rows, "step {step}");
    }
}

#[test]
fn an_oversize_line_across_reads_is_skipped_and_counted() {
    let dir = TestDir::new("oversize");
    let path = dir.join("s.jsonl");
    let mut cursor = TranscriptCursor::new(&path);
    append(&path, &row("m1", "claude-opus-5-5", 1, 1));
    let half = "x".repeat(MAX_LINE_BYTES / 2 + 1);
    append(&path, &half);
    cursor.advance(BIG).expect("reads");
    append(&path, &half);
    cursor.advance(BIG).expect("reads");
    assert!(cursor.pending.is_empty(), "the oversize line is never held");
    append(
        &path,
        &format!("tail\n{}", row("m2", "claude-opus-5-5", 1, 2)),
    );
    cursor.advance(BIG).expect("reads");
    assert_eq!(cursor.fold().rows_skipped_long, 1);
    assert_eq!(output_of(cursor.fold(), "claude-opus-5-5"), 3);
}

#[test]
fn a_line_that_is_not_utf8_is_counted_unparsed() {
    let dir = TestDir::new("not-utf8");
    let path = dir.join("s.jsonl");
    fs::write(&path, b"\xff\xfe not utf8\n").expect("write");
    append(&path, &row("m1", "claude-opus-5-5", 1, 2));
    let mut cursor = TranscriptCursor::new(&path);
    cursor.advance(BIG).expect("reads");
    assert_eq!(cursor.fold().rows, 2);
    assert_eq!(cursor.fold().rows_unparsed, 1);
    assert_eq!(output_of(cursor.fold(), "claude-opus-5-5"), 2);
}

#[cfg(unix)]
#[test]
fn a_fifo_is_not_folded() {
    let dir = TestDir::new("fifo");
    let fifo = dir.join("s.jsonl");
    let c = std::ffi::CString::new(fifo.to_string_lossy().as_bytes()).expect("path");
    // SAFETY: a NUL-terminated path this test owns; mkfifo only creates a node.
    assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
    let mut cursor = TranscriptCursor::new(&fifo);
    assert!(cursor.advance(BIG).is_err());
    assert_eq!(cursor.bytes_read(), 0);
}

#[test]
fn a_session_sums_its_subagent_transcripts_and_nothing_else() {
    let dir = TestDir::new("session");
    let main = dir.join("abc-123.jsonl");
    append(&main, &row("m1", "claude-opus-5-5", 10, 100));
    append(
        &dir.join("other-session.jsonl"),
        &row("o1", "claude-opus-5-5", 10, 99_999),
    );
    let subs = dir.join("abc-123").join("subagents");
    fs::create_dir_all(subs.join("workflows").join("run-1")).expect("dirs");
    append(
        &subs.join("agent-a1.jsonl"),
        &row("s1", "claude-haiku-4-5", 5, 40),
    );
    append(
        &subs.join("workflows").join("run-1").join("agent-w1.jsonl"),
        &row("s2", "claude-haiku-4-5", 5, 2),
    );
    append(&subs.join("notes.txt"), "not a transcript\n");
    append(
        &subs.join("workflows").join("run-1").join("journal.jsonl"),
        &row("j1", "claude-haiku-4-5", 5, 7_000),
    );
    let mut session = SessionUsage::new(&main);
    let r = session.refresh(BIG);
    assert!(r.caught_up && r.complete());
    assert_eq!(session.subagent_files(), 2, "two subagents");
    let total = session.total_fold();
    assert_eq!(total.per_model()["claude-opus-5-5"].output, 100);
    assert_eq!(total.per_model()["claude-haiku-4-5"].output, 42);
    let late = row("s3", "claude-haiku-4-5", 5, 1000);
    append(&subs.join("agent-a2.jsonl"), &late);
    let before = session.bytes_read();
    session.refresh(BIG);
    let total = session.total_fold();
    assert_eq!(total.per_model()["claude-haiku-4-5"].output, 1042);
    assert_eq!(
        session.bytes_read() - before,
        late.len() as u64,
        "the new file's bytes and nothing else"
    );
    assert_eq!(total.assistant_rows, 4);
}

#[test]
fn the_traffic_view_is_the_whole_sessions() {
    let dir = TestDir::new("traffic");
    let main = dir.join("abc-123.jsonl");
    let stamped =
        |text: String, at: &str| text.replacen('{', &format!("{{\"timestamp\":\"{at}\","), 1);
    append(
        &main,
        &stamped(
            row("m1", "claude-opus-5-5", 10, 100),
            "2026-10-19T12:00:05Z",
        ),
    );
    append(&main, &row("m2", "claude-opus-5-5", 1, 1));
    append(
        &main,
        "{\"isSidechain\":false,\"type\":\"system\",\"subtype\":\"turn_duration\",\"durationMs\":61500,\"timestamp\":\"2026-10-19T12:01:10.250Z\"}\n",
    );
    let subs = dir.join("abc-123").join("subagents");
    fs::create_dir_all(&subs).expect("dirs");
    append(
        &subs.join("agent-a1.jsonl"),
        &stamped(row("s1", "claude-haiku-4-5", 5, 40), "2026-10-19T12:02:30Z"),
    );
    let mut session = SessionUsage::new(&main);
    assert!(session.refresh(BIG).caught_up);
    let view = session.traffic();
    let minute = i64::try_from(
        crate::harness::upgrade_models::parse_utc("2026-10-19T12:00:00Z").expect("a stamp"),
    )
    .expect("fits");
    assert_eq!((view.start_s, view.width_s), (minute, 60));
    assert_eq!(&*view.buckets, &[(1_020, 100), (0, 0), (1_015, 40)]);
    assert_eq!(view.totals, (1_020 + 1_011 + 1_015, 141));
    assert_eq!(view.unread, 1, "m2 has no stamp");
    assert_eq!(&*view.busy, &[(minute + 8, minute + 71)]);
    assert_eq!(view.runtime_s, Some(61));
    let generation = session.traffic_generation();
    session.refresh(BIG);
    assert_eq!(session.traffic_generation(), generation, "nothing appended");
    assert_eq!(session.traffic(), view);
    append(
        &subs.join("agent-a1.jsonl"),
        &stamped(row("s2", "claude-haiku-4-5", 5, 2), "2026-10-19T12:02:40Z"),
    );
    session.refresh(BIG);
    assert_ne!(session.traffic_generation(), generation, "a subagent's row");
}

#[test]
fn a_fold_that_starts_over_moves_its_generation() {
    let dir = TestDir::new("generation");
    let path = dir.join("s.jsonl");
    append(&path, &row("m1", "claude-opus-5-5", 1, 100));
    let mut cursor = TranscriptCursor::new(&path);
    cursor.advance(BIG).expect("reads");
    let before = cursor.fold().generation();
    fs::write(&path, row("m9", "claude-haiku-4-5", 1, 7)).expect("truncate and rewrite");
    assert!(cursor.advance(BIG).expect("reads").restarted);
    assert_eq!(output_of(cursor.fold(), "claude-haiku-4-5"), 7);
    assert_ne!(cursor.fold().generation(), before);
}

#[test]
fn a_subagent_behind_keeps_the_session_from_catching_up() {
    let dir = TestDir::new("behind");
    let main = dir.join("abc-1.jsonl");
    let one = row("m1", "claude-opus-5-5", 1, 1);
    append(&main, &one);
    let subs = dir.join("abc-1").join("subagents");
    fs::create_dir_all(&subs).expect("dirs");
    let text: String = (0..50)
        .map(|i| row(&format!("s{i}"), "claude-haiku-4-5", 1, 1))
        .collect();
    append(&subs.join("agent-a.jsonl"), &text);
    let mut session = SessionUsage::new(&main);
    let r = session.refresh(one.len() as u64 + 10);
    assert!(
        session.main().offset() == one.len() as u64,
        "the transcript is read whole"
    );
    assert!(!r.caught_up, "the subagent is not: a prefix");
    assert!(!r.complete());
    let r = session.refresh(10);
    assert!(!r.caught_up, "a HELD cursor still behind is still a prefix");
    let r = session.refresh(BIG);
    assert!(r.caught_up && r.complete());
    assert_eq!(
        session.total_fold().per_model()["claude-haiku-4-5"].output,
        50
    );
}

#[test]
fn a_finished_subagent_keeps_its_totals_and_grows_on() {
    let dir = TestDir::new("finished");
    let main = dir.join("abc-5.jsonl");
    append(&main, &row("m1", "claude-opus-5-5", 1, 1));
    let subs = dir.join("abc-5").join("subagents");
    fs::create_dir_all(&subs).expect("dirs");
    let agent = subs.join("agent-a.jsonl");
    let text: String = (0..40)
        .map(|i| row(&format!("s{i}"), "claude-haiku-4-5", 1, 10))
        .collect();
    append(&agent, &text);
    let mut session = SessionUsage::new(&main);
    assert!(session.refresh(BIG).complete());
    let held = &session.subagents[&agent];
    assert!(
        held.fold().seen_ids() <= FINISHED_IDS_KEPT,
        "a finished file drops its dedupe set: {}",
        held.fold().seen_ids()
    );
    assert_eq!(held.pending.capacity(), 0);
    let before = session.total_fold().per_model()["claude-haiku-4-5"].output;
    assert_eq!(before, 400);
    append(
        &agent,
        &format!(
            "{}{}",
            row("s39", "claude-haiku-4-5", 1, 25),
            row("s40", "claude-haiku-4-5", 1, 5000)
        ),
    );
    let r = session.refresh(BIG);
    assert!(r.complete(), "{r:?}");
    assert_eq!(
        session.total_fold().per_model()["claude-haiku-4-5"].output,
        400 + 15 + 5000,
        "carried, raised, and the new message counted once"
    );
    assert!(
        session.subagents[&agent].fold().seen_ids() <= FINISHED_IDS_KEPT,
        "finished again: its set dropped again"
    );
}

fn set_age(path: &Path, secs_ago: u64) {
    let f = fs::OpenOptions::new()
        .append(true)
        .open(path)
        .expect("open");
    f.set_modified(SystemTime::now() - std::time::Duration::from_secs(secs_ago))
        .expect("mtime");
}

#[test]
fn at_the_bound_new_files_are_unread() {
    let dir = TestDir::new("bound");
    let main = dir.join("abc-6.jsonl");
    append(&main, &row("m1", "claude-opus-5-5", 1, 1));
    let subs = dir.join("abc-6").join("subagents");
    fs::create_dir_all(&subs).expect("dirs");
    for (name, age, out) in [
        ("agent-old", 900, 1),
        ("agent-mid", 60, 20),
        ("agent-new", 5, 300),
    ] {
        let path = subs.join(format!("{name}.jsonl"));
        append(&path, &row(name, "claude-haiku-4-5", 1, out));
        set_age(&path, age);
    }
    let mut session = SessionUsage::new(&main);
    session.cap = 2;
    let r = session.refresh(BIG);
    assert_eq!((session.subagent_files(), r.unread), (2, 1), "{r:?}");
    assert!(r.caught_up && !r.complete(), "{r:?}");
    assert!(
        !session
            .subagents
            .contains_key(&subs.join("agent-old.jsonl")),
        "the newest are followed"
    );
    assert_eq!(
        session.total_fold().per_model()["claude-haiku-4-5"].output,
        320,
        "the unread file is not in the totals"
    );
    let later = subs.join("agent-later.jsonl");
    append(&later, &row("agent-later", "claude-haiku-4-5", 1, 4000));
    append(
        &subs.join("agent-new.jsonl"),
        &row("more", "claude-haiku-4-5", 1, 50_000),
    );
    let r = session.refresh(BIG);
    assert_eq!(r.unread, 2, "past the bound every new file: {r:?}");
    assert!(!session.subagents.contains_key(&later));
    assert_eq!(
        session.total_fold().per_model()["claude-haiku-4-5"].output,
        50_320,
        "a followed file grows on"
    );
    let text = usage_text(
        &UsageFacts::of(
            model_tokens(session.total_fold().per_model()),
            None,
            Some(&r),
        )
        .expect("tokens"),
    )
    .expect("text");
    assert!(text.ends_with("+2 unread"), "{text}");
}

#[test]
fn a_walk_cut_short_is_said_on_the_glass() {
    let dir = TestDir::new("walk");
    let main = dir.join("abc-7.jsonl");
    append(&main, &row("m1", "claude-opus-5-5", 1, 1));
    let subs = dir.join("abc-7").join("subagents");
    fs::create_dir_all(&subs).expect("dirs");
    for i in 0..6 {
        append(
            &subs.join(format!("agent-{i}.jsonl")),
            &row(&format!("s{i}"), "claude-haiku-4-5", 1, 1),
        );
    }
    let text_of = |session: &SessionUsage, r: &Refresh| {
        let facts = UsageFacts::of(
            model_tokens(session.total_fold().per_model()),
            None,
            Some(r),
        )
        .expect("tokens");
        usage_text(&facts).expect("text")
    };
    let mut session = SessionUsage::new(&main);
    session.walk_max = 3;
    let r = session.refresh(BIG);
    assert!(r.walk_cut && r.caught_up && !r.complete(), "{r:?}");
    let text = text_of(&session, &r);
    assert!(text.ends_with("+? unread"), "{text}");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mut locked = SessionUsage::new(&main);
        for i in 0..6 {
            fs::set_permissions(
                subs.join(format!("agent-{i}.jsonl")),
                fs::Permissions::from_mode(0o000),
            )
            .expect("chmod");
        }
        locked.walk_max = 3;
        let r = locked.refresh(BIG);
        for i in 0..6 {
            fs::set_permissions(
                subs.join(format!("agent-{i}.jsonl")),
                fs::Permissions::from_mode(0o600),
            )
            .expect("chmod");
        }
        assert!(r.unread > 0 && r.walk_cut, "{r:?}");
        let text = text_of(&locked, &r);
        assert!(
            text.ends_with(&format!("+{}+ unread", r.unread)),
            "{text} {r:?}"
        );
    }
    let mut whole = SessionUsage::new(&main);
    assert!(
        !whole.refresh(BIG).walk_cut,
        "control: the full walk is whole"
    );
}

#[test]
fn a_racy_directory_is_listed_again_and_a_settled_one_is_not() {
    let walked = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000);
    let stamp = |secs_before: u64| Stamp {
        len: 4096,
        mtime: Some(walked - std::time::Duration::from_secs(secs_before)),
        id: None,
    };
    let path = PathBuf::from("/s/subagents");
    let racy = DirSeen::at(path.clone(), Some(stamp(1)), 3, walked);
    assert_eq!(racy.racy, Some(3), "changed a second before the walk");
    let settled = DirSeen::at(path.clone(), Some(stamp(3)), 3, walked);
    assert_eq!(settled.racy, None, "changed three seconds before the walk");
    let later = Stamp {
        mtime: Some(walked + std::time::Duration::from_millis(5)),
        ..stamp(0)
    };
    assert_eq!(
        DirSeen::at(path.clone(), Some(later), 3, walked).racy,
        Some(3),
        "after it"
    );
    let timeless = Stamp {
        mtime: None,
        ..stamp(0)
    };
    assert_eq!(
        DirSeen::at(path.clone(), Some(timeless), 3, walked).racy,
        Some(3),
        "no time"
    );
    assert_eq!(DirSeen::at(path, None, 0, walked).racy, None, "not there");

    assert!(
        dir_moved(&racy, racy.stamp, || Some(4)),
        "a new entry in the walk's tick"
    );
    assert!(
        !dir_moved(&racy, racy.stamp, || Some(3)),
        "the walk's own entries"
    );
    assert!(dir_moved(&racy, racy.stamp, || None), "gone since");
    assert!(
        !dir_moved(&settled, settled.stamp, || panic!(
            "a settled stamp is never listed"
        )),
        "settled"
    );
    assert!(
        dir_moved(&settled, Some(stamp(0)), || None),
        "a stamp that moved"
    );
}

#[test]
fn moved_sees_subagent_growth_and_an_unfinished_fold() {
    let dir = TestDir::new("moved");
    let main = dir.join("abc-8.jsonl");
    append(&main, &row("m1", "claude-opus-5-5", 1, 1));
    let subs = dir.join("abc-8").join("subagents");
    fs::create_dir_all(&subs).expect("dirs");
    let running = subs.join("agent-r.jsonl");
    let text: String = (0..30)
        .map(|i| row(&format!("s{i}"), "claude-haiku-4-5", 1, 1))
        .collect();
    append(&running, &text);
    let mut session = SessionUsage::new(&main);
    session.refresh(fs::metadata(&main).expect("meta").len() + 10);
    assert!(session.moved(), "behind, with progress to make");
    session.refresh(0);
    assert!(
        !session.moved(),
        "behind, but the last read moved nothing and no file changed"
    );
    session.refresh(BIG);
    assert!(!session.moved(), "caught up, nothing moved");
    append(&running, &row("s30", "claude-haiku-4-5", 1, 1));
    assert!(session.moved(), "a running subagent grew");
    session.refresh(BIG);
    assert!(!session.moved());
    append(
        &subs.join("agent-new.jsonl"),
        &row("n", "claude-haiku-4-5", 1, 1),
    );
    assert!(session.moved(), "a new subagent appeared");
    session.refresh(BIG);
    append(&main, &row("m2", "claude-opus-5-5", 1, 1));
    assert!(session.moved(), "the transcript grew");
}

#[test]
fn moved_watches_every_followed_file() {
    let dir = TestDir::new("recent");
    let main = dir.join("abc-r.jsonl");
    append(&main, &row("m1", "claude-opus-5-5", 1, 1));
    let subs = dir.join("abc-r").join("subagents");
    fs::create_dir_all(&subs).expect("dirs");
    let agent = |i: u64| subs.join(format!("agent-{i:02}.jsonl"));
    for i in 0..10u64 {
        append(&agent(i), &row(&format!("s{i}"), "claude-haiku-4-5", 1, 1));
        set_age(&agent(i), 1000 - i * 100);
    }
    let mut session = SessionUsage::new(&main);
    session.refresh(BIG);
    assert!(!session.moved());
    append(&agent(9), &row("s9b", "claude-haiku-4-5", 1, 1));
    assert!(session.moved(), "the newest, running subagent grew");
    session.refresh(BIG);
    append(&agent(0), &row("s0b", "claude-haiku-4-5", 1, 50_000));
    assert!(
        session.moved(),
        "the least recently written agent's final message is seen"
    );
    session.refresh(BIG);
    assert_eq!(
        session.total_fold().per_model()["claude-haiku-4-5"].output,
        10 + 1 + 50_000
    );
    assert!(!session.moved());
    fs::remove_file(agent(5)).expect("remove");
    session.refresh(BIG);
    assert!(!session.moved(), "a followed file gone has nothing to read");
}

#[test]
fn a_spent_budget_opens_no_more_files() {
    let dir = TestDir::new("spent");
    let main = dir.join("abc-s.jsonl");
    append(&main, &row("m1", "claude-opus-5-5", 1, 1));
    let subs = dir.join("abc-s").join("subagents");
    fs::create_dir_all(&subs).expect("dirs");
    let text: String = (0..30)
        .map(|i| row(&format!("s{i}"), "claude-haiku-4-5", 1, 1))
        .collect();
    append(&subs.join("agent-a.jsonl"), &text);
    let mut session = SessionUsage::new(&main);
    session.refresh(fs::metadata(&main).expect("meta").len() + 50);
    let big: String = (0..10)
        .map(|i| row(&format!("m{i}x"), "claude-opus-5-5", 1, 1))
        .collect();
    append(&main, &big);
    let fresh = subs.join("agent-y.jsonl");
    append(&fresh, &row("y", "claude-haiku-4-5", 1, 7000));
    #[cfg(unix)]
    let locked = {
        use std::os::unix::fs::PermissionsExt as _;
        let locked = subs.join("agent-z.jsonl");
        append(&locked, &row("z", "claude-haiku-4-5", 1, 1));
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).expect("chmod");
        locked
    };
    let before = session.bytes_read();
    let r = session.refresh(100);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o600)).expect("chmod");
        assert_eq!(r.unread, 0, "the new file was not opened: {r:?}");
    }
    assert!(!r.caught_up);
    assert_eq!(r.spent, 100, "the whole budget");
    assert_eq!(
        session.bytes_read() - before,
        1 + 100,
        "the transcript's boundary byte and the budget — not one byte (nor an \
         open) of the subagent left behind"
    );
    assert!(!session.subagents.contains_key(&fresh), "not admitted");
    assert!(session.moved(), "behind, with progress to make");
    let r = session.refresh(BIG);
    assert!(r.complete(), "{r:?}");
    assert_eq!(
        session.total_fold().per_model()["claude-haiku-4-5"].output,
        30 + 7000 + u64::from(cfg!(unix)),
        "admitted once there was budget to open it"
    );
}

#[cfg(unix)]
#[test]
fn an_unreadable_subagent_is_unread_and_nothing_else() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = TestDir::new("locked");
    let main = dir.join("abc-l.jsonl");
    append(&main, &row("m1", "claude-opus-5-5", 1, 1));
    let subs = dir.join("abc-l").join("subagents");
    fs::create_dir_all(&subs).expect("dirs");
    append(
        &subs.join("agent-a.jsonl"),
        &row("a", "claude-haiku-4-5", 1, 10),
    );
    let locked = subs.join("agent-b.jsonl");
    append(&locked, &row("b", "claude-haiku-4-5", 1, 20));
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).expect("chmod");
    let mut session = SessionUsage::new(&main);
    session.cap = 1;
    set_age(&subs.join("agent-a.jsonl"), 100);
    let r = session.refresh(BIG);
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o600)).expect("chmod");
    assert_eq!((session.subagent_files(), r.unread), (1, 1), "{r:?}");
    assert!(!session.subagents.contains_key(&locked), "not followed");
    assert_eq!(
        session.total_fold().per_model()["claude-haiku-4-5"].output,
        10,
        "the readable one is counted"
    );
    session.cap = MAX_SUBAGENT_FILES;
    let r = session.refresh(BIG);
    assert_eq!((session.subagent_files(), r.unread), (2, 0), "{r:?}");
    assert_eq!(
        session.total_fold().per_model()["claude-haiku-4-5"].output,
        30
    );
    append(&locked, &row("b2", "claude-haiku-4-5", 1, 5));
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).expect("chmod");
    let r = session.refresh(BIG);
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o600)).expect("chmod");
    assert_eq!((session.subagent_files(), r.unread), (1, 1), "{r:?}");
}

#[test]
fn a_quiet_subagent_keeps_only_its_totals() {
    let dir = TestDir::new("quiet");
    let main = dir.join("abc-q.jsonl");
    append(&main, &row("m1", "claude-opus-5-5", 1, 1));
    let subs = dir.join("abc-q").join("subagents");
    fs::create_dir_all(&subs).expect("dirs");
    let rows: String = (0..5)
        .map(|i| row(&format!("s{i}"), "claude-haiku-4-5", 1, 1))
        .collect();
    let quiet = subs.join("agent-quiet.jsonl");
    let live = subs.join("agent-live.jsonl");
    append(&quiet, &rows);
    append(&live, &rows.replace("\"s", "\"t"));
    set_age(&quiet, QUIET_AFTER.as_secs() + 60);
    let mut session = SessionUsage::new(&main);
    session.refresh(BIG);
    session.refresh(BIG);
    assert_eq!(
        session.subagents[&quiet].fold().seen_ids(),
        0,
        "quiet: totals only"
    );
    assert!(
        session.subagents[&live].fold().seen_ids() > 0,
        "may still grow"
    );
    assert_eq!(
        session.total_fold().per_model()["claude-haiku-4-5"].output,
        10
    );
}

fn date_dir_back(dir: &Path) {
    let mut open = fs::OpenOptions::new();
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        open.access_mode(0x0100).custom_flags(0x0200_0000);
    }
    #[cfg(not(windows))]
    open.read(true);
    let f = open.open(dir).expect("open the directory");
    f.set_modified(SystemTime::now() - std::time::Duration::from_secs(3600))
        .expect("mtime");
}

#[test]
fn moved_sees_a_new_file_at_any_depth() {
    let dir = TestDir::new("depth");
    let main = dir.join("abc-d.jsonl");
    append(&main, &row("m1", "claude-opus-5-5", 1, 1));
    let subs = dir.join("abc-d").join("subagents");
    let mut session = SessionUsage::new(&main);
    assert!(session.refresh(BIG).complete());
    assert!(!session.moved());
    let run = subs.join("workflows").join("run-1");
    fs::create_dir_all(&run).expect("dirs");
    assert!(session.moved(), "subagents/ appeared");
    date_dir_back(&run);
    session.refresh(BIG);
    assert!(!session.moved());
    append(
        &run.join("agent-w1.jsonl"),
        &row("w1", "claude-haiku-4-5", 1, 5),
    );
    assert!(session.moved(), "a file in a run directory");
    session.refresh(BIG);
    assert!(!session.moved());
    for i in 2..10 {
        append(
            &run.join(format!("agent-w{i}.jsonl")),
            &row(&format!("w{i}"), "claude-haiku-4-5", 1, 5),
        );
    }
    date_dir_back(&run);
    session.refresh(BIG);
    assert!(!session.moved());
    append(
        &run.join("agent-w10.jsonl"),
        &row("w10", "claude-haiku-4-5", 1, 5),
    );
    assert!(session.moved(), "one more, three levels down");
    session.refresh(BIG);
    assert_eq!(
        session.total_fold().per_model()["claude-haiku-4-5"].output,
        50
    );
}

fn rewrite_in_place(path: &Path, text: &str) {
    let mut f = fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(path)
        .expect("open");
    f.write_all(text.as_bytes()).expect("write");
}

#[test]
fn a_released_file_changed_but_appended_is_unread() {
    let dir = TestDir::new("released");
    let main = dir.join("abc-x.jsonl");
    append(&main, &row("m1", "claude-opus-5-5", 1, 1));
    let subs = dir.join("abc-x").join("subagents");
    fs::create_dir_all(&subs).expect("dirs");
    let body: String = (0..5)
        .map(|i| row(&format!("q{i}"), "claude-haiku-4-5", 1, 10))
        .collect();
    let quiet = |name: &str| {
        let path = subs.join(format!("agent-{name}.jsonl"));
        append(&path, &body.replace("\"q", &format!("\"{name}")));
        set_age(&path, QUIET_AFTER.as_secs() + 60);
        path
    };
    let (grown, shrunk, replaced, rewritten) = (quiet("g"), quiet("s"), quiet("r"), quiet("w"));
    let mut session = SessionUsage::new(&main);
    session.refresh(BIG);
    session.refresh(BIG);
    assert!(session.subagents.values().all(|c| c.released));
    let haiku = |s: &SessionUsage| s.total_fold().per_model()["claude-haiku-4-5"].output;
    assert_eq!(haiku(&session), 200);

    append(&grown, &row("g-more", "claude-haiku-4-5", 1, 1000));
    let r = session.refresh(BIG);
    assert!(r.complete(), "an append is read on: {r:?}");
    assert_eq!(haiku(&session), 1200);

    rewrite_in_place(&shrunk, &row("s-new", "claude-haiku-4-5", 1, 7));
    let tmp = subs.join("agent-r.tmp");
    fs::write(
        &tmp,
        format!("{body}{}", row("r-new", "claude-haiku-4-5", 1, 7)),
    )
    .expect("tmp");
    fs::rename(&tmp, &replaced).expect("replace");
    rewrite_in_place(
        &rewritten,
        &format!(
            "{}{}",
            row("w-first", "claude-haiku-4-5", 1, 7),
            body.replace("\"q", "\"w")
        ),
    );
    let before = session.bytes_read();
    let r = session.refresh(BIG);
    assert_eq!(r.unread, 3, "{r:?}");
    assert_eq!(haiku(&session), 1200, "earlier totals stand, nothing added");
    assert_eq!(
        session.bytes_read() - before,
        1,
        "not re-read: the rewritten file's boundary byte alone"
    );
    assert_eq!(session.subagent_files(), 1, "only the grown one is whole");
    let before = session.bytes_read();
    let r = session.refresh(BIG);
    assert_eq!(r.unread, 3, "still unread on the next read: {r:?}");
    assert_eq!(haiku(&session), 1200);
    assert_eq!(session.bytes_read(), before, "and not opened again");

    let live = subs.join("agent-live.jsonl");
    append(&live, &row("l1", "claude-haiku-4-5", 1, 3));
    session.refresh(BIG);
    rewrite_in_place(&live, &row("x", "claude-haiku-4-5", 1, 4));
    let r = session.refresh(BIG);
    assert_eq!(r.unread, 3, "{r:?}");
    assert_eq!(session.subagents[&live].restarts(), 1);
    assert_eq!(haiku(&session), 1204);
}

#[test]
fn a_quiet_file_behind_is_not_released() {
    let dir = TestDir::new("behind-quiet");
    let main = dir.join("abc-p.jsonl");
    let main_row = row("m1", "claude-opus-5-5", 1, 1);
    append(&main, &main_row);
    let subs = dir.join("abc-p").join("subagents");
    fs::create_dir_all(&subs).expect("dirs");
    let agent = subs.join("agent-p.jsonl");
    let (r1, r2) = (
        row("s1", "claude-haiku-4-5", 1, 8),
        row("s1", "claude-haiku-4-5", 1, 349),
    );
    append(&agent, &format!("{r1}{r2}"));
    set_age(&agent, QUIET_AFTER.as_secs() + 60);
    let mut session = SessionUsage::new(&main);
    assert!(
        !session
            .refresh((main_row.len() + r1.len()) as u64)
            .caught_up
    );
    assert!(!session.refresh(2).caught_up);
    assert!(
        !session.subagents[&agent].released,
        "still behind: not released"
    );
    assert!(session.refresh(BIG).complete());
    assert_eq!(
        session.total_fold().per_model()["claude-haiku-4-5"].output,
        349,
        "one streamed message, at its largest count"
    );
}

#[test]
fn a_missing_transcript_is_not_caught_up() {
    let dir = TestDir::new("missing");
    let mut session = SessionUsage::new(dir.join("nope.jsonl"));
    let r = session.refresh(BIG);
    assert!(!r.caught_up);
    assert!(session.total_fold().per_model().is_empty());
}

fn notices(rows: &[(&str, Option<&str>, i64)]) -> BTreeMap<String, LimitNoticeRow> {
    rows.iter()
        .map(|(key, reset, at)| {
            (
                (*key).to_owned(),
                LimitNoticeRow {
                    reset_text: reset.map(str::to_owned),
                    at: *at,
                },
            )
        })
        .collect()
}

fn utc(_: Option<&str>, _: i64) -> Option<i64> {
    Some(0)
}

const DAY: i64 = 1_790_000_000 - 1_790_000_000 % 86_400;

#[test]
fn the_wall_stands_until_its_reset() {
    let two_pm = DAY + 14 * 3600;
    let rows = notices(&[("five_hour", Some("3pm (UTC)"), two_pm)]);
    let wall = wall_of(&rows, None, two_pm + 60, &utc);
    assert_eq!(
        wall,
        Some(Wall {
            label: "5h",
            resets: "3pm".into()
        })
    );
    assert!(wall_of(&rows, None, DAY + 15 * 3600 - 1, &utc).is_some());
    assert_eq!(wall_of(&rows, None, DAY + 15 * 3600, &utc), None, "reset");
    assert_eq!(
        wall_of(&rows, None, DAY + 86_400 + 11 * 3600, &utc),
        None,
        "read the next day: day 0's 3pm, long passed"
    );
    let both = notices(&[
        ("five_hour", Some("3pm (UTC)"), two_pm),
        ("seven_day", Some("Oct 1 at 9am (UTC)"), two_pm + 600),
    ]);
    assert_eq!(
        wall_of(&both, None, two_pm + 700, &utc),
        Some(Wall {
            label: "7d",
            resets: "Oct 1 at 9am".into()
        })
    );
}

#[test]
fn a_response_served_since_takes_the_wall_down() {
    let two_pm = DAY + 14 * 3600;
    let rows = notices(&[("five_hour", Some("3pm (UTC)"), two_pm)]);
    let at = two_pm + 1200;
    assert!(wall_of(&rows, None, at, &utc).is_some(), "control");
    assert_eq!(wall_of(&rows, Some(two_pm + 600), at, &utc), None);
    assert!(wall_of(&rows, Some(two_pm - 600), at, &utc).is_some());
    assert!(wall_of(&rows, Some(two_pm), at, &utc).is_some());
}

#[test]
fn an_unplaceable_wall_shows_nothing() {
    let at = DAY + 14 * 3600;
    let rows = notices(&[
        ("five_hour", Some("3pm (UTC)"), at),
        ("seven_day", Some("when it does"), at + 60),
    ]);
    assert_eq!(wall_of(&rows, None, at + 120, &utc), None);
    let bare = notices(&[("five_hour", None, at)]);
    assert_eq!(wall_of(&bare, None, at + 60, &utc), None);
    assert_eq!(wall_of(&BTreeMap::new(), None, at, &utc), None);
    let known = |zone: Option<&str>, _: i64| match zone {
        None | Some("UTC") => Some(0),
        Some(_) => None,
    };
    let mars = notices(&[("five_hour", Some("3pm (Mars/Olympus)"), at)]);
    assert_eq!(wall_of(&mars, None, at + 60, &known), None);
    let control = notices(&[("five_hour", Some("3pm (UTC)"), at)]);
    assert!(wall_of(&control, None, at + 60, &known).is_some());
}

#[test]
fn a_weekly_clock_time_is_its_next_occurrence() {
    let two_pm = DAY + 14 * 3600;
    let rows = notices(&[("seven_day", Some("9am (UTC)"), two_pm)]);
    assert!(
        wall_of(&rows, None, two_pm + 18 * 3600, &utc).is_some(),
        "walled until 9am tomorrow"
    );
    assert_eq!(wall_of(&rows, None, DAY + 86_400 + 9 * 3600, &utc), None);
    let session = notices(&[("five_hour", Some("9am (UTC)"), two_pm)]);
    assert_eq!(wall_of(&session, None, two_pm + 60, &utc), None);
    let dated = notices(&[("seven_day", Some("Sep 21 at 9am (UTC)"), two_pm)]);
    assert_eq!(wall_of(&dated, None, two_pm + 60, &utc), None);
}

#[test]
fn a_reset_in_the_next_year_is_placed() {
    let dec30 = 1_798_632_000;
    let jan2_9am = 1_798_880_400;
    let rows = notices(&[("seven_day", Some("Jan 2, 2027 at 9am (UTC)"), dec30)]);
    assert_eq!(
        wall_of(&rows, None, jan2_9am - 60, &utc).map(|w| w.resets),
        Some("Jan 2, 2027 at 9am".to_owned())
    );
    assert_eq!(wall_of(&rows, None, jan2_9am, &utc), None);
}

fn pacific(_: Option<&str>, t: i64) -> Option<i64> {
    Some(if t >= 1_772_964_000 {
        -7 * 3600
    } else {
        -8 * 3600
    })
}

#[test]
fn a_wall_across_a_dst_jump_stands_its_real_time() {
    let written = 1_772_951_400; // 2026-03-08T06:30Z
    let rows = notices(&[("five_hour", Some("4am (America/Los_Angeles)"), written)]);
    assert_eq!(
        wall_of(&rows, None, written + 1800, &pacific).map(|w| w.resets),
        Some("4am".to_owned())
    );
    assert!(wall_of(&rows, None, 1_772_967_599, &pacific).is_some());
    assert_eq!(wall_of(&rows, None, 1_772_967_600, &pacific), None);
}

#[test]
fn another_zones_clock_keeps_its_zone() {
    let one_pm_pdt = DAY + 20 * 3600;
    let rows = notices(&[("five_hour", Some("10pm (UTC)"), one_pm_pdt)]);
    let pdt = |zone: Option<&str>, _: i64| match zone {
        Some("UTC") => Some(0),
        _ => Some(-7 * 3600),
    };
    assert_eq!(
        wall_of(&rows, None, one_pm_pdt + 60, &pdt).map(|w| w.resets),
        Some("10pm (UTC)".to_owned())
    );
    assert_eq!(
        wall_of(&rows, None, one_pm_pdt + 60, &utc).map(|w| w.resets),
        Some("10pm".to_owned()),
        "control: UTC is the local zone"
    );
    let phoenix = notices(&[("five_hour", Some("3pm (America/Phoenix)"), one_pm_pdt)]);
    assert_eq!(
        wall_of(&phoenix, None, one_pm_pdt + 60, &pdt).map(|w| w.resets),
        Some("3pm".to_owned()),
        "the same offset at the reset: the local clock"
    );
    let unknown_local = |zone: Option<&str>, _: i64| zone.map(|_| 0);
    assert_eq!(
        wall_of(&rows, None, one_pm_pdt + 60, &unknown_local).map(|w| w.resets),
        Some("10pm (UTC)".to_owned())
    );
}

#[test]
fn a_span_is_said_as_the_local_clock() {
    let two_pm = DAY + 14 * 3600;
    let pdt = |_: Option<&str>, _: i64| Some(-7 * 3600);
    let rows = notices(&[("five_hour", Some("in 3h 30m"), two_pm)]);
    assert_eq!(
        wall_of(&rows, None, two_pm + 3600, &pdt).map(|w| w.resets),
        Some("10:30am".to_owned())
    );
    let weekly = notices(&[("seven_day", Some("in 72h"), two_pm)]);
    assert_eq!(
        wall_of(&weekly, None, two_pm + 60, &pdt).map(|w| w.resets),
        Some("Sep 24 at 7am".to_owned())
    );
    assert_eq!(
        wall_of(&rows, None, two_pm + 60, &|zone: Option<&str>, _: i64| zone
            .map(|_| 0)),
        None,
        "no local clock to say it on"
    );
}

fn spend(input: u64, output: u64, cache_write: u64, cache_read: u64) -> ModelSpend {
    ModelSpend {
        input,
        output,
        cache_write,
        cache_write_1h: 0,
        cache_read,
        messages: 1,
    }
}

#[test]
fn a_saturated_count_is_named_without_overflow() {
    let mut per = BTreeMap::new();
    per.insert(
        "claude-opus-5-5".to_owned(),
        spend(u64::MAX, 5, u64::MAX, 1),
    );
    per.insert("claude-haiku-4-5".to_owned(), spend(1, 1, 0, 0));
    let (models, _) = model_tokens(&per);
    assert_eq!(models[0].label, "opus");
    assert_eq!(models[0].input, u64::MAX);
    assert_eq!(models.len(), 2);
}

#[test]
fn token_counts_are_short_and_never_rounded_up() {
    for (n, want) in [
        (0, "0"),
        (940, "940"),
        (1_000, "1.0k"),
        (1_299, "1.2k"),
        (9_999, "9.9k"),
        (40_000, "40k"),
        (310_999, "310k"),
        (1_250_000, "1.2M"),
        (48_900_000, "48M"),
        (3_100_000_000, "3.1B"),
    ] {
        assert_eq!(tokens_short(n), want, "{n}");
    }
}

#[test]
fn models_are_named_short_and_ordered_by_tokens() {
    let mut per = BTreeMap::new();
    per.insert(
        "claude-opus-5-5".to_owned(),
        spend(1000, 310_000, 0, 48_000_000),
    );
    per.insert(
        "claude-haiku-4-5-20251001".to_owned(),
        spend(100, 20_000, 0, 1_000_000),
    );
    per.insert("<synthetic>".to_owned(), spend(0, 0, 0, 0));
    let (models, more) = model_tokens(&per);
    assert_eq!(more, More::default());
    assert_eq!(
        models,
        vec![
            ModelTokens {
                label: "opus".into(),
                input: 48_001_000,
                output: 310_000
            },
            ModelTokens {
                label: "haiku".into(),
                input: 1_000_100,
                output: 20_000
            },
        ]
    );
    per.insert("claude-opus-5".to_owned(), spend(5, 5, 0, 0));
    per.insert("claude-sonnet-5".to_owned(), spend(5, 1, 0, 0));
    let (models, more) = model_tokens(&per);
    let labels: Vec<&str> = models.iter().map(|m| m.label.as_str()).collect();
    assert_eq!(labels, ["opus5.5", "haiku", "opus5"], "{models:?}");
    assert_eq!(
        more,
        More {
            models: 1,
            input: 5,
            output: 1
        },
        "sonnet is gathered, not named"
    );
}

#[test]
fn a_routed_model_id_is_named_as_its_model() {
    let mut per = BTreeMap::new();
    per.insert(
        "us.anthropic.claude-opus-4-1-20250805-v1:0".to_owned(),
        spend(0, 40_000_000, 0, 40_000_000),
    );
    per.insert(
        "us.anthropic.claude-haiku-4-5-20251001-v1:0".to_owned(),
        spend(0, 5, 0, 5),
    );
    let (models, _) = model_tokens(&per);
    let labels: Vec<&str> = models.iter().map(|m| m.label.as_str()).collect();
    assert_eq!(labels, ["opus", "haiku"], "{models:?}");
    per.insert("claude-opus-4-1-20250805".to_owned(), spend(0, 1, 0, 0));
    per.insert("claude-sonnet-4-5@20250929".to_owned(), spend(0, 7, 0, 0));
    per.insert(
        "arn:aws:bedrock:us-east-1:1:inference-profile/global.anthropic.claude-opus-5-5-v1:0"
            .to_owned(),
        spend(0, 3, 0, 0),
    );
    let (models, more) = model_tokens(&per);
    let named: Vec<(&str, u64)> = models
        .iter()
        .map(|m| (m.label.as_str(), m.output))
        .collect();
    assert_eq!(
        named,
        [("opus4.1", 40_000_001), ("haiku", 5), ("sonnet", 7)],
        "one model by two routes summed; two opus versions told apart"
    );
    assert_eq!(more.models, 1, "opus5.5, gathered");
}

#[test]
fn usage_totals_cover_every_model() {
    let spends = [
        ("claude-opus-5-5", spend(1_001, 70_003, 20_005, 9_000_007)),
        ("claude-fable-5-1", spend(2_011, 50_013, 30_017, 4_000_019)),
        ("claude-sonnet-5", spend(3_023, 9_029, 0, 700_031)),
        ("claude-haiku-4-5-20251001", spend(4, 5, 6, 7)),
        ("claude-nimbus-1", spend(11, 13, 17, 19)),
    ];
    let per: BTreeMap<String, ModelSpend> = spends
        .iter()
        .map(|(id, s)| ((*id).to_owned(), *s))
        .collect();
    let (models, more) = model_tokens(&per);
    assert_eq!((models.len(), more.models), (NAMED_MODELS, 2));
    let input: u64 = spends
        .iter()
        .map(|(_, s)| s.input + s.cache_write + s.cache_read)
        .sum();
    let output: u64 = spends.iter().map(|(_, s)| s.output).sum();
    let whole = Refresh {
        caught_up: true,
        ..Refresh::default()
    };
    for (refresh, complete) in [
        (whole, true),
        (
            Refresh {
                caught_up: false,
                ..whole
            },
            false,
        ),
        (Refresh { unread: 1, ..whole }, false),
        (
            Refresh {
                walk_cut: true,
                ..whole
            },
            false,
        ),
    ] {
        let facts = UsageFacts::of((models.clone(), more), None, Some(&refresh)).expect("facts");
        assert_eq!((facts.input_total(), facts.output_total()), (input, output));
        assert_eq!(facts.complete, refresh.complete(), "{refresh:?}");
        assert_eq!(facts.complete, complete, "{refresh:?}");
    }
    let unfolded = UsageFacts::of((models, more), None, None).expect("facts");
    assert!(!unfolded.complete, "no refresh said the totals were whole");
}

#[test]
fn the_segment_says_runtime_and_tokens_at_each_grain() {
    let opus = ModelTokens {
        label: "opus".into(),
        input: 395_000_000,
        output: 2_000_000,
    };
    let haiku = ModelTokens {
        label: "haiku".into(),
        input: 12_000_000,
        output: 88_000,
    };
    let wall = || Wall {
        label: "5h",
        resets: "3pm".into(),
    };
    let one = UsageFacts::of((vec![opus.clone()], More::default()), None, None)
        .expect("tokens")
        .with_runtime(28_320_999);
    for grain in [UsageGrain::Total, UsageGrain::PerModel] {
        assert_eq!(
            usage_text_at(&one, grain).as_deref(),
            Some("7h52m \u{2191}395M \u{2193}2.0M"),
            "{grain:?}: one model is never named"
        );
    }
    let more = More {
        models: 1,
        input: 1_000_000,
        output: 12_000,
    };
    let several = UsageFacts::of((vec![opus, haiku], more), Some(wall()), None)
        .expect("facts")
        .with_runtime(67_560_000);
    assert_eq!(
        usage_text(&several).as_deref(),
        Some("18h46m opus \u{2191}395M \u{2193}2.0M \u{00B7} haiku \u{2191}12M \u{2193}88k +1"),
        "the wall is not in the usage text"
    );
    assert_eq!(
        usage_text_at(&several, UsageGrain::Total).as_deref(),
        Some("18h46m \u{2191}408M \u{2193}2.1M")
    );
    assert_eq!(
        usage_text_at(&several, UsageGrain::Runtime).as_deref(),
        Some("18h46m")
    );
    assert_eq!(
        usage_text_at(&several, UsageGrain::Rule).as_deref(),
        Some("18h46m"),
        "the rule's: the runtime, the tokens the hover's"
    );
    let unread = UsageFacts {
        unread: 3,
        ..several.clone()
    };
    assert_eq!(
        usage_text_at(&unread, UsageGrain::Rule).as_deref(),
        Some("18h46m +3 unread"),
        "an unread count stays"
    );
    let unrun = UsageFacts {
        runtime: None,
        ..several
    };
    assert_eq!(usage_text_at(&unrun, UsageGrain::Runtime), None);
    assert_eq!(
        usage_text_at(&unrun, UsageGrain::Rule).as_deref(),
        Some("\u{2191}408M \u{2193}2.1M"),
        "CONTROL: no runtime, the rule's `Σ` keeps the pair"
    );
    let walled = UsageFacts::of((Vec::new(), More::default()), Some(wall()), None)
        .expect("the wall alone")
        .with_runtime(999);
    assert_eq!(usage_text(&walled), None, "under a second, no runtime");
    assert_eq!(walled.clone().wall_only(), Some(walled));
    assert_eq!(
        UsageFacts::of((Vec::new(), More::default()), None, None),
        None
    );
    for (secs, text) in [
        (42, "42s"),
        (60 * 12 + 59, "12m59s"),
        (3600 * 7 + 60 * 5, "7h05m"),
        (3600 * 130 + 59 * 60, "130h"),
    ] {
        assert_eq!(runtime_short(secs), text);
    }
}

#[test]
fn unread_is_said_with_no_tokens_shown() {
    let left_out = Refresh {
        caught_up: false,
        unread: 2,
        walk_cut: false,
        spent: 0,
    };
    let wall = Wall {
        label: "5h",
        resets: "3pm".into(),
    };
    let walled =
        UsageFacts::of((Vec::new(), More::default()), Some(wall), Some(&left_out)).expect("facts");
    assert_eq!(usage_text(&walled).as_deref(), Some("+2 unread"));
    let alone =
        UsageFacts::of((Vec::new(), More::default()), None, Some(&left_out)).expect("facts");
    assert_eq!(usage_text(&alone).as_deref(), Some("+2 unread"));
    let whole = Refresh {
        caught_up: true,
        ..Refresh::default()
    };
    assert_eq!(
        UsageFacts::of((Vec::new(), More::default()), None, Some(&whole)),
        None
    );
}

fn measured(name: &str) -> Vec<String> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/harness/testdata/flight")
        .join(name);
    fs::read_to_string(path)
        .expect("test data")
        .lines()
        .filter(|l| !l.starts_with('#'))
        .map(|l| format!("{l}\n"))
        .collect()
}

fn testdata(name: &str) -> String {
    fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/harness/testdata/flight")
            .join(name),
    )
    .expect("test data")
}

#[test]
fn a_subagents_calls_in_flight_ride_the_usage_fold() {
    let dir = TestDir::new("flight");
    let main = dir.join("abc-123.jsonl");
    append(&main, &row("m1", "claude-opus-5-5", 10, 100));
    let subs = dir.join("abc-123").join("subagents");
    fs::create_dir_all(&subs).expect("dirs");
    let file = subs.join("agent-a372f0f51e1893105.jsonl");
    fs::write(
        subs.join("agent-a372f0f51e1893105.meta.json"),
        testdata("claude-2.1.304-long-bash.meta.json"),
    )
    .expect("meta");
    let rows = measured("claude-2.1.304-long-bash.jsonl");
    for r in &rows[..2] {
        append(&file, r);
    }
    let mut session = SessionUsage::new(&main);
    assert!(session.refresh(BIG).complete());
    let table = session.flights();
    let [flyer] = table.flyers.as_slice() else {
        panic!("one agent: {table:?}");
    };
    assert_eq!(
        *flyer,
        crate::harness::flight::Flyer {
            start_ms: crate::harness::codex_rollout::stamp_ms("2026-10-20T15:58:20.682Z").unwrap(),
            flight: flyer.flight,
            ..crate::harness::flight::Flyer::agent(
                "general-purpose",
                Some("long probe"),
                0,
                FileFlight::Unknown
            )
        }
    );
    assert!(matches!(flyer.flight, FileFlight::Tool(c) if c.budget_ms == 400_000));
    assert_eq!(session.meta_reads(), 1);
    let (bytes, generation) = (session.bytes_read(), session.flight_generation());
    assert!(!session.moved());
    session.refresh(BIG);
    assert_eq!(session.bytes_read(), bytes);
    assert_eq!(session.meta_reads(), 1);
    assert_eq!(session.flight_generation(), generation);
    append(&file, &rows[2]);
    session.refresh(BIG);
    assert_eq!(session.bytes_read() - bytes, rows[2].len() as u64 + 1);
    assert_eq!(session.flights().flyers[0].flight, FileFlight::Model);
    assert_ne!(session.flight_generation(), generation);
    assert_eq!(session.meta_reads(), 1, "the meta is read once");
    assert_eq!(session.main().file_flight(), FileFlight::Unknown);
}

#[test]
fn a_run_is_looked_for_once_a_refresh_whatever_its_agents() {
    let dir = TestDir::new("flight-run-tries");
    let main = dir.join("abc-123.jsonl");
    append(&main, &row("m1", "claude-opus-5-5", 10, 100));
    let run = dir
        .join("abc-123")
        .join("subagents")
        .join("workflows")
        .join("wf_stallrun-f56");
    fs::create_dir_all(&run).expect("dirs");
    let wf = measured("claude-2.1.304-workflow-long.jsonl");
    let agents: Vec<_> = (0..5)
        .map(|i| run.join(format!("agent-a{i}.jsonl")))
        .collect();
    let mut session = SessionUsage::new(&main);
    for k in 1..=u64::from(META_TRIES) + 2 {
        for agent in &agents {
            append(agent, &wf[0]);
        }
        session.refresh(BIG);
        assert_eq!(
            session.meta_reads(),
            k.min(u64::from(META_TRIES)),
            "refresh {k}: one look a refresh, bounded"
        );
    }
}

#[test]
fn metas_and_scripts_are_looked_for_a_bounded_number_of_times() {
    let dir = TestDir::new("flight-meta");
    let main = dir.join("abc-123.jsonl");
    append(&main, &row("m1", "claude-opus-5-5", 10, 100));
    let subs = dir.join("abc-123").join("subagents");
    let run = subs.join("workflows").join("wf_stallrun-f56");
    fs::create_dir_all(&run).expect("dirs");
    let file = subs.join("agent-a1.jsonl");
    let rows = measured("claude-2.1.304-long-bash.jsonl");
    append(&file, &rows[0]);
    let mut session = SessionUsage::new(&main);
    session.refresh(BIG);
    assert!(session.flights().flyers.is_empty(), "no meta, no entry");
    fs::write(
        subs.join("agent-a1.meta.json"),
        testdata("claude-2.1.304-long-bash.meta.json"),
    )
    .expect("meta");
    append(&file, &rows[1]);
    session.refresh(BIG);
    assert_eq!(session.flights().flyers.len(), 1);
    assert_eq!(session.meta_reads(), 2);
    let wf = measured("claude-2.1.304-workflow-long.jsonl");
    let agent = run.join("agent-aa69f18335229ca32.jsonl");
    for (k, r) in wf.iter().enumerate() {
        append(&agent, r);
        session.refresh(BIG);
        if k == 0 {
            assert_eq!(session.meta_reads(), 3, "the run's first look");
        }
    }
    assert_eq!(session.meta_reads(), 2 + u64::from(META_TRIES), "bounded");
    assert_eq!(session.flights().flyers.len(), 1, "no script, no run");
    let dir = TestDir::new("flight-run");
    let main = dir.join("abc-123.jsonl");
    append(&main, &row("m1", "claude-opus-5-5", 10, 100));
    let session_dir = dir.join("abc-123");
    let run = session_dir
        .join("subagents")
        .join("workflows")
        .join("wf_stallrun-f56");
    let scripts = session_dir.join("workflows").join("scripts");
    fs::create_dir_all(&run).expect("dirs");
    fs::create_dir_all(&scripts).expect("dirs");
    fs::write(
        scripts.join("stall-probe-wf_stallrun-f56.js"),
        "// stand-in\n",
    )
    .expect("script");
    for (name, rows) in [
        ("agent-aa69f18335229ca32.jsonl", &wf[..3]),
        (
            "agent-a20aa78f1612099f1.jsonl",
            &measured("claude-2.1.304-workflow-quick.jsonl")[..],
        ),
    ] {
        for r in rows {
            append(&run.join(name), r);
        }
    }
    let mut session = SessionUsage::new(&main);
    session.refresh(BIG);
    let table = session.flights();
    let [flyer] = table.flyers.as_slice() else {
        panic!("one run: {table:?}");
    };
    assert_eq!(flyer.kind, aterm_phase::agents_panel::RowKind::Workflow);
    assert_eq!(
        flyer.name_hash,
        aterm_phase::agents_panel::text_hash("stall-probe")
    );
    assert!(matches!(flyer.flight, FileFlight::Tool(c) if c.budget_ms == 300_000));
    assert_eq!(session.meta_reads(), 1, "one listing for the run");
}
