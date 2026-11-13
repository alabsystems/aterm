// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::io::{Seek as _, Write as _};

fn artifact() -> &'static str {
    r#"{"reason":"compiler-artifact","target":{"name":"safe_unit","kind":["lib"]}}"#
}

#[test]
fn an_independent_reader_preserves_writer_offset_and_exact_capture() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join("stdout");
    let mut writer = super::super::create_stage_log(&path).unwrap();
    let mut progress = Progress::open(&path, &writer, "test compile").unwrap();
    write!(writer, "{}\r\n", artifact()).unwrap();
    let offset = writer.stream_position().unwrap();
    assert_eq!(progress.sample().len(), 1);
    assert_eq!(writer.stream_position().unwrap(), offset);
    writer.write_all(b"exact suffix").unwrap();
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        format!("{}\r\nexact suffix", artifact())
    );
    let other = dir.path().join("other");
    let foreign = super::super::create_stage_log(&other).unwrap();
    assert!(Progress::open(&path, &foreign, "test compile").is_err());
}

#[test]
fn partial_invalid_and_oversized_lines_never_become_observations() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join("stdout");
    let mut writer = super::super::create_stage_log(&path).unwrap();
    let mut progress = Progress::open(&path, &writer, "test compile").unwrap();
    write!(writer, "{}", artifact()).unwrap();
    assert!(progress.sample().is_empty());
    writer.write_all(b"\r\n\xff\n").unwrap();
    assert_eq!(progress.sample().len(), 1);
    writer.write_all(&vec![b'x'; LINE_BYTES + 1]).unwrap();
    writeln!(writer, "\n{}", artifact()).unwrap();
    assert!(progress.sample().is_empty());
    assert_eq!(progress.sample().len(), 1);
    assert!(progress.line.len() <= LINE_BYTES);
}

#[test]
fn samples_keep_unconsumed_lines_and_stop_at_the_total_cap() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join("stdout");
    let mut writer = super::super::create_stage_log(&path).unwrap();
    let mut progress = Progress::open(&path, &writer, "test compile").unwrap();
    for _ in 0..EVENTS_TOTAL + 1 {
        writeln!(writer, "{}", artifact()).unwrap();
    }
    for _ in 0..EVENTS_TOTAL / EVENTS_PER_POLL {
        assert_eq!(progress.sample().len(), EVENTS_PER_POLL);
    }
    assert_eq!(progress.emitted, EVENTS_TOTAL);
    let offset = progress.file.stream_position().unwrap();
    assert!(progress.sample().is_empty());
    assert_eq!(progress.file.stream_position().unwrap(), offset);
}
