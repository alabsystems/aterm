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

fn fence_open(raw: &str) -> Option<(u8, usize)> {
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

fn fence_closes(raw: &str, ch: u8, min_len: usize) -> bool {
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

fn conflict_marker(tree: &Tree<'_>, out: &mut Vec<Finding>) {
    for rel in &tree.corpus.paths {
        if skip_path(rel) {
            continue;
        }
        let Some(body) = tree.text(rel) else {
            continue;
        };
        if !["<<<<<<<", "|||||||", "=======", ">>>>>>>"]
            .iter()
            .any(|m| body.contains(m))
        {
            continue;
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
                (raw == "======="
                    && (!markdown || is_changelog || has_outer_marker || !prev_nonblank))
                    .then_some("=======")
            });
            if let Some(marker) = found_marker {
                out.push(Finding {
                    rule: CONFLICT_MARKER.id,
                    path: rel.clone(),
                    line: idx + 1,
                    detail: format!(
                        "leftover git conflict marker `{marker}`: finish resolving the merge"
                    ),
                });
            }
            prev_nonblank = !raw.trim().is_empty();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{run, spots};
    use super::*;

    #[test]
    fn clean_docs_setext_headings_and_fenced_examples_pass() {
        let found = run(
            &CONFLICT_MARKER,
            &[
                (
                    "CHANGELOG.md",
                    "# Changelog\n\n## [Unreleased]\n\n### Fixed\n\n- **Clean entry.**\n",
                ),
                (
                    "docs/GUIDE.md",
                    "Summary\n=======\n\nExample merge conflict:\n\n```diff\n<<<<<<< HEAD\na\n=======\nb\n>>>>>>> main\n```\n",
                ),
                (
                    "crates/a/src/tests.rs",
                    "const FIXTURE: &str = \"<<<<<<< HEAD\\n=======\\n>>>>>>> main\\n\";\n",
                ),
                ("vendor/sub/README.md", ">>>>>>> origin/main\n"),
            ],
        );
        assert_eq!(spots(&found), Vec::<String>::new(), "{found:?}");
    }

    #[test]
    fn leftover_conflict_markers_in_changelog_docs_and_manifests_fail() {
        let found = run(
            &CONFLICT_MARKER,
            &[
                (
                    "CHANGELOG.md",
                    "# Changelog\n\n## [Unreleased]\n\n- entry\n\n>>>>>>> origin/main\n",
                ),
                (
                    "docs/DESIGN.md",
                    "# Design\n\n<<<<<<< HEAD\nours\n||||||| base\nbase\n=======\ntheirs\n>>>>>>> topic\n",
                ),
                ("crates/a/Cargo.toml", "[package]\n=======\nname = \"a\"\n"),
            ],
        );
        assert_eq!(
            spots(&found),
            vec![
                "CHANGELOG.md:7",
                "crates/a/Cargo.toml:2",
                "docs/DESIGN.md:3",
                "docs/DESIGN.md:5",
                "docs/DESIGN.md:7",
                "docs/DESIGN.md:9",
            ],
            "{found:?}"
        );
    }
}
