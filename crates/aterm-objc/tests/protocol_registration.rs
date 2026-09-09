// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A class may claim a protocol the host's frameworks never registered — and
//! the claim must be TRUE afterwards, not skipped and not fatal.
//!
//! This is the v0.72.0–v0.75.0 launch crash, kept as a tooth. Those releases
//! asserted in `ClassBuilder::add_protocol` that `objc_getProtocol` finds every
//! claimed protocol, with a message blaming an unlinked framework. AppKit was
//! linked; it simply does not register `NSApplicationDelegate` on macOS 14.4.1
//! (a protocol is registered only by an image whose `__objc_protolist` carries
//! it, and a Rust binary has none), so the application delegate's class could
//! not be built and the app died before its first window. The release cutter
//! runs on macOS 26, where AppKit does register it, so every gate that launched
//! the binary was green.
//!
//! Nothing here depends on which macOS runs the test: the first two tests use
//! a name NO image can carry, the third a name libobjc itself carries, and the
//! fourth reports which way this host went.

#![cfg(target_os = "macos")]

use std::ffi::{CStr, c_void};

use aterm_objc::{
    Bool, ClassPtr, ClassType, Sel, class, class_protocols, declare_class, msg, protocol,
    protocol_or_register, protocols_registered_by_aterm, sel,
};

/// `+conformsToProtocol:` — the question `-[NSObject conformsToProtocol:]`
/// forwards to the class, and the one every conformance assertion in the tree
/// asks.
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

/// A protocol name no image in any process carries.
const NEVER_CARRIED: &CStr = c"ATermObjcProtocolNoImageCarries";

declare_class! {
    /// Claims a protocol that is absent from the process until this crate
    /// supplies it.
    struct ClaimsAbsent: NSObject {
        const NAME: &str = "ATermObjcClaimsAbsentProtocol";
        type Ivars = ();
        protocols: [NSObject, ATermObjcProtocolNoImageCarries];
    }
}

declare_class! {
    /// A SECOND claimant of the same absent protocol: the registration must be
    /// shared, not repeated.
    struct AlsoClaimsAbsent: NSObject {
        const NAME: &str = "ATermObjcAlsoClaimsAbsentProtocol";
        type Ivars = ();
        protocols: [ATermObjcProtocolNoImageCarries];
    }
}

declare_class! {
    /// The application delegate's exact claim, built the way `vendor/winit`'s
    /// `app_state.rs` builds it — the class whose construction was the crash.
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
    // The protocol exists now, and the class really conforms — the answer the
    // objc2-era silent skip could never give.
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
    // No length comparison: the registry is process-global and the sibling
    // tests in this binary push into it from parallel threads, so a count is
    // not a fact about THIS call; the absence of `NSObject` above is.
}

/// The v0.72.0 crash, exactly: build a class that claims
/// `NSApplicationDelegate` with AppKit loaded. It must build on every macOS
/// aterm ships for, and it must conform afterwards, whichever side supplied
/// the protocol on this host.
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
