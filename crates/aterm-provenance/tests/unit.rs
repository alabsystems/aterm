// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Integration-level unit tests for the `aterm-provenance` crate: origin tags,
//! and the `Provenance<T, O>` wrapper.

#![allow(clippy::unwrap_used)] // tests
#![allow(clippy::expect_used)] // tests

use aterm_provenance::{
    Ai, ConfigFile, Host, NetworkUntrusted, Origin, OriginTag, Provenance, Pty, User,
};

// -- OriginTag ----------------------------------------------------------

#[test]
fn origin_trait_tags_match_markers() {
    assert_eq!(<Host as Origin>::TAG, OriginTag::Host);
    assert_eq!(<ConfigFile as Origin>::TAG, OriginTag::ConfigFile);
    assert_eq!(<User as Origin>::TAG, OriginTag::User);
    assert_eq!(<Ai as Origin>::TAG, OriginTag::Ai);
    assert_eq!(
        <NetworkUntrusted as Origin>::TAG,
        OriginTag::NetworkUntrusted
    );
    assert_eq!(<Pty as Origin>::TAG, OriginTag::Pty);
}

// -- Provenance wrapper -------------------------------------------------

#[test]
fn provenance_is_repr_transparent() {
    // Acceptance criterion from #8000: `size_of::<Provenance<u8, Pty>>() == 1`.
    assert_eq!(core::mem::size_of::<Provenance<u8, Pty>>(), 1);
    assert_eq!(core::mem::size_of::<Provenance<u8, Host>>(), 1);
    assert_eq!(core::mem::size_of::<Provenance<u32, NetworkUntrusted>>(), 4);
}

#[test]
fn provenance_slice_is_repr_transparent() {
    // Acceptance criterion from #8000:
    // `size_of::<Provenance<&[u8], Host>>() == size_of::<&[u8]>()`.
    assert_eq!(
        core::mem::size_of::<Provenance<&[u8], Host>>(),
        core::mem::size_of::<&[u8]>()
    );
    assert_eq!(
        core::mem::size_of::<Provenance<&str, User>>(),
        core::mem::size_of::<&str>()
    );
}

#[test]
fn provenance_from_t_only_for_host() {
    // `From<T>` is implemented only for the Host origin (§4.1 audit
    // ergonomics — every non-Host tag must be opted into explicitly).
    let _: Provenance<u8, Host> = 5u8.into();
    // Compile-fail coverage for the other origins is the `compile_fail`
    // doctests on `Provenance` (src/provenance.rs).
}

#[test]
fn origin_tag_discriminants_are_stable() {
    assert_eq!(OriginTag::Host as u8, 0);
    assert_eq!(OriginTag::ConfigFile as u8, 1);
    assert_eq!(OriginTag::User as u8, 2);
    assert_eq!(OriginTag::Ai as u8, 3);
    assert_eq!(OriginTag::NetworkUntrusted as u8, 4);
    assert_eq!(OriginTag::Pty as u8, 5);
}

#[test]
fn provenance_as_ref_preserves_value() {
    let p = Provenance::<_, Pty>::from_pty(String::from("hello"));
    assert_eq!(p.as_ref(), "hello");
}
