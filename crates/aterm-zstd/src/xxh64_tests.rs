// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::test_sanity::sanity;
use super::{STRIPE, Xxh64, xxh64};

#[test]
fn split_feeding_matches_one_shot() {
    let buf = sanity(3 * STRIPE + 5);
    let want = xxh64(7, &buf);
    for a in 0..=buf.len() {
        for b in a..=buf.len().min(a + STRIPE + 1) {
            let mut h = Xxh64::new(7);
            h.update(&buf[..a]);
            h.update(&buf[a..b]);
            h.update(&buf[b..]);
            assert_eq!(h.digest(), want, "cut at {a} and {b}");
        }
    }
    let mut bytewise = Xxh64::new(7);
    for byte in &buf {
        bytewise.update(std::slice::from_ref(byte));
    }
    assert_eq!(bytewise.digest(), want);
}

#[test]
fn digest_then_continue() {
    let buf = sanity(100);
    let mut h = Xxh64::new(0);
    h.update(&buf[..40]);
    assert_eq!(h.digest(), xxh64(0, &buf[..40]));
    h.update(&buf[40..]);
    assert_eq!(h.digest(), xxh64(0, &buf));
}
