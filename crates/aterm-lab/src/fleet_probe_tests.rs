// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// The proof is the documented line: a word, a space, 64 lowercase hex
/// digits; another secret, exporter or op makes another tag.
#[test]
fn the_raw_proof_is_one_documented_line_bound_to_secret_exporter_and_op() {
    assert_eq!(
        dialer_proof(&[0x0b; 22], &[0xa5; 32], "survey"),
        "survey f92e33cf27fc4091ec8577e69a3afae1985bfca7b81d9d98738e89272125c732\n"
    );
    let a = dialer_proof(&[0x11; 20], &[0xa5; 32], "survey");
    let (word, tag) = a.trim_end().split_once(' ').expect("a word and a tag");
    assert_eq!(word, "survey");
    assert!(aterm_codec::hex::is_lower(tag, 64), "{tag}");
    assert!(a.ends_with('\n'));
    assert_ne!(a, dialer_proof(&[0x22; 20], &[0xa5; 32], "survey"));
    assert_ne!(a, dialer_proof(&[0x11; 20], &[0x5a; 32], "survey"));
    assert_ne!(a, dialer_proof(&[0x11; 20], &[0xa5; 32], "join"));
}
