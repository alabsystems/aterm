// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! conflict-marker: no tracked non-Rust text file carries a leftover git conflict marker.
//!
//! `trustc` and `trustfmt` refuse a conflict marker in `.rs` code while `.rs` tests may construct
//! synthetic conflict fixtures inside string literals. Markdown, TOML, JSON, YAML, shell scripts
//! and release notes (`CHANGELOG.md`, `SPECS.md`, `docs/*.md`) have no parser holding every line,
//! so a stray `<<<<<<<`, `|||||||`, `=======` or `>>>>>>>` line left behind after resolving a
//! merge can land unseen and ship verbatim in release notes.

use std::sync::atomic::{AtomicUsize, Ordering};

use super::{Finding, Rule, Tree};

pub(super) const CONFLICT_MARKER: Rule = Rule {
    id: "conflict-marker",
    why: "a leftover git merge-conflict marker (`<<<<<<<`, `|||||||`, `=======`, `>>>>>>>`) in \
          `CHANGELOG.md`, docs, manifests or scripts lands unseen by the Rust compiler and can \
          ship verbatim in release notes or corrupt configuration",
    scope: &[],
    check: conflict_marker,
};

const BINARY_EXTS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "webp", "ico", "icns", "woff", "woff2", "ttf", "otf", "wasm",
    "bin", "pdf", "zip", "gz", "zst", "tar", "dmg", "mp3", "mp4", "wav", "ogg",
];

fn skip_path(rel: &str) -> bool {
    if rel.starts_with("vendor/") || rel.ends_with(".rs") {
        return true;
    }
    rel.rsplit_once('.')
        .is_some_and(|(_, ext)| BINARY_EXTS.iter().any(|b| ext.eq_ignore_ascii_case(b)))
}

pub(super) fn fence_open(raw: &str) -> Option<(u8, usize)> {
    let trimmed = raw
        .strip_prefix("   ")
        .or_else(|| raw.strip_prefix("  "))
        .or_else(|| raw.strip_prefix(' '))
        .unwrap_or(raw);
    let first = *trimmed.as_bytes().first()?;
    if first != b'`' && first != b'~' {
        return None;
    }
    let run = trimmed.bytes().take_while(|&b| b == first).count();
    if run < 3 {
        return None;
    }
    if first == b'`' && trimmed[run..].contains('`') {
        return None;
    }
    Some((first, run))
}

pub(super) fn fence_closes(raw: &str, ch: u8, min_len: usize) -> bool {
    let trimmed = raw
        .strip_prefix("   ")
        .or_else(|| raw.strip_prefix("  "))
        .or_else(|| raw.strip_prefix(' '))
        .unwrap_or(raw);
    let run = trimmed.bytes().take_while(|&b| b == ch).count();
    run >= min_len && trimmed[run..].trim_matches([' ', '\t']).is_empty()
}

fn non_divider_marker(raw: &str) -> Option<&'static str> {
    if raw == "<<<<<<<" || raw.starts_with("<<<<<<< ") {
        Some("<<<<<<<")
    } else if raw == "|||||||" || raw.starts_with("||||||| ") {
        Some("|||||||")
    } else if raw == ">>>>>>>" || raw.starts_with(">>>>>>> ") {
        Some(">>>>>>>")
    } else {
        None
    }
}

/// How many paths a reader takes at a time.
const BATCH: usize = 64;

fn conflict_marker(tree: &Tree<'_>, out: &mut Vec<Finding>) {
    let rels: Vec<&str> = tree
        .corpus
        .paths()
        .iter()
        .map(String::as_str)
        .filter(|rel| !skip_path(rel))
        .collect();
    // Opening each file is most of the rule's time, and the kernel overlaps opens, so a few
    // readers take the paths a batch at a time; the batches are put back in the paths' order.
    let next = AtomicUsize::new(0);
    let read = || {
        let mut judged = Vec::new();
        loop {
            let batch = next.fetch_add(1, Ordering::Relaxed);
            let Some(rels) = rels.chunks(BATCH).nth(batch) else {
                return judged;
            };
            let mut found = Vec::new();
            for rel in rels {
                if let Some(body) = tree.text(rel) {
                    judge(rel, &body, &mut found);
                }
            }
            judged.push((batch, found));
        }
    };
    let readers = aterm_cores::available_parallelism().map_or(1, |n| n.get().min(8));
    let mut judged: Vec<(usize, Vec<Finding>)> = crate::memo::threads(|scope| {
        let running: Vec<_> = (1..readers).map(|_| scope.spawn(read)).collect();
        let mut judged = read();
        for reader in running {
            judged.extend(
                reader
                    .join()
                    .unwrap_or_else(|panic| std::panic::resume_unwind(panic)),
            );
        }
        judged
    });
    judged.sort_unstable_by_key(|&(batch, _)| batch);
    out.extend(judged.into_iter().flat_map(|(_, found)| found));
}

/// The findings of the file at `rel`, whose text is `body`.
fn judge(rel: &str, body: &str, out: &mut Vec<Finding>) {
    if !["<<<<<<<", "|||||||", "=======", ">>>>>>>"]
        .iter()
        .any(|m| body.contains(m))
    {
        return;
    }
    let markdown = rel.ends_with(".md");
    let is_changelog = rel == "CHANGELOG.md" || rel.ends_with("/CHANGELOG.md");

    let mut fence: Option<(u8, usize)> = None;
    let mut has_outer_marker = false;
    if markdown && !is_changelog {
        for raw in body.lines() {
            if let Some((ch, len)) = fence {
                if fence_closes(raw, ch, len) {
                    fence = None;
                }
                continue;
            }
            if let Some(opened) = fence_open(raw) {
                fence = Some(opened);
                continue;
            }
            if non_divider_marker(raw).is_some() {
                has_outer_marker = true;
                break;
            }
        }
        fence = None;
    }

    let mut prev_nonblank = false;
    for (idx, raw) in body.lines().enumerate() {
        if markdown {
            if let Some((ch, len)) = fence {
                if fence_closes(raw, ch, len) {
                    fence = None;
                }
                prev_nonblank = false;
                continue;
            }
            if let Some(opened) = fence_open(raw) {
                fence = Some(opened);
                prev_nonblank = false;
                continue;
            }
        }
        let found_marker = non_divider_marker(raw).or_else(|| {
            (raw == "=======" && (!markdown || is_changelog || has_outer_marker || !prev_nonblank))
                .then_some("=======")
        });
        if let Some(marker) = found_marker {
            out.push(Finding {
                rule: CONFLICT_MARKER.id,
                path: rel.to_owned(),
                line: idx + 1,
                detail: format!(
                    "leftover git conflict marker `{marker}`: finish resolving the merge"
                ),
            });
        }
        prev_nonblank = !raw.trim().is_empty();
    }
}

#[cfg(test)]
#[path = "conflict_marker_tests.rs"]
mod tests;
