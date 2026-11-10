// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use aterm_objc::{Bool, ClassPtr, Contained, Id, Sel};
use std::ffi::c_void;

#[derive(Debug)]
pub struct Probe {
    pub created_class: String,
    pub is_nsfont: bool,
    pub system_class: Result<String, Contained>,
}

/// Query the native font bridge in this process, optionally registering AppKit first.
///
/// # Panics
/// Panics if Core Text or AppKit returns no system font.
pub fn inspect(early: bool) -> Probe {
    #[link(name = "CoreText", kind = "framework")]
    unsafe extern "C" {
        fn CTFontCreateUIFontForLanguage(
            kind: u32,
            size: f64,
            language: *const c_void,
        ) -> *const c_void;
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFRelease(object: *const c_void);
    }

    if early {
        aterm_macos_ui::appkit::load();
    }
    // SAFETY: 2 is the SDK's system-font type; a null language selects the
    // default. The Create result is retained until the final CFRelease.
    let font = unsafe { CTFontCreateUIFontForLanguage(2, 12.0, std::ptr::null()) };
    assert!(!font.is_null());
    let font_id = Id::from_ptr(font.cast_mut());
    // SAFETY: font is a live, retained Core Text object.
    let created_class = unsafe {
        aterm_objc::class_name(aterm_objc::class_of(font_id))
            .to_string_lossy()
            .into_owned()
    };
    aterm_macos_ui::appkit::load();
    let mut is_nsfont = false;
    let system_class = aterm_objc::autoreleasepool(|_| {
        aterm_objc::exception::contain("font bridge order", || {
            // SAFETY: NSFont's factory takes CGFloat and returns a borrowed
            // object used within this autorelease pool. isKindOfClass: accepts
            // a class pointer and reads the live Core Text object's ancestry.
            unsafe {
                let get: unsafe extern "C-unwind" fn(Id, Sel, f64) -> Id =
                    aterm_objc::checked_msg();
                let is_kind: unsafe extern "C-unwind" fn(Id, Sel, ClassPtr) -> Bool =
                    aterm_objc::checked_msg();
                let class = aterm_objc::class(c"NSFont");
                is_nsfont = is_kind(font_id, aterm_objc::sel!(isKindOfClass:), class).as_bool();
                let found = get(class.as_id(), aterm_objc::sel!(systemFontOfSize:), 12.0);
                assert!(!found.is_null());
                aterm_objc::class_name(aterm_objc::class_of(found))
                    .to_string_lossy()
                    .into_owned()
            }
        })
    });
    // SAFETY: exactly one release of the successful Create result above.
    unsafe { CFRelease(font) };
    Probe {
        created_class,
        is_nsfont,
        system_class,
    }
}
