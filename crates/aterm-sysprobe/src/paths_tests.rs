// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Native Windows oracles for the shared lexical path rules.

use std::path::Path;

use aterm_path::{windows_key, windows_under};

fn key(p: &str) -> Option<String> {
    windows_key(Path::new(p))
}

#[cfg(windows)]
#[test]
fn every_bmp_character_keys_as_the_system_upcases_it() {
    let mut apart = Vec::new();
    for unit in (0..=0xFFFF_u16).filter(|u| !(0xD800..=0xDFFF).contains(u)) {
        let (c, up) = (
            char::from_u32(u32::from(unit)).unwrap(),
            char::from_u32(u32::from(crate::win_paths::upcase(unit))).unwrap(),
        );
        if key(&format!(r"C:\a{c}b")) != key(&format!(r"C:\a{up}b")) {
            apart.push(format!("U+{unit:04X}"));
        }
    }
    assert!(apart.is_empty(), "keyed apart from their upcase: {apart:?}");
}

#[cfg(windows)]
#[test]
fn ntfs_opens_a_simple_uppercase_pair_as_one_name() {
    let dir = aterm_tempfile::tempdir().expect("scratch dir");
    let pairs = (0x1F80..=0x1F87_u32)
        .chain(0x1F90..=0x1F97)
        .chain(0x1FA0..=0x1FA7)
        .map(|u| (u, u + 8))
        .chain([(0x1FB3, 0x1FBC), (0x1FC3, 0x1FCC), (0x1FF3, 0x1FFC)]);
    for (lower, upper) in pairs {
        let (l, u) = (
            char::from_u32(lower).unwrap(),
            char::from_u32(upper).unwrap(),
        );
        let made = dir.path().join(format!("x{l}"));
        std::fs::create_dir(&made).expect("a scratch name");
        assert!(
            dir.path().join(format!("x{u}")).is_dir(),
            "U+{lower:04X} opened as U+{upper:04X}"
        );
        assert_eq!(
            windows_under(&made, &dir.path().join(format!("x{u}"))),
            Some(true)
        );
    }
    std::fs::create_dir(dir.path().join("straße")).expect("a scratch name");
    assert!(!dir.path().join("STRASSE").exists(), "two names to NTFS");
}
