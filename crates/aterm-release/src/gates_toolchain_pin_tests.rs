// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::sync::OnceLock;

#[cfg(unix)]
#[test]
fn toolchain_aliases_share_a_pin_but_different_builds_do_not() {
    let root = aterm_tempfile::tempdir().unwrap();
    let real = root.path().join("store/1/bin");
    let other = root.path().join("store/2/bin");
    fs::create_dir_all(&real).unwrap();
    fs::create_dir_all(&other).unwrap();
    let alias = root.path().join("current");
    std::os::unix::fs::symlink(&real, &alias).unwrap();
    let cell = OnceLock::new();
    pin_into(&cell, &canonical_toolchain(&real).unwrap()).unwrap();
    pin_into(&cell, &canonical_toolchain(&alias).unwrap()).unwrap();
    fs::remove_file(&alias).unwrap();
    std::os::unix::fs::symlink(&other, &alias).unwrap();
    assert!(pin_into(&cell, &canonical_toolchain(&alias).unwrap()).is_err());
}

#[test]
fn the_first_resolution_is_the_one_the_whole_process_gets() {
    let cell: OnceLock<PathBuf> = OnceLock::new();
    let first = pin_first_success(&cell, || Ok(PathBuf::from("/seal/alpha/bin")))
        .expect("the first resolution succeeds");
    assert_eq!(first, Path::new("/seal/alpha/bin"));

    let second = pin_first_success(&cell, || {
        panic!("a pinned toolchain must never be re-resolved")
    })
    .expect("the pin answers without resolving");
    assert_eq!(second, first);
}

#[test]
fn a_failed_resolution_pins_nothing_and_is_retried() {
    let cell: OnceLock<PathBuf> = OnceLock::new();
    assert!(pin_first_success(&cell, || Err(Error::new("no Trust toolchain found"))).is_err());
    assert!(
        cell.get().is_none(),
        "a failure has no toolchain identity to protect; caching it would outlive \
         the install the error text tells the operator to perform"
    );
    let after = pin_first_success(&cell, || Ok(PathBuf::from("/seal/beta/bin")))
        .expect("the retry resolves");
    assert_eq!(after, Path::new("/seal/beta/bin"));
}

#[test]
fn a_release_pins_its_journaled_prover_before_any_resolution() {
    let cell: OnceLock<PathBuf> = OnceLock::new();
    let journaled = Path::new("/store/trust/9192/bin");
    pin_into(&cell, journaled).expect("first pin");
    let asked =
        pin_first_success(&cell, || Ok(PathBuf::from("/store/trust/current/bin"))).expect("pinned");
    assert_eq!(asked, journaled, "the store's current build is never asked");
    assert!(
        pin_into(&cell, journaled).is_ok(),
        "the same pin again is no change"
    );
    assert!(pin_into(&cell, Path::new("/store/trust/9300/bin")).is_err());
}
