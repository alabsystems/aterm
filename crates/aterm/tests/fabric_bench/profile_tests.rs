// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The bench arms' table ([`crate::profile`]), checked; mounted only by the ordinary
//! fabric_bench test target.

use std::path::Path;

use crate::profile::*;

#[test]
fn each_arm_parses_from_its_name_and_nothing_else_does() {
    for p in Profile::ALL {
        assert_eq!(Profile::parse(p.name()), Ok(p));
    }
    assert_eq!(
        Profile::default(),
        Profile::Dev,
        "the arm every entry so far ran"
    );
    for bad in ["", "release", "opt-test", "Dev", "debug", "test"] {
        assert!(Profile::parse(bad).is_err(), "{bad}");
    }
}

#[test]
fn a_binary_names_the_arm_whose_directory_holds_it() {
    for p in Profile::ALL {
        let exe = Path::new("/w/target").join(p.dir()).join("aterm");
        assert_eq!(Profile::of_binary(&exe), Some(p));
    }
    assert_eq!(
        Profile::of_binary(Path::new("/w/target/debug/aterm")),
        Some(Profile::Dev)
    );
    assert_eq!(
        Profile::of_binary(Path::new("/w/target/opt-test/aterm")),
        Some(Profile::Opt)
    );
    assert_eq!(
        Profile::of_binary(Path::new("/w/target/aarch64-apple-darwin/opt-test/aterm")),
        Some(Profile::Opt)
    );
    for other in [
        "/w/target/release/aterm",
        "/w/target/profiling/aterm",
        "/Applications/aterm.app/Contents/MacOS/aterm",
        "aterm",
        "/",
    ] {
        assert_eq!(Profile::of_binary(Path::new(other)), None, "{other}");
    }
    assert_eq!(
        Profile::of_binary(Path::new("/debug/w/target/opt-test/aterm")),
        Some(Profile::Opt)
    );
    assert_eq!(
        Profile::of_binary(Path::new("/debug/w/target/release/aterm")),
        None
    );
}

#[test]
fn an_arm_is_looked_up_where_cargo_writes_its_profile() {
    for p in Profile::ALL {
        match p.cargo_profile() {
            custom if custom != "test" => assert_eq!(p.dir(), custom, "{p:?}"),
            _ => assert_eq!(p.dir(), "debug", "{p:?}"),
        }
        if let Some(level) = p.every_crate_at() {
            let promise = format!("opt-level {level} on every crate");
            assert!(p.describe().contains(&promise), "{p:?}: {}", p.describe());
        }
    }
}

#[test]
fn a_rewritten_binary_is_another_identity() {
    let dir = std::env::temp_dir().join(format!("fabric-identity-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch");
    let exe = dir.join("aterm");
    std::fs::write(&exe, b"one build").expect("write");
    let first = identity(&exe).expect("reads");
    assert_eq!(identity(&exe), Ok(first.clone()), "one file reads the same");
    assert!(first.starts_with("9 bytes, modified "), "{first}");
    std::fs::write(&exe, b"another build").expect("write");
    assert_ne!(identity(&exe).expect("reads"), first);
    std::fs::remove_dir_all(&dir).expect("cleanup");
    assert!(identity(&exe).is_err(), "no file is no identity");
}
