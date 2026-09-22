// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the bounded ledger against real files: every crash shape is
//! FABRICATED on disk, since no test can cut power. Every temp directory is a
//! unique child of `std::env::temp_dir()` and is removed when its guard drops.
//!
//! The ring's bounded model is NOT written here. It lives once, as
//! `aterm_spec::derive::harness_ledger_ring_model`, is proved at Tier 0 in
//! that crate, and is bound to this ring transition-by-transition in
//! `tests/conformance_harness.rs`. A second hand-written copy used to sit in
//! this file with a different `Buggy` encoding and different variables, so
//! the two descriptions of one machine were both green and neither
//! constrained the other.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use super::*;

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

static NEXT_DIR: AtomicU64 = AtomicU64::new(1);

/// A unique scratch root; the ring's own directory is created BY the ring
/// below it, so directory admission is exercised by every test.
struct TestDir(PathBuf);

impl TestDir {
    fn new(label: &str) -> Self {
        let nonce = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "aterm-harness-ring-{label}-{}-{nonce}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create the scratch root");
        Self(path)
    }

    fn ledger(&self) -> PathBuf {
        self.0.join("ledger")
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Every framed [`uniform_line`] record is this long, whatever its id.
const RECORD: u64 = 48;
/// Records per segment and segments per ring in the uniform config — the
/// `SegCap` and `Segments` constants of the model.
const SEG_CAP: u64 = 2;
const SEGMENTS: usize = 3;

/// Three segments of exactly two uniform records each.
fn uniform_cfg() -> RingConfig {
    RingConfig {
        max_bytes: RECORD * SEG_CAP * 3,
        segments: SEGMENTS,
    }
}

/// Room for everything a small test appends, so nothing rotates by accident.
fn roomy() -> RingConfig {
    RingConfig {
        max_bytes: 64 * 1024,
        segments: 4,
    }
}

/// A row whose framed record is exactly [`RECORD`] bytes for any id below 100.
fn uniform_line(id: u64) -> String {
    let digits = id.to_string().len();
    let width = 26 - digits;
    let line = format!("{:.<width$}", format!("row-{id}"));
    assert_eq!(byte_len(frame(id, &line).len()), RECORD, "fixture drift");
    line
}

fn append_uniform(ring: &mut Ring) -> u64 {
    let id = ring.next_id();
    let got = ring
        .append(&uniform_line(id))
        .expect("append a uniform row");
    assert_eq!(got, id, "append returns the id next_id() announced");
    got
}

/// The ring's segment files as the DIRECTORY shows them, parsed here rather
/// than by the code under test: `(first id, path)`, oldest first.
fn segment_files(dir: &Path, name: &str) -> Vec<(u64, PathBuf)> {
    let mut found = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return found;
    };
    for entry in entries {
        let entry = entry.expect("read a directory entry");
        let file_name = entry.file_name().to_string_lossy().into_owned();
        let Some(rest) = file_name.strip_prefix(&format!("{name}.")) else {
            continue;
        };
        let Some(digits) = rest.strip_suffix(".jsonl") else {
            continue;
        };
        if digits.len() == 20 && digits.bytes().all(|byte| byte.is_ascii_digit()) {
            found.push((digits.parse().expect("twenty digits"), entry.path()));
        }
    }
    found.sort();
    found
}

fn disk_bytes(dir: &Path, name: &str) -> u64 {
    segment_files(dir, name)
        .iter()
        .map(|(_, path)| fs::metadata(path).expect("stat a segment").len())
        .sum()
}

fn newest_segment(dir: &Path, name: &str) -> PathBuf {
    segment_files(dir, name)
        .pop()
        .expect("the ring has a segment")
        .1
}

/// What a crashed writer leaves behind: bytes on the end of a file, no sync.
fn append_raw(path: &Path, bytes: &[u8]) {
    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(path)
        .expect("open a segment to tear it");
    file.write_all(bytes).expect("write the torn bytes");
}

fn ids(entries: &[Entry]) -> Vec<u64> {
    entries.iter().map(|entry| entry.id).collect()
}

fn all_rows(ring: &Ring) -> Vec<Entry> {
    ring.read_since(0, usize::MAX).expect("read the whole ring")
}

// ---------------------------------------------------------------------------
// Append and read
// ---------------------------------------------------------------------------

#[test]
fn append_hands_out_ids_from_one_and_read_since_returns_the_rows_byte_for_byte() {
    let root = TestDir::new("basic");
    let mut ring = Ring::open(&root.ledger(), "rm", roomy()).expect("open");
    assert_eq!(ring.next_id(), 1);
    assert_eq!(ring.floor_id(), 1);
    assert!(ring.is_empty().expect("is_empty"));
    assert_eq!(ring.bytes(), 0);
    assert_eq!(ring.segment_count(), 0, "an unused ring creates no segment");

    let lines = [
        r#"{"cmd":"rm -rf target/x","verdict":"approve"}"#,
        "not json at all, and the ring does not care",
        "",
        "carriage return \r and a brace } and a quote \" are data",
        "unicode: \u{2028} is not a newline to a byte framer — naïve ✓",
    ];
    for (index, line) in lines.iter().enumerate() {
        let id = ring.append(line).expect("append");
        assert_eq!(id, u64::try_from(index).expect("small") + 1);
    }
    assert_eq!(ring.next_id(), 6);
    assert_eq!(ring.len().expect("len"), 5);
    assert!(!ring.is_empty().expect("is_empty"));

    let rows = all_rows(&ring);
    assert_eq!(ids(&rows), [1, 2, 3, 4, 5]);
    for (row, line) in rows.iter().zip(lines) {
        assert_eq!(row.line, line);
    }
    assert_eq!(ring.bytes(), disk_bytes(&root.ledger(), "rm"));

    // When every row is JSON the segment is JSONL a stranger's tool can read.
    let first = fs::read_to_string(newest_segment(&root.ledger(), "rm")).expect("read segment");
    assert!(first.starts_with(
        "{\"id\":1,\"n\":45,\"row\":{\"cmd\":\"rm -rf target/x\",\"verdict\":\"approve\"}}\n"
    ));
}

#[test]
fn a_line_with_a_newline_or_a_nul_is_refused_and_consumes_nothing() {
    let root = TestDir::new("newline");
    let mut ring = Ring::open(&root.ledger(), "rm", roomy()).expect("open");
    ring.append("kept").expect("append");
    let bytes = ring.bytes();
    for bad in ["two\nlines", "\n", "trailing\n", "nul\0inside", "\0"] {
        let error = ring
            .append(bad)
            .expect_err("a multi-line row must be refused");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput, "{bad:?}");
        assert_eq!(ring.next_id(), 2, "{bad:?} consumed an id");
        assert_eq!(ring.bytes(), bytes, "{bad:?} wrote something");
    }
    assert_eq!(ring.append("next").expect("append"), 2);
    assert_eq!(ids(&all_rows(&ring)), [1, 2]);
}

#[test]
fn a_row_larger_than_one_segment_is_refused_without_flushing_the_ledger() {
    let root = TestDir::new("oversize");
    let cfg = RingConfig {
        max_bytes: 4096,
        segments: 4,
    };
    let mut ring = Ring::open(&root.ledger(), "rm", cfg).expect("open");
    for _ in 0..10 {
        ring.append(&"x".repeat(100)).expect("append");
    }
    let before = (ring.bytes(), ring.next_id(), ring.segment_count());
    let rows_before = all_rows(&ring);

    let error = ring
        .append(&"y".repeat(2000))
        .expect_err("a row bigger than a segment");
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    assert_eq!(
        (ring.bytes(), ring.next_id(), ring.segment_count()),
        before,
        "a refused row changed the ring"
    );
    assert_eq!(
        all_rows(&ring),
        rows_before,
        "a refused row dropped history"
    );

    // The largest row that fits is accepted: the refusal is about size alone.
    let id = ring.next_id();
    let widest = (0..=1024usize)
        .rev()
        .find(|width| byte_len(frame(id, &"z".repeat(*width)).len()) <= cfg.segment_bytes())
        .expect("some row fits");
    let line = "z".repeat(widest);
    assert_eq!(byte_len(frame(id, &line).len()), cfg.segment_bytes());
    let error = ring
        .append(&"z".repeat(widest + 1))
        .expect_err("one byte over a segment");
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    assert_eq!(ring.append(&line).expect("an exactly-one-segment row"), id);
    assert!(ring.bytes() <= cfg.max_bytes);
    assert_eq!(all_rows(&ring).last().expect("rows").line, line);
}

#[test]
fn degenerate_configs_names_and_directories_are_refused() {
    let root = TestDir::new("refuse");
    let dir = root.ledger();
    for cfg in [
        RingConfig {
            max_bytes: 4096,
            segments: 0,
        },
        RingConfig {
            max_bytes: 4096,
            segments: 1,
        },
        RingConfig {
            max_bytes: u64::MAX,
            segments: MAX_SEGMENTS + 1,
        },
        RingConfig {
            max_bytes: 0,
            segments: 2,
        },
        RingConfig {
            max_bytes: MIN_SEGMENT_BYTES * 2 - 1,
            segments: 2,
        },
    ] {
        let error = Ring::open(&dir, "rm", cfg).expect_err("a degenerate config");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput, "{cfg:?}");
    }
    assert!(!dir.exists(), "a refused config must not create anything");

    let too_long = "n".repeat(MAX_NAME_BYTES + 1);
    for name in ["", "RM", "rm.0", "../rm", "rm/x", "rm x", "rm\n", &too_long] {
        let error = Ring::open(&dir, name, roomy()).expect_err("a bad name");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput, "{name:?}");
    }
    assert!(!dir.exists(), "a refused name must not create anything");

    let error =
        Ring::open(Path::new("relative/ledger"), "rm", roomy()).expect_err("a relative directory");
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);

    // The smallest legal config opens and takes a row.
    let smallest = RingConfig {
        max_bytes: MIN_SEGMENT_BYTES * 2,
        segments: 2,
    };
    let mut ring = Ring::open(&dir, "rm", smallest).expect("the smallest legal config");
    assert_eq!(ring.append("ok").expect("append"), 1);
}

#[test]
fn read_since_honours_the_cursor_the_cap_and_the_floor() {
    let root = TestDir::new("cursor");
    let mut ring = Ring::open(&root.ledger(), "rm", uniform_cfg()).expect("open");
    for _ in 0..9 {
        append_uniform(&mut ring);
    }
    // 9 rows at 2 per segment, 3 segments kept: [5,6] [7,8] [9].
    assert_eq!(ring.floor_id(), 5);
    assert_eq!(ids(&all_rows(&ring)), [5, 6, 7, 8, 9]);
    assert_eq!(ids(&ring.read_since(0, 0).expect("read")), [0u64; 0]);
    assert_eq!(ids(&ring.read_since(0, 3).expect("read")), [5, 6, 7]);
    assert_eq!(ids(&ring.read_since(2, 2).expect("read")), [5, 6]);
    assert_eq!(ids(&ring.read_since(5, 10).expect("read")), [6, 7, 8, 9]);
    assert_eq!(ids(&ring.read_since(6, 10).expect("read")), [7, 8, 9]);
    assert_eq!(ids(&ring.read_since(8, 10).expect("read")), [9]);
    assert_eq!(ids(&ring.read_since(9, 10).expect("read")), [0u64; 0]);
    assert_eq!(
        ids(&ring.read_since(u64::MAX, 10).expect("read")),
        [0u64; 0]
    );
    for row in all_rows(&ring) {
        assert_eq!(row.line, uniform_line(row.id));
    }
    // A reader whose cursor is below the floor can tell rows were dropped.
    let cursor = 2u64;
    assert!(cursor + 1 < ring.floor_id());
}

// ---------------------------------------------------------------------------
// The bound and rotation
// ---------------------------------------------------------------------------

#[test]
fn the_byte_bound_holds_after_every_append() {
    let root = TestDir::new("bound");
    let dir = root.ledger();
    let cfg = RingConfig {
        max_bytes: 4096,
        segments: 4,
    };
    let mut ring = Ring::open(&dir, "usage", cfg).expect("open");
    ring.set_durable(false);
    let mut seed = 0x2545_f491_4f6c_dd1du64;
    for expected in 1..=600u64 {
        seed = seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let width = usize::try_from(seed >> 56).expect("a byte"); // 0..=255
        let id = ring.append(&"r".repeat(width)).expect("append");
        assert_eq!(id, expected, "ids are consecutive while nothing fails");
        assert!(
            ring.bytes() <= cfg.max_bytes,
            "{} bytes after row {id}",
            ring.bytes()
        );
        assert_eq!(ring.bytes(), disk_bytes(&dir, "usage"), "after row {id}");
        assert!(ring.segment_count() <= cfg.segments, "after row {id}");
        for (_, path) in segment_files(&dir, "usage") {
            assert!(fs::metadata(&path).expect("stat").len() <= cfg.segment_bytes());
        }
    }
    let rows = all_rows(&ring);
    let first = rows.first().expect("rows survive").id;
    assert!(first >= ring.floor_id());
    assert!(
        first > 1,
        "600 rows cannot fit 4096 bytes: something rotated"
    );
    let expected: Vec<u64> = (first..=600).collect();
    assert_eq!(
        ids(&rows),
        expected,
        "survivors are one gap-free run to the newest"
    );
    assert_eq!(
        ring.len().expect("len"),
        u64::try_from(rows.len()).expect("small")
    );
}

#[test]
fn a_rotation_unlinks_the_oldest_segment_and_touches_no_other() {
    let root = TestDir::new("oldest");
    let dir = root.ledger();
    let mut ring = Ring::open(&dir, "rm", uniform_cfg()).expect("open");
    for _ in 0..6 {
        append_uniform(&mut ring);
    }
    let before = segment_files(&dir, "rm");
    assert_eq!(
        before.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
        [1, 3, 5]
    );
    assert_eq!(
        ring.bytes(),
        uniform_cfg().max_bytes,
        "the ring is exactly full"
    );
    let kept: Vec<Vec<u8>> = before[1..]
        .iter()
        .map(|(_, path)| fs::read(path).expect("read a kept segment"))
        .collect();

    assert_eq!(
        append_uniform(&mut ring),
        7,
        "a full ring still takes the row"
    );

    let after = segment_files(&dir, "rm");
    assert_eq!(
        after.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
        [3, 5, 7],
        "exactly the oldest segment went"
    );
    for ((_, path), bytes) in after.iter().zip(&kept) {
        assert_eq!(
            &fs::read(path).expect("read"),
            bytes,
            "a kept segment changed"
        );
    }
    assert_eq!(ring.floor_id(), 3);
    assert_eq!(ring.segment_count(), 3);
    assert_eq!(ids(&all_rows(&ring)), [3, 4, 5, 6, 7]);
    assert_eq!(ring.len().expect("len"), 5);
    assert!(ring.bytes() <= uniform_cfg().max_bytes);
}

#[test]
fn the_rotation_boundary_is_exact_a_record_that_fits_to_the_byte_does_not_rotate() {
    let root = TestDir::new("boundary");
    let dir = root.ledger();
    let mut ring = Ring::open(&dir, "rm", uniform_cfg()).expect("open");
    append_uniform(&mut ring);
    append_uniform(&mut ring);
    assert_eq!(
        ring.segment_count(),
        1,
        "two records fill a segment to the byte"
    );
    assert_eq!(ring.bytes(), RECORD * SEG_CAP);
    append_uniform(&mut ring);
    assert_eq!(ring.segment_count(), 2, "the next record starts a segment");
    assert_eq!(
        segment_files(&dir, "rm")
            .iter()
            .map(|(id, _)| *id)
            .collect::<Vec<_>>(),
        [1, 3],
        "a segment is named by the first id it holds"
    );

    // One byte over rotates one record early, and ids do not notice.
    let mut line = uniform_line(4);
    line.push('!');
    assert_eq!(ring.append(&line).expect("append"), 4);
    assert_eq!(ring.segment_count(), 3, "48 + 49 bytes do not fit 96");
    assert_eq!(ids(&all_rows(&ring)), [1, 2, 3, 4]);
    drop(ring);
    let ring = Ring::open(&dir, "rm", uniform_cfg()).expect("reopen");
    assert_eq!(ring.next_id(), 5);
    assert_eq!(ids(&all_rows(&ring)), [1, 2, 3, 4]);
}

#[test]
fn ids_stay_monotone_across_rotation_and_reopen() {
    let root = TestDir::new("monotone");
    let dir = root.ledger();
    let mut last = 0u64;
    for round in 0..7 {
        let mut ring = Ring::open(&dir, "rm", uniform_cfg()).expect("open");
        assert_eq!(ring.next_id(), last + 1, "round {round}");
        // 1, 2, 3 rows per round walks the reopen across every fill level.
        for _ in 0..=(round % 3) {
            let id = append_uniform(&mut ring);
            assert_eq!(id, last + 1);
            last = id;
        }
        let rows = all_rows(&ring);
        assert_eq!(rows.last().expect("rows").id, last);
        assert!(ids(&rows).windows(2).all(|pair| pair[0] + 1 == pair[1]));
    }
    assert!(last > 6, "the trace wrapped the ring at least once");
}

#[test]
fn a_smaller_config_on_reopen_trims_from_the_oldest_end() {
    let root = TestDir::new("shrink");
    let dir = root.ledger();
    {
        let mut ring = Ring::open(&dir, "rm", uniform_cfg()).expect("open");
        for _ in 0..6 {
            append_uniform(&mut ring);
        }
    }
    let smaller = RingConfig {
        max_bytes: RECORD * SEG_CAP * 2,
        segments: 2,
    };
    let mut ring = Ring::open(&dir, "rm", smaller).expect("reopen smaller");
    assert_eq!(ring.segment_count(), 2);
    assert_eq!(ring.floor_id(), 3);
    assert_eq!(ring.next_id(), 7, "trimming never moves the next id");
    assert_eq!(ids(&all_rows(&ring)), [3, 4, 5, 6]);
    assert!(ring.bytes() <= smaller.max_bytes);
    assert_eq!(append_uniform(&mut ring), 7);
    assert!(ring.bytes() <= smaller.max_bytes);
    assert_eq!(ids(&all_rows(&ring)), [5, 6, 7]);
}

// ---------------------------------------------------------------------------
// Crashes, fabricated on disk
// ---------------------------------------------------------------------------

#[test]
fn a_torn_tail_at_every_cut_point_is_ignored_and_its_id_is_never_handed_out_again() {
    let root = TestDir::new("torn");
    // Braces and quotes inside the row: a cut that lands after an inner `}`
    // must not be mistaken for the end of a whole record.
    let torn_row = r#"{"cmd":"rm -rf 'a}b'","k":{"deep":{}}}"#;
    let record = frame(3, torn_row);
    let whole_without_newline = record.len() - 1;
    for cut in 1..whole_without_newline {
        let dir = root.0.join(format!("cut-{cut}"));
        {
            let mut ring = Ring::open(&dir, "rm", roomy()).expect("open");
            ring.append("first").expect("append");
            ring.append("second").expect("append");
        }
        let segment = newest_segment(&dir, "rm");
        append_raw(&segment, &record.as_bytes()[..cut]);
        let torn_len = fs::metadata(&segment).expect("stat").len();

        // Reopen twice WITHOUT appending: the same answer both times, and the
        // open itself writes nothing.
        for _ in 0..2 {
            let ring = Ring::open(&dir, "rm", roomy()).expect("reopen over a torn tail");
            assert_eq!(ring.next_id(), 4, "cut {cut}: the torn slot is burned");
            assert_eq!(ring.len().expect("len"), 2, "cut {cut}");
            assert_eq!(ids(&all_rows(&ring)), [1, 2], "cut {cut}");
            assert_eq!(ring.bytes(), torn_len, "cut {cut}: bytes count the junk");
        }
        assert_eq!(fs::metadata(&segment).expect("stat").len(), torn_len);

        let mut ring = Ring::open(&dir, "rm", roomy()).expect("reopen");
        assert_eq!(
            ring.append("after the crash").expect("append"),
            4,
            "cut {cut}"
        );
        let rows = all_rows(&ring);
        assert_eq!(ids(&rows), [1, 2, 4], "cut {cut}: id 3 belongs to nobody");
        assert_eq!(rows[2].line, "after the crash");
        drop(ring);

        // The fragment is sealed into a junk line and keeps burning its slot.
        let text = fs::read(&segment).expect("read");
        let mut expected = Vec::new();
        expected.extend_from_slice(frame(1, "first").as_bytes());
        expected.extend_from_slice(frame(2, "second").as_bytes());
        expected.extend_from_slice(&record.as_bytes()[..cut]);
        expected.push(b'\n');
        expected.extend_from_slice(frame(4, "after the crash").as_bytes());
        assert_eq!(text, expected, "cut {cut}");
        let ring = Ring::open(&dir, "rm", roomy()).expect("third open");
        assert_eq!(ring.next_id(), 5, "cut {cut}");
        assert_eq!(ids(&all_rows(&ring)), [1, 2, 4], "cut {cut}");
    }
}

#[test]
fn a_record_that_lost_only_its_newline_is_whole_and_is_kept() {
    let root = TestDir::new("no-newline");
    let dir = root.ledger();
    {
        let mut ring = Ring::open(&dir, "rm", roomy()).expect("open");
        ring.append("first").expect("append");
    }
    let record = frame(2, "whole but unterminated");
    let segment = newest_segment(&dir, "rm");
    append_raw(&segment, &record.as_bytes()[..record.len() - 1]);

    let mut ring = Ring::open(&dir, "rm", roomy()).expect("reopen");
    assert_eq!(ring.next_id(), 3);
    assert_eq!(ring.len().expect("len"), 2);
    assert_eq!(all_rows(&ring)[1].line, "whole but unterminated");
    assert_eq!(ring.append("third").expect("append"), 3);
    assert_eq!(ids(&all_rows(&ring)), [1, 2, 3]);
    drop(ring);
    let mut expected = frame(1, "first");
    expected.push_str(&record);
    expected.push_str(&frame(3, "third"));
    assert_eq!(fs::read_to_string(&segment).expect("read"), expected);
}

#[test]
fn a_zero_filled_tail_is_junk_not_a_row() {
    let root = TestDir::new("zero-fill");
    let dir = root.ledger();
    {
        let mut ring = Ring::open(&dir, "rm", roomy()).expect("open");
        ring.append("first").expect("append");
    }
    let segment = newest_segment(&dir, "rm");
    // A filesystem that extended the file and lost the data: a frame head
    // followed by NULs of exactly the declared length, brace and all.
    append_raw(&segment, b"{\"id\":2,\"n\":4,\"row\":\0\0\0\0}");
    let mut ring = Ring::open(&dir, "rm", roomy()).expect("reopen");
    assert_eq!(ids(&all_rows(&ring)), [1], "a NUL row is never data");
    assert_eq!(ring.next_id(), 3);
    assert_eq!(ring.append("second try").expect("append"), 3);
    assert_eq!(ids(&all_rows(&ring)), [1, 3]);
}

#[test]
fn junk_in_the_middle_burns_one_slot_per_line_and_shifts_no_later_id() {
    let root = TestDir::new("junk");
    let dir = root.ledger();
    drop(Ring::open(&dir, "rm", roomy()).expect("create the directory"));
    let segment = dir.join("rm.00000000000000000001.jsonl");
    let mut text = frame(1, "one");
    text.push_str("### a stranger wrote here ###\n");
    text.push_str(&frame(3, "three"));
    text.push_str("{\"id\":4,\"n\":99,\"row\":length lies}\n");
    text.push_str("{\"id\":05,\"n\":4,\"row\":zero}\n"); // non-canonical id
    text.push_str(&frame(2, "replayed: an id that goes backwards"));
    text.push_str(&frame(9, "a gap is believed: ids are explicit"));
    fs::write(&segment, text).expect("write a hand-made segment");

    let mut ring = Ring::open(&dir, "rm", roomy()).expect("open over junk");
    let rows = all_rows(&ring);
    assert_eq!(ids(&rows), [1, 3, 9]);
    assert_eq!(rows[1].line, "three");
    assert_eq!(ring.len().expect("len"), 3);
    assert_eq!(ring.next_id(), 10);
    assert_eq!(ring.append("ten").expect("append"), 10);
}

#[test]
fn a_crash_after_the_new_segment_and_before_the_unlink_is_trimmed_on_reopen() {
    let root = TestDir::new("mid-rotate");
    let dir = root.ledger();
    {
        let mut ring = Ring::open(&dir, "rm", uniform_cfg()).expect("open");
        for _ in 0..6 {
            append_uniform(&mut ring);
        }
    }
    // Rotation creates the new segment FIRST. Stop there.
    fs::write(dir.join("rm.00000000000000000007.jsonl"), b"").expect("fabricate");
    assert_eq!(segment_files(&dir, "rm").len(), 4, "one file too many");

    let mut ring = Ring::open(&dir, "rm", uniform_cfg()).expect("reopen mid-rotation");
    assert_eq!(ring.segment_count(), 3);
    assert_eq!(
        segment_files(&dir, "rm")
            .iter()
            .map(|(id, _)| *id)
            .collect::<Vec<_>>(),
        [3, 5, 7],
        "only the oldest went"
    );
    assert_eq!(ring.next_id(), 7);
    assert_eq!(ids(&all_rows(&ring)), [3, 4, 5, 6]);
    assert_eq!(append_uniform(&mut ring), 7);
    assert_eq!(ring.segment_count(), 3, "the adopted segment took the row");
    assert_eq!(ids(&all_rows(&ring)), [3, 4, 5, 6, 7]);
}

#[test]
fn a_crash_between_the_filling_write_and_the_rotation_is_safe() {
    let root = TestDir::new("write-then-crash");
    let dir = root.ledger();
    {
        let mut ring = Ring::open(&dir, "rm", uniform_cfg()).expect("open");
        for _ in 0..6 {
            append_uniform(&mut ring);
        }
        // Dropped with every segment full to the byte and no rotation begun.
    }
    let mut ring = Ring::open(&dir, "rm", uniform_cfg()).expect("reopen full");
    assert_eq!(ring.segment_count(), 3);
    assert_eq!(ring.next_id(), 7);
    assert_eq!(
        ids(&all_rows(&ring)),
        [1, 2, 3, 4, 5, 6],
        "reopen drops nothing"
    );
    assert_eq!(
        append_uniform(&mut ring),
        7,
        "the rotation happens on this append"
    );
    assert_eq!(ids(&all_rows(&ring)), [3, 4, 5, 6, 7]);
    assert!(ring.bytes() <= uniform_cfg().max_bytes);
}

#[test]
fn an_empty_newest_segment_carries_the_next_id_in_its_name() {
    let root = TestDir::new("named-next");
    let dir = root.ledger();
    {
        let mut ring = Ring::open(&dir, "rm", roomy()).expect("open");
        ring.append("one").expect("append");
        ring.append("two").expect("append");
    }
    // Slot 3 was burned in memory by a write that could not be undone, the
    // fresh segment for slot 4 was created, and then the process died.
    fs::write(dir.join("rm.00000000000000000004.jsonl"), b"").expect("fabricate");
    let mut ring = Ring::open(&dir, "rm", roomy()).expect("reopen");
    assert_eq!(
        ring.next_id(),
        4,
        "the name, not the older segment's row count"
    );
    assert_eq!(ring.append("four").expect("append"), 4);
    assert_eq!(ids(&all_rows(&ring)), [1, 2, 4]);
}

#[test]
fn a_write_that_cannot_be_undone_burns_its_id_and_starts_a_fresh_segment() {
    let root = TestDir::new("failed-write");
    let dir = root.ledger();
    let mut ring = Ring::open(&dir, "rm", roomy()).expect("open");
    ring.append("one").expect("append");
    ring.append("two").expect("append");
    // A read-only handle fails the write AND the truncate that would undo it.
    let path = ring.segments.last().expect("active segment").path.clone();
    ring.active = Some(File::open(&path).expect("open read-only"));

    let error = ring.append("lost").expect_err("the write must fail");
    assert_ne!(
        error.kind(),
        io::ErrorKind::InvalidInput,
        "a filesystem error"
    );
    assert_eq!(ring.next_id(), 4, "slot 3 is burned, never reported");
    assert_eq!(ring.append("four").expect("the ring recovers"), 4);
    assert_eq!(
        ring.segment_count(),
        2,
        "the doubtful segment is left alone"
    );
    assert_eq!(
        segment_files(&dir, "rm")
            .iter()
            .map(|(id, _)| *id)
            .collect::<Vec<_>>(),
        [1, 4]
    );
    assert_eq!(ids(&all_rows(&ring)), [1, 2, 4]);
    assert_eq!(ring.len().expect("len"), 3);
    drop(ring);
    let ring = Ring::open(&dir, "rm", roomy()).expect("reopen");
    assert_eq!(ring.next_id(), 5);
    assert_eq!(ids(&all_rows(&ring)), [1, 2, 4]);
}

#[test]
fn len_counts_rows_not_slots_and_survives_reopen_and_rotation() {
    let root = TestDir::new("len");
    let dir = root.ledger();
    {
        let mut ring = Ring::open(&dir, "rm", uniform_cfg()).expect("open");
        append_uniform(&mut ring);
    }
    append_raw(
        &newest_segment(&dir, "rm"),
        b"{\"id\":2,\"n\":25,\"row\":torn",
    );
    let mut ring = Ring::open(&dir, "rm", uniform_cfg()).expect("reopen");
    assert_eq!(ring.len().expect("len"), 1);
    assert_eq!(append_uniform(&mut ring), 3);
    assert_eq!(append_uniform(&mut ring), 4);
    assert_eq!(ring.len().expect("len"), 3, "slots 1..=4 hold three rows");
    drop(ring);
    // A fresh process has to count the segments it did not write.
    let mut ring = Ring::open(&dir, "rm", uniform_cfg()).expect("reopen");
    assert_eq!(ring.len().expect("len"), 3);
    assert_eq!(ring.len().expect("len"), 3, "the cached answer agrees");
    while ring.floor_id() == 1 {
        append_uniform(&mut ring);
    }
    let rows = all_rows(&ring);
    assert_eq!(
        ring.len().expect("len"),
        u64::try_from(rows.len()).expect("small")
    );
    assert!(rows.iter().all(|row| row.id >= ring.floor_id()));
}

// ---------------------------------------------------------------------------
// One writer, private files
// ---------------------------------------------------------------------------

#[test]
fn a_second_ring_on_the_same_directory_and_name_is_refused_until_the_first_drops() {
    let root = TestDir::new("lock");
    let dir = root.ledger();
    let mut first = Ring::open(&dir, "rm", roomy()).expect("open");
    first.append("held").expect("append");

    let error = Ring::open(&dir, "rm", roomy()).expect_err("a second writer");
    assert_eq!(error.kind(), io::ErrorKind::ResourceBusy);
    let error = Ring::open(&dir, "rm", uniform_cfg()).expect_err("whatever its config");
    assert_eq!(error.kind(), io::ErrorKind::ResourceBusy);

    // A different ledger in the same directory is a different ring.
    let mut usage = Ring::open(&dir, "usage", roomy()).expect("a sibling ring");
    assert_eq!(usage.append("other ledger").expect("append"), 1);
    // `rm` must not read `rm-denied`'s files as its own, nor the reverse.
    let mut denied = Ring::open(&dir, "rm-denied", roomy()).expect("a prefix sibling");
    assert_eq!(denied.append("prefix sibling").expect("append"), 1);
    assert_eq!(ids(&all_rows(&first)), [1]);
    assert_eq!(first.append("still mine").expect("append"), 2);

    drop(first);
    let reopened = Ring::open(&dir, "rm", roomy()).expect("the lock went with the ring");
    assert_eq!(reopened.next_id(), 3);
    assert_eq!(all_rows(&reopened)[0].line, "held");
}

#[test]
fn foreign_files_are_ignored_and_a_directory_squatting_on_a_segment_name_is_refused() {
    let root = TestDir::new("foreign");
    let dir = root.ledger();
    {
        let mut ring = Ring::open(&dir, "rm", roomy()).expect("open");
        ring.append("one").expect("append");
    }
    for name in [
        "rm.jsonl",
        "rm.7.jsonl",
        "rm.00000000000000000000.jsonl",
        "rm.0000000000000000000x.jsonl",
        "rm.00000000000000000009.json",
        "rmx.00000000000000000009.jsonl",
        "notes.txt",
    ] {
        fs::write(dir.join(name), b"{\"id\":99,\"n\":1,\"row\":x}\n").expect("plant a stranger");
    }
    let mut ring = Ring::open(&dir, "rm", roomy()).expect("reopen among strangers");
    assert_eq!(ring.segment_count(), 1);
    assert_eq!(ring.next_id(), 2);
    assert_eq!(ring.append("two").expect("append"), 2);
    assert_eq!(ids(&all_rows(&ring)), [1, 2]);
    drop(ring);

    fs::create_dir(dir.join("rm.00000000000000000050.jsonl")).expect("plant a directory");
    let error = Ring::open(&dir, "rm", roomy()).expect_err("a directory is not a segment");
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
}

#[cfg(unix)]
#[test]
fn the_directory_is_0700_and_every_file_0600_even_when_found_looser() {
    use std::os::unix::fs::PermissionsExt as _;
    let mode = |path: &Path| fs::metadata(path).expect("stat").permissions().mode() & 0o777;

    let root = TestDir::new("modes");
    let dir = root.0.join("a").join("b").join("ledger");
    {
        let mut ring = Ring::open(&dir, "rm", uniform_cfg()).expect("open");
        for _ in 0..3 {
            append_uniform(&mut ring);
        }
    }
    assert_eq!(
        mode(&root.0.join("a")),
        0o700,
        "created components are private"
    );
    assert_eq!(mode(&root.0.join("a").join("b")), 0o700);
    assert_eq!(mode(&dir), 0o700);
    assert_eq!(mode(&dir.join("rm.lock")), 0o600);
    let segments = segment_files(&dir, "rm");
    assert_eq!(segments.len(), 2);
    for (_, path) in &segments {
        assert_eq!(mode(path), 0o600);
    }

    // Found looser: forced back, not trusted as found.
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).expect("loosen dir");
    fs::set_permissions(&segments[0].1, fs::Permissions::from_mode(0o644)).expect("loosen file");
    let ring = Ring::open(&dir, "rm", uniform_cfg()).expect("reopen");
    assert_eq!(mode(&dir), 0o700);
    assert_eq!(mode(&segments[0].1), 0o600);
    assert_eq!(ids(&all_rows(&ring)), [1, 2, 3]);
}

#[cfg(unix)]
#[test]
fn links_where_the_directory_the_lock_or_a_segment_should_be_are_refused() {
    use std::os::unix::fs::symlink;

    let root = TestDir::new("links");
    let real = root.0.join("real");
    {
        let mut ring = Ring::open(&real, "rm", roomy()).expect("open the real ring");
        ring.append("one").expect("append");
    }

    // The directory itself is a link.
    let linked = root.0.join("linked");
    symlink(&real, &linked).expect("symlink the directory");
    let error = Ring::open(&linked, "rm", roomy()).expect_err("a linked directory");
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    // ... or the closest existing ancestor of a directory still to be made.
    let error = Ring::open(&linked.join("deeper"), "rm", roomy()).expect_err("a linked ancestor");
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    assert!(
        !real.join("deeper").exists(),
        "nothing was created through the link"
    );

    // A segment is a link to a file somewhere else.
    let elsewhere = root.0.join("elsewhere.jsonl");
    fs::write(&elsewhere, frame(1, "planted")).expect("plant");
    let victim = root.0.join("victim");
    drop(Ring::open(&victim, "rm", roomy()).expect("create the victim directory"));
    symlink(&elsewhere, victim.join("rm.00000000000000000001.jsonl")).expect("symlink a segment");
    let error = Ring::open(&victim, "rm", roomy()).expect_err("a linked segment");
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    assert_eq!(
        fs::read_to_string(&elsewhere).expect("read"),
        frame(1, "planted"),
        "the link target was not written through"
    );

    // The lock file is a link.
    let locked = root.0.join("locked");
    drop(Ring::open(&locked, "usage", roomy()).expect("create the directory"));
    symlink(&elsewhere, locked.join("rm.lock")).expect("symlink the lock");
    let error = Ring::open(&locked, "rm", roomy()).expect_err("a linked lock");
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
}

#[cfg(unix)]
#[test]
fn open_owned_by_admits_the_owner_and_refuses_everyone_else() {
    use std::os::unix::fs::MetadataExt as _;

    let root = TestDir::new("owner");
    let dir = root.ledger();
    let me = fs::metadata(&root.0).expect("stat the scratch root").uid();
    {
        let mut ring = Ring::open_owned_by(&dir, "rm", roomy(), me).expect("the owner");
        ring.append("mine").expect("append");
    }
    let error = Ring::open_owned_by(&dir, "rm", roomy(), me.wrapping_add(1))
        .expect_err("somebody else's uid");
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    // The refusal released the lock and left the ring as it was.
    let ring = Ring::open_owned_by(&dir, "rm", roomy(), me).expect("the owner again");
    assert_eq!(ids(&all_rows(&ring)), [1]);
}

#[test]
fn the_frame_round_trips_and_the_parser_accepts_nothing_frame_could_not_write() {
    for (id, row) in [
        (1u64, ""),
        (7, "plain"),
        (u64::MAX - 1, r#"{"nested":{"}":"}"}}"#),
        (42, "ends in a brace }"),
    ] {
        let record = frame(id, row);
        let line = record.strip_suffix('\n').expect("frame ends the line");
        assert!(!line.contains('\n'));
        assert_eq!(parse_record(line.as_bytes()), Some((id, row)));
    }
    assert_eq!(
        byte_len(frame(u64::MAX, "").len()),
        41,
        "MIN_SEGMENT_BYTES is sized from this"
    );
    assert!(byte_len(frame(u64::MAX, "").len()) < MIN_SEGMENT_BYTES);
    for bad in [
        &b""[..],
        b"{}",
        b"{\"id\":1,\"n\":1,\"row\":x",
        b"{\"id\":1,\"n\":2,\"row\":x}",
        b"{\"id\":1,\"n\":0,\"row\":x}",
        b"{\"id\":01,\"n\":1,\"row\":x}",
        b"{\"id\":1,\"n\":01,\"row\":x}",
        b"{\"id\":+1,\"n\":1,\"row\":x}",
        b"{\"id\":-1,\"n\":1,\"row\":x}",
        b"{\"id\":,\"n\":1,\"row\":x}",
        b"{\"id\":99999999999999999999,\"n\":1,\"row\":x}",
        b"{\"id\": 1,\"n\":1,\"row\":x}",
        b"{\"id\":1,\"n\":1,\"row\":\0}",
        b"{\"id\":1,\"n\":1,\"row\":\xff}",
        b" {\"id\":1,\"n\":1,\"row\":x}",
    ] {
        assert_eq!(
            parse_record(bad),
            None,
            "{:?}",
            String::from_utf8_lossy(bad)
        );
    }
}

// ---------------------------------------------------------------------------
// What a scan is allowed to call junk (`line_cap`)
// ---------------------------------------------------------------------------

#[test]
fn a_row_bigger_than_the_readers_segment_but_inside_max_bytes_is_still_returned() {
    let root = TestDir::new("line-cap");
    let dir = root.0.join("ring");
    // Written by a two-segment ring: a segment holds 1024 bytes, so a ~900
    // byte row is a row this ring legitimately holds.
    let wide = RingConfig {
        max_bytes: 2048,
        segments: 2,
    };
    let big = "x".repeat(880);
    {
        let mut ring = Ring::open(&dir, "rm", wide).expect("open the wide ring");
        assert_eq!(ring.append(&big).expect("append the wide row"), 1);
        assert_eq!(ring.append("small").expect("append a small row"), 2);
    }

    // Reopened by an eight-segment ring of the same total size: a segment is
    // now 256 bytes, well under the row that is already on disk.
    let narrow = RingConfig {
        max_bytes: 2048,
        segments: 8,
    };
    assert!(
        narrow.segment_bytes() < u64::try_from(big.len()).expect("small"),
        "the fixture must put the row above the READER's segment size"
    );
    let ring = Ring::open(&dir, "rm", narrow).expect("reopen with more, smaller segments");
    let rows = all_rows(&ring);
    let counted = ring.len().expect("count");
    assert_eq!(
        rows.iter().map(|e| e.line.as_str()).collect::<Vec<_>>(),
        [big.as_str(), "small"],
        "a row inside max_bytes is a row, whatever THIS config's segment size is: \
         capping the scan at segment_bytes() would read it as junk and drop it"
    );
    assert_eq!(ids(&rows), [1, 2]);
    assert_eq!(counted, 2);
    drop(ring);

    // NEGATIVE CASE: past `max_bytes` a line IS junk — its slot stays burned
    // and its text is never returned.
    let newest = newest_segment(&dir, "rm");
    let mut junk = vec![b'z'; 4096];
    junk.push(b'\n');
    append_raw(&newest, &junk);
    let ring = Ring::open(&dir, "rm", narrow).expect("reopen over the junk line");
    assert_eq!(
        ids(&all_rows(&ring)),
        [1, 2],
        "a line longer than max_bytes is junk"
    );
    assert_eq!(ring.next_id(), 4, "and it burned slot 3");
}

/// Efficiency finding 2 (2026-09-22). `harness status`/`harness mark` asked
/// `len()` — which reads every byte of every segment this process did not
/// write — for three booleans. MEASURED: 6.66 ms against a tiny ledger
/// against 55.48 ms against a full 64 MiB rm ring. `has_rows` answers the
/// same question from the two ids already in memory after the open, and this
/// test pins that the two AGREE over an empty ring, a filled one, and one
/// that has rotated rows away.
#[test]
fn has_rows_agrees_with_len_and_reads_no_segment() {
    let dir = TestDir::new("has-rows");
    let mut ring = Ring::open(&dir.ledger(), "actuation", uniform_cfg()).expect("open");
    assert!(!ring.has_rows());
    assert_eq!(ring.len().expect("len"), 0);

    for _ in 0..4 {
        append_uniform(&mut ring);
    }
    assert!(ring.has_rows());
    assert_eq!(ring.len().expect("len") > 0, ring.has_rows());

    // Past the ring's whole capacity, so the oldest segment has been
    // dropped: the floor moved and `has_rows` must still agree.
    for _ in 0..12 {
        append_uniform(&mut ring);
    }
    assert!(ring.has_rows());
    assert_eq!(ring.len().expect("len") > 0, ring.has_rows());

    // And across a REOPEN, where `len()` would scan from scratch.
    drop(ring);
    let again = Ring::open(&dir.ledger(), "actuation", uniform_cfg()).expect("reopen");
    assert!(again.has_rows());
    assert_eq!(again.len().expect("len") > 0, again.has_rows());

    // NEGATIVE CONTROL: a ring that was never written stays false across a
    // reopen too.
    let empty_dir = TestDir::new("has-rows-empty");
    let empty = Ring::open(&empty_dir.ledger(), "actuation", roomy()).expect("open");
    assert!(!empty.has_rows());
    drop(empty);
    let empty_again = Ring::open(&empty_dir.ledger(), "actuation", roomy()).expect("reopen");
    assert!(!empty_again.has_rows());
    assert_eq!(empty_again.len().expect("len"), 0);
}

/// The word-at-a-time byte search answers EXACTLY what the byte walk answers.
///
/// It replaced two byte walks the scan pays per segment (the newline split and
/// the framing parser's NUL check), so "it is faster" is worth nothing unless
/// it is the same function. This checks it against `iter().position` — the
/// code it replaced — at every offset of every length up to past two machine
/// words, with the needle present once, absent, and repeated.
#[test]
fn the_word_at_a_time_byte_search_is_the_byte_walk_exactly() {
    let walk = |bytes: &[u8], needle: u8| bytes.iter().position(|byte| *byte == needle);
    for len in 0..40usize {
        // NEGATIVE CONTROL: no needle anywhere, at every length — the case a
        // broken word test would report a false hit for.
        let empty = vec![b'x'; len];
        assert_eq!(find_byte(&empty, b'\n'), None, "len {len}");
        assert_eq!(find_byte(&empty, 0), None, "len {len}");
        for at in 0..len {
            let mut one = vec![b'x'; len];
            one[at] = b'\n';
            assert_eq!(
                find_byte(&one, b'\n'),
                walk(&one, b'\n'),
                "len {len} at {at}"
            );
            assert_eq!(find_byte(&one, b'\n'), Some(at), "len {len} at {at}");
            // A NUL is data-shaped and must be found the same way.
            let mut nul = vec![b'x'; len];
            nul[at] = 0;
            assert_eq!(find_byte(&nul, 0), Some(at), "len {len} at {at}");
            // Repeated: the FIRST one wins, however many follow.
            let mut many = vec![b'\n'; len];
            for (i, byte) in many.iter_mut().enumerate() {
                if i < at {
                    *byte = b'x';
                }
            }
            assert_eq!(
                find_byte(&many, b'\n'),
                walk(&many, b'\n'),
                "len {len} at {at}"
            );
        }
    }
    // And over real framed bytes, including high bytes that set the lane's
    // top bit — the input shape the zero test is easiest to get wrong on.
    let framed = frame(7, "{\"cmd\":\"rm -rf é😀 \u{7f}\u{80}\"}");
    assert_eq!(
        find_byte(framed.as_bytes(), b'\n'),
        walk(framed.as_bytes(), b'\n')
    );
    assert_eq!(find_byte(framed.as_bytes(), 0), None);
}
