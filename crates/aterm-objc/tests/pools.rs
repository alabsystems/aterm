// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Autorelease-pool nesting — D2's arming.
//!
//! W1 shipped `AutoreleasePool::new()` as a SAFE constructor under this
//! comment, promoted verbatim from `aterm-gpu/src/metal/ffi.rs:238`:
//!
//! > Pools are strictly nested because this type is neither `Send` nor
//! > cloneable and the token never escapes.
//!
//! Neither `!Send` nor `!Clone` implies LIFO drop order. This file runs the
//! four safe lines that refute it — in a CHILD PROCESS, because what they do is
//! kill the process — and then shows the same shape written as scopes, which is
//! the only form the crate now offers without `unsafe`.

#![cfg(target_os = "macos")]

use std::os::unix::process::ExitStatusExt as _;
use std::process::Command;

use aterm_objc::{AutoreleasePool, autoreleasepool, ns_string, ns_string_to_rust};

/// Set in the child so it runs the body instead of re-spawning.
const CHILD: &str = "ATERM_OBJC_POOL_OUT_OF_ORDER_CHILD";

/// `SIGABRT`, spelled out rather than pulled from `libc`: this crate has ZERO
/// dependencies by construction and the number is fixed by POSIX.
const SIGABRT: i32 = 6;

#[test]
fn an_out_of_order_pop_is_fatal_which_is_why_new_is_unsafe() {
    if std::env::var_os(CHILD).is_some() {
        // The judge's counterexample, verbatim except for the `unsafe` the fix
        // added — which is the fix: this is no longer expressible in safe Rust.
        // SAFETY: deliberately VIOLATED. The whole point of this child process
        // is to execute the misuse the old safe API allowed and let the
        // Objective-C runtime say what it thinks of it.
        unsafe {
            let outer = AutoreleasePool::new();
            let inner = AutoreleasePool::new();
            drop(outer);
            drop(inner);
        }
        // Reached only if the runtime tolerated the out-of-order pop, in which
        // case the parent's assertion below fails and says so.
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
    // The same four lines' INTENT, written the only way the crate now allows.
    // Out-of-order pop is unconstructible here: the inner scope cannot outlive
    // the outer one, because it is inside it.
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
    // If a panic could skip the pop, the next pool on this thread would pop
    // against a stack the runtime no longer agrees with — the same fatal error
    // as an out-of-order drop, just delayed. The pool is a local of
    // `autoreleasepool`'s own frame, so unwinding drops it.
    let caught = std::panic::catch_unwind(|| {
        autoreleasepool(|_| {
            let s = ns_string("about to unwind").expect("NSString");
            let _ = s;
            panic!("deliberate");
        })
    });
    assert!(caught.is_err());
    // A pool pushed AFTER the unwind must still pop cleanly. If the previous
    // one had leaked its token this line would abort the test binary.
    let after = autoreleasepool(|_| {
        let s = ns_string("after").expect("NSString");
        // SAFETY: `s` is a live +1 NSString owned by this scope.
        unsafe { ns_string_to_rust(s.id()) }
    });
    assert_eq!(after, "after");
}
