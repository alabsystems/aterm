// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use std::collections::BTreeSet;
use std::path::Path;

use super::super::{Corpus, Tree};
use super::{inline_test_module, unread};

const TESTS: &str = "#[cfg(test)]\nmod tests {\n    #[test]\n    fn t() {}\n}\n";

fn corpus(files: &[(String, String)]) -> Corpus {
    let files: Vec<(&str, &str)> = files
        .iter()
        .map(|(p, t)| (p.as_str(), t.as_str()))
        .collect();
    Corpus::of(&files)
}

/// The findings over `files`, as `path:line` spots.
fn spots(files: &[(String, String)]) -> Vec<String> {
    let corpus = corpus(files);
    let tree = Tree::new(
        Path::new("/nowhere"),
        &corpus,
        crate::guards::tests::NO_ENGINE,
    );
    let mut out = Vec::new();
    inline_test_module(&tree, &mut out);
    let mut spots: Vec<String> = out
        .iter()
        .map(|f| format!("{}:{}", f.path, f.line))
        .collect();
    spots.sort();
    spots
}

fn owned(s: &[(&str, &str)]) -> Vec<(String, String)> {
    s.iter()
        .map(|(p, t)| ((*p).to_string(), (*t).to_string()))
        .collect()
}

/// Each inline test module in a file a build without tests opens is a finding, in every package
/// whatever links it: a library's module (an `all(test, …)` gate counts), a binary's root, a
/// `src/bin` binary, a build script, a file a build script names, and a module gated by its own
/// inner `cfg`; so is a `#![cfg(test)]` file a shipped line mounts.
///
/// NEGATIVE CONTROLS: following test-gated loads adds `lib_tests.rs:1` and `rig.rs:1`; leaving out
/// `src/bin` loses `tool.rs:1`; leaving out build scripts loses `build.rs:2`; ignoring what a build
/// script names loses `order.rs:1`, and reading it from the package's folder without resolving `..`
/// loses `pins.rs:1`; recording only outer gates loses `inner.rs:1`; reading literals
/// as code adds `a.rs:5`'s quoted module; reading test targets adds `tests/it.rs:1`.
#[test]
fn inline_test_modules_in_files_a_build_without_tests_opens_are_found() {
    let got = spots(&owned(&[
        ("crates/app/Cargo.toml", "[package]\nname = \"app\"\n"),
        (
            "crates/app/src/main.rs",
            &format!("fn main() {{}}\n{TESTS}"),
        ),
        ("crates/app/src/bin/tool.rs", TESTS),
        (
            "crates/app/build.rs",
            "fn main() {\n    println!(\"cargo:rerun-if-changed=src/order.rs\");\n    \
             println!(\"cargo::rerun-if-changed=../linked/src/pins.rs\");\n}\n",
        ),
        ("crates/linked/src/pins.rs", TESTS),
        ("crates/app/src/order.rs", TESTS),
        ("crates/linked/Cargo.toml", "[package]\nname = \"linked\"\n"),
        (
            "crates/linked/src/lib.rs",
            "pub mod a;\nmod whole;\nmod inner;\n#[cfg(test)]\n#[path = \"lib_tests.rs\"]\n\
             mod tests;\n#[cfg(test)]\nmod rig;\n",
        ),
        (
            "crates/linked/src/a.rs",
            "pub fn f() {}\n#[cfg(all(test, unix))]\nmod t {\n}\n\
             const S: &str = r#\"#[cfg(test)] mod quoted {}\"#;\n",
        ),
        ("crates/linked/src/whole.rs", "#![cfg(test)]\nfn f() {}\n"),
        (
            "crates/linked/src/inner.rs",
            "mod tests {\n    #![cfg(test)]\n    fn t() {}\n}\n",
        ),
        ("crates/linked/src/lib_tests.rs", TESTS),
        ("crates/linked/src/rig.rs", TESTS),
        (
            "crates/linked/build.rs",
            &format!("fn main() {{}}\n{TESTS}"),
        ),
        ("crates/linked/tests/it.rs", TESTS),
        (
            "crates/macros/Cargo.toml",
            "[package]\nname = \"macros\"\n[lib]\nproc-macro = true\n",
        ),
        ("crates/macros/src/lib.rs", TESTS),
    ]));
    assert_eq!(
        got,
        [
            "crates/app/src/bin/tool.rs:1",
            "crates/app/src/main.rs:2",
            "crates/app/src/order.rs:1",
            "crates/linked/build.rs:2",
            "crates/linked/src/a.rs:2",
            "crates/linked/src/inner.rs:1",
            "crates/linked/src/pins.rs:1",
            "crates/linked/src/whole.rs:0",
            "crates/macros/src/lib.rs:1",
        ]
    );
}

/// Out of line, the same test code is no finding.
///
/// NEGATIVE CONTROL: writing `lib.rs`'s module back inline adds `crates/linked/src/lib.rs:2`.
#[test]
fn out_of_line_test_modules_are_no_finding() {
    let got = spots(&owned(&[
        ("crates/linked/Cargo.toml", "[package]\nname = \"linked\"\n"),
        (
            "crates/linked/src/lib.rs",
            "pub fn f() {}\n#[cfg(test)]\n#[path = \"lib_tests.rs\"]\nmod tests;\n",
        ),
        ("crates/linked/src/lib_tests.rs", TESTS),
    ]));
    assert!(got.is_empty(), "{got:?}");
}

/// A file's ownership of its folder is read from every target that compiles it with tests: an
/// included file owns its own, whoever includes it; a shipped file an integration test mounts by
/// `#[path]` is reached both ways; a build script, which never compiles `cfg(test)`, adds nothing
/// for a file it mounts.
///
/// NEGATIVE CONTROLS: an included file taking its includer's ownership gives `inc.rs` `false`;
/// reading ownership from the library, binaries and build script alone gives `m.rs` only
/// `false`; reading it from the build script too, whose `#[path]` mount owns its folder, gives
/// `order.rs` `true` as well.
#[test]
fn ownership_is_read_from_every_build() {
    let files = owned(&[
        ("crates/x/Cargo.toml", "[package]\nname = \"x\"\n"),
        (
            "crates/x/src/lib.rs",
            "pub mod m;\n#[cfg(test)]\nmod order;\n",
        ),
        (
            "crates/x/src/m.rs",
            &format!("include!(\"inc.rs\");\n{TESTS}"),
        ),
        (
            "crates/x/tests/it.rs",
            "#[path = \"../src/m.rs\"]\nmod m;\n",
        ),
        ("crates/x/src/inc.rs", TESTS),
        (
            "crates/x/build.rs",
            "#[path = \"src/order.rs\"]\nmod order;\nfn main() {}\n",
        ),
        ("crates/x/src/order.rs", TESTS),
    ]);
    let corpus = corpus(&files);
    let tree = Tree::new(
        Path::new("/nowhere"),
        &corpus,
        crate::guards::tests::NO_ENGINE,
    );
    let got: Vec<(String, BTreeSet<bool>)> = unread(&tree)
        .into_iter()
        .map(|entry| entry.map(|u| (u.rel, u.owns_dir)).unwrap())
        .collect();
    assert_eq!(
        got,
        [
            ("crates/x/src/inc.rs".to_string(), BTreeSet::from([true])),
            (
                "crates/x/src/m.rs".to_string(),
                BTreeSet::from([false, true])
            ),
            ("crates/x/src/order.rs".to_string(), BTreeSet::from([false])),
        ]
    );
}

/// A package's targets are read from its manifest: a `build` path is its build script and
/// `build = false` none, and `autobins = false` leaves `src/bin` to what `[[bin]]` declares.
///
/// NEGATIVE CONTROLS: reading `build.rs` whatever `build` says loses `tools/gen.rs:2` and adds
/// `crates/off/build.rs:2`; reading `src/bin` under `autobins = false` adds `fixture.rs:1`.
#[test]
fn a_package_s_targets_are_read_from_its_manifest() {
    let got = spots(&owned(&[
        (
            "crates/gen/Cargo.toml",
            "[package]\nname = \"gen\"\nbuild = \"tools/gen.rs\"\nautobins = false\n",
        ),
        ("crates/gen/src/lib.rs", "pub fn f() {}\n"),
        (
            "crates/gen/tools/gen.rs",
            &format!("fn main() {{}}\n{TESTS}"),
        ),
        ("crates/gen/src/bin/fixture.rs", TESTS),
        (
            "crates/off/Cargo.toml",
            "[package]\nname = \"off\"\nbuild = false\n",
        ),
        ("crates/off/src/lib.rs", "pub fn f() {}\n"),
        ("crates/off/build.rs", &format!("fn main() {{}}\n{TESTS}")),
    ]));
    assert_eq!(got, ["crates/gen/tools/gen.rs:2"]);
}
