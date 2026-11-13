// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn args(words: &[&str]) -> Result<Args, Fail> {
    parse(&words.iter().map(|w| (*w).to_string()).collect::<Vec<_>>())
}

fn judged(words: &[&str]) -> Result<Option<Machine>, Fail> {
    machine(&args(words)?)
}

#[test]
fn the_default_machine_is_the_newest_mac_and_flags_replace_its_facts() {
    let newest = judged(&[]).unwrap().unwrap();
    assert_eq!(newest.describe(), Machine::newest_mac().describe());
    let intel = judged(&["--arch", "x86_64", "--macos", "15.5", "--no-clt"])
        .unwrap()
        .unwrap();
    assert_eq!(
        intel.describe(),
        "x86_64, macOS 15.5, Xcode 16.0, no Command Line Tools"
    );
    let no_xcode = judged(&["--xcode", "none"]).unwrap().unwrap();
    assert_eq!(no_xcode.xcode_version(), None);
    assert!(judged(&["--this-machine"]).unwrap().is_none());
}

/// The lab works from `/`, so a relative `--indexes` is the caller's.
#[test]
fn a_relative_index_directory_is_the_caller_s() {
    let caller = crate::paths::caller_dir();
    assert_eq!(
        args(&["--indexes", "idx"]).unwrap().indexes,
        Some(format!("{}/idx", caller.trim_end_matches('/')))
    );
    assert_eq!(
        args(&["--indexes", "/srv/idx"]).unwrap().indexes.as_deref(),
        Some("/srv/idx")
    );
}

#[test]
fn usage_errors_exit_two() {
    for bad in [
        &["--bogus"][..],
        &["--indexes"],
        &["--this-machine", "--arch", "arm64"],
        &["--macos", "garbage"],
        &["--xcode", "26.x"],
        &["--arch", "aarch64"],
        &["--arch", "mips"],
    ] {
        assert_eq!(judged(bad).unwrap_err().code, 2, "{bad:?}");
    }
}
