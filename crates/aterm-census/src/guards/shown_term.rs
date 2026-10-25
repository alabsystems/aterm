// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! shown-term: render code fetches a session's terminal as shown, never `Session.term` direct.

use super::rules::test_path;
use super::{Bound, Finding, Rule, Tree, hits, is_word};

const RENDER: &[&str] = &[
    "crates/aterm-gui/src/app_render.rs",
    "crates/aterm-gui/src/pane_chrome.rs",
];

/// Receivers whose `.term` is already the shown terminal (`App::shown_front`) or a glide's handle compared by pointer, by name.
const SHOWN: &[&str] = &["front_terminal", "st", "glide"];

pub(super) const SHOWN_TERM: Rule = Rule {
    id: "shown-term",
    why: "a rewound pane shows the past screen its Rewind view replayed \
          (docs/DESIGN-aos-hosted-execution-2026-10-21.md §14.4.2), so render code that reads \
          `Session.term`, or takes `front_terminal_mirror`, draws the live screen over it: \
          fetch through `SessionPool::shown_term`, `SessionPool::shown` or `App::shown_front`, \
          and name `Session::live_term` where the live terminal is meant (a resize, a report)",
    scope: RENDER,
    check: shown_term,
};

fn shown_term(tree: &Tree<'_>, out: &mut Vec<Finding>) {
    for file in tree.corpus.under(RENDER) {
        if test_path(&file.rel) || !file.mentions(&["term"]) {
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
mod tests {
    use super::super::tests::{run, spots};
    use super::*;

    #[test]
    fn a_live_term_read_planted_in_render_code_is_found_and_the_shown_fetches_are_not() {
        let planted = "fn a(&self) { let t = self.pool.get(id).unwrap().term.clone(); }\n\
                       fn b(s: &Session) { lock(&s.term); }\n\
                       fn c(&self) { let f = self.front_terminal_mirror(id); }\n\
                       fn d(&self) {\n    let t = app\n        .front_terminal(wid)\n        .expect(\"x\")\n        .term\n        .clone();\n}\n\
                       fn e(front_terminal: &M) { lock(&front_terminal.term); }\n\
                       fn f(&self) { let t = self.pool.shown_term(id); }\n\
                       fn g(s: &Session) { lock(s.live_term()); }\n\
                       fn h(t: &Terminal) { t.term(); let term = 1; let r = 0..term; }\n\
                       fn i(st: &Glide, term: &T) { ptr_eq(&st.term, term); }\n\
                       // s.term is what render code never reads\n\
                       #[cfg(test)]\n\
                       mod tests {\n    fn t(s: &Session) { lock(&s.term); }\n}\n";
        let found = run(
            &SHOWN_TERM,
            &[
                ("crates/aterm-gui/src/app_render.rs", planted),
                ("crates/aterm-gui/src/pane_chrome.rs", planted),
                ("crates/aterm-gui/src/app_input.rs", planted),
            ],
        );
        let lines = [1, 2, 3, 8];
        let mut want: Vec<String> = Vec::new();
        for file in ["app_render.rs", "pane_chrome.rs"] {
            want.extend(
                lines
                    .iter()
                    .map(|l| format!("crates/aterm-gui/src/{file}:{l}")),
            );
        }
        assert_eq!(spots(&found), want, "{found:#?}");
        assert!(found.iter().all(|f| f.rule == "shown-term"));
        assert!(
            found[1].detail.starts_with("`.term` off s:"),
            "{}",
            found[1].detail
        );
        assert!(
            found[0].detail.starts_with("`.term` off an expression:"),
            "{}",
            found[0].detail
        );
    }
}
