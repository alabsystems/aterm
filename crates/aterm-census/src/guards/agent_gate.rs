// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! `agent-gate`: requires per-agent decisions on `aterm_phase::Program` and `upgrade::Agent` to use an exhaustive `match` rather than `==`, `!=`, `matches!`, or refutable `let` patterns.

use std::ops::Range;

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
        if tree.corpus.test_code().holds(&file.rel)
            || agents_own(&file.rel, &words)
            || !file.mentions(&tails)
        {
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
#[path = "agent_gate_tests.rs"]
mod tests;
