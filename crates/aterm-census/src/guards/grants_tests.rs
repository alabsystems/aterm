// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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
