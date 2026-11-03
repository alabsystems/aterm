// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Observe the first libtest failure while its binary is still running.

use std::io::Read;
use std::path::Path;

pub(super) struct Progress {
    file: std::fs::File,
    line: Vec<u8>,
    oversized: bool,
    reported: bool,
}

impl Progress {
    pub(super) fn open(path: &Path) -> std::io::Result<Self> {
        Ok(Self {
            file: crate::open_regular(path)?,
            line: Vec::new(),
            oversized: false,
            reported: false,
        })
    }

    /// A bounded read of newly appended output; this never determines the verdict.
    pub(super) fn poll(&mut self, mut failed: impl FnMut(&str)) {
        if self.reported {
            return;
        }
        let mut bytes = [0; 8192];
        for _ in 0..32 {
            let Ok(n) = self.file.read(&mut bytes) else {
                return;
            };
            if n == 0 {
                return;
            }
            for &byte in &bytes[..n] {
                if byte != b'\n' {
                    if self.line.len() < 8192 && !self.oversized {
                        self.line.push(byte);
                    } else {
                        self.oversized = true;
                    }
                    continue;
                }
                if !self.oversized
                    && let Some(case) = failed_case(&self.line)
                {
                    failed(&case);
                    self.reported = true;
                    return;
                }
                self.line.clear();
                self.oversized = false;
            }
        }
    }
}

fn failed_case(line: &[u8]) -> Option<String> {
    let line = std::str::from_utf8(line).ok()?;
    let plain = if line.contains('\x1b') {
        std::borrow::Cow::Owned(crate::libtest::strip_ansi(line))
    } else {
        std::borrow::Cow::Borrowed(line)
    };
    plain
        .trim_end_matches('\r')
        .strip_prefix("test ")?
        .strip_suffix(" ... FAILED")
        .filter(|case| !case.is_empty())
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn a_failure_is_observed_before_the_writer_finishes_and_only_once() {
        let root = aterm_tempfile::tempdir().unwrap();
        let path = root.path().join("running.log");
        let mut writer = std::fs::File::create(&path).unwrap();
        let mut progress = Progress::open(&path).unwrap();
        let mut failures = Vec::new();
        writer
            .write_all(b"test first ... ok\ntest bad ... FAIL")
            .unwrap();
        progress.poll(|case| failures.push(case.to_owned()));
        assert!(failures.is_empty(), "a partial line is not a failure");
        writer.write_all(b"ED\n").unwrap();
        progress.poll(|case| failures.push(case.to_owned()));
        assert_eq!(failures, ["bad"]);
        writer.write_all(b"test later ... FAILED\n").unwrap();
        progress.poll(|case| failures.push(case.to_owned()));
        assert_eq!(failures, ["bad"]);
    }

    #[test]
    fn colored_libtest_failures_keep_the_case_identity() {
        assert_eq!(
            failed_case(b"test colored ... \x1b[31mFAILED\x1b[0m\r"),
            Some("colored".into())
        );
        assert_eq!(failed_case(b"test okay ... \x1b[32mok\x1b[0m"), None);
    }

    #[test]
    fn oversized_and_ordinary_diagnostics_do_not_hide_the_next_failure() {
        let root = aterm_tempfile::tempdir().unwrap();
        let path = root.path().join("running.log");
        let mut writer = std::fs::File::create(&path).unwrap();
        writer.write_all(&vec![b'x'; 9000]).unwrap();
        writer
            .write_all(b"\nFAILED\ntest result: FAILED.\ntest actual ... FAILED\n")
            .unwrap();
        let mut progress = Progress::open(&path).unwrap();
        let mut failures = Vec::new();
        progress.poll(|case| failures.push(case.to_owned()));
        assert_eq!(failures, ["actual"]);
    }
}
