// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Dependency-free Markdown to plain-text renderer for changelogs.

/// Render a Markdown changelog to plain text: headings lose their `#` markers, bullets become
/// `•` and rules are dropped. Other lines pass through with inline syntax unwrapped; runs of
/// blank lines collapse to one and the result is trimmed.
pub(crate) fn to_plain_text(md: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    for raw in md.lines() {
        let line = raw.trim_end();
        let trimmed = line.trim_start();
        let indent = &line[..line.len() - trimmed.len()];

        if trimmed.is_empty() {
            // Blank line — but never two in a row, and never as the first line.
            if !out.last().map(String::is_empty).unwrap_or(true) {
                out.push(String::new());
            }
            continue;
        }

        if let Some(text) = atx_heading(trimmed) {
            if !out.is_empty() && !out.last().map(String::is_empty).unwrap_or(false) {
                out.push(String::new());
            }
            out.push(inline(text));
            continue;
        }

        if is_hrule(trimmed) {
            continue;
        }

        if let Some(rest) = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
            .or_else(|| trimmed.strip_prefix("+ "))
        {
            out.push(format!("{indent}\u{2022} {}", inline(rest)));
            continue;
        }

        if let Some(rest) = ordered_item(trimmed) {
            out.push(format!("{indent}{rest}"));
            continue;
        }

        out.push(format!("{indent}{}", inline(trimmed)));
    }
    while out.first().map(String::is_empty).unwrap_or(false) {
        out.remove(0);
    }
    while out.last().map(String::is_empty).unwrap_or(false) {
        out.pop();
    }
    out.join("\n")
}

fn atx_heading(s: &str) -> Option<&str> {
    let hashes = s.len() - s.trim_start_matches('#').len();
    if (1..=6).contains(&hashes) {
        let rest = &s[hashes..];
        if let Some(text) = rest.strip_prefix(' ') {
            return Some(text.trim().trim_end_matches('#').trim_end());
        }
    }
    None
}

fn is_hrule(s: &str) -> bool {
    for mark in ['-', '*', '_'] {
        let stripped: String = s.chars().filter(|c| !c.is_whitespace()).collect();
        if stripped.len() >= 3 && stripped.chars().all(|c| c == mark) {
            return true;
        }
    }
    false
}

fn ordered_item(s: &str) -> Option<String> {
    let digits = s.len() - s.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    if digits == 0 {
        return None;
    }
    let rest = &s[digits..];
    let sep = rest
        .strip_prefix(". ")
        .or_else(|| rest.strip_prefix(") "))?;
    Some(format!("{}. {}", &s[..digits], inline(sep)))
}

fn inline(s: &str) -> String {
    let delinked = strip_links(s);
    let mut t = delinked;
    for marker in ["**", "__", "*", "_", "`"] {
        t = strip_paired(&t, marker);
    }
    t
}

fn strip_links(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < s.len() {
        if bytes[i] == b'['
            && let Some(close) = s[i + 1..].find(']').map(|p| i + 1 + p)
            && s.as_bytes().get(close + 1) == Some(&b'(')
            && let Some(paren) = s[close + 2..].find(')').map(|p| close + 2 + p)
        {
            out.push_str(&s[i + 1..close]);
            i = paren + 1;
            continue;
        }
        let ch = s[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

fn strip_paired(s: &str, marker: &str) -> String {
    let count = s.matches(marker).count();
    if count >= 2 && count.is_multiple_of(2) {
        s.replace(marker, "")
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_markup_becomes_plain_text() {
        for (case, md, want) in [
            (
                "a heading loses its markers and gets a blank separator",
                "Intro line\n### Features\n- a",
                "Intro line\n\nFeatures\n\u{2022} a",
            ),
            (
                "bullets become dots, keeping their indent",
                "- top\n  - nested\n* star\n+ plus",
                "\u{2022} top\n  \u{2022} nested\n\u{2022} star\n\u{2022} plus",
            ),
            (
                "ordered lists keep their numbers",
                "1. first\n2. **second**",
                "1. first\n2. second",
            ),
            (
                "rules drop and blank runs collapse",
                "A\n\n\n---\n\n\nB",
                "A\n\nB",
            ),
        ] {
            assert_eq!(to_plain_text(md), want, "{case}");
        }
    }

    #[test]
    fn inline_emphasis_code_and_links_unwrapped() {
        assert_eq!(
            inline("**bold** and _em_ and `code`"),
            "bold and em and code"
        );
        assert_eq!(
            inline("see [the docs](https://x.y) now"),
            "see the docs now"
        );
        assert_eq!(inline("2 * 3 = 6"), "2 * 3 = 6");
    }

    #[test]
    fn trims_and_is_idempotent_on_plain_text() {
        let plain = "Version 0.5.14\n\nJust some notes.";
        assert_eq!(to_plain_text(plain), plain);
        let once = to_plain_text("### Title\n- x");
        assert_eq!(to_plain_text(&once), once);
    }
}
