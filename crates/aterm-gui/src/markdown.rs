// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A tiny, dependency-free Markdown → PLAIN-TEXT prettifier for the "Software Update"
//! alert. The changelog ships as Markdown (`### Features`, `- item`, `**bold**`), but the
//! macOS `NSAlert` `informativeText` is PLAIN text — set the Markdown verbatim and the
//! reader sees the raw `###` / `-` / `**` syntax. [`to_plain_text`] renders it down to
//! clean, legible lines: headings lose their `#` markers (and gain a blank line so they
//! read as sections), list bullets become `•`, and inline emphasis / code / link syntax
//! is unwrapped to its text. It is intentionally SMALL and pure (no allocator-heavy
//! parser, no external crate) — just enough to make a release changelog read well in a
//! one-shot alert, and fully unit-tested so it can't silently regress.

/// Render a Markdown changelog to clean plain text for a plain-text alert body.
/// Deterministic + pure. Unknown / plain lines pass through unchanged (minus inline
/// emphasis). Runs of blank lines collapse to a single blank line, and the result is
/// trimmed, so the alert never opens with stray leading/trailing whitespace.
pub(crate) fn to_plain_text(md: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    for raw in md.lines() {
        let line = raw.trim_end();
        let trimmed = line.trim_start();
        // Leading whitespace (indent) is preserved for nested lists; measured on the
        // ORIGINAL line so a nested "  - x" keeps its two-space indent.
        let indent = &line[..line.len() - trimmed.len()];

        if trimmed.is_empty() {
            // Blank line — but never two in a row, and never as the first line.
            if !out.last().map(String::is_empty).unwrap_or(true) {
                out.push(String::new());
            }
            continue;
        }

        // ATX heading: `#`..`######` + space + text. Strip the markers (and a trailing
        // run of `#`), keep the text, and set it off with a preceding blank line so it
        // reads as a section header in the flat alert body.
        if let Some(text) = atx_heading(trimmed) {
            if !out.is_empty() && !out.last().map(String::is_empty).unwrap_or(false) {
                out.push(String::new());
            }
            out.push(inline(text));
            continue;
        }

        // Horizontal rule (`---`, `***`, `___`): drop it — a plain alert has no room for
        // decorative dividers, and the surrounding blank lines already separate sections.
        if is_hrule(trimmed) {
            continue;
        }

        // Unordered list item: `-`, `*`, or `+` + space → `• `, preserving indent.
        if let Some(rest) = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
            .or_else(|| trimmed.strip_prefix("+ "))
        {
            out.push(format!("{indent}\u{2022} {}", inline(rest)));
            continue;
        }

        // Ordered list item (`1. text`): keep the number, just unwrap inline syntax.
        if let Some(rest) = ordered_item(trimmed) {
            out.push(format!("{indent}{rest}"));
            continue;
        }

        // Plain paragraph line: unwrap inline emphasis/code/links, keep as-is.
        out.push(format!("{indent}{}", inline(trimmed)));
    }
    // Trim leading/trailing blank lines.
    while out.first().map(String::is_empty).unwrap_or(false) {
        out.remove(0);
    }
    while out.last().map(String::is_empty).unwrap_or(false) {
        out.pop();
    }
    out.join("\n")
}

/// `# text` .. `###### text` → `Some("text")` (trailing `#`s stripped); else `None`.
fn atx_heading(s: &str) -> Option<&str> {
    let hashes = s.len() - s.trim_start_matches('#').len();
    if (1..=6).contains(&hashes) {
        let rest = &s[hashes..];
        // A real ATX heading needs a space after the `#`s (else it's e.g. `#123`).
        if let Some(text) = rest.strip_prefix(' ') {
            return Some(text.trim().trim_end_matches('#').trim_end());
        }
    }
    None
}

/// A markdown thematic break: 3+ of `-`, `*`, or `_` (optionally spaced), nothing else.
fn is_hrule(s: &str) -> bool {
    for mark in ['-', '*', '_'] {
        let stripped: String = s.chars().filter(|c| !c.is_whitespace()).collect();
        if stripped.len() >= 3 && stripped.chars().all(|c| c == mark) {
            return true;
        }
    }
    false
}

/// `12. text` → `Some("12. text")` (with inline syntax unwrapped in `text`); else `None`.
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

/// Unwrap inline Markdown to its text: `**b**`/`__b__` → `b`, `*i*`/`_i_` → `i`,
/// `` `code` `` → `code`, and `[text](url)` → `text`. A minimal single-pass unwrapper —
/// it does not try to be a full CommonMark inline parser, just to strip the syntax a
/// changelog actually uses so no literal `*`/`_`/`` ` ``/`[]()` leaks into the alert.
fn inline(s: &str) -> String {
    // Links first: `[text](url)` → `text` (the URL is noise in a plain alert).
    let delinked = strip_links(s);
    // Then emphasis + code markers. Order matters: the 2-char markers before the 1-char
    // ones so `**` isn't seen as two `*`.
    let mut t = delinked;
    for marker in ["**", "__", "*", "_", "`"] {
        t = strip_paired(&t, marker);
    }
    t
}

/// Remove balanced `[text](url)` spans, keeping `text`. Unbalanced brackets pass through.
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
        // Not a link: copy this char (respecting UTF-8 boundaries).
        let ch = s[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// Delete every occurrence of a paired `marker` (e.g. `**`) when it appears an EVEN
/// number of times, so `**bold**` → `bold`. An odd/unbalanced count is left untouched
/// (a lone `*` in prose stays literal rather than eating the rest of the line).
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
    fn headings_lose_markers_and_get_a_blank_separator() {
        let md = "Intro line\n### Features\n- a";
        let out = to_plain_text(md);
        assert_eq!(out, "Intro line\n\nFeatures\n\u{2022} a");
        assert!(!out.contains('#'), "no raw hash: {out:?}");
    }

    #[test]
    fn bullets_become_dots_preserving_indent() {
        let md = "- top\n  - nested\n* star\n+ plus";
        let out = to_plain_text(md);
        assert_eq!(
            out,
            "\u{2022} top\n  \u{2022} nested\n\u{2022} star\n\u{2022} plus"
        );
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
        // A lone asterisk in prose is left literal (odd count).
        assert_eq!(inline("2 * 3 = 6"), "2 * 3 = 6");
    }

    #[test]
    fn ordered_lists_keep_numbers() {
        assert_eq!(
            to_plain_text("1. first\n2. **second**"),
            "1. first\n2. second"
        );
    }

    #[test]
    fn hrules_dropped_and_blank_runs_collapsed() {
        let md = "A\n\n\n---\n\n\nB";
        assert_eq!(to_plain_text(md), "A\n\nB");
    }

    #[test]
    fn trims_and_is_idempotent_on_plain_text() {
        let plain = "Version 0.5.14\n\nJust some notes.";
        assert_eq!(to_plain_text(plain), plain);
        // Idempotent: prettifying already-clean output changes nothing.
        let once = to_plain_text("### Title\n- x");
        assert_eq!(to_plain_text(&once), once);
    }

    #[test]
    fn realistic_changelog_reads_clean() {
        let md = "## What's new\n\n### Features\n- **DSU**: hot-swap without restart\n- faster startup\n\n### Fixes\n- fixed `curl` argv bug\n";
        let out = to_plain_text(md);
        assert!(!out.contains('#') && !out.contains("**") && !out.contains('`'));
        assert!(out.contains("\u{2022} DSU: hot-swap without restart"));
        assert!(out.contains("What's new"));
        assert!(out.contains("Fixes"));
    }
}
