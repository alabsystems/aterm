// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::super::tests::{run, spots};
use super::*;

#[test]
fn the_workspace_scan_refuses_its_planted_fixtures() {
    let lab = "crates/aterm-lab/src/aos_search.rs";
    let sibling = "crates/aterm-effects/src/aos_momentum/search.rs";
    let refused = |path: &str, src: &str| !scan_workspace_file(path, src).is_empty();
    assert!(!refused(
        lab,
        concat!("use aterm_effects::aos_momentum::", "m6_policy::JUDGED;")
    ));
    assert!(!refused(
        lab,
        "use aterm_effects::aos_momentum::{m4, policy};"
    ));
    assert!(!refused(
        sibling,
        concat!("use super::", "m6_policy::JUDGED;")
    ));
    assert!(!refused(
        lab,
        concat!("// because aos_momentum ", "as a whole;")
    ));
    for (path, planted) in [
        (
            lab,
            concat!("use aterm_effects::aos_momentum::", "m6::Store;"),
        ),
        (
            lab,
            concat!("use aterm_effects::aos_momentum::{m4, ", "m6};"),
        ),
        (
            lab,
            concat!("let s = aterm_effects::aos_momentum::", "m6::Store::load;"),
        ),
        (
            lab,
            concat!(
                "let s = aterm_effects::aos_momentum:: ",
                "m6 ::Store::load;"
            ),
        ),
        (lab, concat!("use aterm_effects::aos_momentum::", "m4::*;")),
        (lab, concat!("use aterm_effects::aos_momentum ", "as am;")),
        (
            lab,
            concat!("use aterm_effects::aos_momentum::{self ", "as am};"),
        ),
        (
            lab,
            concat!("use aterm_effects::{aos_momentum::", "m6, x};"),
        ),
        (sibling, concat!("use super::", "m6::Store;")),
        (sibling, concat!("use super::{m4, ", "m6};")),
        (sibling, concat!("let s = super::", "m6::Store::load;")),
        (sibling, concat!("use self::super::", "m6::Store;")),
        (
            MOMENTUM_RS,
            concat!("fn f() { let s = self::", "m6::Store::load; }"),
        ),
        (MOMENTUM_RS, concat!("pub use ", "m6::{Ranker, Store};")),
        (MOMENTUM_RS, concat!("pub use ", "m6 as learner;")),
        (sibling, concat!("use super::*;\nuse ", "m6::Store;")),
        (sibling, concat!("fn f() { return super::", "m6::f(); }")),
        (sibling, concat!("reach!(super::", "m6, Store);")),
        (sibling, concat!("pub(in super::", "m6) fn f() {}")),
        (sibling, concat!("use super::", "m6 as k;")),
        (lab, concat!("fn f() { return aos_momentum::", "m6::f(); }")),
        (
            sibling,
            concat!("use super::*;\nfn f() { let _ = ", "m6::Store::load; }"),
        ),
    ] {
        assert!(refused(path, planted), "{path}: {planted}");
    }
    assert!(!refused(
        MOMENTUM_RS,
        concat!(
            "#[cfg(any(test, feature = \"aos-learn\"))]\npub mod ",
            "m6;\npub mod m6_policy;"
        )
    ));

    let scan = |path: &str, src: &str| scan_confinement(path, src);
    let test_src = "crates/aterm-effects/src/aos_momentum/m6_tests.rs";
    let real = concat!("Real", "Sets");
    for (path, planted) in [
        (test_src, format!("fn f() {{ let _ = {real}::new(); }}")),
        (M4_TESTS_RS, format!("fn f(_: {real}) {{}}")),
        (
            M4_TESTS_RS,
            concat!("let t = LookToken::mint_", "real(&s, 1, &l);").to_owned(),
        ),
        (
            M4_TESTS_RS,
            concat!("let m = LookToken::mint_", "real;").to_owned(),
        ),
        (
            test_src,
            concat!("let t = LookToken::mint_", "practice(s, &l);").to_owned(),
        ),
        (
            test_src,
            concat!("let args = [\"session\", \"--", "pair\", \"1\"];").to_owned(),
        ),
        (lab, format!("fn f(_: {real}) {{}}")),
        (
            M6_RS,
            format!("#[cfg(test)]\nfn f() {{ let _ = {real}::new(); }}"),
        ),
        (
            M6_RS,
            format!("/// ```\n/// let s = {real}::new();\n/// ```\nfn f() {{}}"),
        ),
        (
            LEARN_RS,
            concat!(
                "#[cfg(test)]\nmod tests {\n    const A: &str = \"--",
                "pair\";\n}\n"
            )
            .to_owned(),
        ),
        (
            "crates/aterm-lab/tests/learn.rs",
            concat!("const A: &str = \"--", "pair\";").to_owned(),
        ),
        (
            M6_RS,
            format!(
                "#[cfg(all(test, target_os = \"macos\"))]\nmod t {{\n    fn f() {{ let _ = {real}::new(); }}\n}}"
            ),
        ),
        (
            M6_RS,
            format!("#[test]\nfn f() {{ let _ = {real}::new(); }}"),
        ),
        (
            M6_RS,
            format!("#![cfg(test)]\nfn f() {{ let _ = {real}::new(); }}"),
        ),
        (
            M6_RS,
            format!(
                "fn f() {{\n    #[cfg(test)]\n    let c = if x {{ None }} else {{ Some({real}::new()) }};\n}}"
            ),
        ),
        (
            M6_RS,
            format!(
                "fn f() {{\n    #[cfg(test)]\n    let s = Probe {{ n: 0 }}.with({real}::new());\n}}"
            ),
        ),
        (
            M4_RS,
            concat!(
                "#[cfg(all(unix, test))]\nfn f() { let _ = LookToken::mint_",
                "real(&s, 1, &l); }"
            )
            .to_owned(),
        ),
        (
            LEARN_RS,
            concat!(
                "#[cfg(all(test, target_os = \"macos\"))]\nmod t {\n    const A: &str = \"--",
                "pair\";\n}\n"
            )
            .to_owned(),
        ),
        (
            "crates/aterm-effects/src/aos_momentum/test_fixtures.rs",
            concat!(
                "pub(super) const A: [&str; 2] = [\"session\", \"--",
                "pair\"];"
            )
            .to_owned(),
        ),
    ] {
        assert!(!scan(path, &planted).is_empty(), "{path}: {planted}");
    }
    for (path, allowed) in [
        (M4_RS, format!("pub struct {real}(NonZeroU64);")),
        (
            M6_RS,
            format!("pub fn real(log: L, i: u32, sets: {real}) {{}}"),
        ),
        (
            M6_RS,
            concat!("let t = LookToken::mint_", "real(real, i, &list)?;").to_owned(),
        ),
        (
            M4_TESTS_RS,
            concat!("let t = LookToken::mint_", "practice(s, &l);").to_owned(),
        ),
        (
            LEARN_RS,
            concat!("match a { \"--", "pair\" => {} _ => {} }").to_owned(),
        ),
        (
            LEARN_BENCH_RS,
            concat!("let args = [\"session\", \"--", "pair\", &i];").to_owned(),
        ),
        (
            M4_RS,
            format!(
                "#[cfg(any(test, debug_assertions, feature = \"aos-learn\"))]\npub fn mint(_: &{real}) {{}}"
            ),
        ),
        (
            M6_RS,
            format!(
                "fn f() {{\n    #[cfg(test)]\n    let v = match x {{ _ => 1 }};\n}}\nfn g(_: {real}) {{}}"
            ),
        ),
        (
            M6_RS,
            format!("#[cfg(test)]\npub(super) fn f() {{}}\nfn g(_: {real}) {{}}"),
        ),
        (
            test_src,
            concat!("let a = [\"--", "pairs\", \"2\"];").to_owned(),
        ),
    ] {
        assert_eq!(
            scan(path, &allowed),
            Vec::<String>::new(),
            "{path}: {allowed}"
        );
    }
}

/// The rule's own control: a file outside the namers that says the capability is found by its
/// path, and the design's count of it in a file allowed to name it is held.
#[test]
fn the_rule_finds_a_name_by_its_path() {
    let real = concat!("Real", "Sets");
    let m4 = format!("pub struct {real};\n").repeat(7);
    let m6 = format!("use super::m4::{real};\n").repeat(4);
    let clean = [
        (M4_RS, m4.as_str()),
        (M4_TESTS_RS, "fn t() {}\n"),
        (M6_RS, m6.as_str()),
    ];
    assert_eq!(run(&M6_NAMES, &clean), Vec::<Finding>::new());
    let stray = format!("fn f(_: {real}) {{}}\n");
    let mut planted = clean.to_vec();
    planted.push(("crates/aterm-lab/src/aos_search.rs", stray.as_str()));
    assert_eq!(
        spots(&run(&M6_NAMES, &planted)),
        ["crates/aterm-lab/src/aos_search.rs:0"]
    );
    let m4_more = format!("{m4}pub struct {real};\n");
    let mut grown = clean.to_vec();
    grown[0] = (M4_RS, m4_more.as_str());
    assert_eq!(spots(&run(&M6_NAMES, &grown)), [format!("{M4_RS}:0")]);
}
