// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A release's own file of its events, apart from its output (`--events`, A19).
//!
//! The `check-finished` and `test-binary-finished` lines a release's cutter keeps its records
//! from go to the run's output too, which its children share: there a child's unfinished line,
//! or a line longer than a pipe writes at once, can land inside one. Here each line is one
//! `write_all` under one lock, to a file opened close-on-exec, so no child inherits it and no two
//! lines interleave.

use std::fs::File;
use std::io::Write as _;
use std::path::Path;
use std::sync::Mutex;

/// The events file a release writes.
#[derive(Debug)]
pub struct EventLog {
    file: Mutex<File>,
}

impl EventLog {
    /// Open `path` to append to (made, owner-only, when it is not there), close-on-exec as every
    /// file std opens is.
    ///
    /// # Errors
    /// It cannot be opened.
    pub fn open(path: &Path) -> std::io::Result<Self> {
        let mut open = std::fs::OpenOptions::new();
        open.append(true).create(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            open.mode(0o600);
        }
        Ok(Self {
            file: Mutex::new(open.open(path)?),
        })
    }

    /// Append `line` (a `verify-event: ` line; its newline added when it has none) whole. A
    /// write that fails loses the line, which its reader takes as nothing said: a check with no
    /// record runs again.
    pub fn write(&self, line: &str) {
        let line = line.strip_suffix('\n').unwrap_or(line);
        let mut whole = String::with_capacity(line.len() + 1);
        whole.push_str(line);
        whole.push('\n');
        let mut file = self
            .file
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _ = file.write_all(whole.as_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Lines from many threads, each far longer than a pipe writes at once, land whole and one
    /// per line, a missing newline added and a present one kept single.
    #[test]
    fn each_line_lands_whole_whatever_writes_beside_it() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let at = dir.path().join("06-verify.events");
        let log = EventLog::open(&at).unwrap();
        let long = |t: usize, i: usize| {
            format!(
                "verify-event: {{\"t\":{t},\"i\":{i},\"pad\":\"{}\"}}",
                "x".repeat(20_000)
            )
        };
        std::thread::scope(|s| {
            for t in 0..4 {
                let log = &log;
                s.spawn(move || {
                    for i in 0..20 {
                        let line = long(t, i);
                        log.write(&if i % 2 == 0 {
                            line
                        } else {
                            format!("{line}\n")
                        });
                    }
                });
            }
        });
        let text = std::fs::read_to_string(&at).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 80);
        for line in lines {
            let json = line.strip_prefix("verify-event: ").expect("a whole line");
            let event: aterm_json::Value = aterm_json::from_str(json).unwrap();
            assert_eq!(event["pad"].as_str().map(str::len), Some(20_000));
        }
    }
}
