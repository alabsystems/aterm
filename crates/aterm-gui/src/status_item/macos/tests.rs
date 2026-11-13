// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use aterm_objc::{ClassType, Sel, autoreleasepool, class, class_name, method_types, sel};

use super::{StatusTarget, new_item};
use crate::appkit;

/// The registered class. It adopts AppKit's NSMenuDelegate, which is there once AppKit
/// is, so AppKit is loaded before the first test to ask registers it.
fn target_class() -> aterm_objc::ClassPtr {
    aterm_macos_ui::appkit::load();
    StatusTarget::class()
}

#[test]
fn the_registered_class_is_what_the_runtime_reports() {
    let cls = target_class();
    assert!(!cls.is_null());
    assert_eq!(class(c"ATermStatusTarget"), cls);
    // SAFETY: `cls` is the class this module registered, so it and its superclass are live, immortal class objects.
    unsafe {
        assert_eq!(class_name(cls), c"ATermStatusTarget");
        assert_eq!(class_name(aterm_objc::superclass_of(cls)), c"NSObject");
    }
    let proto = aterm_objc::protocol(c"NSMenuDelegate");
    assert!(
        !proto.is_null()
            && !aterm_objc::protocols_registered_by_aterm().contains(&c"NSMenuDelegate"),
        "this host's AppKit registers NSMenuDelegate — not one aterm had to supply"
    );
    // SAFETY: `+conformsToProtocol:` and `+instancesRespondToSelector:` are side-effect-free NSObject queries on a live class object.
    unsafe {
        assert!(
            appkit::send_bool_id(
                cls.as_id(),
                sel!(conformsToProtocol:),
                aterm_objc::Id::from_ptr(proto.as_ptr()),
            ),
            "the declared class does not answer NSMenuDelegate"
        );
        assert!(appkit::send_bool_sel(
            cls.as_id(),
            sel!(instancesRespondToSelector:),
            sel!(statusAction:)
        ));
        assert!(appkit::send_bool_sel(
            cls.as_id(),
            sel!(instancesRespondToSelector:),
            sel!(menuNeedsUpdate:)
        ));
        assert!(appkit::send_bool_sel(
            cls.as_id(),
            sel!(instancesRespondToSelector:),
            sel!(menuWillOpen:)
        ));
        for click in [sel!(statusClicked:), sel!(showMenuAction:), sel!(openMenu:)] {
            assert!(
                appkit::send_bool_sel(cls.as_id(), sel!(instancesRespondToSelector:), click),
                "{click:?}"
            );
        }
    }
}

#[test]
fn the_runtime_reports_the_encodings_the_table_says() {
    let cls = target_class();
    // SAFETY: `cls` is the live registered class.
    unsafe {
        assert_eq!(
            method_types(cls, sel!(statusAction:)).as_deref(),
            Some("v@:@")
        );
        assert_eq!(
            method_types(cls, sel!(menuNeedsUpdate:)).as_deref(),
            Some("v@:@")
        );
        assert_eq!(
            method_types(cls, sel!(menuWillOpen:)).as_deref(),
            Some("v@:@")
        );
        // The button's action and the delayed perform take the sender; the Show Menu
        // accessibility action's handler answers a BOOL.
        assert_eq!(
            method_types(cls, sel!(statusClicked:)).as_deref(),
            Some("v@:@")
        );
        assert_eq!(method_types(cls, sel!(openMenu:)).as_deref(), Some("v@:@"));
        // `BOOL` is a C `bool` on Apple silicon and a `signed char` on Intel.
        let yes_or_no = if cfg!(target_arch = "aarch64") {
            "B@:@"
        } else {
            "c@:@"
        };
        assert_eq!(
            method_types(cls, sel!(showMenuAction:)).as_deref(),
            Some(yes_or_no)
        );
        assert_eq!(method_types(cls, sel!(dealloc)).as_deref(), Some("v@:"));
    }
}

#[test]
fn an_information_row_has_a_nil_action() {
    autoreleasepool(|_| {
        let item = new_item("info", Sel::NULL).expect("item");
        let wired = new_item("cmd", sel!(statusAction:)).expect("item");
        // SAFETY: `-action` is `-(SEL)` on a live NSMenuItem, and `-title` is `-(NSString *)`.
        unsafe {
            let action: unsafe extern "C-unwind" fn(aterm_objc::Id, Sel) -> Sel =
                aterm_objc::checked_msg();
            assert!(action(item.id(), sel!(action)).is_null());
            assert_eq!(action(wired.id(), sel!(action)), sel!(statusAction:));
            assert_eq!(
                appkit::nsstring_to_rust(appkit::send_id(item.id(), sel!(title))),
                "info"
            );
        }
    });
}
