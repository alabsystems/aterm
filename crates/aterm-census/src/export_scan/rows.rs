// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The forbidden-content rows: the export's own (`publish/forbidden-extra.txt`) and the
//! publication engine's baseline, compiled as the engine's `grep -E` runs them. The scan holds
//! the export's text to them ([`super::forbidden`]), and `capture-privacy` holds decoded test
//! captures to the same rows. The census reads no engine itself: its caller hands it one
//! commit's baseline ([`crate::EngineBaseline`]), as `aterm-verify`'s engine reader read it.

use aterm_regex::{Regex, RegexBuilder};

/// The export's own rows, relative to the checkout.
pub const EXTRA: &str = "publish/forbidden-extra.txt";

/// The engine's central rows, relative to its root.
pub const BASELINE: &str = "baseline/forbidden-content.txt";

/// One pattern the text is held to.
pub(crate) struct Row {
    /// Where it comes from, as a finding names it.
    pub(crate) source: String,
    /// The pattern as grep runs it, on one line.
    pub(crate) line: Regex,
    /// The same with `(?m)`: a whole view without a match of it holds no line
    /// with one.
    pub(crate) any: Regex,
}

const VIEW_STEPS: u64 = 1 << 32;

impl Row {
    pub(crate) fn new(source: String, pattern: &str) -> Result<Self, String> {
        let line = Regex::new(pattern).map_err(|e| e.to_string())?;
        let any = RegexBuilder::new(&format!("(?m){pattern}"))
            .step_limit(VIEW_STEPS)
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Self { source, line, any })
    }
}

/// The non-comment pattern rows of `text`, labelled `label:<line>`, and each row that does not
/// compile as `(1-based line, why)`: nothing is held to that row, which its caller reports.
pub(crate) fn file_rows(text: &str, label: &str) -> (Vec<Row>, Vec<(usize, String)>) {
    let (mut rows, mut refused) = (Vec::new(), Vec::new());
    for (i, raw) in text.lines().enumerate() {
        let pattern = raw.trim();
        if pattern.is_empty() || pattern.starts_with('#') {
            continue;
        }
        match Row::new(format!("{label}:{}", i + 1), pattern) {
            Ok(row) => rows.push(row),
            Err(why) => refused.push((
                i + 1,
                format!(
                    "this row does not compile in aterm-regex ({why}), so nothing is held to it"
                ),
            )),
        }
    }
    (rows, refused)
}
