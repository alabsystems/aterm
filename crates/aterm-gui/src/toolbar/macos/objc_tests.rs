// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_objc::{
    Bool, ClassType, Encode, Id, Obj, Sel, autoreleasepool, checked_msg, class, class_name,
    method_types, sel,
};

use super::{ChromeButton, STRIP_ITEM_ID, TabGeometry, TabRenameTarget, TabView, ToolbarDelegate};
use crate::appkit;

fn bool_enc() -> &'static str {
    Bool::ENCODING
}

fn a_view() -> Obj {
    // SAFETY: `+alloc`/`-init` on `NSView` gives a +1 zero-frame view;
    // `Obj::from_owned` adopts that one +1.
    unsafe { Obj::from_owned(appkit::send_id(appkit::alloc(class(c"NSView")), sel!(init))) }
        .expect("NSView")
}

#[test]
fn all_four_declared_classes_register_as_they_did() {
    for (cls, name, superclass) in [
        (ChromeButton::class(), c"ATermChromeButton", c"NSView"),
        (TabView::class(), c"ATermTabView", c"NSView"),
        (
            ToolbarDelegate::class(),
            c"ATermToolbarDelegate",
            c"NSObject",
        ),
        (
            TabRenameTarget::class(),
            c"ATermTabRenameTarget",
            c"NSObject",
        ),
    ] {
        assert!(!cls.is_null());
        assert_eq!(class(name), cls, "objc_getClass disagrees for {name:?}");
        // SAFETY: `cls` is a class this module registered, so it and its superclass are live, immortal class objects.
        unsafe {
            assert_eq!(class_name(cls), name);
            assert_eq!(class_name(aterm_objc::superclass_of(cls)), superclass);
        }
    }
}

#[test]
fn the_toolbar_delegate_conforms_to_nstoolbardelegate() {
    let cls = ToolbarDelegate::class();
    let proto = aterm_objc::protocol(c"NSToolbarDelegate");
    assert!(
        !proto.is_null()
            && !aterm_objc::protocols_registered_by_aterm().contains(&c"NSToolbarDelegate"),
        "this host's AppKit registers NSToolbarDelegate — not one aterm had to supply"
    );
    // SAFETY: `+conformsToProtocol:` and `+instancesRespondToSelector:` are side-effect-free NSObject queries on a live class object.
    unsafe {
        assert!(
            appkit::send_bool_id(
                cls.as_id(),
                sel!(conformsToProtocol:),
                Id::from_ptr(proto.as_ptr()),
            ),
            "the declared class does not answer NSToolbarDelegate"
        );
        for s in [
            sel!(toolbarDefaultItemIdentifiers:),
            sel!(toolbarAllowedItemIdentifiers:),
            sel!(toolbar:itemForItemIdentifier:willBeInsertedIntoToolbar:),
        ] {
            assert!(appkit::send_bool_sel(
                cls.as_id(),
                sel!(instancesRespondToSelector:),
                s
            ));
        }
    }
}

#[test]
fn the_rename_rows_encode_to_the_protocol_authority() {
    let cls = TabRenameTarget::class();
    let proto = aterm_objc::protocol(c"NSTextFieldDelegate");
    assert!(
        !proto.is_null()
            && !aterm_objc::protocols_registered_by_aterm().contains(&c"NSTextFieldDelegate"),
        "this host's AppKit registers NSTextFieldDelegate — not one aterm had to supply"
    );
    // SAFETY: `+conformsToProtocol:` is a side-effect-free `NSObject` query on a live class object.
    assert!(
        unsafe {
            appkit::send_bool_id(
                cls.as_id(),
                sel!(conformsToProtocol:),
                Id::from_ptr(proto.as_ptr()),
            )
        },
        "the declared class does not answer NSTextFieldDelegate"
    );
    let mut checked = 0usize;
    for s in [
        sel!(controlTextDidChange:),
        sel!(controlTextDidEndEditing:),
        sel!(control:textView:doCommandBySelector:),
    ] {
        // SAFETY: `proto` is a live protocol object, `cls` a live class this module registered, and both queries are read-only.
        let (want, got) = unsafe {
            (
                aterm_objc::protocol_method_types(proto, s, true),
                method_types(cls, s),
            )
        };
        let want = want
            .as_deref()
            .map(aterm_objc::strip_method_offsets)
            .unwrap_or_else(|| panic!("{s:?} is not declared by NSTextFieldDelegate"));
        assert_eq!(got.as_deref(), Some(want.as_str()), "{s:?}");
        checked += 1;
    }
    assert_eq!(checked, 3, "every rename row was read from the authority");
}

#[test]
fn the_runtime_reports_every_encoding_the_table_says() {
    let b = bool_enc();
    let rect = "{CGRect={CGPoint=dd}{CGSize=dd}}";
    let cases: Vec<(aterm_objc::ClassPtr, Sel, String)> = vec![
        (ChromeButton::class(), sel!(drawRect:), format!("v@:{rect}")),
        (ChromeButton::class(), sel!(mouseDown:), "v@:@".into()),
        (ChromeButton::class(), sel!(mouseDragged:), "v@:@".into()),
        (ChromeButton::class(), sel!(mouseUp:), "v@:@".into()),
        (ChromeButton::class(), sel!(mouseEntered:), "v@:@".into()),
        (ChromeButton::class(), sel!(mouseExited:), "v@:@".into()),
        (
            ChromeButton::class(),
            sel!(updateTrackingAreas),
            "v@:".into(),
        ),
        (
            ChromeButton::class(),
            sel!(acceptsFirstMouse:),
            format!("{b}@:@"),
        ),
        (
            ChromeButton::class(),
            sel!(accessibilityPerformPress),
            format!("{b}@:"),
        ),
        (ChromeButton::class(), sel!(dealloc), "v@:".into()),
        (TabView::class(), sel!(drawRect:), format!("v@:{rect}")),
        (TabView::class(), sel!(mouseDown:), "v@:@".into()),
        (TabView::class(), sel!(rightMouseDown:), "v@:@".into()),
        (TabView::class(), sel!(mouseDragged:), "v@:@".into()),
        (TabView::class(), sel!(mouseUp:), "v@:@".into()),
        (TabView::class(), sel!(mouseEntered:), "v@:@".into()),
        (TabView::class(), sel!(mouseExited:), "v@:@".into()),
        (TabView::class(), sel!(updateTrackingAreas), "v@:".into()),
        (
            TabView::class(),
            sel!(acceptsFirstMouse:),
            format!("{b}@:@"),
        ),
        (
            TabView::class(),
            sel!(accessibilityPerformPress),
            format!("{b}@:"),
        ),
        (TabView::class(), sel!(closeTab:), "v@:@".into()),
        (TabView::class(), sel!(tabMenuAction:), "v@:@".into()),
        (TabView::class(), sel!(tabMenuSessionAction:), "v@:@".into()),
        (TabView::class(), sel!(tabMenuConnection:), "v@:@".into()),
        (TabView::class(), sel!(tabMenuUpgrade:), "v@:@".into()),
        (TabView::class(), sel!(tabMenuRewind:), "v@:@".into()),
        (TabView::class(), sel!(dealloc), "v@:".into()),
        (
            ToolbarDelegate::class(),
            sel!(toolbarDefaultItemIdentifiers:),
            "@@:@".into(),
        ),
        (
            ToolbarDelegate::class(),
            sel!(toolbarAllowedItemIdentifiers:),
            "@@:@".into(),
        ),
        (
            ToolbarDelegate::class(),
            sel!(toolbar:itemForItemIdentifier:willBeInsertedIntoToolbar:),
            format!("@@:@@{b}"),
        ),
        (ToolbarDelegate::class(), sel!(dealloc), "v@:".into()),
        (
            TabRenameTarget::class(),
            sel!(controlTextDidChange:),
            "v@:@".into(),
        ),
        (
            TabRenameTarget::class(),
            sel!(control:textView:doCommandBySelector:),
            format!("{b}@:@@:"),
        ),
        (
            TabRenameTarget::class(),
            sel!(controlTextDidEndEditing:),
            "v@:@".into(),
        ),
        (TabRenameTarget::class(), sel!(dealloc), "v@:".into()),
    ];
    for (cls, s, want) in cases {
        // SAFETY: every `cls` is a live class this module registered.
        let got = unsafe { method_types(cls, s) };
        assert_eq!(
            got.as_deref(),
            Some(want.as_str()),
            "{s:?} on {:?}",
            // SAFETY: as above.
            unsafe { class_name(cls) }
        );
    }
    #[cfg(target_arch = "aarch64")]
    {
        assert_eq!(b, "B");
        // SAFETY: live registered class.
        assert_eq!(
            unsafe {
                method_types(
                    ToolbarDelegate::class(),
                    sel!(toolbar:itemForItemIdentifier:willBeInsertedIntoToolbar:),
                )
            }
            .as_deref(),
            Some("@@:@@B")
        );
        // SAFETY: live registered class.
        assert_eq!(
            unsafe {
                method_types(
                    TabRenameTarget::class(),
                    sel!(control:textView:doCommandBySelector:),
                )
            }
            .as_deref(),
            Some("B@:@@:")
        );
    }
}

#[test]
fn the_identifier_methods_vend_exactly_the_strip_item() {
    let delegate = ToolbarDelegate::new(crate::appkit::test_witness(), a_view()).expect("delegate");
    autoreleasepool(|_| {
        // SAFETY: `-performSelector:withObject:` is the runtime's own dispatch, `-count` is `-(NSUInteger)` and `-objectAtIndex:` is `-(id)(NSUInteger)` on the returned NSArray, which is alive for this pool.
        unsafe {
            for s in [
                sel!(toolbarDefaultItemIdentifiers:),
                sel!(toolbarAllowedItemIdentifiers:),
            ] {
                let perform: unsafe extern "C-unwind" fn(Id, Sel, Sel, Id) -> Id = checked_msg();
                let arr = perform(
                    delegate.as_id(),
                    sel!(performSelector:withObject:),
                    s,
                    Id::NIL,
                );
                assert!(!arr.is_null(), "{s:?} returned nil");
                assert_eq!(appkit::send_usize(arr, sel!(count)), 1);
                let first: unsafe extern "C-unwind" fn(Id, Sel, usize) -> Id = checked_msg();
                assert_eq!(
                    appkit::nsstring_to_rust(first(arr, sel!(objectAtIndex:), 0)),
                    STRIP_ITEM_ID
                );
            }
        }
    });
}

#[test]
fn tab_dividers_skip_the_first_chip_and_both_edges_of_active_up_to_usize_max() {
    assert!(!TabGeometry::separates(0, 0));
    assert!(!TabGeometry::separates(0, 2));
    assert!(!TabGeometry::separates(1, 0));
    assert!(!TabGeometry::separates(1, 1));
    assert!(TabGeometry::separates(2, 0));
    assert!(TabGeometry::separates(1, usize::MAX));
    assert!(!TabGeometry::separates(usize::MAX, usize::MAX));
    assert!(!TabGeometry::separates(usize::MAX, usize::MAX - 1));
}
