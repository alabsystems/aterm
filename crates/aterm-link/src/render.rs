// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bus record trust labels, screen quoting label, and single-line terminal escaping.

/// Maximum source characters of a record's content rendered on one line.
pub const TEXT_CAP: usize = 1024;

/// Computes the trust label (`human`, `agent`, or `relayed`) from `src` and `relayed`.
#[must_use]
pub fn trust_of(src: &str, relayed: bool) -> &'static str {
    if relayed {
        return "relayed";
    }
    if src.starts_with("h-") {
        "human"
    } else {
        "agent"
    }
}

/// Trust label for content quoted from a screen rather than received as a message.
pub const SCREEN: &str = "screen";

/// Renders `text` as a single escaped line truncated at `cap` source characters with `…`.
#[must_use]
pub fn safe(text: &str, cap: usize) -> String {
    aterm_codec::tty::one_line(text, cap)
}

/// `words` filled into lines of at most `width` columns, each indented `indent` spaces (a word
/// longer than a line stands alone on its own), joined by newlines with none after the last.
#[must_use]
pub fn fill(words: &[String], indent: usize, width: usize) -> String {
    let pad = " ".repeat(indent);
    let mut lines: Vec<String> = Vec::new();
    let mut line = String::new();
    for w in words {
        if !line.is_empty() && line.len() + 1 + w.len() > width {
            lines.push(std::mem::take(&mut line));
        }
        if line.is_empty() {
            line.push_str(&pad);
        } else {
            line.push(' ');
        }
        line.push_str(w);
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines.join("\n")
}

#[cfg(test)]
#[path = "render_tests.rs"]
mod tests;
