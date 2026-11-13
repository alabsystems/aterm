// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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
            ("crates/aterm-gui/src/app_search.rs", planted),
            ("crates/aterm-gui/src/pane_chrome.rs", planted),
            ("crates/aterm-gui/src/app_input.rs", planted),
        ],
    );
    let lines = [1, 2, 3, 8];
    let mut want: Vec<String> = Vec::new();
    for file in ["app_render.rs", "app_search.rs", "pane_chrome.rs"] {
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
