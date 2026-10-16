// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Autorelease-pool nesting. Neither `!Send` nor `!Clone` implies LIFO drop order, so
//! `AutoreleasePool::new()` is `unsafe`. The out-of-order pop that breaks nesting runs in a child
//! process (it kills the process); the scope form, the only safe one, nests to any depth.

#![cfg(target_os = "macos")]
#![allow(
    clippy::disallowed_types,
    reason = "test code: a stand-in child may inherit descriptors"
)]

use std::os::unix::process::ExitStatusExt as _;
use std::process::Command;

use aterm_objc::{AutoreleasePool, autoreleasepool, ns_string, ns_string_to_rust};

/// Set in the child so it runs the body instead of re-spawning.
const CHILD: &str = "ATERM_OBJC_POOL_OUT_OF_ORDER_CHILD";

/// `SIGABRT`, spelled out rather than taken from `libc`: this crate has no dependencies, and
/// POSIX fixes the number.
const SIGABRT: i32 = 6;

#[test]
fn an_out_of_order_pop_is_fatal_which_is_why_new_is_unsafe() {
    if std::env::var_os(CHILD).is_some() {
        // SAFETY: deliberately violated, in a child, so the runtime reports the misuse.
        unsafe {
            let outer = AutoreleasePool::new();
            let inner = AutoreleasePool::new();
            drop(outer);
            drop(inner);
        }
        // Reached only if the runtime tolerated the pop; the parent then fails.
        println!("SURVIVED-THE-OUT-OF-ORDER-POP");
        return;
    }

    let exe = std::env::current_exe().expect("the test binary's own path");
    let out = Command::new(exe)
        .args([
            "--exact",
            "--nocapture",
            "an_out_of_order_pop_is_fatal_which_is_why_new_is_unsafe",
        ])
        .env(CHILD, "1")
        .output()
        .expect("re-running this test binary as a child");

    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !stdout.contains("SURVIVED-THE-OUT-OF-ORDER-POP"),
        "the runtime accepted an out-of-order pop; the hazard this test arms \
         has changed shape and the SAFETY reasoning must be re-derived\n\
         stdout: {stdout}\nstderr: {stderr}"
    );
    assert_eq!(
        out.status.signal(),
        Some(SIGABRT),
        "expected SIGABRT from the Objective-C runtime, got {:?}\n\
         stdout: {stdout}\nstderr: {stderr}",
        out.status
    );
    assert!(
        stderr.contains("autorelease pool"),
        "aborted, but not with the runtime's pool diagnostic:\n{stderr}"
    );
}

#[test]
fn the_scope_form_nests_to_any_depth_and_keeps_objects_alive_to_its_end() {
    // The out-of-order shape as scopes: an inner scope cannot outlive the one it is inside.
    let deep = autoreleasepool(|_a| {
        autoreleasepool(|_b| {
            autoreleasepool(|_c| {
                let s = ns_string("three deep").expect("NSString");
                // SAFETY: `s` is a live +1 NSString owned by this scope.
                unsafe { ns_string_to_rust(s.id()) }
            })
        })
    });
    assert_eq!(deep, "three deep");
}

#[test]
fn a_scope_pops_its_pool_even_when_the_body_unwinds() {
    // The pool is a local of `autoreleasepool`'s frame, so unwinding pops it. A skipped pop would
    // make the next pool on this thread pop against a stack the runtime disagrees with.
    let caught = std::panic::catch_unwind(|| {
        autoreleasepool(|_| {
            let s = ns_string("about to unwind").expect("NSString");
            let _ = s;
            panic!("deliberate");
        })
    });
    assert!(caught.is_err());
    // A pool pushed after the unwind still pops cleanly; a leaked token would kill the binary here.
    let after = autoreleasepool(|_| {
        let s = ns_string("after").expect("NSString");
        // SAFETY: `s` is a live +1 NSString owned by this scope.
        unsafe { ns_string_to_rust(s.id()) }
    });
    assert_eq!(after, "after");
}
