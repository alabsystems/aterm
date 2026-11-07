// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Test verdicts must not depend on the cutter's environment or the owner's release history.
//! This lexical guard follows direct environment reads and the two ambient release adapters;
//! it does not claim to follow arbitrary calls through a test's dependency graph.

use super::rules::PRODUCT;
use super::{Bound, Finding, Rule, Tree, brace_close, hits, literal, token};

pub(super) const RELEASE_TEST_STATE: Rule = Rule {
    id: "release-test-state",
    why: "inject release state and scheduling context in tests: HOME's release history and \
          an inherited ATERM_RELEASE_RUN must not choose a test's verdict; use a fixture \
          directory, a fixed Machine or an explicit Record",
    scope: &[],
    check,
};

fn check(tree: &Tree<'_>, out: &mut Vec<Finding>) {
    let test_files = tree.corpus.test_code();
    for file in tree.corpus.under(PRODUCT) {
        if !file.mentions(&["HOME", "RELEASE_RUN", "Live", "disk_record", "record_in"]) {
            continue;
        }
        let lexed = file.lexed();
        let code = &lexed.code;
        let shipped = file.shipped_code();
        for at in hits(code, "fn", Bound::Word) {
            let line = code[..at].matches('\n').count() + 1;
            let attr_start = code[..at].rfind([';', '{', '}']).map_or(0, |i| i + 1);
            let is_test = test_files.holds(&file.rel)
                || shipped.as_bytes().get(at) == Some(&b' ')
                || code[attr_start..at].contains("#[test]");
            if !is_test {
                continue;
            }
            let Some(open) = code[at..].find(['{', ';']).map(|i| at + i) else {
                continue;
            };
            if code.as_bytes()[open] != b'{' {
                continue;
            }
            let Some(close) = brace_close(code, open) else {
                continue;
            };
            let body = &code[open..=close];
            let text = &lexed.text[open..=close];
            let compact: String = body.chars().filter(|c| !c.is_whitespace()).collect();
            let direct_marker = ["var", "var_os"].iter().any(|name| {
                hits(body, name, Bound::Word).any(|p| {
                    let rest = &body[p + name.len()..];
                    let Some(end) = rest.find(')') else {
                        return false;
                    };
                    let argument = &text[p + name.len()..p + name.len() + end];
                    argument.contains("ATERM_RELEASE_RUN")
                        || rest[..end].contains("RELEASE_RUN_ENV")
                })
            });
            // A child probe may read the marker it explicitly injects. Ordinary tests must
            // pass the value into their subject instead of reading their launcher's marker.
            let injected_marker = compact.contains(".env(aterm_capacity::RELEASE_RUN_ENV,")
                || compact.contains(".env(RELEASE_RUN_ENV,")
                || injects(body, text, "ATERM_RELEASE_RUN");
            let home_release = text.contains("HOME")
                && (text.contains(".local/state/aterm-release") || body.contains("RELEASE_STATE"))
                && (compact.contains("env::var(") || compact.contains("env::var_os("))
                && [
                    "read",
                    "read_to_string",
                    "read_dir",
                    "load",
                    "open",
                    "metadata",
                ]
                .iter()
                .any(|name| hits(body, name, Bound::Word).next().is_some())
                && !injects(body, text, "HOME");
            // A locally constructed need admitted on Live can silently change rank under
            // the cutter. A need handed into a test's injected admission callback already
            // carries the subject's policy (for example a line's explicit Release rank).
            let live_admission = hits(body, "Live", Bound::Word).next().is_some()
                && compact.contains("Need::new(")
                && (compact.contains("admit_with(") || compact.contains("Waiter::enter("));
            let real_record = compact.contains("Real::new(")
                && (compact.contains(".disk_record(")
                    || compact.contains("release_disk::record_in("))
                && !compact.contains(".state=");
            if (direct_marker && !injected_marker) || home_release || live_admission || real_record
            {
                out.push(file.finding(RELEASE_TEST_STATE.id, line,
                    "test reads ambient release state or scheduling context; inject its home/state, Machine or Record".into()));
            }
        }
    }
}

fn injects(code: &str, text: &str, key: &str) -> bool {
    hits(code, "env", Bound::Word).any(|at| {
        token(code, at + 3, "(")
            .and_then(|open| literal(code, text, open))
            .is_some_and(|(name, _)| name == key)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::guards::tests::run;

    #[test]
    fn ambient_adapters_are_red_and_injected_values_are_green() {
        for body in [
            "let h = std::env::var_os(\"HOME\").unwrap(); read(Path::new(&h).join(\".local/state/aterm-release/disk-footprint.json\"));",
            "let release = std::env::var_os(\"ATERM_RELEASE_RUN\").is_some();",
            "let release = std::env::var_os(aterm_capacity::RELEASE_RUN_ENV).is_some();",
            "aterm_capacity::admit_with(&ledger, &aterm_capacity::Live, Need::new(\"fixture\"), mode);",
            "let mut r = Real::new(repo, None, true, false); assert!(r.disk_record().runs().is_empty());",
            "let r = Real::new(repo, None, true, false); assert!(!crate::release_disk::record_in(&r.state).exists());",
        ] {
            let source = format!("#[cfg(test)] mod tests {{ #[test] fn verdict() {{ {body} }} }}");
            assert_eq!(
                run(&RELEASE_TEST_STATE, &[("crates/a/src/lib.rs", &source)]).len(),
                1,
                "{body}"
            );
        }
        for body in [
            "let h = fixture.path(); read(h.join(\".local/state/aterm-release/disk-footprint.json\"));",
            "child.env(\"HOME\", fixture.path()); let h = std::env::var_os(\"HOME\"); read(h.join(\".local/state/aterm-release/disk-footprint.json\"));",
            "aterm_capacity::admit_with(&ledger, &fixed, need, mode);",
            "let mut r = Real::new(repo, None, true, false); r.state = fixture.path().into(); assert!(r.disk_record().runs().is_empty());",
            "let marker = \"ATERM_RELEASE_RUN\"; // std::env::var_os(marker)\n",
            "let home = std::env::var_os(\"HOME\"); assert!(!logs.starts_with(home.join(RELEASE_STATE)));",
        ] {
            let source = format!("#[test] fn verdict() {{ {body} }}");
            assert!(
                run(&RELEASE_TEST_STATE, &[("crates/a/tests/probe.rs", &source)]).is_empty(),
                "{body}"
            );
        }
        // The live adapters are legitimate production code. Negating test detection must
        // make this control red, and dropping the adapter checks must green the cases above.
        assert!(
            run(
                &RELEASE_TEST_STATE,
                &[(
                    "crates/a/src/lib.rs",
                    "fn production() { admit_with(&ledger, &aterm_capacity::Live, need, mode); }"
                )]
            )
            .is_empty()
        );
    }
}
