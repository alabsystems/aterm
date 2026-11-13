// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use aterm_types::Appearance;

#[test]
fn the_kvo_callback_runs_the_relay() {
    use aterm_objc::{ClassType, class};
    let hits = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&hits);
    let target = super::new_target(
        crate::appkit::test_witness(),
        Box::new(move || {
            seen.fetch_add(1, Ordering::SeqCst);
        }),
    )
    .expect("an OsAppearanceTarget");
    assert_eq!(
        class(c"ATermOsAppearanceTarget"),
        super::OsAppearanceTarget::class()
    );
    // SAFETY: the declared selector's exact prototype, on a live
    // instance; nil arguments are what the body ignores.
    unsafe {
        let send: unsafe extern "C-unwind" fn(
            aterm_objc::Id,
            aterm_objc::Sel,
            aterm_objc::Id,
            aterm_objc::Id,
            aterm_objc::Id,
            *mut core::ffi::c_void,
        ) = aterm_objc::checked_msg();
        send(
            target.as_id(),
            aterm_objc::sel!(observeValueForKeyPath:ofObject:change:context:),
            aterm_objc::Id::NIL,
            aterm_objc::Id::NIL,
            aterm_objc::Id::NIL,
            core::ptr::null_mut(),
        );
    }
    assert_eq!(hits.load(Ordering::SeqCst), 1);
}

#[test]
fn best_match_names_map_to_appearances() {
    // SAFETY: AppKit's own name globals, and nil.
    unsafe {
        assert_eq!(
            super::appearance_of_best_match(crate::appkit::appearance_name(true)),
            Some(Appearance::Dark)
        );
        assert_eq!(
            super::appearance_of_best_match(crate::appkit::appearance_name(false)),
            Some(Appearance::Light)
        );
        assert_eq!(super::appearance_of_best_match(aterm_objc::Id::NIL), None);
    }
}

#[test]
fn the_interface_style_default_maps() {
    assert_eq!(super::style_appearance("Dark"), Appearance::Dark);
    assert_eq!(super::style_appearance(""), Appearance::Light);
    assert_eq!(super::style_appearance("Light"), Appearance::Light);
}
