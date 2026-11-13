// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::super::tests::{run, spots};
use super::*;

/// Synthetic enum declarations with `extra` variants inserted before `Generic`.
fn enums(extra: &str) -> [(&'static str, String); 2] {
    [
        (
            "crates/aterm-phase/src/reader.rs",
            format!(
                "#[derive(Clone, Copy)]\npub enum Program {{\n    Claude,\n    Codex,\n{extra}    \
                 /// Anything else, or nothing named.\n    Generic,\n}}\n"
            ),
        ),
        (
            "crates/aterm-agent/src/harness/upgrade.rs",
            "pub enum Agent {\n    /// Claude Code.\n    #[default]\n    Claude,\n    \
             /// Codex.\n    Codex,\n}\n"
                .to_string(),
        ),
    ]
}

/// Runs `AGENT_GATE` over `files` alongside the synthetic enum declarations.
fn gate(extra: &str, files: &[(&str, &str)]) -> Vec<Finding> {
    let enums = enums(extra);
    let mut all: Vec<(&str, &str)> = enums
        .iter()
        .map(|(rel, src)| (*rel, src.as_str()))
        .collect();
    all.extend_from_slice(files);
    run(&AGENT_GATE, &all)
}

/// Every non-exhaustive agent test form is flagged on the expected line.
#[test]
fn every_form_of_an_agent_test_is_found() {
    let cases: &[(&str, &str, usize)] = &[
        (
            "==",
            "fn f(a: Program) -> bool {\n    a == Program::Claude\n}\n",
            2,
        ),
        (
            "!= under a path prefix",
            "if st.agent != upgrade::Agent::Codex {\n    return;\n}\n",
            1,
        ),
        (
            "the variant on the left",
            "let on = aterm_phase::Program::Claude == st.program;\n",
            1,
        ),
        (
            "Some(…) compared",
            "if Some(Program::Codex) == self.reader {}\n",
            1,
        ),
        (
            "a wrapped comparison",
            "let off = st.program\n    != Some(&crate::harness::upgrade::Agent::Codex);\n",
            2,
        ),
        (
            "a filter closure",
            "let mine = load().filter(|st| st.agent == Agent::Claude);\n",
            1,
        ),
        (
            "matches!",
            "if matches!(agent, Program::Claude | Program::Codex) {}\n",
            1,
        ),
        (
            "a wrapped matches!",
            "let m = matches!(\n    st.program,\n    Some(Program::Codex)\n);\n",
            3,
        ),
        (
            "if let",
            "if let Some(Agent::Claude) = st.agent {\n    go();\n}\n",
            1,
        ),
        (
            "let … else",
            "let Program::Codex = agent else {\n    return;\n};\n",
            1,
        ),
        (
            "a struct pattern in while let",
            "while let Snap {\n    agent: Agent::Codex,\n    ..\n} = next() {}\n",
            2,
        ),
    ];
    for (why, src, line) in cases {
        let found = gate("", &[("crates/aterm-gui/src/harness_host.rs", src)]);
        assert_eq!(
            spots(&found),
            [format!("crates/aterm-gui/src/harness_host.rs:{line}")],
            "MISS — {why}:\n{src}"
        );
    }
}

#[test]
fn arms_values_prose_and_exempt_files_are_silent() {
    let src = "fn g(agent: Program, st: &State) -> Option<Agent> {\n\
               let lanes = lanes(Program::Claude);\n\
               let snap = Snap { agent: Agent::Codex, tab };\n\
               let first = Program::Claude;\n\
               if lanes(Program::Codex) == lanes {}\n\
               if let Some(row) = rows.get(&Program::Codex) {}\n\
               let on = cfg.on_for(Program::Claude) && st.ok;\n\
               restart_of(st, upgrade::Agent::Claude, tab);\n\
               // agent == Program::Claude would be a gate\n\
               let s = \"matches!(a, Agent::Codex)\";\n\
               match agent {\n    Program::Claude => Some(Agent::Claude),\n    \
               Program::Codex => Some(Agent::Codex),\n    Program::Generic => None,\n}\n}\n\
               #[cfg(test)]\nmod tests {\n    fn t(p: Program) -> bool { p == Program::Claude }\n}\n";
    let gated = "fn f(a: Program) -> bool {\n    a == Program::Claude\n}\n";
    let found = gate(
        "",
        &[
            ("crates/aterm-gui/src/harness_host.rs", src),
            ("crates/aterm-gui/src/harness_host_tests.rs", gated),
            ("crates/aterm-agent/src/harness/codex_alab_drive.rs", gated),
            ("crates/aterm-gui/src/claude_lights.rs", gated),
            ("crates/aterm-agent/tests/t.rs", gated),
            ("crates/atpkg/src/vendors.rs", gated),
        ],
    );
    assert!(found.is_empty(), "{found:?}");
}

/// A newly declared `Program` variant is automatically fenced outside its own module.
#[test]
fn a_new_variant_is_fenced_from_its_declaration() {
    let src = "fn f(a: Program) -> bool {\n    a == Program::Opencode\n}\n\
               fn g(a: Program) -> bool {\n    matches!(a, Program::Opencode)\n}\n";
    let found = gate(
        "    Opencode,\n",
        &[
            ("crates/aterm-gui/src/harness_host.rs", src),
            ("crates/aterm-phase/src/opencode.rs", src),
        ],
    );
    assert_eq!(
        spots(&found),
        [
            "crates/aterm-gui/src/harness_host.rs:2",
            "crates/aterm-gui/src/harness_host.rs:5"
        ]
    );
    assert!(gate("", &[("crates/aterm-gui/src/harness_host.rs", src)]).is_empty());
}

/// Missing enum declarations fail closed with a finding on the declaration file.
#[test]
fn a_missing_declaration_fails_closed() {
    let gated = "fn f(a: Program) -> bool {\n    a == Program::Claude\n}\n";
    let found = run(
        &AGENT_GATE,
        &[
            ("crates/aterm-phase/src/reader.rs", "pub struct Program;\n"),
            ("crates/aterm-gui/src/harness_host.rs", gated),
        ],
    );
    assert_eq!(
        spots(&found),
        [
            "crates/aterm-agent/src/harness/upgrade.rs:0",
            "crates/aterm-phase/src/reader.rs:0"
        ],
        "{found:?}"
    );
}
