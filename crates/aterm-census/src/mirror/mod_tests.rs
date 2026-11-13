// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;

/// A mirror table holds these digests, so the fold must not move.
#[test]
fn a_mirrors_digest_is_aterm_hashs_fold() {
    assert_eq!(fnv(""), format!("{:016x}", aterm_hash::FNV64_OFFSET));
    assert_eq!(fnv("foobar"), "85944171f73967e8");
    assert_eq!(
        fnv("foobar"),
        format!("{:016x}", aterm_hash::fnv1a_64(b"foobar"))
    );
}

fn row() -> Row {
    Row {
        clean: "R1".into(),
        row: "r".into(),
        krate: "k".into(),
        clause: NO_CLAUSE.into(),
        aterm: "abc".into(),
        harness: "d2b_rows/r.rs".into(),
        items: vec![
            Item::parse("crates/k/src/lib.rs#f").expect("an item"),
            Item::parse("crates/k/src/lib.rs#<A as From<bool>>::from").expect("an item"),
        ],
        digests: vec!["0123456789abcdef".into(), "fedcba9876543210".into()],
        clause_digest: NO_CLAUSE.into(),
    }
}

#[test]
fn the_registry_round_trips_sealed() {
    let text = render(&[row()]);
    assert!(matches!(seal::check(&text), seal::Seal::Holds { .. }));
    assert_eq!(parse(&text), Ok(vec![row()]));
}

#[test]
fn a_malformed_registry_is_refused() {
    let text = render(&[row()]);
    let line = text.lines().last().expect("a row");
    let headless = text.replace(&format!("{COLUMNS}\n"), "");
    assert!(parse(&headless).is_err(), "no header");
    assert!(parse(&format!("{text}{line}\n")).is_err(), "a row twice");
    let short = text.replace("; fedcba9876543210", "");
    assert!(parse(&short).is_err(), "one digest for two items");
    assert!(
        parse(&text.replace("lib.rs#f", "lib.rs f")).is_err(),
        "an item with no #"
    );
}

#[test]
fn a_package_is_found_by_its_name() {
    let manifests = [
        ("crates/x/Cargo.toml", "[package]\nname = \"aterm-x\"\n"),
        (
            "crates/y/Cargo.toml",
            "[dependencies]\nname = \"aterm-y\"\n",
        ),
    ];
    assert_eq!(
        package_dir(manifests, "aterm-x").as_deref(),
        Some("crates/x")
    );
    assert_eq!(package_dir(manifests, "aterm-y"), None);
}

#[test]
fn the_source_must_be_the_commit_recorded() {
    let dir = std::env::temp_dir().join(format!("aterm-census-mirror-{}", std::process::id()));
    let git = dir.join(".git");
    std::fs::create_dir_all(git.join("refs/heads")).expect("a scratch repo");
    std::fs::write(git.join("HEAD"), "ref: refs/heads/main\n").expect("HEAD");
    let sha = "0123456789abcdef0123456789abcdef01234567";
    std::fs::write(
        git.join("packed-refs"),
        format!("# pack\n{sha} refs/heads/main\n"),
    )
    .expect("packed-refs");
    assert_eq!(head_commit(&dir).as_deref(), Ok(sha));
    std::fs::write(git.join("refs/heads/main"), "fedcba\n").expect("a loose ref");
    assert_eq!(head_commit(&dir).as_deref(), Ok("fedcba"));
    let args: Vec<String> = [
        "reproved", "--clean", "R2", "--aterm", "0123456", "--source",
    ]
    .iter()
    .map(ToString::to_string)
    .chain([dir.display().to_string(), "r".to_string()])
    .collect();
    let refused = cli(&dir, &args).expect_err("HEAD is fedcba, not 0123456");
    assert!(refused.contains("not --aterm 0123456"), "{refused}");
    let _ = std::fs::remove_dir_all(&dir);
}
