// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use std::io;

#[derive(Default)]
struct WarningSink {
    bytes: Vec<u8>,
    remaining: Option<usize>,
    fail_flush: bool,
    flushes: usize,
}

impl Write for WarningSink {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let n = self.remaining.map_or(bytes.len(), |r| r.min(bytes.len()));
        if n == 0 && !bytes.is_empty() {
            return Err(io::Error::other("fixture warning write failure"));
        }
        self.bytes.extend_from_slice(&bytes[..n]);
        if let Some(remaining) = &mut self.remaining {
            *remaining -= n;
        }
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.flushes += 1;
        if self.fail_flush {
            Err(io::Error::other("fixture warning flush failure"))
        } else {
            Ok(())
        }
    }
}

fn safe_line(text: &str) -> bool {
    text.bytes().filter(|b| *b == b'\n').count() == 1
        && text
            .chars()
            .all(|c| c == '\n' || !aterm_codec::tty::is_active(c))
}

fn capture(path: &Path, why: &str) -> String {
    // A named journal only formats data; it never opens this path.
    let mut journal = Journal::named(Some(path), None);
    let mut sink = WarningSink::default();
    journal.warn(why, &mut sink);
    assert_eq!(sink.flushes, 1);
    let first = sink.bytes.clone();
    journal.warn("second warning", &mut sink);
    assert_eq!(sink.bytes, first, "warn only once");
    assert_eq!(sink.flushes, 1, "a suppressed warning does no IO");
    String::from_utf8(sink.bytes).unwrap()
}

#[test]
fn journal_warning_text_metadata_is_one_visible_line() {
    let mut failures = Vec::new();
    for in_path in [false, true] {
        for (control, escaped) in [
            ('\0', "\\x00"),
            ('\t', "\\t"),
            ('\n', "\\n"),
            ('\r', "\\r"),
            ('\u{1b}', "\\x1b"),
            ('\u{9b}', "\\u{009b}"),
            ('\u{2028}', "\\u{2028}"),
            ('\u{202e}', "\\u{202e}"),
        ] {
            let value = format!("A{control}B");
            let text = if in_path {
                capture(Path::new(&value), "fixture")
            } else {
                capture(Path::new("fixture"), &value)
            };
            if !safe_line(&text) || !text.contains(&format!("A{escaped}B")) {
                failures.push(format!(
                    "unsafe warning in_path={in_path} control={control:?}"
                ));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn journal_warning_text_open_failure_keeps_append_and_record_semantics() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    // NUL makes the path invalid on every OS. No control-bearing file is created.
    let path = tmp.path().join("bad\n\u{1b}[2J\0end");
    let mut sink = WarningSink::default();
    let mut journal = Journal::open(Some(&path), Some("fixture"), &mut sink);
    assert!(
        journal.is_on(),
        "an unsuccessful open keeps its append target"
    );
    let first = sink.bytes.clone();
    assert!(!journal.append_raw("{\"id\":1}", &mut sink));
    let record = journal.record(1, "TIMEOUT", &mut sink).unwrap();
    let value: aterm_json::Value = aterm_json::from_str(record.trim_end()).unwrap();
    assert!(value["sid"] == aterm_json::Value::from("fixture"));
    assert!(value["line"] == aterm_json::Value::from("TIMEOUT"));
    assert_eq!(sink.bytes, first);
    assert_eq!(sink.flushes, 1);
    assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 0);
    let text = String::from_utf8(sink.bytes).unwrap();
    assert!(
        safe_line(&text) && text.contains("bad\\n\\x1b[2J\\x00end"),
        "unsafe open warning"
    );
}

#[test]
fn journal_warning_text_failed_error_is_data_without_a_disk_full_poke() {
    let error = io::Error::other("failureA\n\u{1b}[2JfailureB");
    assert!(!crate::harness::disk::poke::is_out_of_space(&error));
    let mut journal = Journal::named(Some(Path::new("fixture")), None);
    let mut sink = WarningSink::default();
    journal.failed(&error, "cannot append to it", &mut sink);
    let first = sink.bytes.clone();
    journal.failed(&error, "second failure", &mut sink);
    assert_eq!(sink.bytes, first);
    assert_eq!(sink.flushes, 1);
    let text = String::from_utf8(sink.bytes).unwrap();
    assert!(
        safe_line(&text) && text.contains("failureA\\n\\x1b[2JfailureB"),
        "unsafe failed warning"
    );
}

#[test]
fn journal_warning_text_ordinary_unicode_and_backslashes_remain_literal() {
    let value = "café 名字 👩\u{200d}💻\\normal trust=literal";
    let text = capture(Path::new(value), value);
    assert!(safe_line(&text));
    assert_eq!(
        text.matches(value).count(),
        2,
        "both metadata fields stay literal"
    );
}

#[test]
fn journal_warning_text_writer_failure_is_best_effort_once_and_flushed() {
    for remaining in [Some(0), Some(1), Some(10), None] {
        let mut journal = Journal::named(Some(Path::new("fixture")), None);
        let mut sink = WarningSink {
            remaining,
            fail_flush: remaining.is_none(),
            ..WarningSink::default()
        };
        journal.warn("ordinary", &mut sink);
        let first = sink.bytes.clone();
        assert_eq!(
            sink.flushes, 1,
            "flush is attempted even after a write error"
        );
        if let Some(n) = remaining {
            assert_eq!(sink.bytes.len(), n);
        } else {
            assert!(sink.bytes.ends_with(b"\n"));
        }
        journal.warn("second warning", &mut sink);
        assert_eq!(sink.bytes, first);
        assert_eq!(sink.flushes, 1);
    }
}
