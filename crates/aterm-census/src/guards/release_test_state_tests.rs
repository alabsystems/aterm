// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::guards::tests::{run, spots};

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

/// A `fn` only a test build compiles is a test's without a `#[test]` of its own, with `\n`
/// or `\r\n` line ends alike, and the same `fn` shipped is none.
#[test]
fn a_helper_in_a_test_module_is_a_test() {
    let helper = "fn helper() { let r = std::env::var_os(\"ATERM_RELEASE_RUN\").is_some(); }";
    let gated = format!("fn shipped() {{}}\n\n#[cfg(test)]\nmod tests {{\n    {helper}\n}}\n");
    for source in [gated.clone(), gated.replace('\n', "\r\n")] {
        let found = run(&RELEASE_TEST_STATE, &[("crates/a/src/lib.rs", &source)]);
        assert_eq!(spots(&found), ["crates/a/src/lib.rs:5"], "{source:?}");
    }
    let shipped = format!("fn shipped() {{}}\n\n{helper}\n");
    assert!(run(&RELEASE_TEST_STATE, &[("crates/a/src/lib.rs", &shipped)]).is_empty());
}
