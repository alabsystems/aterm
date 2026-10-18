// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Drives the block ABI through system code that copies and invokes the block
//! (libdispatch, Foundation, CoreFoundation). Calling our own `invoke` pointer
//! would prove only that it is where we put it; the risk is whether
//! `libclosure` accepts the layout, runs the copy/dispose helpers, and hands
//! `invoke` the block back as its first argument.

#![cfg(target_os = "macos")]
#![expect(
    clippy::disallowed_methods,
    reason = "aterm-objc's own runtime tests drive the bare `msg` cast; everything else uses checked_msg"
)]

use std::ffi::c_void;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use aterm_objc::{Id, RcBlock, Sel, msg, ns_string, sel};

unsafe extern "C" {
    fn dispatch_queue_create(label: *const std::ffi::c_char, attr: *const c_void) -> *mut c_void;
    fn dispatch_release(obj: *mut c_void);
    /// `dispatch_sync(queue, ^{ … })` — a real arity-0 block driver.
    fn dispatch_sync(queue: *mut c_void, block: *mut c_void);
    /// `dispatch_apply(count, queue, ^(size_t i){ … })` — arity 1.
    fn dispatch_apply(iterations: usize, queue: *mut c_void, block: *mut c_void);
}

/// A payload whose drop is observable, so "the dispose helper runs exactly
/// once" is measured rather than assumed.
struct DropSpy(Arc<AtomicUsize>);

impl Drop for DropSpy {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

fn queue() -> *mut c_void {
    // SAFETY: a NULL label and NULL attributes request an unlabelled serial
    // queue, which is the documented default.
    let q = unsafe { dispatch_queue_create(std::ptr::null(), std::ptr::null()) };
    assert!(!q.is_null());
    q
}

#[test]
fn libdispatch_invokes_an_arity_zero_block() {
    let hits = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&hits);
    // SAFETY: `dispatch_sync` calls a `void (^)(void)` block, which is exactly
    // the `Fn() -> ()` prototype declared here.
    let block = unsafe { RcBlock::new0(move || seen.fetch_add(1, Ordering::SeqCst)) }
        .expect("_Block_copy produced a heap block");
    let q = queue();
    // SAFETY: `q` is a live queue and `block` a live heap block of the right
    // signature; `dispatch_sync` runs it to completion before returning.
    unsafe {
        dispatch_sync(q, block.as_ptr());
        dispatch_sync(q, block.as_ptr());
        dispatch_release(q);
    }
    assert_eq!(hits.load(Ordering::SeqCst), 2);
}

#[test]
fn libdispatch_invokes_an_arity_one_block_with_its_argument() {
    let sum = Arc::new(AtomicUsize::new(0));
    let acc = Arc::clone(&sum);
    // SAFETY: `dispatch_apply` calls a `void (^)(size_t)` block — the `Fn(usize)`
    // prototype declared here.
    let block = unsafe {
        RcBlock::new1(move |i: usize| {
            acc.fetch_add(i, Ordering::SeqCst);
        })
    }
    .expect("_Block_copy produced a heap block");
    let q = queue();
    // SAFETY: as above; `dispatch_apply` is synchronous.
    unsafe {
        dispatch_apply(5, q, block.as_ptr());
        dispatch_release(q);
    }
    // The runtime passed 0..5, so the arguments arrived intact (0+1+2+3+4).
    assert_eq!(sum.load(Ordering::SeqCst), 10);
}

#[test]
fn foundation_invokes_an_arity_two_block() {
    // `-[NSString enumerateLinesUsingBlock:]` takes `void (^)(NSString *line,
    // BOOL *stop)` — a real two-argument block whose first argument is an
    // object, which is the shape `app_launch_successor.rs`'s LaunchServices
    // completion handler has.
    let lines = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let sink = Arc::clone(&lines);
    // `stop` is `BOOL *`, a `signed char` on the x86_64 compat slice, so
    // `*mut bool` would be undefined behaviour; `*mut Bool` is the only
    // spelling `RcBlock::newN`'s `Encode` bound accepts (see
    // `BOOL_PTR_ENCODING`).
    // SAFETY: the prototype below is exactly what Foundation documents for
    // `enumerateLinesUsingBlock:`.
    let block = unsafe {
        RcBlock::new2(move |line: Id, _stop: *mut aterm_objc::Bool| {
            // SAFETY (inherited from the enclosing block): Foundation passes a
            // live, autoreleased NSString for the duration of the call; the
            // bytes are copied out before returning.
            sink.lock()
                .expect("uncontended")
                .push(aterm_objc::ns_string_to_rust(line));
        })
    }
    .expect("_Block_copy produced a heap block");

    let text = ns_string("alpha\nbeta\ngamma").expect("NSString");
    // SAFETY: `enumerateLinesUsingBlock:` is `-(void)` taking one block
    // argument; `text` is a live +1 NSString.
    unsafe {
        let enumerate: unsafe extern "C-unwind" fn(Id, Sel, *mut c_void) = msg();
        enumerate(text.id(), sel!(enumerateLinesUsingBlock:), block.as_ptr());
    }
    assert_eq!(
        *lines.lock().expect("uncontended"),
        vec!["alpha".to_owned(), "beta".to_owned(), "gamma".to_owned()]
    );
}

#[test]
fn a_returning_block_hands_its_value_back() {
    // The `alert_keys.rs` shape, one object in and one out, invoked through the
    // `invoke` slot of the runtime's heap copy rather than our stack original.
    let sentinel = ns_string("sentinel").expect("NSString");
    let expect = sentinel.id();
    // SAFETY: an `id (^)(id)` block, matching the prototype used below.
    let block = unsafe { RcBlock::new1(move |x: Id| x) }.expect("heap block");

    #[repr(C)]
    struct HeaderPrefix {
        isa: *const c_void,
        flags: i32,
        reserved: i32,
        invoke: unsafe extern "C" fn(*mut c_void, Id) -> Id,
    }
    // SAFETY: the first four fields of every block are fixed ABI, and `block`
    // is a live heap block this crate built with an `id (^)(id)` invoke.
    let out = unsafe {
        let hdr = &*block.as_ptr().cast::<HeaderPrefix>();
        (hdr.invoke)(block.as_ptr(), expect)
    };
    assert_eq!(out, expect);
}

#[test]
fn a_retained_block_defers_the_dispose() {
    let drops = Arc::new(AtomicUsize::new(0));
    let spy = DropSpy(Arc::clone(&drops));
    // SAFETY: a `void (^)(void)` block.
    let block = unsafe {
        RcBlock::new0(move || {
            let _ = &spy;
        })
    }
    .expect("heap block");
    let second = block.clone_retained();
    assert_eq!(
        second.as_ptr(),
        block.as_ptr(),
        "_Block_copy on a heap block must retain, not copy"
    );
    drop(block);
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    drop(second);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}
