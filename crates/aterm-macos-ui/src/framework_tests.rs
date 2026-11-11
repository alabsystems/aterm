// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn class_lookup_acquires_its_framework_before_querying_the_runtime() {
    let library = Framework::new(&[
        c"/System/Library/Frameworks/ServiceManagement.framework/ServiceManagement",
    ]);
    assert!(library.library.get().is_none());
    let class = library.class_using(c"NSObject", |name| {
        assert!(library.library.get().is_some());
        aterm_objc::class(name)
    });
    assert!(!class.is_null());
}

#[test]
fn a_resolved_function_executes_and_missing_exports_refuse() {
    let library = Framework::new(&[c"/usr/lib/libSystem.B.dylib"]);
    // SAFETY: getpid(void) returns Darwin pid_t, a signed 32-bit integer.
    let getpid: unsafe extern "C" fn() -> i32 = unsafe { library.function(c"getpid") };
    // SAFETY: getpid has no arguments or caller preconditions.
    // SAFETY: the same process-lifetime function remains callable after a failed lookup.
    assert_eq!(unsafe { getpid() } as u32, std::process::id());
    assert!(
        std::panic::catch_unwind(|| library.address(c"aterm_nonexistent_framework_export"))
            .is_err()
    );
    // A failed lookup cannot invalidate a previously resolved function or its handle.
    assert_eq!(unsafe { getpid() } as u32, std::process::id());
}

#[test]
fn an_unavailable_framework_refuses_instead_of_publishing_a_handle() {
    let library = Framework::new(&[
        c"/System/Library/Frameworks/aterm_nonexistent.framework/aterm_nonexistent",
    ]);
    assert!(std::panic::catch_unwind(|| library.load()).is_err());
    assert!(library.library.get().is_none());
}
