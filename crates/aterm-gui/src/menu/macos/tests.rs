// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_objc::{Bool, ClassType, Encode, class_name, method_types, sel};

use super::{MenuTarget, class};
use crate::appkit;

/// A quit Apple event's reason and sender read back through prototypes the runtime
/// agrees with: `-attributeDescriptorForKeyword:` takes a 32-bit `AEKeyword` and
/// `-enumCodeValue` answers a 32-bit `OSType`. NEGATIVE CONTROL: with the keyword passed
/// as an `NSUInteger` (`@@:Q`, as this read sent it at d7d33a47f3) the debug-build send
/// check panics, as it did inside `applicationShouldTerminate:` for every Dock quit.
#[test]
fn a_quit_events_reason_and_sender_read_with_the_runtimes_prototypes() {
    let code = |s: &[u8; 4]| u32::from_be_bytes(*s);
    // SAFETY: Foundation constructors and setters on live descriptors this pool keeps:
    // `+descriptorWithEnumCode:` is `(OSType) -> id`, `+appleEventWithEventClass:eventID:
    // targetDescriptor:returnID:transactionID:` is `(AEEventClass, AEEventID, id,
    // AEReturnID, AETransactionID) -> id`, and `-setAttributeDescriptor:forKeyword:` is
    // `(id, AEKeyword)`.
    aterm_objc::autoreleasepool(|_| unsafe {
        let descriptor = class(c"NSAppleEventDescriptor").as_id();
        let make: unsafe extern "C-unwind" fn(
            aterm_objc::Id,
            aterm_objc::Sel,
            u32,
            u32,
            aterm_objc::Id,
            i16,
            i32,
        ) -> aterm_objc::Id = aterm_objc::checked_msg();
        let event = make(
            descriptor,
            sel!(appleEventWithEventClass:eventID:targetDescriptor:returnID:transactionID:),
            code(b"aevt"),
            code(b"quit"),
            aterm_objc::Id::NIL,
            -1,
            0,
        );
        assert!(!event.is_null());
        assert_eq!(super::quit_event_facts(event), (None, None), "a bare quit");
        let with_enum: unsafe extern "C-unwind" fn(
            aterm_objc::Id,
            aterm_objc::Sel,
            u32,
        ) -> aterm_objc::Id = aterm_objc::checked_msg();
        let set: unsafe extern "C-unwind" fn(aterm_objc::Id, aterm_objc::Sel, aterm_objc::Id, u32) =
            aterm_objc::checked_msg();
        set(
            event,
            sel!(setAttributeDescriptor:forKeyword:),
            with_enum(descriptor, sel!(descriptorWithEnumCode:), code(b"rlgo")),
            crate::system_quit::KEY_QUIT_REASON,
        );
        // The Apple Event Manager stamps `spid` only on an event it delivers, so a built
        // one carries no sender: the real Dock quit reads that one.
        assert_eq!(super::quit_event_facts(event), (Some(code(b"rlgo")), None));
    });
}

#[test]
fn an_unavailable_menu_confirmation_never_supplies_approval() {
    std::thread::spawn(|| {
        use crate::platform::AppRt as _;

        assert!(appkit::MainThread::new().is_none());
        let runtime = crate::platform::AppRtMacOS {
            probe_preferences: false,
        };
        assert_eq!(runtime.confirm("Change state?", "Test", "Proceed"), None);
    })
    .join()
    .expect("off-main confirmation check");
}

#[test]
fn the_registered_class_is_what_the_runtime_reports() {
    let cls = MenuTarget::class();
    assert!(!cls.is_null());
    assert_eq!(class(c"ATermMenuTarget"), cls);
    // SAFETY: `cls` is the class this module registered.
    unsafe {
        assert_eq!(class_name(cls), c"ATermMenuTarget");
        assert_eq!(class_name(aterm_objc::superclass_of(cls)), c"NSObject");
        assert!(appkit::send_bool_sel(
            cls.as_id(),
            sel!(instancesRespondToSelector:),
            sel!(menuAction:)
        ));
        assert!(appkit::send_bool_sel(
            cls.as_id(),
            sel!(instancesRespondToSelector:),
            sel!(validateMenuItem:)
        ));
    }
}

#[test]
fn the_runtime_reports_the_encodings_the_table_says() {
    let cls = MenuTarget::class();
    // SAFETY: `cls` is the live registered class.
    unsafe {
        assert_eq!(
            method_types(cls, sel!(menuAction:)).as_deref(),
            Some("v@:@")
        );
        assert_eq!(
            method_types(cls, sel!(validateMenuItem:)),
            Some(format!("{}@:@", Bool::ENCODING))
        );
        assert_eq!(method_types(cls, sel!(dealloc)).as_deref(), Some("v@:"));
    }
    #[cfg(target_arch = "aarch64")]
    assert_eq!(Bool::ENCODING, "B");
}

/// The System Settings open's sends are the runtime's own prototypes, and the
/// configuration it passes is a class the runtime has.
#[test]
fn the_unwaited_open_states_the_runtimes_prototypes() {
    let workspace = class(c"NSWorkspace");
    assert!(!workspace.is_null());
    assert!(!class(c"NSWorkspaceOpenConfiguration").is_null());
    // SAFETY: a live class; `prototype_check` reads an encoding and sends nothing.
    unsafe {
        assert_eq!(
            aterm_objc::prototype_check::<super::OpenUrl>(
                workspace,
                sel!(openURL:configuration:completionHandler:)
            ),
            Ok(())
        );
        assert_eq!(
            aterm_objc::prototype_check::<super::AppForUrl>(
                workspace,
                sel!(URLForApplicationToOpenURL:)
            ),
            Ok(())
        );
        assert!(
            aterm_objc::prototype_check::<
                unsafe extern "C-unwind" fn(
                    aterm_objc::Id,
                    aterm_objc::Sel,
                    aterm_objc::Id,
                ) -> Bool,
            >(workspace, sel!(openURL:configuration:completionHandler:))
            .is_err(),
            "the control: the synchronous prototype is not this method's"
        );
    }
}
