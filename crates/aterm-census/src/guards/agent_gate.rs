// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! `agent-gate`: requires per-agent decisions on `aterm_phase::Program` and `upgrade::Agent` to use an exhaustive `match` rather than `==`, `!=`, `matches!`, or refutable `let` patterns.

use std::ops::Range;

use super::rules::test_path;
use super::{Bound, Finding, Rule, SourceFile, Tree, after_ws, has, hits, is_word};

/// Crates whose shipped source is checked for per-agent decisions.
const AGENT_CRATES: &[&str] = &[
    "crates/aterm-agent/src/",
    "crates/aterm-gui/src/",
    "crates/aterm-gui-",
    "crates/aterm-phase/src/",
];

/// Declarations of the agent enums; every variant except `Generic` is fenced.
const AGENT_ENUMS: &[(&str, &str)] = &[
    ("crates/aterm-phase/src/reader.rs", "Program"),
    ("crates/aterm-agent/src/harness/upgrade.rs", "Agent"),
];

pub(super) const AGENT_GATE: Rule = Rule {
    id: "agent-gate",
    why: "a new agent variant compiles cleanly past an equality test, a `matches!` or an `if let` \
          on another variant and silently takes its else branch; an exhaustive `match` on \
          Program or upgrade::Agent makes the compiler name the site",
    scope: AGENT_CRATES,
    check: agent_gate,
};

fn agent_gate(tree: &Tree<'_>, out: &mut Vec<Finding>) {
    let mut variants = Vec::new();
    for (rel, name) in AGENT_ENUMS {
        let Some(agents) = tree.corpus.get(rel).and_then(|f| enum_variants(f, name)) else {
            out.push(Finding {
                rule: AGENT_GATE.id,
                path: (*rel).to_string(),
                line: 0,
                detail: format!("no `enum {name}` here to read the agents from"),
            });
            continue;
        };
        variants.extend(
            agents
                .into_iter()
                .filter(|v| v != "Generic")
                .map(|v| format!("{name}::{v}")),
        );
    }
    let tails: Vec<&str> = variants
        .iter()
        .filter_map(|v| v.rfind("::").map(|at| &v[at..]))
        .collect();
    let words: Vec<String> = tails
        .iter()
        .map(|t| t.trim_start_matches(':').to_ascii_lowercase())
        .collect();
    for file in tree.corpus.under(AGENT_CRATES) {
        if test_path(&file.rel) || agents_own(&file.rel, &words) || !file.mentions(&tails) {
            continue;
        }
        let code = file.shipped_code();
        let mut last = 0;
        for (at, variant, form) in gates(&code, &variants) {
            let no = code[..at].matches('\n').count() + 1;
            if no != last {
                out.push(file.finding(AGENT_GATE.id, no, format!("tests `{variant}` by {form}")));
                last = no;
            }
        }
    }
}

/// Returns true if `rel` is inside an agent-specific module or file (`claude`, `codex`, etc.).
fn agents_own(rel: &str, agents: &[String]) -> bool {
    rel.split('/')
        .any(|part| agents.iter().any(|agent| part.contains(agent.as_str())))
}

/// Extracts the variant identifiers declared on `enum <name>` in `file`.
fn enum_variants(file: &SourceFile, name: &str) -> Option<Vec<String>> {
    let mut lines = file.lines();
    lines.find(|l| {
        let code = l.code.trim_start();
        let code = code.strip_prefix("pub ").unwrap_or(code);
        code.strip_prefix("enum ").is_some_and(|rest| {
            rest.trim_start()
                .strip_prefix(name)
                .is_some_and(|tail| tail.trim_start().starts_with('{'))
        })
    })?;
    let mut variants = Vec::new();
    for line in lines {
        let code = line.code.trim();
        if code.starts_with('}') {
            return Some(variants);
        }
        let ident: String = code
            .bytes()
            .take_while(|&b| is_word(b))
            .map(char::from)
            .collect();
        if ident.starts_with(|c: char| c.is_ascii_uppercase()) {
            variants.push(ident);
        }
    }
    None
}

/// Finds non-exhaustive variant checks in `code`, returning `(offset, variant, form)`.
fn gates<'v>(code: &str, variants: &'v [String]) -> Vec<(usize, &'v str, &'static str)> {
    let invocations = matches_args(code);
    let mut found: Vec<_> = variants
        .iter()
        .flat_map(|variant| hits(code, variant, Bound::Word).map(move |at| (at, variant.as_str())))
        .filter_map(|(at, variant)| {
            let start = path_start(code, at);
            let form = if compared(code, start, at + variant.len()) {
                "`==`/`!=`"
            } else if invocations.iter().any(|args| args.contains(&at)) {
                "`matches!`"
            } else if in_let_pattern(code, start) {
                "a refutable `let` pattern"
            } else {
                return None;
            };
            Some((at, variant, form))
        })
        .collect();
    found.sort_unstable();
    found
}

/// Walks backward across leading `segment::` prefixes to the start of the path at `at`.
fn path_start(code: &str, mut at: usize) -> usize {
    let b = code.as_bytes();
    while code[..at].ends_with("::") {
        at -= 2;
        while at > 0 && is_word(b[at - 1]) {
            at -= 1;
        }
    }
    at
}

/// Returns true if `code[start..end]` (including surrounding `&` or `Some(...)`) is an operand of `==` or `!=`.
fn compared(code: &str, start: usize, end: usize) -> bool {
    let mut before = code[..start].trim_end();
    let mut open = 0;
    loop {
        if let Some(rest) = before.strip_suffix('&') {
            before = rest.trim_end();
            continue;
        }
        let some = before
            .strip_suffix('(')
            .map(str::trim_end)
            .and_then(|rest| rest.strip_suffix("Some"))
            .filter(|rest| !rest.bytes().next_back().is_some_and(is_word));
        let Some(rest) = some else {
            break;
        };
        before = rest.trim_end();
        open += 1;
    }
    let mut after = code[end..].trim_start();
    for _ in 0..open {
        let Some(rest) = after.strip_prefix(')') else {
            break;
        };
        after = rest.trim_start();
    }
    before.ends_with("==")
        || before.ends_with("!=")
        || after.starts_with("==")
        || after.starts_with("!=")
}

/// Returns true if the path at `start` appears in a refutable `let` pattern.
fn in_let_pattern(code: &str, start: usize) -> bool {
    let head = &code[..start];
    let from = head.rfind(['=', ';']).map_or(0, |at| at + 1);
    has(&head[from..], "let", Bound::Word)
}

/// Byte ranges of all `matches!` invocations in `code`, including delimiters.
fn matches_args(code: &str) -> Vec<Range<usize>> {
    let b = code.as_bytes();
    let mut out = Vec::new();
    for at in hits(code, "matches", Bound::Word) {
        let Some(args) = after_ws(&code[at + "matches".len()..], "!") else {
            continue;
        };
        let open = code.len() - args.trim_start().len();
        let close = match b.get(open) {
            Some(b'(') => b')',
            Some(b'[') => b']',
            Some(b'{') => b'}',
            _ => continue,
        };
        let mut depth = 0usize;
        let end = (open..b.len())
            .find(|&i| {
                if b[i] == b[open] {
                    depth += 1;
                } else if b[i] == close {
                    depth -= 1;
                }
                depth == 0
            })
            .map_or(b.len(), |i| i + 1);
        out.push(open..end);
    }
    out
}

#[cfg(test)]
mod tests {
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
}
