// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! shown-term: render and find code fetch a session's terminal as shown, never `Session.term` direct.

use super::{Bound, Finding, Rule, Tree, hits, is_word};

const RENDER: &[&str] = &[
    "crates/aterm-gui/src/app_render.rs",
    "crates/aterm-gui/src/app_search.rs",
    "crates/aterm-gui/src/pane_chrome.rs",
];

/// Receivers whose `.term` is already the shown terminal (`App::shown_front`) or a glide's handle compared by pointer, by name.
const SHOWN: &[&str] = &["front_terminal", "st", "glide"];

pub(super) const SHOWN_TERM: Rule = Rule {
    id: "shown-term",
    why: "a rewound pane shows the past screen its Rewind view replayed, so render or find \
          code that \
          reads `Session.term`, or takes `front_terminal_mirror`, draws or counts the live screen \
          over it: fetch through `SessionPool::shown_term`, `SessionPool::shown`, \
          `App::shown_front` or `App::front_shown`, and name `Session::live_term` where the live \
          terminal is meant (a resize, a report)",
    scope: RENDER,
    check: shown_term,
};

fn shown_term(tree: &Tree<'_>, out: &mut Vec<Finding>) {
    for file in tree.corpus.under(RENDER) {
        if tree.corpus.test_code().holds(&file.rel) || !file.mentions(&["term"]) {
            continue;
        }
        let code = file.shipped_code();
        for (at, what) in reads(&code) {
            let no = code[..at].matches('\n').count() + 1;
            out.push(file.finding(SHOWN_TERM.id, no, what));
        }
    }
}

/// Each `.term` field read in `code` off a receiver outside [`SHOWN`], and each `front_terminal_mirror(` call.
fn reads(code: &str) -> Vec<(usize, String)> {
    let b = code.as_bytes();
    let back = |mut i: usize| {
        while i > 0 && b[i - 1].is_ascii_whitespace() {
            i -= 1;
        }
        i
    };
    let fwd = |mut i: usize| {
        while b.get(i).is_some_and(u8::is_ascii_whitespace) {
            i += 1;
        }
        i
    };
    let mut out = Vec::new();
    for at in hits(code, "term", Bound::Word) {
        let dot = back(at);
        if dot == 0 || b[dot - 1] != b'.' || (dot >= 2 && b[dot - 2] == b'.') {
            continue;
        }
        if b.get(fwd(at + "term".len())) == Some(&b'(') {
            continue;
        }
        let end = back(dot - 1);
        let mut start = end;
        while start > 0 && is_word(b[start - 1]) {
            start -= 1;
        }
        let recv = &code[start..end];
        if SHOWN.contains(&recv) {
            continue;
        }
        let shown = if recv.is_empty() {
            "an expression"
        } else {
            recv
        };
        out.push((
            at,
            format!(
                "`.term` off {shown}: a session's live terminal read in render code; fetch it \
                 with `SessionPool::shown_term` (or name `Session::live_term` where live is meant)"
            ),
        ));
    }
    for at in hits(code, "front_terminal_mirror", Bound::Word) {
        if b.get(fwd(at + "front_terminal_mirror".len())) == Some(&b'(') {
            out.push((
                at,
                "`front_terminal_mirror(`: the live front terminal in render code; take \
                 `App::shown_front`"
                    .to_string(),
            ));
        }
    }
    out.sort_unstable_by_key(|(at, _)| *at);
    out
}

#[cfg(test)]
#[path = "shown_term_tests.rs"]
mod tests;
