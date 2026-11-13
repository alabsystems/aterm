// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::super::tests::{run, spots};
use super::*;

/// A seam named in code a release build compiles is a finding, however the code names it
/// (its quoted name, as `vars_os()` searched for it, or `dev_seam!` takes it; the constant
/// bound to it, bare in its own file, imported, glob-imported or through its module's path;
/// a `static` bound to it; a wrapper handed it; a `use` or re-export of it; a call rustfmt
/// wrapped); a mention under a cfg entailing `debug_assertions` or `test`, a test file, the
/// bindings themselves, another variable, another crate's constant of the same name in a file
/// that never names this crate, a word that only contains the constant's name, and prose are
/// not.
/// NEGATIVE CONTROLS: `release_never_compiles` answering `false` whatever the attribute fails
/// this test at the gated reads (facts.rs:9 and :12 become findings); `mentions` without its
/// quoted-name search misses `away.rs:5` (the `vars_os()` search) and `away.rs:3`; and with
/// bare names counted only in the binding's own file it misses `home.rs:3` (the glob import's
/// read) and `home.rs:5` (the alias's).
#[test]
fn a_debug_seam_named_in_code_a_release_build_compiles_is_found() {
    let facts = "pub const SEAM: &str = \"ATERM_DEBUG_NETPLACE\";\n\
                 pub fn scripted() -> Option<String> {\n\
                 \x20   #[cfg(any(test, debug_assertions))]\n\
                 \x20   if let Some(s) = here() {\n\
                 \x20       return Some(s);\n\
                 \x20   }\n\
                 \x20   #[cfg(all(debug_assertions, not(test)))]\n\
                 \x20   {\n\
                 \x20       let file = std::env::var_os(SEAM)?;\n\
                 \x20   }\n\
                 \x20   #[cfg(debug_assertions)]\n\
                 \x20   let _ = std::env::var(\"ATERM_DEBUG_NETPLACE\");\n\
                 \x20   let leak = std::env::var_os(SEAM);\n\
                 \x20   let wrapped = std::env::var_os(\n\
                 \x20       crate::facts::SEAM,\n\
                 \x20   );\n\
                 \x20   let other = std::env::var_os(SEAMS);\n\
                 \x20   let unrelated = std::env::var_os(\"HOME\");\n\
                 \x20   // std::env::var_os(SEAM) in prose\n\
                 \x20   let imported = var_os(SEAM);\n\
                 \x20   None\n\
                 }\n";
    let lab = "fn lab() {\n\
               \x20   #[cfg(any(debug_assertions, feature = \"dev-seams\"))]\n\
               \x20   let a = aterm_types::dev_seam!(\"ATERM_DEBUG_NETPLACE\");\n\
               \x20   let b = std::env::var(aterm_netplace::facts::SEAM);\n\
               \x20   let c = std::env::vars_os().find(|(k, _)| k == \"ATERM_DEBUG_NETPLACE\");\n\
               \x20   let d = crate::fleet_listen::seam_or(facts::SEAM, \"x\");\n\
               }\n";
    let test_file = "fn t() { let _ = std::env::var_os(aterm_netplace::facts::SEAM); }\n";
    let loopback = "pub const SEAM: &str = \"ATERM_DEBUG_LOOPBACK\";\n\
                    fn f() { let _ = std::env::var_os(SEAM); }\n";
    let other_crate = "fn g() {\n\
                       \x20   let _ = std::env::var_os(aterm_net::loopback::SEAM);\n\
                       \x20   let _ = aterm_types::dev_seam!(SEAM);\n\
                       \x20   let _ = std::env::var_os(xfacts::SEAM);\n\
                       \x20   let _ = std::env::var_os(facts::SEAM);\n\
                       }\n";
    let importer = "use aterm_netplace::facts::SEAM;\n\
                    use aterm_netplace::facts::*;\n\
                    fn h() { let _ = std::env::var_os(SEAM); }\n\
                    use aterm_netplace::facts as place;\n\
                    fn i() { let _ = std::env::var_os(place::SEAM); }\n";
    let root = "pub mod facts;\n\
                pub use facts::SEAM;\n";
    let held = "static SCRIPT: &str = \"ATERM_DEBUG_NETPLACE\";\n\
                fn j() { let _ = std::env::var_os(SCRIPT); }\n";
    let found = run(
        &DEBUG_SEAM,
        &[
            ("crates/aterm-types/src/negative_control.rs", READER),
            (
                "crates/aterm-agent/src/harness/accounts_token_run.rs",
                SPARE,
            ),
            ("crates/aterm-netplace/src/facts.rs", facts),
            ("crates/aterm-lab/src/away.rs", lab),
            ("crates/aterm-gui/src/net_place_tests.rs", test_file),
            ("crates/aterm-net/src/loopback.rs", loopback),
            ("crates/aterm-db/src/runtime/isolation.rs", other_crate),
            ("crates/aterm-link/src/home.rs", importer),
            ("crates/aterm-netplace/src/lib.rs", root),
            ("crates/aterm-gui/src/held.rs", held),
        ],
    );
    assert_eq!(
        spots(&found),
        [
            "crates/aterm-db/src/runtime/isolation.rs:5",
            "crates/aterm-gui/src/held.rs:2",
            "crates/aterm-lab/src/away.rs:3",
            "crates/aterm-lab/src/away.rs:4",
            "crates/aterm-lab/src/away.rs:5",
            "crates/aterm-lab/src/away.rs:6",
            "crates/aterm-link/src/home.rs:1",
            "crates/aterm-link/src/home.rs:3",
            "crates/aterm-link/src/home.rs:5",
            "crates/aterm-netplace/src/facts.rs:13",
            "crates/aterm-netplace/src/facts.rs:15",
            "crates/aterm-netplace/src/facts.rs:20",
            "crates/aterm-netplace/src/lib.rs:2",
        ],
        "{found:#?}"
    );
}

/// The token wrapper's spare-socket seam, as `accounts_token_run` binds and reads it: under
/// a `cfg` no release build satisfies.
const SPARE: &str = "#[cfg(all(unix, any(test, debug_assertions)))]\n\
                     pub const SPARE_SEAM: &str = \"ATERM_DEBUG_TOKEN_RUN_SPARE\";\n\
                     #[cfg(all(unix, any(test, debug_assertions)))]\n\
                     fn with_spare() -> bool { std::env::var_os(SPARE_SEAM).is_some() }\n";

/// The negative-control variable's one reader, as `aterm_types::negative_control::on` reads
/// it: under a `cfg` no release build satisfies.
const READER: &str = "#[cfg(any(test, debug_assertions))]\n\
                      pub fn on(name: &str) -> bool {\n\
                      \x20   std::env::var_os(\"ATERM_TEST_NEGATIVE_CONTROL\").is_some_and(|v| v == name)\n\
                      }\n";

/// `ATERM_TEST_NEGATIVE_CONTROL` is named only by its reader and by code a release build
/// never compiles: a lab lane that sets it on a child process under a debug `cfg` passes,
/// and the same lane setting it ungated, and a constant bound to it and read in a release
/// build, are each found.
/// NEGATIVE CONTROL: without `ATERM_TEST_NEGATIVE_CONTROL` in `DEBUG_ONLY` neither is
/// found.
#[test]
fn the_negative_control_variable_is_named_only_by_its_reader() {
    let lane = "fn instance(cmd: &mut Command, control: Option<&str>) {\n\
                \x20   #[cfg(debug_assertions)]\n\
                \x20   if let Some(name) = control {\n\
                \x20       cmd.env(\"ATERM_TEST_NEGATIVE_CONTROL\", name);\n\
                \x20   }\n\
                \x20   cmd.env(\"ATERM_TEST_NEGATIVE_CONTROL\", \"old-gap-rule\");\n\
                }\n";
    let bound = "pub const CONTROLS: &str = \"ATERM_TEST_NEGATIVE_CONTROL\";\n\
                 fn controls() -> bool { std::env::var_os(CONTROLS).is_some() }\n";
    let netplace = "#[cfg(debug_assertions)]\n\
                    fn f() { let _ = std::env::var_os(\"ATERM_DEBUG_NETPLACE\"); }\n";
    let found = run(
        &DEBUG_SEAM,
        &[
            ("crates/aterm-types/src/negative_control.rs", READER),
            (
                "crates/aterm-agent/src/harness/accounts_token_run.rs",
                SPARE,
            ),
            ("crates/aterm-lab/src/mesh_churn.rs", lane),
            (
                "crates/aterm-secrets/src/membership_log/admission.rs",
                bound,
            ),
            ("crates/aterm-netplace/src/facts.rs", netplace),
        ],
    );
    assert_eq!(
        spots(&found),
        [
            "crates/aterm-lab/src/mesh_churn.rs:6",
            "crates/aterm-secrets/src/membership_log/admission.rs:2",
        ],
        "{found:#?}"
    );
    assert!(
        found
            .iter()
            .all(|f| f.detail.contains("ATERM_TEST_NEGATIVE_CONTROL"))
    );
}

/// `ATERM_DEBUG_TOKEN_RUN_SPARE` is named only under a debug `cfg`, as the token wrapper
/// reads it: the lab lane that sets it on a launch under the same kind of `cfg` passes, and an
/// ungated read of the wrapper's constant, through its module's path, is found.
/// NEGATIVE CONTROL: without `ATERM_DEBUG_TOKEN_RUN_SPARE` in `DEBUG_ONLY` the ungated read
/// is not found.
#[test]
fn the_spare_socket_seam_is_named_only_under_a_debug_cfg() {
    let lane = "#[cfg(debug_assertions)]\n\
                fn control(line: &mut String) {\n\
                \x20   line.push_str(\"ATERM_DEBUG_TOKEN_RUN_SPARE=1 \");\n\
                }\n";
    let leak = "fn serve() -> bool {\n\
                \x20   std::env::var_os(accounts_token_run::SPARE_SEAM).is_some()\n\
                }\n";
    let found = run(
        &DEBUG_SEAM,
        &[
            ("crates/aterm-types/src/negative_control.rs", READER),
            (
                "crates/aterm-agent/src/harness/accounts_token_run.rs",
                SPARE,
            ),
            ("crates/aterm-lab/src/setup_token_lab.rs", lane),
            ("crates/aterm-agent/src/harness/relaunch.rs", leak),
            (
                "crates/aterm-netplace/src/facts.rs",
                "#[cfg(debug_assertions)]\n\
                 fn f() { let _ = std::env::var_os(\"ATERM_DEBUG_NETPLACE\"); }\n",
            ),
        ],
    );
    assert_eq!(
        spots(&found),
        ["crates/aterm-agent/src/harness/relaunch.rs:2"],
        "{found:#?}"
    );
}

/// A seam the rule finds no mention of outside its bindings is a finding of its own, so a
/// renamed variable or constant cannot leave the rule holding nothing: here no seam is read,
/// the netplace one renamed and the others absent.
/// NEGATIVE CONTROL: dropping the unseen-seam loop leaves this corpus with no finding, and
/// "a seam with no read is named" fails.
#[test]
fn a_seam_with_no_read_is_named() {
    let found = run(
        &DEBUG_SEAM,
        &[(
            "crates/aterm-netplace/src/facts.rs",
            "pub const SCRIPT: &str = \"ATERM_DEBUG_NETPLACE_V2\";\n\
             fn f() { let _ = std::env::var_os(SCRIPT); }\n",
        )],
    );
    assert_eq!(
        found.len(),
        DEBUG_ONLY.len(),
        "each seam with no read is named: {found:#?}"
    );
    for seam in DEBUG_ONLY {
        assert!(
            found.iter().any(|f| f.line == 0 && f.detail.contains(seam)),
            "{seam} is named: {found:#?}"
        );
    }
}

/// Which cfg predicates keep code out of every release build.
/// NEGATIVE CONTROL: `any(` taking one debug or test child as `all(` does fails at
/// `any(debug_assertions, feature = "dev-seams")`.
#[test]
fn only_a_debug_or_test_predicate_keeps_code_out_of_a_release_build() {
    for (pred, never) in [
        ("debug_assertions", true),
        ("test", true),
        ("all(debug_assertions, not(test))", true),
        ("any(test, debug_assertions)", true),
        ("all(unix, any(test, debug_assertions))", true),
        ("any(debug_assertions, feature = \"dev-seams\")", false),
        ("not(test)", false),
        ("unix", false),
        ("any(test, unix)", false),
        ("all()", false),
    ] {
        assert_eq!(entails_debug_or_test(pred), never, "{pred}");
    }
}
