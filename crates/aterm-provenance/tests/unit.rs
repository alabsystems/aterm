// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Integration-level unit tests for the `Provenance<T, O>` wrapper.

use aterm_provenance::{Provenance, Pty};

#[test]
fn provenance_is_repr_transparent() {
    assert_eq!(core::mem::size_of::<Provenance<u8, Pty>>(), 1);
    assert_eq!(core::mem::size_of::<Provenance<u32, Pty>>(), 4);
    assert_eq!(
        core::mem::size_of::<Provenance<&[u8], Pty>>(),
        core::mem::size_of::<&[u8]>()
    );
}

#[test]
fn provenance_as_ref_preserves_value() {
    let p = Provenance::<_, Pty>::from_pty(String::from("hello"));
    assert_eq!(p.as_ref(), "hello");
}
