// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Runtime registration tests for declared protocols not carried by loaded images.

#![cfg(target_os = "macos")]
#![expect(
    clippy::disallowed_methods,
    reason = "aterm-objc's own runtime tests drive the bare `msg` cast; everything else uses checked_msg"
)]

use std::ffi::{CStr, c_void};

use aterm_objc::{
    Bool, ClassPtr, ClassType, Sel, class, class_protocols, declare_class, msg, protocol,
    protocol_or_register, protocols_registered_by_aterm, sel,
};

fn conforms(cls: ClassPtr, name: &'static CStr) -> bool {
    let proto = protocol(name);
    if proto.is_null() {
        return false;
    }
    // SAFETY: `+conformsToProtocol:` is a side-effect-free class-level query
    // (`B@:@`) on a live class object and a live protocol object.
    unsafe {
        let f: unsafe extern "C-unwind" fn(ClassPtr, Sel, aterm_objc::ProtocolPtr) -> Bool = msg();
        f(cls, sel!(conformsToProtocol:), proto).as_bool()
    }
}

const NEVER_CARRIED: &CStr = c"ATermObjcProtocolNoImageCarries";

declare_class! {
    struct ClaimsAbsent: NSObject {
        const NAME: &str = "ATermObjcClaimsAbsentProtocol";
        type Ivars = ();
        protocols: [NSObject, ATermObjcProtocolNoImageCarries];
    }
}

declare_class! {
    struct AlsoClaimsAbsent: NSObject {
        const NAME: &str = "ATermObjcAlsoClaimsAbsentProtocol";
        type Ivars = ();
        protocols: [ATermObjcProtocolNoImageCarries];
    }
}

declare_class! {
    struct ClaimsAppDelegate: NSObject {
        const NAME: &str = "ATermObjcClaimsNSApplicationDelegate";
        type Ivars = ();
        protocols: [NSObject, NSApplicationDelegate];
    }
}

#[test]
fn claiming_a_protocol_no_image_carries_registers_it_and_the_claim_is_true() {
    assert!(
        protocol(NEVER_CARRIED).is_null()
            || protocols_registered_by_aterm().contains(&NEVER_CARRIED),
        "precondition: nothing but this crate can have registered {NEVER_CARRIED:?}"
    );
    let cls = ClaimsAbsent::class();
    assert!(
        !protocol(NEVER_CARRIED).is_null(),
        "the claim supplied the protocol"
    );
    assert!(conforms(cls, NEVER_CARRIED));
    assert!(conforms(cls, c"NSObject"));
    // SAFETY: `cls` is the live class just registered.
    let claimed = unsafe { class_protocols(cls) };
    assert!(
        claimed
            .iter()
            .any(|p| p == "ATermObjcProtocolNoImageCarries"),
        "the class's own protocol list names it: {claimed:?}"
    );
    assert!(
        protocols_registered_by_aterm().contains(&NEVER_CARRIED),
        "and the registry says this crate supplied it"
    );
}

#[test]
fn a_second_claim_of_the_same_absent_protocol_shares_one_registration() {
    let first = ClaimsAbsent::class();
    let second = AlsoClaimsAbsent::class();
    assert!(conforms(first, NEVER_CARRIED));
    assert!(conforms(second, NEVER_CARRIED));
    let one = protocol_or_register(NEVER_CARRIED);
    let again = protocol_or_register(NEVER_CARRIED);
    assert_eq!(
        one, again,
        "one protocol object per name, however many claim it"
    );
    assert_eq!(
        protocols_registered_by_aterm()
            .iter()
            .filter(|n| **n == NEVER_CARRIED)
            .count(),
        1,
        "recorded once"
    );
}

#[test]
fn a_protocol_the_host_registers_is_never_re_registered() {
    let p = protocol_or_register(c"NSObject");
    assert_eq!(
        p,
        protocol(c"NSObject"),
        "libobjc's own NSObject protocol, untouched"
    );
    assert!(
        !protocols_registered_by_aterm().contains(&c"NSObject"),
        "NSObject is libobjc's, not this crate's"
    );
}

#[test]
fn the_application_delegate_claim_builds_on_this_host_and_is_true() {
    unsafe extern "C" {
        fn dlopen(path: *const std::ffi::c_char, mode: i32) -> *mut c_void;
    }
    const RTLD_LAZY: i32 = 0x1;
    // SAFETY: a valid C path and a documented mode; the handle is deliberately
    // never closed — AppKit is process-lifetime once loaded.
    let appkit = unsafe {
        dlopen(
            c"/System/Library/Frameworks/AppKit.framework/AppKit".as_ptr(),
            RTLD_LAZY,
        )
    };
    assert!(
        !appkit.is_null(),
        "AppKit must load for this to be the real claim"
    );
    assert!(
        !class(c"NSApplication").is_null(),
        "AppKit's classes are present"
    );

    let host_had_it = !protocol(c"NSApplicationDelegate").is_null()
        && !protocols_registered_by_aterm().contains(&c"NSApplicationDelegate");

    let cls = ClaimsAppDelegate::class();

    assert!(conforms(cls, c"NSApplicationDelegate"));
    assert!(conforms(cls, c"NSObject"));
    let supplied_by_aterm = protocols_registered_by_aterm().contains(&c"NSApplicationDelegate");
    assert_ne!(
        host_had_it, supplied_by_aterm,
        "exactly one side supplies the protocol: the host's AppKit or this crate"
    );
    eprintln!(
        "NSApplicationDelegate on this host: {}",
        if host_had_it {
            "registered by AppKit (the release cutter's shape)"
        } else {
            "NOT registered by AppKit — supplied by aterm-objc (the v0.72.0 crash's shape)"
        }
    );
}
