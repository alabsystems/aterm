// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::pty_wrap_ref;

#[test]
fn provenance_adds_no_bytes() {
    let bytes: &[u8] = b"abc";
    let wrapped_bytes = pty_wrap_ref(bytes);
    assert_eq!(
        core::mem::size_of_val(wrapped_bytes),
        core::mem::size_of_val(bytes)
    );
    assert_eq!(
        core::mem::align_of_val(wrapped_bytes),
        core::mem::align_of_val(bytes)
    );

    let words: &[u32] = &[1, 2, 3];
    let wrapped_words = pty_wrap_ref(words);
    assert_eq!(
        core::mem::size_of_val(wrapped_words),
        core::mem::size_of_val(words)
    );
    assert_eq!(
        core::mem::align_of_val(wrapped_words),
        core::mem::align_of_val(words)
    );

    let text: &str = "héllo";
    let wrapped_text = pty_wrap_ref(text);
    assert_eq!(
        core::mem::size_of_val(wrapped_text),
        core::mem::size_of_val(text)
    );
    assert_eq!(
        core::mem::align_of_val(wrapped_text),
        core::mem::align_of_val(text)
    );
}

#[test]
fn wrap_scalar_reference_preserves_identity() {
    let value: u8 = 0x7F;
    let wrapped = pty_wrap_ref(&value);
    assert_eq!(
        core::ptr::from_ref(wrapped.as_ref()),
        core::ptr::from_ref(&value)
    );
    assert_eq!(*wrapped.as_ref(), 0x7F);

    let zst = ();
    let wrapped_zst = pty_wrap_ref(&zst);
    assert_eq!(
        core::ptr::from_ref(wrapped_zst.as_ref()),
        core::ptr::from_ref(&zst)
    );
}

#[test]
fn wrap_str_preserves_bytes() {
    let s: &str = "héllo";
    let wrapped = pty_wrap_ref(s);
    assert!(core::ptr::eq(wrapped.as_ref(), s));
    assert_eq!(wrapped.as_ref(), s);

    let words: &[u32] = &[0xDEAD_BEEF, 0xCAFE_BABE];
    let wrapped_words = pty_wrap_ref(words);
    assert!(core::ptr::eq(wrapped_words.as_ref(), words));
    assert_eq!(wrapped_words.as_ref(), words);

    let zsts: &[()] = &[(), (), ()];
    let wrapped_zsts = pty_wrap_ref(zsts);
    assert!(core::ptr::eq(wrapped_zsts.as_ref(), zsts));
}
