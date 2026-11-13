// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::super::tests::{run, spots};
use super::*;
use crate::mirror::{Item, item_digest, render};

const MANIFEST: &str = "[package]\nname = \"aterm-x\"\n";
const OTHER: &str = "[package]\nname = \"aterm-y\"\n";
const LIB: &str = "pub const CAP: u8 = 2;\n\
    pub fn level(x: u8) -> u8 {\n    if x > CAP && x != 9 { CAP } else { x }\n}\n";
const STATEMENT: &str = "level(x) = min(x, 2)";
const PROVED: &str = "Proved via mirror: Clean R213 row `x_level`";
const STALE_NOTE: &str = "Stale proof: Clean R213 row `x_level` (the clause moved)";

fn specs(statement: &str, note: &str) -> String {
    format!(
        "# aterm-x\n\nid\tkind\tfunctions\tstatement\tgaps\n\
         x-level\ttable\tlevel\t{statement}\t{note}\n\
         x-other\ttable\tother\tother(x) = x\t\n"
    )
}

fn row(name: &str, clause: &str) -> Row {
    let clause_digest = mirror::clauses(&specs(STATEMENT, ""))
        .into_iter()
        .find(|c| c.id == clause)
        .map_or_else(|| NO_CLAUSE.to_string(), |c| c.digest());
    Row {
        clean: "R213".into(),
        row: name.into(),
        krate: "aterm-x".into(),
        clause: clause.into(),
        aterm: "abc".into(),
        harness: "d2b_rows/x.rs".into(),
        items: vec![
            Item::parse("crates/x/src/lib.rs#CAP").expect("an item"),
            Item::parse("crates/x/src/lib.rs#level").expect("an item"),
        ],
        digests: ["CAP", "level"]
            .iter()
            .map(|p| item_digest(LIB, p).expect("the item is there"))
            .collect(),
        clause_digest,
    }
}

fn registry() -> String {
    render(&[row("x_level", "x-level")])
}

fn check(lib: &str, specs: &str, registry: &str) -> Vec<String> {
    let mut files = vec![
        ("crates/x/Cargo.toml", MANIFEST),
        ("crates/x/src/lib.rs", lib),
        ("crates/x/SPECS.md", specs),
        ("crates/y/Cargo.toml", OTHER),
    ];
    if !registry.is_empty() {
        files.push((REGISTRY, registry));
    }
    spots(&run(&MIRROR_ROWS, &files))
}

const CLEAN: [&str; 0] = [];

#[test]
fn a_recorded_row_whose_code_and_clause_hold_passes() {
    assert_eq!(check(LIB, &specs(STATEMENT, PROVED), &registry()), CLEAN);
}

#[test]
fn a_whitespace_or_comment_edit_keeps_the_proof() {
    let reformatted = "/// Doc.\npub const CAP: u8 = 2; // the cap\n\
        pub fn level(\n    x: u8,\n) -> u8 {\n    // why\n    if x > CAP && x != 9 {\n        CAP\n    } else {\n        x\n    }\n}\n";
    let respaced = specs("level(x)  =  min(x, 2)", PROVED);
    assert_eq!(check(reformatted, &respaced, &registry()), CLEAN);
}

#[test]
fn a_body_edit_trips_it_at_the_clause() {
    for edited in [
        LIB.replace("x > CAP", "x >= CAP"),
        LIB.replace("x > CAP && x", "x > CAP & &x"),
    ] {
        assert_eq!(
            check(&edited, &specs(STATEMENT, PROVED), &registry()),
            ["crates/x/SPECS.md:4"],
            "{edited}"
        );
    }
}

#[test]
fn a_constant_the_function_reads_is_part_of_the_proof() {
    let edited = LIB.replace("CAP: u8 = 2", "CAP: u8 = 3");
    assert_eq!(
        check(&edited, &specs(STATEMENT, PROVED), &registry()),
        ["crates/x/SPECS.md:4"]
    );
}

#[test]
fn a_clause_edit_trips_it() {
    assert_eq!(
        check(LIB, &specs("level(x) = min(x, 3)", PROVED), &registry()),
        ["crates/x/SPECS.md:4"]
    );
    let refunctioned = specs(STATEMENT, PROVED).replace("\tlevel\t", "\tlevel, cap\t");
    assert_eq!(
        check(LIB, &refunctioned, &registry()),
        ["crates/x/SPECS.md:4"]
    );
}

#[test]
fn a_stale_note_naming_the_row_owns_its_drift() {
    let edited = LIB.replace("x > CAP", "x >= CAP");
    assert_eq!(
        check(&edited, &specs(STATEMENT, STALE_NOTE), &registry()),
        CLEAN
    );
}

#[test]
fn a_stale_note_excuses_only_the_rows_it_names() {
    let two = render(&[row("x_level", "x-level"), row("x_level_b", "x-level")]);
    let edited = LIB.replace("x > CAP", "x >= CAP");
    let one_stale = format!("{STALE_NOTE}; Proved via mirror: Clean R213 row `x_level_b`");
    assert_eq!(
        check(&edited, &specs(STATEMENT, &one_stale), &two),
        ["crates/x/SPECS.md:4"],
        "x_level_b is still claimed"
    );
    let loose = format!("{PROVED}, `x_level_b` (one part is a stale proof)");
    assert_eq!(
        check(&edited, &specs(STATEMENT, &loose), &two),
        ["crates/x/SPECS.md:4", "crates/x/SPECS.md:4"],
        "a loose \"stale proof\" names no row: both rows stay claimed, and both moved"
    );
    let in_statement = specs(&format!("{STATEMENT} (not a {STALE_NOTE})"), PROVED);
    assert!(
        check(&edited, &in_statement, &registry()).len() >= 2,
        "a note in the statement column is no note"
    );
}

#[test]
fn an_unregistered_or_misplaced_claim_trips_it() {
    assert_eq!(
        check(LIB, &specs(STATEMENT, PROVED), ""),
        ["crates/x/SPECS.md:4"]
    );
    assert_eq!(check(LIB, &specs(STATEMENT, "no proof"), ""), CLEAN);
    let other_row = format!("{PROVED}; Proved via mirror: Clean R299 row `x_third`");
    assert_eq!(
        check(LIB, &specs(STATEMENT, &other_row), &registry()),
        ["crates/x/SPECS.md:4"]
    );
    let other_entry = PROVED.replace("R213", "R214");
    assert_eq!(
        check(LIB, &specs(STATEMENT, &other_entry), &registry()),
        ["crates/x/SPECS.md:4"]
    );
    let other_clause =
        specs(STATEMENT, "").replace("other(x) = x\t", &format!("other(x) = x\t{PROVED}"));
    assert_eq!(
        check(LIB, &other_clause, &registry()),
        ["crates/x/SPECS.md:0", "crates/x/SPECS.md:5"]
    );
    let prose = format!("{}\n{PROVED}\n", specs(STATEMENT, PROVED));
    assert_eq!(
        check(LIB, &prose, &registry()),
        ["crates/x/SPECS.md:7"],
        "a claim in prose is held to no row"
    );
}

#[test]
fn a_claim_in_another_package_is_not_this_row() {
    let mut moved = row("x_level", "x-level");
    moved.krate = "aterm-y".into();
    let found = check(LIB, &specs(STATEMENT, PROVED), &render(&[moved]));
    assert!(
        found.contains(&"crates/x/SPECS.md:4".to_string()),
        "{found:?}"
    );
}

#[test]
fn a_claim_in_another_wording_trips_it() {
    for note in [
        "Proved  via mirror: Clean R213 row `x_level`",
        "Mirror-proved: Clean R213 row `x_level`",
        "proved (via mirror) in Clean R213",
    ] {
        let found = check(LIB, &specs(STATEMENT, note), &registry());
        assert!(found.len() >= 2, "{note}: {found:?}");
    }
    let negated = format!("{PROVED}; the clause half is not yet proved via mirror");
    assert_eq!(check(LIB, &specs(STATEMENT, &negated), &registry()), CLEAN);
}

#[test]
fn a_row_both_claimed_and_withdrawn_trips_it() {
    let edited = LIB.replace("x > CAP", "x >= CAP");
    let both = format!("{PROVED}; {STALE_NOTE}");
    assert_eq!(
        check(&edited, &specs(STATEMENT, &both), &registry()),
        ["crates/x/SPECS.md:4", "crates/x/SPECS.md:4"],
        "two notes name it, and the claim stands over a moved body"
    );
}

#[test]
fn a_registry_row_no_note_names_trips_it() {
    assert_eq!(
        check(LIB, &specs(STATEMENT, "3; open"), &registry()),
        ["crates/x/SPECS.md:0"]
    );
}

#[test]
fn a_repeated_clause_id_trips_it() {
    let twice = format!(
        "{}x-level\ttable\tlevel\tlevel(x) = 0\t\n",
        specs(STATEMENT, PROVED)
    );
    assert_eq!(check(LIB, &twice, &registry()), [format!("{REGISTRY}:0")]);
}

#[test]
fn a_row_whose_item_is_gone_trips_it() {
    let renamed = LIB.replace("fn level", "fn cap");
    assert_eq!(
        check(&renamed, &specs(STATEMENT, PROVED), &registry()),
        [format!("{REGISTRY}:0")]
    );
    let gated = LIB.replace("pub fn level", "#[cfg(test)]\nmod tests {\npub fn level") + "}\n";
    assert_eq!(
        check(&gated, &specs(STATEMENT, PROVED), &registry()),
        [format!("{REGISTRY}:0")],
        "a test module's copy is not the code"
    );
}

#[test]
fn a_row_with_no_clause_holds_on_its_items() {
    let free = render(&[row("x_free", NO_CLAUSE)]);
    assert_eq!(check(LIB, &specs(STATEMENT, ""), &free), CLEAN);
    let edited = LIB.replace("x > CAP", "x >= CAP");
    assert_eq!(
        check(&edited, &specs(STATEMENT, ""), &free),
        [format!("{REGISTRY}:0")]
    );
}
