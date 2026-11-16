// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! `vendor-names`: the lines of aterm's non-test Rust source that name an agent vendor, per crate
//! (`docs/HARNESS-CONTRACT.md` §2: aterm keeps no per-application code; each harness carries its
//! vendor's knowledge in its own repository).
//!
//! The count is `git grep -I -i -w -E '<the words>' -- 'crates/**/*.rs'` without any path whose
//! component under `crates/` has `test` in it or is `fixtures`, `testdata` or `benches`, taken
//! over the files on disk: the same count as `docs/DESIGN-harness-interface-2026-11-12.md` §1's
//! command and the tracker's `vendor-names`. The words live in `vendor_words.txt`, which is no
//! `.rs` file, so this census's source counts like any other. It reports the count per crate and
//! holds it to no ceiling: the count goes down with the packages that move vendor knowledge into
//! the harnesses, and a branch that adds a line is not refused for it.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

use crate::CensusOutcome;

/// The vendor words counted, as whole words in any case, one per line.
const VENDOR_WORDS: &str = include_str!("vendor_words.txt");

/// The vendor words, in [`VENDOR_WORDS`]'s order.
pub fn vendor_words() -> impl Iterator<Item = &'static str> {
    VENDOR_WORDS
        .lines()
        .map(str::trim)
        .filter(|w| !w.is_empty())
}

const TITLE: &str = "vendor-names census";

/// Whether `rel` (a path under `crates/`, `/`-separated) is counted: a `.rs` file with no
/// component holding `test`, nor one named `fixtures`, `testdata` or `benches`.
#[must_use]
pub fn counted(rel: &str) -> bool {
    rel.ends_with(".rs")
        && !rel
            .split('/')
            .any(|c| c.contains("test") || matches!(c, "fixtures" | "testdata" | "benches"))
}

/// Whether `line` holds a vendor word as a whole word (letters, digits and `_` make a word), in
/// any case.
#[must_use]
pub fn names_a_vendor(line: &str) -> bool {
    let bytes = line.as_bytes();
    let word_byte = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let mut start = 0;
    while start < bytes.len() {
        if !word_byte(bytes[start]) {
            start += 1;
            continue;
        }
        let mut end = start;
        while end < bytes.len() && word_byte(bytes[end]) {
            end += 1;
        }
        let word = &line[start..end];
        if vendor_words().any(|v| word.eq_ignore_ascii_case(v)) {
            return true;
        }
        start = end;
    }
    false
}

/// The counted lines of every crate under `root/crates`, by crate.
fn count(root: &Path) -> (BTreeMap<String, usize>, Vec<String>) {
    let mut by_crate = BTreeMap::new();
    let mut unread = Vec::new();
    let crates = root.join("crates");
    let mut pending = vec![(crates.clone(), String::new())];
    while let Some((dir, rel)) = pending.pop() {
        let listed = match crate::tree::read_dir_typed(&dir) {
            Ok(listed) => listed,
            Err(e) => {
                unread.push(format!("crates/{rel}: {e}"));
                continue;
            }
        };
        for entry in listed {
            let Ok((path, is_dir)) = entry else {
                unread.push(format!("an entry of crates/{rel}"));
                continue;
            };
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let child = if rel.is_empty() {
                name.to_string()
            } else {
                format!("{rel}/{name}")
            };
            if is_dir {
                if !name.starts_with('.') && name != "target" {
                    pending.push((path, child));
                }
                continue;
            }
            if !counted(&child) {
                continue;
            }
            match crate::tree::read_to_string(&path) {
                Ok(text) => {
                    let n = text.lines().filter(|l| names_a_vendor(l)).count();
                    if n > 0 {
                        let krate = child.split('/').next().unwrap_or_default().to_string();
                        *by_crate.entry(krate).or_insert(0) += n;
                    }
                }
                Err(e) => unread.push(format!("crates/{child}: {e}")),
            }
        }
    }
    (by_crate, unread)
}

/// The census over `root`: each crate's count and the total, reported.
#[must_use]
pub fn run_vendor_names_census(root: &Path) -> CensusOutcome {
    let (by_crate, unread) = count(root);
    let total: usize = by_crate.values().sum();
    let mut log = String::new();
    let _ = writeln!(
        log,
        "=== vendor-names census over {}: lines of non-test source naming a vendor ===",
        root.display()
    );
    let mut rows: Vec<(&String, &usize)> = by_crate.iter().collect();
    rows.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
    for (krate, n) in rows {
        let _ = writeln!(log, "  {n:>6}  {krate}");
    }
    if !unread.is_empty() {
        for why in &unread {
            let _ = writeln!(log, "  ✗ unread: {why}");
        }
        return CensusOutcome::red(TITLE, log, format!("{} path(s) did not read", unread.len()));
    }
    CensusOutcome::green(
        TITLE,
        log,
        format!("{total} line(s) in {} crate(s)", by_crate.len()),
    )
}

#[cfg(test)]
#[path = "vendor_names_tests.rs"]
mod tests;
