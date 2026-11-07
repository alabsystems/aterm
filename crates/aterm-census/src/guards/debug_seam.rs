// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! debug-seam: a debug-only seam's variable is named by no release build.
//!
//! A seam such as `ATERM_DEBUG_NETPLACE` (`aterm_netplace::facts::SEAM`) scripts what the app
//! believes of the Mac (where it is on the network), so the lab can put one instance on a phone
//! hotspot. A release build that read it would let an environment variable lie to every reporter
//! and to the vault's admission, so every mention of it sits in code a release build never
//! compiles: under a `cfg` that entails `debug_assertions` or `test` (`all(debug_assertions,
//! not(test))`, `any(test, debug_assertions)`), never `dev_seam!` (whose `dev-seams` feature a
//! release build can turn on). The rule reads mentions, not reads, since every way to read the
//! variable starts at one (`env::var_os(SEAM)`, an imported `var_os(SEAM)`, `vars_os()` searched
//! for the name, a wrapper handed `facts::SEAM`, a `use` or re-export of it): the seam's quoted
//! name, and each `const` or `static` bound to it, by its bare name in its own crate or in a file
//! that names that crate, and by its module's path (`facts::SEAM`) anywhere. A binding itself is
//! no mention. It fails a mention a release build compiles, and a seam it finds no mention of
//! outside its bindings (renamed: name it here again).
//!
//! `ATERM_TEST_NEGATIVE_CONTROL` is the other seam: it turns a negative control on for a whole
//! lab process (`docs/DESIGN-alab-one-mesh-2026-11-05.md` §9), restoring an old trust rule (the
//! gap rule that read an omitted seat as revoked, a batch read as settled before it is). Its one
//! reader is `aterm_types::negative_control::on`, under `cfg(any(test, debug_assertions))`; a
//! lab lane that sets it on a child names it under the same kind of `cfg`.
//!
//! `ATERM_DEBUG_TOKEN_RUN_SPARE` makes a debug build's token wrapper hand Claude Code a second,
//! unread socket holding the token (`aterm_agent::harness::accounts_token_run::SPARE_SEAM`): the
//! lab's probe of whether a descriptor left open in a session reaches Claude Code's tool children
//! (DESIGN-claude-setup-token S9 lane C). A release build that read it would leave the token open,
//! unread, in a session's descriptors.

use super::rules::{CENSUS_CRATE, PRODUCT};
use super::{Finding, Rule, Tree};
use crate::lex::Lexed;

/// The seams only a debug build may read: each variable's name.
const DEBUG_ONLY: &[&str] = &[
    "ATERM_DEBUG_NETPLACE",
    "ATERM_TEST_NEGATIVE_CONTROL",
    "ATERM_DEBUG_TOKEN_RUN_SPARE",
];

pub(super) const DEBUG_SEAM: Rule = Rule {
    id: "debug-seam",
    why: "a debug-only seam (ATERM_DEBUG_NETPLACE scripts where this Mac is on the network; \
          ATERM_TEST_NEGATIVE_CONTROL restores an old trust rule for a lab process; \
          ATERM_DEBUG_TOKEN_RUN_SPARE leaves an unread token socket open in a session) named by a \
          release build lets an environment variable lie to every reporter and to the vault's \
          admission; name it only under a cfg that entails debug_assertions or test (never \
          dev_seam!, whose dev-seams feature a release build can turn on)",
    scope: &[
        "crates/aterm-netplace/src/facts.rs",
        "crates/aterm-types/src/negative_control.rs",
        "crates/aterm-agent/src/harness/accounts_token_run.rs",
    ],
    check: debug_seam,
};

fn debug_seam(tree: &Tree<'_>, out: &mut Vec<Finding>) {
    let bound = bound_names(tree);
    let test_code = tree.corpus.test_code();
    let mut seen = vec![false; DEBUG_ONLY.len()];
    let words: Vec<&str> = DEBUG_ONLY
        .iter()
        .copied()
        .chain(bound.iter().map(|b| b.name.as_str()))
        .collect();
    for file in tree.corpus.under(PRODUCT) {
        if file.rel.starts_with(CENSUS_CRATE)
            || test_code.holds(&file.rel)
            || !file.mentions(&words)
        {
            continue;
        }
        let lexed = file.lexed();
        let compiled = crate::lock_order::unmasked_lines(&lexed.code, &release_never_compiles);
        for (line, seam) in mentions(&file.rel, lexed, &bound) {
            seen[seam] = true;
            if compiled.get(line).copied().unwrap_or(true) {
                out.push(file.finding(
                    DEBUG_SEAM.id,
                    line + 1,
                    format!(
                        "names `{}` in code a release build compiles",
                        DEBUG_ONLY[seam]
                    ),
                ));
            }
        }
    }
    for (seam, seen) in DEBUG_ONLY.iter().zip(seen) {
        if !seen {
            out.push(Finding {
                rule: DEBUG_SEAM.id,
                path: String::new(),
                line: 0,
                detail: format!(
                    "no mention of `{seam}` was found outside its bindings, so the rule holds \
                     nothing of it (renamed? name its variable here again)"
                ),
            });
        }
    }
}

/// Each line (0-based) of the file `rel`, lexed as `lexed`, that names a seam of [`DEBUG_ONLY`],
/// with the seam's index: its quoted name in code, or a constant of `bound` (bare in its own
/// crate or in a file whose code names that crate, by its module's path anywhere). A line that
/// binds a seam is none.
fn mentions(rel: &str, lexed: &Lexed, bound: &[Bound]) -> Vec<(usize, usize)> {
    let line_of = |at: usize| lexed.code[..at].matches('\n').count();
    let mut out = Vec::new();
    for (seam, name) in DEBUG_ONLY.iter().enumerate() {
        let quoted = format!("\"{name}\"");
        out.extend(
            lexed
                .text
                .match_indices(&quoted)
                .map(|(at, _)| (line_of(at), seam)),
        );
    }
    for b in bound {
        let Some(seam) = DEBUG_ONLY.iter().position(|seam| *seam == b.seam) else {
            continue;
        };
        let bare = rel.starts_with(&b.krate) || names(&lexed.code, &b.crate_ident);
        let qualified = format!("{}::", b.module);
        for (at, _) in lexed.code.match_indices(&b.name) {
            let before = &lexed.code[..at];
            if !whole_word(&lexed.code, at, b.name.len()) {
                continue;
            }
            let by_path = before
                .strip_suffix(&qualified)
                .is_some_and(|head| !head.ends_with(is_ident));
            if bare || by_path {
                out.push((line_of(at), seam));
            }
        }
    }
    out.retain(|(line, _)| !bound.iter().any(|b| b.file == rel && b.line == *line));
    out.sort_unstable();
    out.dedup();
    out
}

/// A `const` or `static` bound to a seam of [`DEBUG_ONLY`] (`pub const SEAM: &str =
/// "ATERM_DEBUG_NETPLACE";`).
struct Bound {
    /// Its name.
    name: String,
    /// The seam.
    seam: &'static str,
    /// The file that binds it, and the line (0-based) that does.
    file: String,
    line: usize,
    /// The module that file is (`facts`), as another file's path names the constant.
    module: String,
    /// The crate's directory (`crates/aterm-netplace/`) and its name in a path
    /// (`aterm_netplace`).
    krate: String,
    crate_ident: String,
}

/// Every `const` or `static` a shipped file binds to a seam of [`DEBUG_ONLY`].
fn bound_names(tree: &Tree<'_>) -> Vec<Bound> {
    let mut out = Vec::new();
    for file in tree.corpus.under(PRODUCT) {
        if file.rel.starts_with(CENSUS_CRATE) || !file.mentions(DEBUG_ONLY) {
            continue;
        }
        for line in file.lines() {
            let Some((head, value)) = line.text.split_once('=') else {
                continue;
            };
            let Some(seam) = DEBUG_ONLY
                .iter()
                .find(|seam| value.trim().trim_end_matches(';').trim() == format!("\"{seam}\""))
            else {
                continue;
            };
            let Some(name) = ["const ", "static "]
                .iter()
                .find_map(|word| {
                    head.match_indices(word)
                        .find(|(at, _)| !head[..*at].ends_with(is_ident))
                        .map(|(at, _)| &head[at + word.len()..])
                })
                .and_then(|rest| {
                    rest.trim_start()
                        .trim_start_matches("mut ")
                        .split(':')
                        .next()
                })
                .map(str::trim)
                .filter(|name| !name.is_empty())
            else {
                continue;
            };
            let (krate, crate_ident) = crate_of(&file.rel);
            out.push(Bound {
                name: name.to_owned(),
                seam,
                file: file.rel.clone(),
                line: line.no - 1,
                module: module_of(&file.rel),
                krate,
                crate_ident,
            });
        }
    }
    out
}

/// The crate a source file is in: its directory (`crates/aterm-netplace/`) and its name in a
/// path (`aterm_netplace`).
pub(super) fn crate_of(rel: &str) -> (String, String) {
    let mut parts = rel.split('/');
    let (top, name) = (
        parts.next().unwrap_or_default(),
        parts.next().unwrap_or_default(),
    );
    (format!("{top}/{name}/"), name.replace('-', "_"))
}

/// The module a source file is (`crates/aterm-netplace/src/facts.rs` is `facts`; a `mod.rs` its
/// directory's; a crate root, `lib.rs` or `main.rs`, its crate's, `-` spelled `_`).
pub(super) fn module_of(rel: &str) -> String {
    let mut parts = rel.trim_end_matches(".rs").rsplit('/');
    let stem = parts.next().unwrap_or_default();
    match stem {
        "mod" => parts.next().unwrap_or_default().to_owned(),
        "lib" | "main" => {
            let _src = parts.next();
            parts.next().unwrap_or_default().replace('-', "_")
        }
        stem => stem.to_owned(),
    }
}

/// Whether `c` can be part of an identifier.
pub(super) fn is_ident(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// Whether the `len` bytes of `text` at `at` are a whole word (`SEAM`; not `SEAMS` or
/// `NO_SEAM`).
pub(super) fn whole_word(text: &str, at: usize, len: usize) -> bool {
    !text[..at].ends_with(is_ident) && !text[at + len..].starts_with(is_ident)
}

/// Whether `text` names the identifier `name` as a whole word.
pub(super) fn names(text: &str, name: &str) -> bool {
    text.match_indices(name)
        .any(|(at, _)| whole_word(text, at, name.len()))
}

/// Whether `attr` (`#[cfg(…)]`) gates what follows out of every release build: its predicate
/// entails `debug_assertions` or `test`.
pub(super) fn release_never_compiles(attr: &str) -> bool {
    attr.strip_prefix("#[cfg(")
        .and_then(|rest| rest.strip_suffix(")]"))
        .is_some_and(entails_debug_or_test)
}

/// Whether the `cfg` predicate `pred` can hold only in a debug or test build: `debug_assertions`
/// or `test` itself, an `all` with one such child, an `any` whose every child is one. A `not`, a
/// feature or a platform proves nothing.
fn entails_debug_or_test(pred: &str) -> bool {
    let pred = pred.trim();
    if pred == "test" || pred == "debug_assertions" {
        return true;
    }
    for (head, every) in [("all(", false), ("any(", true)] {
        if let Some(inner) = pred
            .strip_prefix(head)
            .and_then(|rest| rest.strip_suffix(')'))
        {
            let children = crate::split_top_level_commas(inner);
            if children.is_empty() {
                return false;
            }
            return if every {
                children.iter().all(|child| entails_debug_or_test(child))
            } else {
                children.iter().any(|child| entails_debug_or_test(child))
            };
        }
    }
    false
}

#[cfg(test)]
mod tests {
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
}
