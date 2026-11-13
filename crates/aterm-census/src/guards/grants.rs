// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Census guard ensuring `PromptGrant` is minted only from configured policy.

use super::rules::{CENSUS_CRATE, PRODUCT};
use super::{Bound, Finding, Rule, Tree, after_ws, calls, has, hits};

const HOME: &str = "crates/aterm-agent/src/supervise/config.rs";
const MINT_CRATE: &str = "crates/aterm-agent/";
const WRITERS: &[&str] = &[
    "crates/aterm-agent/src/supervise/occupancy_loop.rs",
    "crates/aterm-agent/src/supervise/turn_end_loop.rs",
];
const DOOR_FILES: &str = "crates/aterm-agent/src/supervise/occupancy_door";
const GRANT: &str = "PromptGrant";

pub(super) const PROMPT_GRANT: Rule = Rule {
    id: "prompt-grant",
    why: "a `PromptGrant` is what lets the harness type an occupancy line into a session, and \
          only the owner's root `[harness] occupancy = \"prompt\"` may give one: outside tests no \
          code but config.rs builds the type, nothing mints one from a literal `Prompt`, and \
          only the loop's writers (occupancy_loop.rs, turn_end_loop.rs, occupancy_door*.rs) ask \
          an occupancy mode for its grant; config.rs hands no grant out past its crate, where the \
          compiler fences the rest",
    scope: &[HOME],
    check: prompt_grant,
};

fn prompt_grant(tree: &Tree<'_>, out: &mut Vec<Finding>) {
    for file in tree.corpus.under(PRODUCT) {
        if file.rel.starts_with(CENSUS_CRATE)
            || tree.corpus.test_code().holds(&file.rel)
            || !file.mentions(&[GRANT, "grant("])
        {
            continue;
        }
        let home = file.rel == HOME;
        let writer = home
            || WRITERS.contains(&file.rel.as_str())
            || (file.rel.starts_with(DOOR_FILES) && file.rel.ends_with(".rs"));
        let fenced = file.rel.starts_with(MINT_CRATE) && file.mentions(&["Occupancy", GRANT]);
        for line in file.shipped_lines() {
            let why = if has(line.code, "Prompt.grant", Bound::Word) {
                Some("mints a grant from a literal `Prompt`, not the owner's configured policy")
            } else if !home && builds_the_type(line.code) {
                Some("builds a `PromptGrant` outside config.rs, its one mint's home")
            } else if home && hands_one_out(line.code) {
                Some("hands a `PromptGrant` past the mint's crate, where no writer is fenced")
            } else if !writer && (asks_a_mode(line.code) || (fenced && calls_grant(line.code))) {
                Some("asks an occupancy mode for its grant outside the loop's writers")
            } else {
                None
            };
            if let Some(why) = why {
                out.push(file.finding(PROMPT_GRANT.id, line.no, why.to_string()));
            }
        }
    }
}

fn builds_the_type(code: &str) -> bool {
    hits(code, GRANT, Bound::Word).any(|at| after_ws(&code[at + GRANT.len()..], "{").is_some())
}

fn asks_a_mode(code: &str) -> bool {
    calls_grant(code)
        && (code
            .as_bytes()
            .windows(b"occupancy".len())
            .any(|w| w.eq_ignore_ascii_case(b"occupancy"))
            || has(code, GRANT, Bound::Word))
}

fn calls_grant(code: &str) -> bool {
    calls(code).contains(&"grant") && !has(code, "fn grant", Bound::Word)
}

fn hands_one_out(code: &str) -> bool {
    hits(code, "fn", Bound::Word).any(|at| {
        let vis = &code[..at];
        has(vis, "pub", Bound::Word)
            && !vis.contains("pub(")
            && code[at..]
                .split_once("->")
                .is_some_and(|(_, ret)| has(ret, GRANT, Bound::Word))
    })
}

#[cfg(test)]
#[path = "grants_tests.rs"]
mod tests;
