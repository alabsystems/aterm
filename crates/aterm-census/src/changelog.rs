// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Shared release-note structural rules, used before landing and before release.

/// The changelog's repo-root-relative path: the notes each release writes into it from the entry
/// files under `changes/` (aterm-verify's `changelog_fragments`), above the releases before.
pub const CHANGELOG_FILE: &str = "CHANGELOG.md";

/// The head of the harness-versions announcement the release derives and writes into its own
/// section (aterm-release's `harness_versions`): generated, so it never counts as a section's
/// release notes.
pub const HARNESS_ENTRY_HEAD: &str = "- **Harness versions since v";

/// What the pre-claim gate learned about a section: `entries`, the count of top-level bullets in the real body, for the transcript only.
#[derive(Debug)]
pub struct GateSummary {
    pub entries: usize,
}

/// Whether this heading names exactly the requested section.
pub fn is_section_header(line: &str, section: &str) -> bool {
    line.strip_prefix("## [")
        .and_then(|rest| rest.strip_prefix(section))
        .and_then(|rest| rest.strip_prefix(']'))
        .is_some_and(|rest| rest.trim().is_empty() || rest.starts_with([' ', '\t', '-']))
}

/// The real body of a section: the lines between `## [<name>]` and the next `## ` heading, with HTML comments (multi-line included), `###` scaffold headings and blank lines removed.
pub fn real_body(text: &str, section: &str) -> Vec<String> {
    let mut in_section = false;
    let mut in_comment = false;
    let mut out = Vec::new();
    for raw in text.lines() {
        if !in_section {
            in_section = is_section_header(raw, section);
            continue;
        }
        if raw.starts_with("## ") {
            break;
        }
        out.extend(body_line(raw, &mut in_comment));
    }
    out
}

/// One body line, excluding HTML comments, scaffold headings and blank lines.
/// `in_comment` carries an open comment across lines, as [`real_body`] does.
pub fn body_line(raw: &str, in_comment: &mut bool) -> Option<String> {
    let rest = if *in_comment {
        let end = raw.find("-->")?;
        *in_comment = false;
        &raw[end + 3..]
    } else {
        raw
    };
    if !rest.contains("<!--") {
        return (!rest.trim_start().starts_with("###") && !rest.trim().is_empty())
            .then(|| rest.to_string());
    }
    let mut line = rest.to_string();
    while let Some(start) = line.find("<!--") {
        match line.rfind("-->").filter(|&end| end >= start + 4) {
            Some(end) => line.replace_range(start..end + 3, ""),
            None => break,
        }
    }
    if let Some(start) = line.find("<!--") {
        line.truncate(start);
        *in_comment = true;
    }
    (!line.trim_start().starts_with("###") && !line.trim().is_empty()).then_some(line)
}

/// Whether the changelog already carries a `## [<version>]` section: the roll's double-roll guard and the claim's "cut elsewhere" abort (ledger.rs) share it.
pub fn has_section(text: &str, version: &str) -> bool {
    text.lines().any(|l| is_section_header(l, version))
}

/// Structural pre-claim gate: `[Unreleased]` exists, has a handwritten entry,
/// and the raw section carries no `'''` or raw control character.
/// Leftover git conflict-marker lines (`<<<<<<<`, `|||||||`, `=======`, `>>>>>>>`) in the raw section are also refused.
pub fn gate_unreleased(text: &str) -> Result<GateSummary, String> {
    gate_section(text, "Unreleased")
}

fn conflict_marker_head(raw: &str) -> Option<&'static str> {
    if raw == "=======" {
        Some("=======")
    } else if raw == "<<<<<<<" || raw.starts_with("<<<<<<< ") {
        Some("<<<<<<<")
    } else if raw == "|||||||" || raw.starts_with("||||||| ") {
        Some("|||||||")
    } else if raw == ">>>>>>>" || raw.starts_with(">>>>>>> ") {
        Some(">>>>>>>")
    } else {
        None
    }
}

/// The 1-based line of the first top-level entry (`- ` or `* `) above the first `## ` heading,
/// outside HTML comments: an entry there belongs to no section, so no release's notes carry it.
fn entry_above_every_section(text: &str) -> Option<usize> {
    let mut in_comment = false;
    for (idx, raw) in text.lines().enumerate() {
        if raw.starts_with("## ") {
            return None;
        }
        let mut line = raw;
        if in_comment {
            match line.find("-->") {
                Some(p) => {
                    line = &line[p + 3..];
                    in_comment = false;
                }
                None => continue,
            }
        }
        if line.starts_with("- ") || line.starts_with("* ") {
            return Some(idx + 1);
        }
        if let Some(start) = line.rfind("<!--") {
            in_comment = !line[start..].contains("-->");
        }
    }
    None
}

/// Keep a Changelog's section order, shared with the release's assembly of `[Unreleased]` from
/// the entry files under `changes/<kind>/` (aterm-verify's `changelog_fragments`).
pub const KINDS: [&str; 6] = [
    "Added",
    "Changed",
    "Deprecated",
    "Removed",
    "Fixed",
    "Security",
];

/// Every structural finding in the section being shipped, plus entries outside all sections.
/// Historical sections remain verbatim; a recut judges its selected historical section.
pub fn findings(text: &str, section: &str, previous_public: Option<&str>) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(line) = entry_above_every_section(text) {
        out.push(format!("{CHANGELOG_FILE} line {line}: an entry above the first `## ` heading belongs to no section — move it under \"## [{section}]\""));
    }
    let Some((start, end)) = section_span(text, section) else {
        out.push(format!(
            "{CHANGELOG_FILE} has no \"## [{section}]\" section"
        ));
        return out;
    };
    if text
        .lines()
        .filter(|line| is_section_header(line, section))
        .count()
        > 1
    {
        out.push(format!("{CHANGELOG_FILE}: duplicate [{section}] section"));
    }
    let body = real_body(text, section);
    if !body.iter().any(|line| {
        (line.starts_with("- ") || line.starts_with("* ")) && !line.starts_with(HARNESS_ENTRY_HEAD)
    }) {
        out.push(format!("the [{section}] section of {CHANGELOG_FILE} has no release notes — comments, blank lines, bare ### scaffolds and the generated harness-versions line do not count"));
    }
    let lines: Vec<_> = text.lines().collect();
    let mut seen = std::collections::BTreeSet::new();
    let mut rank = None;
    let mut harnesses = 0;
    let mut comment = false;
    let mut in_entry = false;
    let mut continuation = false;
    for (idx, &raw) in lines.iter().enumerate().take(end).skip(start + 1) {
        let mut report = |detail: String| {
            out.push(format!(
                "{CHANGELOG_FILE} line {}: [{section}] {detail}",
                idx + 1
            ))
        };
        if raw.contains("'''") {
            report(
                "contains ''' — this would terminate the release manifest's TOML multiline literal"
                    .into(),
            );
        }
        if let Some(marker) = conflict_marker_head(raw) {
            report(format!("contains git conflict marker `{marker}`"));
        }
        if let Some(bad) = raw.chars().find(|c| c.is_control() && *c != '\t') {
            report(format!("contains control character {bad:?}"));
        }
        // Markdown structure inside HTML comments is scaffolding, not an entry or heading.
        let mut visible = String::new();
        let mut rest = raw;
        loop {
            if comment {
                let Some(at) = rest.find("-->") else { break };
                comment = false;
                rest = &rest[at + 3..];
            } else if let Some(at) = rest.find("<!--") {
                visible.push_str(&rest[..at]);
                rest = &rest[at + 4..];
                comment = true;
            } else {
                visible.push_str(rest);
                break;
            }
        }
        let line = visible.as_str();
        if let Some(kind) = line.strip_prefix("### ") {
            let kind = kind.trim();
            if !seen.insert(kind.to_string()) {
                report(format!("duplicate ### {kind} subsection"));
            }
            if let Some(next) = KINDS.iter().position(|k| *k == kind) {
                if rank.is_some_and(|previous| previous > next) {
                    report(format!("### {kind} is out of Keep a Changelog order"));
                }
                rank = Some(rank.map_or(next, |previous: usize| previous.max(next)));
            } else {
                report(format!("unknown subsection ### {kind}"));
            }
        }
        // The file separates multiline entries by blanks. Markdown permits lazy (unindented)
        // continuation lines too; adjacent one-line list items remain ordinary Markdown.
        let bullet = line.starts_with("- ") || line.starts_with("* ");
        if line.starts_with("- ") && continuation {
            report("entry is glued to a non-blank continuation line; separate entries with a blank line".into());
        }
        if bullet {
            in_entry = true;
            continuation = false;
        } else if line.starts_with("### ") {
            in_entry = false;
            continuation = false;
        } else {
            continuation = in_entry && !line.trim().is_empty();
        }
        if section == "Unreleased"
            && let Some(tail) = line.strip_prefix(HARNESS_ENTRY_HEAD)
        {
            harnesses += 1;
            if harnesses > 1 {
                report("duplicate Harness versions since line".into());
            }
            if let Some(expected) = previous_public {
                let actual = tail.split_once(':').map(|(version, _)| version);
                if actual != Some(expected.trim_start_matches('v')) {
                    report(format!(
                        "Harness versions since line must name previous public release v{expected}"
                    ));
                }
            }
        }
    }
    out
}

/// Judge the selected release section through the same rules as the dev set.
/// # Errors
/// Any structural finding, collected so one repair can address every defect.
pub fn gate_section(text: &str, section: &str) -> Result<GateSummary, String> {
    let found = findings(text, section, None);
    if !found.is_empty() {
        return Err(found.join("\n"));
    }
    Ok(GateSummary {
        entries: real_body(text, section)
            .iter()
            .filter(|l| l.starts_with("- ") || l.starts_with("* "))
            .count(),
    })
}

/// The section heading and its exclusive end, as zero-based line indices.
pub fn section_span(text: &str, section: &str) -> Option<(usize, usize)> {
    let mut start = None;
    let mut count = 0;
    for (idx, line) in text.lines().enumerate() {
        count = idx + 1;
        if let Some(s) = start {
            if line.starts_with("## ") {
                return Some((s, idx));
            }
        } else if is_section_header(line, section) {
            start = Some(idx);
        }
    }
    start.map(|s| (s, count))
}

#[cfg(test)]
#[path = "changelog_tests.rs"]
mod tests;
