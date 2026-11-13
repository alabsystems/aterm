// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use std::path::Path;

use super::super::tests::{run, spots};
use super::*;

/// Code a release build compiles that names a debug-only item is a finding: a public seam
/// another crate's production code imports (one `use`, a grouped `use`, a nested group) or
/// calls by a path from that crate, a private one called from its own file (bare or through
/// `self`), a `pub(crate)` one named through its module from another file of its crate, a
/// re-export a release build compiles. A mention under a gate entailing `test` or
/// `debug_assertions` (an item's, a test module's, a parameter's on its own line), in a test
/// file, an integration test or a debug-only module, a bare word outside its file (a local, an
/// import's later use), a path through another module or crate or a type, a field, another
/// crate's item of the same name in a file that never names this crate, a release twin, a
/// method call, a macro, a declaration, a longer word and prose are not, nor is an item gated
/// by `test` alone or by a feature, nor `const _`.
/// NEGATIVE CONTROLS: `seam_gate` answering `false` leaves no finding; reading every file
/// (not only what a release build compiles) adds `fx/sim.rs:1`, `thing_tests.rs:1` and
/// `tests/it.rs:1`; dropping the release-twin filter adds `twin.rs:5`; dropping the
/// method-call skip adds a second `unix.rs:12`; without the gate-on-its-line skip, `unix.rs:16`
/// is one, and without the binding skip `unix.rs:18`; reading `const _` as an item adds
/// `unix.rs:22`; counting bare words outside their file adds `agy_signin.rs:2` and two more
/// `other.rs:2`; and without the type-segment skip, `agy_signin.rs:7` is one.
#[test]
fn a_debug_only_item_named_by_code_a_release_build_compiles_is_found() {
    let unix = "#[cfg(any(test, debug_assertions))]\n\
                pub fn spawn_session_leader_on_pty() {}\n\
                #[cfg(any(test, debug_assertions))]\n\
                fn ensure_above_stdio() {}\n\
                #[cfg(debug_assertions)]\n\
                pub(crate) struct Probe;\n\
                pub fn shipped() { ensure_above_stdio(); }\n\
                #[cfg(any(test, debug_assertions))]\n\
                pub fn gated_caller() { ensure_above_stdio(); let _ = Probe; }\n\
                #[cfg(test)]\n\
                fn only_test() {}\n\
                fn other(x: X) { let p = Probe; x.spawn_session_leader_on_pty(); }\n\
                #[cfg(feature = \"dev-seams\")]\n\
                pub fn featured() {}\n\
                fn uses() { featured(); only_test(); ensure_above_stdio_twin(); }\n\
                fn param(#[cfg(debug_assertions)] probe: Probe) {}\n\
                // ensure_above_stdio() in prose\n\
                struct V { ensure_above_stdio: fn() }\n\
                fn n() { nss::ensure_above_stdio(); Plan::Probe; self::ensure_above_stdio(); }\n\
                #[cfg(any(test, debug_assertions))]\n\
                const _: () = ();\n\
                fn m(x: u8) { match x { _ => {} } }\n";
    let lib = "pub mod unix;\n\
               mod other;\n\
               pub use unix::shipped;\n\
               fn f() { let _ = unix::Probe; }\n\
               #[cfg(any(test, debug_assertions))]\n\
               pub use unix::spawn_session_leader_on_pty;\n\
               #[cfg(test)]\n\
               mod tests { fn t() { super::unix::Probe; } }\n\
               pub(crate) use unix::Probe as Gate;\n";
    let other = "fn g() { ensure_above_stdio(); }\n\
                 fn h() -> Probe { Probe::new(); super::unix::Probe; crate::other::Probe }\n";
    let uds = "mod twin;\n";
    let twin = "#[cfg(debug_assertions)]\n\
                fn sweep_at_exit() {}\n\
                #[cfg(not(debug_assertions))]\n\
                fn sweep_at_exit() {}\n\
                fn exit() { sweep_at_exit(); }\n";
    let agent = "pub mod harness;\n";
    let harness = "pub mod agy_signin;\nmod grouped;\n";
    let signin = "use aterm_pty::spawn_session_leader_on_pty;\n\
                  pub fn run() { spawn_session_leader_on_pty(); }\n\
                  fn path() { aterm_pty::spawn_session_leader_on_pty(); }\n\
                  macro_rules! spawn_session_leader_on_pty { () => {} }\n\
                  fn mac() { spawn_session_leader_on_pty!(); }\n\
                  fn local() { let spawn_session_leader_on_pty = 1; }\n\
                  fn heads() { crate::local::spawn_session_leader_on_pty(); \
                  Plan::spawn_session_leader_on_pty(); \
                  aterm_pty::Plan::spawn_session_leader_on_pty(); }\n";
    let grouped = "fn before() {}\n\
                   use aterm_pty::{\n\
                   \x20   shipped,\n\
                   \x20   spawn_session_leader_on_pty,\n\
                   };\n\
                   use crate::spawn_folder::spawn_session_leader_on_pty;\n\
                   use aterm_pty::{other::{x, spawn_session_leader_on_pty}};\n";
    let gui = "mod thing;\n\
               #[cfg(test)]\n\
               mod thing_tests;\n";
    let thing = "fn f() { aterm_pty::shipped(); }\n\
                 #[cfg(test)]\n\
                 mod tests {\n\
                 \x20   fn t() { aterm_pty::spawn_session_leader_on_pty(); }\n\
                 }\n\
                 #[cfg(any(test, debug_assertions))]\n\
                 fn seam_user() { aterm_pty::spawn_session_leader_on_pty(); }\n";
    let calls = "fn t() { aterm_pty::spawn_session_leader_on_pty(); }\n";
    let fx = "pub mod child;\n\
              #[cfg(any(test, debug_assertions))]\n\
              mod sim;\n";
    let child = "#[cfg(any(test, debug_assertions))]\n\
                 pub struct ScriptedChild;\n";
    let sim = "use super::child::ScriptedChild;\n";
    let net = "mod x;\n";
    let unrelated = "fn spawn_session_leader_on_pty() {}\n\
                     fn h() { spawn_session_leader_on_pty(); }\n";
    let found = run(
        &SEAM_CALLER,
        &[
            ("crates/aterm-pty/src/unix.rs", unix),
            ("crates/aterm-pty/src/lib.rs", lib),
            ("crates/aterm-pty/src/other.rs", other),
            ("crates/aterm-uds/src/lib.rs", uds),
            ("crates/aterm-uds/src/twin.rs", twin),
            ("crates/aterm-agent/src/lib.rs", agent),
            ("crates/aterm-agent/src/harness/mod.rs", harness),
            ("crates/aterm-agent/src/harness/agy_signin.rs", signin),
            ("crates/aterm-agent/src/harness/grouped.rs", grouped),
            ("crates/aterm-gui/src/lib.rs", gui),
            ("crates/aterm-gui/src/thing.rs", thing),
            ("crates/aterm-gui/src/thing_tests.rs", calls),
            ("crates/aterm-gui/tests/it.rs", calls),
            ("crates/agh/src/lib.rs", "pub mod fx;\n"),
            ("crates/agh/src/fx/mod.rs", fx),
            ("crates/agh/src/fx/child.rs", child),
            ("crates/agh/src/fx/sim.rs", sim),
            ("crates/aterm-net/src/lib.rs", net),
            ("crates/aterm-net/src/x.rs", unrelated),
        ],
    );
    assert_eq!(
        spots(&found),
        [
            "crates/aterm-agent/src/harness/agy_signin.rs:1",
            "crates/aterm-agent/src/harness/agy_signin.rs:3",
            "crates/aterm-agent/src/harness/grouped.rs:4",
            "crates/aterm-agent/src/harness/grouped.rs:7",
            "crates/aterm-pty/src/lib.rs:4",
            "crates/aterm-pty/src/lib.rs:9",
            "crates/aterm-pty/src/other.rs:2",
            "crates/aterm-pty/src/unix.rs:7",
            "crates/aterm-pty/src/unix.rs:12",
            "crates/aterm-pty/src/unix.rs:19",
        ],
        "{found:#?}"
    );
}

/// What a release build compiles: each crate root, and what a `mod` on a release-compiled line
/// loads from one (beside a root or a `mod.rs`, under a file's stem, or at its `#[path]`).
/// NEGATIVE CONTROL: resolving every `mod` beside its file (no stem directory) loses
/// `crates/a/src/a/b.rs`.
#[test]
fn a_release_build_compiles_its_roots_and_what_their_mods_load() {
    let corpus = super::super::Corpus::of(&[
        (
            "crates/a/src/lib.rs",
            "pub mod a;\n#[cfg(test)]\nmod t;\n#[path = \"../gen/g.rs\"]\nmod g;\n",
        ),
        (
            "crates/a/src/a.rs",
            "mod b;\n#[cfg(debug_assertions)]\nmod d;\n",
        ),
        ("crates/a/src/a/b.rs", "mod c;\n"),
        ("crates/a/src/a/b/c/mod.rs", ""),
        ("crates/a/src/a/d.rs", ""),
        ("crates/a/src/t.rs", ""),
        ("crates/a/gen/g.rs", ""),
        ("crates/a/src/bin/tool.rs", ""),
        ("crates/a/src/bin/big/main.rs", "mod part;\n"),
        ("crates/a/src/bin/big/part.rs", ""),
        ("crates/a/src/orphan.rs", ""),
        ("crates/a/build.rs", ""),
    ]);
    let tree = Tree::new(
        Path::new("/nonexistent-aterm-census-root"),
        &corpus,
        crate::guards::tests::NO_ENGINE,
    );
    let shipped: Vec<String> = released_files(&Sources::of(&tree)).into_iter().collect();
    assert_eq!(
        shipped,
        [
            "crates/a/build.rs",
            "crates/a/gen/g.rs",
            "crates/a/src/a.rs",
            "crates/a/src/a/b.rs",
            "crates/a/src/a/b/c/mod.rs",
            "crates/a/src/bin/big/main.rs",
            "crates/a/src/bin/big/part.rs",
            "crates/a/src/bin/tool.rs",
            "crates/a/src/lib.rs",
        ]
    );
}

fn seam(name: &str, rel: &str, reach: Reach) -> Seam {
    let (krate, crate_ident) = crate_of(rel);
    Seam {
        name: name.to_owned(),
        file: rel.to_owned(),
        line: 0,
        reach,
        krate,
        crate_ident,
        module: module_of(rel),
        root: crate_root(rel),
    }
}

/// A code view's mentions of the seams are its identifiers equal to a seam's name (not a
/// longer word, a word after `_`, nor a name past 63 bytes with one more letter; a word after
/// a non-ASCII letter is one), one per seam of a name two crates share, each seam in turn
/// and its mentions in offset order: the order the rule's findings take within a line. Which
/// crates it names reads as [`names`] does, an identifier or not.
/// NEGATIVE CONTROLS: leaving the hits in the code's order puts `(2, 9)` second; keeping one
/// seam per name loses seam 0's or seam 2's; a length mask without `min(63)` overflows its
/// shift on the 70-byte name; reading a non-ASCII byte as a word's loses `(0, 203)`.
#[test]
fn a_seams_mentions_are_the_identifiers_equal_to_its_name_in_seam_then_offset_order() {
    let long = "a".repeat(70);
    let gated = [
        seam("probe", "crates/aterm-a/src/lib.rs", Reach::Dependents),
        seam("other", "crates/aterm-a/src/lib.rs", Reach::Crate),
        seam("probe", "crates/aterm-b/src/x.rs", Reach::Dependents),
        seam(&long, "crates/aterm-c/src/lib.rs", Reach::File),
        seam("probe", "crates/a.b/src/lib.rs", Reach::Dependents),
    ];
    let index = Index::of(&gated);
    let code =
        format!("aterm_b::probe(); aterm_a::other(probes, re_probe); probe {long} {long}a éprobe");
    let mentions = index.mentions(&code).expect("the code names seams");
    assert_eq!(
        mentions.hits,
        [
            (0, 9),
            (0, 52),
            (0, 203),
            (1, 27),
            (2, 9),
            (2, 52),
            (2, 203),
            (3, 58),
            (4, 9),
            (4, 52),
            (4, 203),
        ]
    );
    for (s, seam) in gated.iter().enumerate() {
        assert_eq!(
            mentions.names_crate(&index, s, seam, &code),
            names(&code, &seam.crate_ident),
            "{}",
            seam.crate_ident
        );
    }
    assert!(index.mentions("fn probes() { re_probe(); }").is_none());
}

/// Which top-level lines declare an item, and how far its name reaches.
/// NEGATIVE CONTROL: skipping `const` before `fn` reads `const fn z` as a `const` named `fn`.
#[test]
fn a_top_level_line_names_its_item_and_reach() {
    for (line, want) in [
        ("pub fn x(a: u8) {", Some((Reach::Dependents, "x"))),
        ("pub(crate) struct Y;", Some((Reach::Crate, "Y"))),
        ("pub(in crate::a) enum E {", Some((Reach::Crate, "E"))),
        ("const fn z() -> u8 {", Some((Reach::File, "z"))),
        (
            "pub unsafe extern \" \" fn w() {",
            Some((Reach::Dependents, "w")),
        ),
        ("static mut COUNT: u8 = 0;", Some((Reach::File, "COUNT"))),
        (
            "pub type Alias<T> = Vec<T>;",
            Some((Reach::Dependents, "Alias")),
        ),
        ("    pub fn method(&self) {", None),
        ("impl Foo {", None),
        ("pub mod testing;", None),
        ("pub use a::b;", None),
        ("macro_rules! m {", None),
    ] {
        assert!(item(line) == want, "{line}");
    }
}

/// A gate is a seam's when no release build satisfies it and a debug build that is not a
/// test build may.
/// NEGATIVE CONTROL: `in_a_debug_build` answering `None` for `test` makes `#[cfg(test)]` a
/// seam's gate.
#[test]
fn a_seams_gate_admits_a_debug_build_and_no_release_build() {
    for (attr, seam) in [
        ("#[cfg(any(test, debug_assertions))]", true),
        ("#[cfg(debug_assertions)]", true),
        ("#[cfg(all(debug_assertions, not(test)))]", true),
        ("#[cfg(all(debug_assertions, unix))]", true),
        ("#[cfg(test)]", false),
        ("#[cfg(all(test, debug_assertions))]", false),
        ("#[cfg(not(debug_assertions))]", false),
        ("#[cfg(feature = \"dev-seams\")]", false),
        (
            "#[cfg(any(debug_assertions, feature = \"dev-seams\"))]",
            false,
        ),
    ] {
        assert_eq!(seam_gate(&[attr, "fn x() {}"], 1), seam, "{attr}");
    }
}
