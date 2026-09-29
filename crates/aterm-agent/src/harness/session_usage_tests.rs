// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for [`super`]: the incremental fold against temp files this test
//! writes (never a real transcript), the limit wall, and the footer's text.

use std::fs;
use std::io::Write as _;
use std::sync::atomic::{AtomicU64, Ordering};

use super::*;

static NEXT_DIR: AtomicU64 = AtomicU64::new(1);

/// A fresh scratch directory under the system temp dir, removed on drop.
struct TestDir(PathBuf);

impl TestDir {
    fn new(label: &str) -> Self {
        let nonce = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "aterm-session-usage-{label}-{}-{nonce}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create the scratch root");
        Self(path)
    }

    fn join(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// One assistant row as the vendor writes it (the measured shape), with its
/// newline.
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

// ---------------------------------------------------------------------------
// The incremental fold
// ---------------------------------------------------------------------------

/// THE COST RULE: a re-read reads what was appended and nothing else — the
/// appended bytes plus the one boundary byte — and a re-read of an unchanged
/// file reads nothing at all.
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

/// A file whose size, mtime and inode have not moved is not even OPENED: a
/// finished subagent transcript costs one `stat` a read. Proved by taking
/// the read permission away — an open would fail — and then, as the
/// control, by an append, which must open (and here, fail).
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
    // The control must see a CHANGED file: set the mtime far from now.
    append_keeping_mode(&path, &row("m2", "claude-opus-5-5", 1, 7));
    let changed = cursor.advance(BIG);
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("chmod");
    assert_eq!(unchanged.expect("not opened, so not refused").read, 0);
    assert!(changed.is_err(), "control: a changed file is opened");
}

/// Append to a file whose mode refuses the owner a write: flip it open,
/// append, flip it back.
#[cfg(unix)]
fn append_keeping_mode(path: &Path, text: &str) {
    use std::os::unix::fs::PermissionsExt as _;
    let mode = fs::metadata(path).expect("meta").permissions().mode();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).expect("chmod");
    append(path, text);
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).expect("chmod");
}

/// Between reads a cursor holds the bytes of a torn last line and no line
/// buffer: a row that spans read chunks is assembled in one, and it is let go
/// when the read ends — hundreds of finished subagent cursors must not each
/// pin one.
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

/// A line is folded once its newline is written: the half-written row the
/// vendor is still appending is not counted, then counted exactly once.
#[test]
fn a_torn_last_line_waits_for_its_newline() {
    let dir = TestDir::new("torn");
    let path = dir.join("s.jsonl");
    let whole = row("m1", "claude-opus-5-5", 1, 2);
    let torn = row("m2", "claude-opus-5-5", 1, 40);
    let (head, tail) = torn.split_at(torn.len() / 2);
    append(&path, &format!("{whole}{head}"));
    let mut cursor = TranscriptCursor::new(&path);
    cursor.advance(BIG).expect("reads");
    assert_eq!(output_of(cursor.fold(), "claude-opus-5-5"), 2);
    assert_eq!(
        cursor.fold().rows_unparsed,
        0,
        "the torn half is never parsed as a broken row"
    );
    assert_eq!(cursor.fold().rows, 1);
    append(&path, tail);
    cursor.advance(BIG).expect("reads");
    assert_eq!(output_of(cursor.fold(), "claude-opus-5-5"), 42);
    assert_eq!(cursor.fold().rows, 2, "the completed row counts once");
}

/// A `message.id` counts once however its repeats straddle two reads.
#[test]
fn a_repeated_id_counts_once_across_reads() {
    let dir = TestDir::new("dupes");
    let path = dir.join("s.jsonl");
    append(&path, &row("m1", "claude-opus-5-5", 100, 20));
    let mut cursor = TranscriptCursor::new(&path);
    cursor.advance(BIG).expect("reads");
    append(
        &path,
        &format!(
            "{}{}",
            row("m1", "claude-opus-5-5", 100, 20),
            row("m1", "claude-opus-5-5", 100, 20)
        ),
    );
    cursor.advance(BIG).expect("reads");
    assert_eq!(output_of(cursor.fold(), "claude-opus-5-5"), 20);
    assert_eq!(cursor.fold().duplicates, 2);
}

/// Truncated below the offset reached: the fold starts over and holds only
/// what the file holds now.
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

/// Rotated: another file renamed over the path is a new inode, and the fold
/// starts over rather than reading on from an offset into someone else's
/// bytes — even when the new file is LONGER and its byte under the old
/// boundary is a newline too (its first row is exactly as long as the old
/// one's), so only the file's identity can tell.
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

/// Rewritten IN PLACE, same inode and longer than before: only the boundary
/// byte can tell, and it does.
#[test]
fn a_file_rewritten_in_place_is_caught_by_the_boundary_byte() {
    let dir = TestDir::new("rewritten");
    let path = dir.join("s.jsonl");
    let first = row("m1", "claude-opus-5-5", 1, 5);
    append(&path, &first);
    let mut cursor = TranscriptCursor::new(&path);
    cursor.advance(BIG).expect("reads");
    // A longer text whose byte at the old boundary is not a newline.
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

/// The budget bounds one read; the next read carries on from where it
/// stopped, and the whole is the same fold one pass makes.
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

/// PROPERTY: however the appends fall — mid-line, mid-character — the
/// incremental fold ends equal to one streaming pass over the whole file.
#[test]
fn any_split_of_the_appends_folds_to_the_one_pass_result() {
    let dir = TestDir::new("splits");
    let text: String = (0..40)
        .map(|i| {
            let model = ["claude-opus-5-5", "claude-haiku-4-5", "claude-fable-5-1"][i % 3];
            // Every fifth row repeats the one before it: dedupe across cuts.
            let id = if i % 5 == 4 { i - 1 } else { i };
            let r = row(&format!("m{id}"), model, i as u64, (i * 7) as u64);
            if i % 7 == 0 {
                // A non-ASCII line (a user row) the cuts can split inside.
                format!("{{\"type\":\"user\",\"text\":\"\u{00E9}\u{6F22}\u{1F600}\"}}\n{r}")
            } else {
                r
            }
        })
        .collect();
    let mut whole = TranscriptUsage::new();
    whole
        .fold_reader(std::io::BufReader::new(text.as_bytes()))
        .expect("in memory");
    for step in [1usize, 3, 17, 64, 333, 4096] {
        let path = dir.join(&format!("s{step}.jsonl"));
        fs::write(&path, b"").expect("create");
        let mut cursor = TranscriptCursor::new(&path);
        for chunk in text.as_bytes().chunks(step) {
            let mut f = fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .expect("open");
            f.write_all(chunk).expect("append");
            drop(f);
            let a = cursor.advance(BIG).expect("reads");
            assert!(!a.restarted, "step {step}: an append is never a restart");
        }
        assert_eq!(cursor.fold().per_model(), whole.per_model(), "step {step}");
        assert_eq!(cursor.fold().duplicates, whole.duplicates, "step {step}");
        assert_eq!(cursor.fold().rows, whole.rows, "step {step}");
    }
}

/// A line past the parse cap is skipped and counted even when it arrives
/// torn across reads, and the row after it still counts.
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

/// A FIFO where a transcript should be is refused, never read (a read would
/// park the resolver thread).
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

// ---------------------------------------------------------------------------
// One session: the transcript and its subagents
// ---------------------------------------------------------------------------

/// The session's spend is its transcript's plus every subagent transcript
/// the vendor keeps beside it (`<session>/subagents/…/agent-*.jsonl`) — and
/// nothing else in the project directory: another session's file is not
/// this one's.
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
    // A workflow's journal sits beside its agents' transcripts (MEASURED:
    // `subagents/workflows/wf_<id>/journal.jsonl`): not a transcript.
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
    // A subagent that starts later is picked up on the next refresh, and only
    // its bytes are read: the files already folded are not opened.
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

/// A subagent still BEHIND is a prefix: the session is not caught up while
/// the transcript is, and it is once the subagent is read to its end.
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

/// A FINISHED subagent file keeps its totals and a few ids — not its whole
/// dedupe set, not a line buffer — and stays followed: when a continued
/// agent appends to it again (its file is fixed per agent id), the fold
/// reads on from where it stopped. Its earlier total is carried, nothing is
/// counted twice (a streamed repeat of its newest message is raised), and
/// nothing is left unread.
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
    // The agent is continued: its newest message streams on (a repeat,
    // raised), and a new one follows.
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

/// Set a file's mtime `secs_ago` seconds in the past.
fn set_age(path: &Path, secs_ago: u64) {
    let f = fs::OpenOptions::new()
        .append(true)
        .open(path)
        .expect("open");
    f.set_modified(SystemTime::now() - std::time::Duration::from_secs(secs_ago))
        .expect("mtime");
}

/// AT THE BOUND, new files are UNREAD — said, never guessed at: within the
/// read that crosses it the newest are followed; past it every new file
/// (a running subagent's included) is counted `unread`, and a followed file
/// is never dropped — it keeps growing in the totals.
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

/// With the bound reached while a followed file is still being read, a new
/// one is left and counted `unread`, and the fold is not complete.
#[test]
fn a_file_that_cannot_be_followed_is_counted_as_unread() {
    let dir = TestDir::new("unfollowed");
    let main = dir.join("abc-9.jsonl");
    append(&main, &row("m1", "claude-opus-5-5", 1, 1));
    let subs = dir.join("abc-9").join("subagents");
    fs::create_dir_all(&subs).expect("dirs");
    let big: String = (0..20)
        .map(|i| row(&format!("s{i}"), "claude-haiku-4-5", 1, 1))
        .collect();
    append(&subs.join("agent-a.jsonl"), &big);
    let mut session = SessionUsage::new(&main);
    session.cap = 1;
    session.refresh(fs::metadata(&main).expect("meta").len() + 10);
    append(
        &subs.join("agent-b.jsonl"),
        &row("b", "claude-haiku-4-5", 1, 1),
    );
    let r = session.refresh(10);
    assert_eq!(r.unread, 1, "{r:?}");
    assert!(!r.complete());
}

/// The walk that stops at its entry bound is SAID end to end: the fold is
/// not complete, and the footer reads `+? unread` (a `0` would read as
/// nothing missing) — or `+N+ unread` beside files it could not count.
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

/// What a refresh now could find is known from a few `stat`s: the fold
/// behind WITH progress to make, the transcript, a new file in the subagent
/// directory, or a RUNNING subagent's file growing — the main transcript can
/// sit still for minutes while its subagents spend. A fold behind whose last
/// read moved nothing is left alone until a file changes.
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

/// `moved` checks EVERY followed file, not only the most recently written:
/// with ten parallel agents, the one whose file was written longest ago (an
/// agent in a long tool call) appending its final message is seen at once,
/// as the newest growing is — and a followed file deleted since is not a
/// change to read.
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

/// Once the read's budget is spent, NO further file is opened: a subagent
/// cursor left behind costs nothing on a read whose budget the transcript
/// took — the meter moves by the budget alone.
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
    // New subagent files appear — one unreadable — while the budget is
    // spent: neither is opened, so neither is admitted (a file never opened
    // is not in the totals) nor found unreadable; the read is not caught up.
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
        30 + 7000 + 1,
        "admitted once there was budget to open it"
    );
}

/// A subagent file that cannot be read is counted ONCE, as unread — never
/// also as followed — and once it can be read it is followed and counted.
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
    // One cursor only, and the unreadable file the newest: it must not take
    // the slot the readable one needs.
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
    // A HELD file that turns unreadable with new rows: its earlier tokens
    // stay in the totals, and it is counted once — unread, not also folded.
    append(&locked, &row("b2", "claude-haiku-4-5", 1, 5));
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).expect("chmod");
    let r = session.refresh(BIG);
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o600)).expect("chmod");
    assert_eq!((session.subagent_files(), r.unread), (1, 1), "{r:?}");
}

/// A subagent file still for [`QUIET_AFTER`] keeps its totals and NO ids;
/// one that may still grow keeps the few newest.
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

/// A subagent file's directory can appear at ANY depth: the vendor writes
/// 97 % of them under `subagents/workflows/<run>/`. `moved` stats every
/// directory the walk looked at — `subagents/` itself before it exists — so a
/// new file in a run directory is seen at once, not at the slow clock.
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
    session.refresh(BIG);
    assert!(!session.moved());
    append(
        &run.join("agent-w1.jsonl"),
        &row("w1", "claude-haiku-4-5", 1, 5),
    );
    assert!(session.moved(), "a file in a run directory");
    session.refresh(BIG);
    assert!(!session.moved());
    // Nine files, so the newest followed ones `moved` stats are all taken.
    for i in 2..10 {
        append(
            &run.join(format!("agent-w{i}.jsonl")),
            &row(&format!("w{i}"), "claude-haiku-4-5", 1, 5),
        );
    }
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

/// Replace `path`'s contents in place, keeping its inode.
fn rewrite_in_place(path: &Path, text: &str) {
    let mut f = fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(path)
        .expect("open");
    f.write_all(text.as_bytes()).expect("write");
}

/// A QUIET file's ids are released, so what it held can no longer be told
/// apart from a rewrite: a plain APPEND is read on and counted, and any other
/// change — shrunk, replaced by another file, rewritten in place — leaves it
/// UNREAD, its earlier totals standing: never re-read from its first byte,
/// never counted twice. A file that is not quiet still starts over.
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
    // Rewritten in place, longer: the byte the fold stopped after is no
    // longer the newline it was.
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

    // Control: a file NOT yet quiet starts over, as any cursor does.
    let live = subs.join("agent-live.jsonl");
    append(&live, &row("l1", "claude-haiku-4-5", 1, 3));
    session.refresh(BIG);
    rewrite_in_place(&live, &row("x", "claude-haiku-4-5", 1, 4));
    let r = session.refresh(BIG);
    assert_eq!(r.unread, 3, "{r:?}");
    assert_eq!(session.subagents[&live].restarts(), 1);
    assert_eq!(haiku(&session), 1204);
}

/// A QUIET file still BEHIND is not released: its ids are what raise the
/// rest of a streamed message instead of counting it again. Read to a cut
/// between the two rows of one message, the file keeps them, and the
/// message counts once, at its final 349.
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

/// A session whose transcript is not there yet is not caught up, and shows
/// no totals as though it were.
#[test]
fn a_missing_transcript_is_not_caught_up() {
    let dir = TestDir::new("missing");
    let mut session = SessionUsage::new(dir.join("nope.jsonl"));
    let r = session.refresh(BIG);
    assert!(!r.caught_up);
    assert!(session.total_fold().per_model().is_empty());
}

// ---------------------------------------------------------------------------
// The limit wall
// ---------------------------------------------------------------------------

/// The newest limit-notice row per window, as the fold keeps them.
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

/// Every zone, the local one included, is UTC.
fn utc(_: Option<&str>, _: i64) -> Option<i64> {
    Some(0)
}

const DAY: i64 = 1_790_000_000 - 1_790_000_000 % 86_400;

/// THE WALL is the newest notice's, shown while its reset — placed from the
/// row's own time — is ahead, as the vendor printed it (its zone left off:
/// it is the local one here), and not at all once it has passed.
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
    // The NEWEST row decides, whichever window it names.
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

/// A RESPONSE SERVED AFTER THE NOTICE takes the wall down: the limit no
/// longer blocks (`/limit-reset`, extra usage, another account), whatever
/// its printed reset. One served before it, or at its instant, does not.
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

/// A reset that cannot be placed shows NOTHING — never a guess, and never
/// an older row in its place: a text the parser does not read, a row with
/// no reset, a zone this machine does not know.
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

/// A WEEKLY reset under a day away is printed with no date (MEASURED in the
/// 2.1.283 bundle), and it is its NEXT occurrence: `9am` written at 14:00 is
/// tomorrow's — shown for the 19 hours the user is walled, never placed in
/// the past. The five-hour window keeps its own rule (today's, passed), and
/// a DATED reset is where its date puts it.
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

/// A weekly reset in the NEXT CALENDAR YEAR names the year (`Jan 2, 2027 at
/// 9am`): placed by the one reset parser, so the wall written on Dec 30
/// stands until Jan 2 09:00 and not after.
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

/// America/Los_Angeles around its 2026 spring switch (10:00Z on Mar 8).
fn pacific(_: Option<&str>, t: i64) -> Option<i64> {
    Some(if t >= 1_772_964_000 {
        -7 * 3600
    } else {
        -8 * 3600
    })
}

/// ACROSS A DAYLIGHT-SAVING JUMP the wall stands for its REAL time: `4am`
/// written at 22:30 PST on Mar 7 is 4am PDT on Mar 8 (11:00Z), four and a
/// half hours on — shown until then, not dropped as Mar 7's.
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

/// ANOTHER ZONE'S CLOCK IS NEVER SHOWN AS THE LOCAL ONE: Claude Code prints
/// the reset in ITS process's zone. On a Pacific machine `10pm (UTC)` keeps
/// its zone; where that zone's clock IS the local one at the reset (the same
/// zone, or another at the same offset then) it is left off; and where the
/// local offset cannot be read, the zone stays.
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

/// A SPAN (`in 3h`) counts from the notice, so it is said as the local clock
/// time it names — `in 3h` read hours later would be false — with the date
/// once it is a day or more off.
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

// ---------------------------------------------------------------------------
// The footer's text
// ---------------------------------------------------------------------------

fn spend(input: u64, output: u64, cache_write: u64, cache_read: u64) -> ModelSpend {
    ModelSpend {
        input,
        output,
        cache_write,
        cache_read,
        messages: 1,
    }
}

/// A count at the top of the range (a corrupt or hostile row saturates to
/// `u64::MAX`) is named, never an overflow — a panic would kill the footer's
/// resolver thread and start every tab's fold over.
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

/// Family names where they are unique, the version where two share one; a
/// model with no tokens is left out; most first; past three are counted.
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
    assert_eq!(more, 0);
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
    assert_eq!(more, 1, "sonnet is counted, not named");
}

/// A model reached through a cloud route is named as the model: Bedrock's
/// `us.anthropic.…-v1:0` and Vertex's `…@date` ids name their family like
/// the API's own, two different models never share one label, and one
/// model reached by two routes is one entry.
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
    assert_eq!(more, 1, "opus5.5, counted");
}

/// The `Σ` segment names tokens and what the totals leave out; the wall is
/// its own segment's text, never folded into the tokens'.
#[test]
fn the_segment_says_tokens_and_the_wall_says_itself() {
    let tokens = vec![
        ModelTokens {
            label: "opus".into(),
            input: 48_001_000,
            output: 310_000,
        },
        ModelTokens {
            label: "haiku".into(),
            input: 1_100_000,
            output: 20_000,
        },
    ];
    let wall = || Wall {
        label: "5h",
        resets: "3pm".into(),
    };
    let only_tokens = UsageFacts::of((tokens.clone(), 0), None, None).expect("tokens");
    assert_eq!(
        usage_text(&only_tokens).as_deref(),
        Some("opus 48M in 310k out \u{00B7} haiku 1.1M in 20k out")
    );
    let together = UsageFacts::of((tokens.clone(), 2), Some(wall()), None).expect("facts");
    assert_eq!(
        usage_text(&together).as_deref(),
        Some("opus 48M in 310k out \u{00B7} haiku 1.1M in 20k out \u{00B7} +2"),
        "the wall is not in the tokens' text"
    );
    assert_eq!(wall_text(&wall()), "5h limit \u{00B7} resets 3pm");
    let walled = UsageFacts::of((Vec::new(), 0), Some(wall()), None).expect("the wall alone");
    assert_eq!(usage_text(&walled), None, "no tokens: no \u{03A3} text");
    assert_eq!(walled.wall, Some(wall()));
    assert_eq!(UsageFacts::of((Vec::new(), 0), None, None), None);
}

/// What is left out of the totals is SAID even while no tokens are shown
/// (the fold not caught up yet): `+2 unread`, beside a wall or alone.
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
    let walled = UsageFacts::of((Vec::new(), 0), Some(wall), Some(&left_out)).expect("facts");
    assert_eq!(usage_text(&walled).as_deref(), Some("+2 unread"));
    let alone = UsageFacts::of((Vec::new(), 0), None, Some(&left_out)).expect("facts");
    assert_eq!(usage_text(&alone).as_deref(), Some("+2 unread"));
    let whole = Refresh {
        caught_up: true,
        ..Refresh::default()
    };
    assert_eq!(UsageFacts::of((Vec::new(), 0), None, Some(&whole)), None);
}
