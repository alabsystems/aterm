// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The forbidden-content rows: the export's own (`publish/forbidden-extra.txt`) and the
//! publication engine's baseline, compiled as the engine's `grep -E` runs them. The scan holds
//! the export's text to them ([`super::forbidden`]), and `capture-privacy` holds decoded test
//! captures to the same rows.

use std::path::{Path, PathBuf};

use aterm_regex::{Regex, RegexBuilder};

/// The export's own rows, relative to the checkout.
pub const EXTRA: &str = "publish/forbidden-extra.txt";

/// The engine's central rows, relative to its checkout.
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

/// The publication engine's checkout as this host names it: `PUBLICATION_ENGINE` when set and
/// not empty, else `$HOME/publication` (where `ship release` and `verify export` look too).
#[must_use]
pub fn engine_dir(var: &dyn Fn(&str) -> Option<std::ffi::OsString>) -> Option<PathBuf> {
    var("PUBLICATION_ENGINE")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| var("HOME").map(|h| PathBuf::from(h).join("publication")))
}

/// The engine's baseline `(where, text)` in the checkout at `engine`, or why it cannot be read.
pub(crate) fn baseline_in(engine: &Path) -> Result<(String, String), String> {
    let path = engine.join(BASELINE);
    let shown = path.display().to_string();
    crate::tree::read_to_string(&path)
        .map(|text| (shown.clone(), text))
        .map_err(|e| format!("{shown}: {e}"))
}

/// The engine's baseline as this process's environment names the engine ([`engine_dir`]).
pub(crate) fn baseline() -> Result<(String, String), String> {
    let engine = engine_dir(&|name| std::env::var_os(name))
        .ok_or("neither PUBLICATION_ENGINE nor HOME names the engine")?;
    baseline_in(&engine)
}
