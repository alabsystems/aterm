// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn an_explicit_product_never_builds_or_falls_back_to_another_one() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let bin = tmp.path().join("aterm");
    std::fs::write(&bin, b"one exact profile arm").unwrap();
    let got = supplied_or(Some(&bin), || panic!("must not build a different product")).unwrap();
    assert_eq!(got.bin, bin.canonicalize().unwrap());
    assert_eq!(got.sha256, sha256_file(&bin).unwrap());
    // NEGATIVE CONTROL: a missing supplied arm must refuse, never build the default arm.
    assert!(
        supplied_or(Some(&tmp.path().join("missing")), || panic!(
            "must not fall back"
        ))
        .is_err()
    );
    assert!(supplied_or(None, || Err("build requested".into())).is_err());
}
