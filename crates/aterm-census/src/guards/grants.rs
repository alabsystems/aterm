// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Census guard ensuring `PromptGrant` is minted only from configured policy.

use super::rules::{CENSUS_CRATE, PRODUCT, test_path};
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
            || test_path(&file.rel)
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
        && (code.to_ascii_lowercase().contains("occupancy") || has(code, GRANT, Bound::Word))
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
mod tests {
    use super::super::tests::{run, spots};
    use super::*;

    #[test]
    fn a_grant_is_minted_from_configured_policy_alone() {
        let plants = "fn a() { let g = Occupancy::Prompt.grant(); }\n\
                      fn b() -> PromptGrant { PromptGrant { _sealed: () } }\n\
                      fn c(cfg: &Cfg) { if cfg.occupancy.grant().is_some() {} }\n\
                      fn d(row: &Row) { let _ = row.grant(); }\n\
                      // Occupancy::Prompt.grant() in prose\n\
                      const S: &str = \"cfg.occupancy.grant()\";\n\
                      #[cfg(test)]\nmod tests {\n    fn t() { let _ = Occupancy::Prompt.grant(); }\n}\n";
        let policy = "fn c(cfg: &Cfg) { if cfg.occupancy.grant().is_some() {} }\n\
                      fn p(m: Mode) -> bool { Occupancy::grant(m).is_some() }\n";
        let home = "pub(crate) const fn grant(self) -> Option<PromptGrant> {\n    \
                    match self { Occupancy::Prompt => Some(PromptGrant { _sealed: () }), _ => None }\n}\n\
                    pub struct PromptGrant {\n    _sealed: (),\n}\n\
                    fn mint() { let _ = Occupancy::Prompt.grant(); }\n\
                    pub const fn grant(self) -> Option<PromptGrant> { None }\n\
                    pub fn handed(self) -> Option<PromptGrant> { self.grant() }\n\
                    pub fn granted(self) -> bool { self.grant().is_some() }\n";
        let writer = "fn w(cfg: &Cfg) { let Some(grant) = cfg.occupancy.grant() else { return }; }\n\
                      fn t(_grant: &crate::supervise::config::PromptGrant) {}\n\
                      fn v(mode: Occupancy) -> bool {\n    mode\n        .grant()\n        .is_some()\n}\n";
        let agent = "use super::config::Occupancy;\n\
                     fn m(mode: Occupancy) -> bool {\n    mode.grant().is_some()\n}\n\
                     fn w(cfg: &Cfg) -> bool {\n    cfg.policy\n        .occupancy\n        .grant()\n        .is_some()\n}\n";
        let unnamed = "fn d(row: &Row) { let _ = row.grant(); }\n";
        let found = run(
            &PROMPT_GRANT,
            &[
                ("crates/aterm-gui/src/exceptions_host.rs", plants),
                ("crates/aterm-gui/src/occupancy_host.rs", policy),
                ("crates/aterm-gui/src/occupancy_host_tests.rs", plants),
                ("crates/aterm-agent/tests/occupancy.rs", plants),
                ("crates/aterm-census/src/guards/grants.rs", plants),
                (HOME, home),
                (WRITERS[0], writer),
                (WRITERS[1], writer),
                ("crates/aterm-agent/src/supervise/occupancy_door.rs", writer),
                ("crates/aterm-agent/src/supervise/occupancy/door.rs", writer),
                ("crates/aterm-agent/src/supervise/run.rs", agent),
                ("crates/aterm-agent/src/supervise/approvals.rs", unnamed),
            ],
        );
        assert_eq!(
            spots(&found),
            [
                "crates/aterm-agent/src/supervise/config.rs:7",
                "crates/aterm-agent/src/supervise/config.rs:8",
                "crates/aterm-agent/src/supervise/config.rs:9",
                "crates/aterm-agent/src/supervise/occupancy/door.rs:1",
                "crates/aterm-agent/src/supervise/occupancy/door.rs:5",
                "crates/aterm-agent/src/supervise/run.rs:3",
                "crates/aterm-agent/src/supervise/run.rs:8",
                "crates/aterm-gui/src/exceptions_host.rs:1",
                "crates/aterm-gui/src/exceptions_host.rs:2",
                "crates/aterm-gui/src/exceptions_host.rs:3",
                "crates/aterm-gui/src/occupancy_host.rs:1",
                "crates/aterm-gui/src/occupancy_host.rs:2",
            ],
            "{found:#?}"
        );
    }
}
