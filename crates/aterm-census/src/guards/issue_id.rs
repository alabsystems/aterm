// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! issue-id: each open issue has one identifier, so a reference names one problem.
//! Canonical level-two definitions exclude fenced code, indented examples and HTML-comment blocks.

use std::collections::BTreeMap;

use super::conflict_marker::{fence_closes, fence_open};
use super::{Finding, Rule, Tree};

pub(super) const ISSUE_ID: Rule = Rule {
    id: "issue-id",
    why: "two open issues with the same identifier make design and review references ambiguous",
    scope: &[],
    check: issue_id,
};

const INDEX: &str = "docs/KNOWN_ISSUES.md";

fn block_line(line: &str) -> Option<&str> {
    let indent = line.bytes().take_while(|&byte| byte == b' ').count();
    (indent <= 3).then_some(&line[indent..])
}

fn heading_id(line: &str) -> Option<&str> {
    let after = block_line(line)?.strip_prefix("##")?;
    if !after.starts_with([' ', '\t']) {
        return None;
    }
    let id = after.split_whitespace().next()?;
    let (date, number) = id.strip_prefix("ATERM-KI-")?.split_once('-')?;
    [date, number]
        .iter()
        .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
        .then_some(id)
}

fn issue_id(tree: &Tree<'_>, out: &mut Vec<Finding>) {
    let Some(text) = tree.text(INDEX) else {
        return;
    };
    let mut seen = BTreeMap::new();
    let mut fence = None;
    let mut comment = false;
    for (at, line) in text.lines().enumerate() {
        if let Some((ch, length)) = fence {
            if fence_closes(line, ch, length) {
                fence = None;
            }
            continue;
        }
        if comment {
            comment = !line.contains("-->");
            continue;
        }
        if block_line(line).is_some_and(|line| line.starts_with("<!--")) {
            comment = !line.contains("-->");
            continue;
        }
        if let Some(open) = fence_open(line) {
            fence = Some(open);
            continue;
        }
        let Some(id) = heading_id(line) else {
            continue;
        };
        let line = at + 1;
        if let Some(first) = seen.get(id) {
            out.push(Finding {
                rule: ISSUE_ID.id,
                path: INDEX.to_owned(),
                line,
                detail: format!(
                    "issue identifier {id} already appears at {INDEX}:{first}; choose a unique \
                     identifier and update its references"
                ),
            });
        } else {
            seen.insert(id, line);
        }
    }
}

#[cfg(test)]
#[path = "issue_id_tests.rs"]
mod tests;
