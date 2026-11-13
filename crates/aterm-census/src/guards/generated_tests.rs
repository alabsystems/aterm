// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::super::tests::{run, spots};
use super::*;

const ATTRS: &str = "*.rs text eol=lf\n\
                     # the libc cells\n\
                     libc-oracle/conformance/src/cell_*.rs linguist-generated\n\
                     crates/a/src/tables/** linguist-generated=true\n\
                     crates/a/src/tables/hand.rs -linguist-generated\n";

/// What a generator writes: its header, its seal, a comment and a table.
fn generated(generator: &str) -> String {
    seal::stamp(&format!(
        "// Copyright 2026 Andrew Yates\n// SPDX-License-Identifier: Apache-2.0\n{}\n\n\
         // !! `sigqueue` ABSENT on this cell.\nconst _: () = ();\n",
        seal::line("//", generator)
    ))
    .expect("the head carries a seal line")
}

#[test]
fn a_declared_file_as_its_generator_wrote_it_passes() {
    let cell = generated("`aterm-libc-oracle regen`");
    let table = generated("`gen tables`");
    let found = run(
        &GENERATED_SEAL,
        &[
            (ATTRIBUTES, ATTRS),
            ("libc-oracle/conformance/src/cell_mac.rs", &cell),
            ("crates/a/src/tables/width/l1.rs", &table),
            ("crates/a/src/tables/hand.rs", "fn hand() {}\n"),
            ("crates/a/src/lib.rs", "mod tables;\n"),
        ],
    );
    assert_eq!(spots(&found), Vec::<String>::new(), "{found:?}");
}

#[test]
fn a_pruned_or_unsealed_declared_file_fails() {
    let cell = generated("`aterm-libc-oracle regen`");
    let pruned = cell.replace("// !! `sigqueue` ABSENT on this cell.\n", "");
    let reheaded = cell.replace("`aterm-libc-oracle regen`", "regen");
    let headless = cell
        .lines()
        .filter(|l| !l.contains(MARK))
        .collect::<Vec<_>>()
        .join("\n");
    let found = run(
        &GENERATED_SEAL,
        &[
            (ATTRIBUTES, ATTRS),
            ("libc-oracle/conformance/src/cell_mac.rs", &pruned),
            ("libc-oracle/conformance/src/cell_macx.rs", &reheaded),
            ("libc-oracle/conformance/src/cell_linux.rs", &headless),
            ("crates/a/src/tables/width.rs", &cell),
        ],
    );
    assert_eq!(
        spots(&found),
        [
            "libc-oracle/conformance/src/cell_linux.rs:1",
            "libc-oracle/conformance/src/cell_mac.rs:3",
            "libc-oracle/conformance/src/cell_macx.rs:3",
        ],
        "{found:?}"
    );
}

#[test]
fn a_row_naming_no_file_and_an_undeclared_seal_fail() {
    let table = generated("`gen tables`");
    let found = run(
        &GENERATED_SEAL,
        &[
            (ATTRIBUTES, ATTRS),
            ("crates/b/src/tables.rs", &table),
            ("vendor/x/src/lib.rs", &table),
        ],
    );
    assert_eq!(
        spots(&found),
        [
            ".gitattributes:3",
            ".gitattributes:4",
            "crates/b/src/tables.rs:3"
        ],
        "{found:?}"
    );
}

#[test]
fn patterns_match_as_git_matches_them() {
    assert!(matches(
        "cell_*.rs",
        "libc-oracle/conformance/src/cell_mac.rs"
    ));
    assert!(matches("/crates/a/src/x.rs", "crates/a/src/x.rs"));
    assert!(!matches("crates/a/src/x.rs", "z/crates/a/src/x.rs"));
    assert!(matches("crates/**/gen.rs", "crates/gen.rs"));
    assert!(matches("crates/**/gen.rs", "crates/a/b/gen.rs"));
    assert!(matches("crates/a/**", "crates/a/b/c.rs"));
    assert!(!matches("crates/a/*.rs", "crates/a/b/c.rs"));
    assert!(matches("crates/a/cell_?.rs", "crates/a/cell_x.rs"));
    assert!(!matches("crates/a/cell_?.rs", "crates/a/cell_xy.rs"));
}
