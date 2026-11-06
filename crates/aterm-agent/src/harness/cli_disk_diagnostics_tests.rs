// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::harness::cli::disk_diagnostics::{config_note, ledger_cut_error};

fn safe_line(text: &str) -> bool {
    text.bytes().filter(|b| *b == b'\n').count() == 1
        && text
            .chars()
            .all(|c| c == '\n' || !aterm_codec::tty::is_active(c))
}

fn render(kind: u8, value: &str, out: &mut dyn Write) {
    match kind {
        0 => config_note(value, out),
        1 => ledger_cut_error(Path::new(value), &io::Error::other("fixture"), out),
        2 => ledger_cut_error(
            Path::new("fixture"),
            &io::Error::other(value.to_owned()),
            out,
        ),
        _ => unreachable!("fixture kind"),
    }
}

#[test]
fn disk_diagnostics_text_metadata_is_one_visible_line() {
    let mut failures = Vec::new();
    for kind in 0..3 {
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
            let mut out = Vec::new();
            render(kind, &format!("A{control}B"), &mut out);
            let text = String::from_utf8(out).unwrap();
            if !safe_line(&text) || !text.contains(&format!("A{escaped}B")) {
                failures.push(format!("unsafe metadata kind={kind} control={control:?}"));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn disk_diagnostics_text_config_read_error_keeps_removal_disabled() {
    let tmp = tmp_dir("disk-note-config");
    // This path cannot be opened on any OS and is never created.
    let path = tmp.path().join("config\n\u{1b}[2J\0end");
    let reading = disk_config_read(Some(&path));
    assert!(reading.off_by_fault);
    assert!(!reading.config.apply);
    assert_eq!(reading.config.auto_free_gib, 0);
    assert_eq!(reading.notes.len(), 1);
    let mut out = Vec::new();
    config_note(&reading.notes[0], &mut out);
    assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 0);
    let text = String::from_utf8(out).unwrap();
    assert!(
        safe_line(&text) && text.contains("config\\n\\x1b[2J\\x00end"),
        "unsafe config diagnostic"
    );
}

#[test]
fn disk_diagnostics_text_cut_error_keeps_invalid_ledger_absent() {
    let tmp = tmp_dir("disk-note-cut");
    let path = tmp.path().join("ledger\n\u{1b}[2J\0end");
    let error = bound_ledger(&path, DISK_LEDGER_MAX_BYTES, DISK_LEDGER_KEEP_ROWS).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    let mut out = Vec::new();
    ledger_cut_error(&path, &error, &mut out);
    assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 0);
    let text = String::from_utf8(out).unwrap();
    assert!(
        safe_line(&text) && text.contains("ledger\\n\\x1b[2J\\x00end"),
        "unsafe cut diagnostic"
    );
}

#[test]
fn disk_diagnostics_text_ordinary_unicode_and_backslashes_remain_literal() {
    let value = "café 名字 👩\u{200d}💻\\normal trust=literal";
    for kind in 0..3 {
        let mut out = Vec::new();
        render(kind, value, &mut out);
        let text = String::from_utf8(out).unwrap();
        assert!(safe_line(&text));
        assert!(text.contains(value));
    }
}

#[test]
fn disk_diagnostics_text_output_errors_remain_best_effort_without_flushing() {
    struct Sink {
        bytes: Vec<u8>,
        remaining: Option<usize>,
    }
    impl Write for Sink {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            let n = self.remaining.map_or(bytes.len(), |r| r.min(bytes.len()));
            if n == 0 && !bytes.is_empty() {
                return Err(io::Error::other("fixture output failure"));
            }
            self.bytes.extend_from_slice(&bytes[..n]);
            if let Some(remaining) = &mut self.remaining {
                *remaining -= n;
            }
            Ok(n)
        }
        fn flush(&mut self) -> io::Result<()> {
            panic!("prelude diagnostics do not own flushing")
        }
    }
    for kind in 0..3 {
        for remaining in [Some(0), Some(1), None] {
            let mut sink = Sink {
                bytes: Vec::new(),
                remaining,
            };
            render(kind, "ordinary", &mut sink);
            render(kind, "second note", &mut sink);
            if let Some(n) = remaining {
                assert_eq!(sink.bytes.len(), n);
            } else {
                assert_eq!(sink.bytes.iter().filter(|b| **b == b'\n').count(), 2);
            }
        }
    }
}
