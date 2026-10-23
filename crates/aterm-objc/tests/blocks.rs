// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Block ABI tests driven through libdispatch, Foundation, and CoreFoundation.

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
    /// `dispatch_sync(queue, ^{ … })`.
    fn dispatch_sync(queue: *mut c_void, block: *mut c_void);
    /// `dispatch_apply(count, queue, ^(size_t i){ … })`.
    fn dispatch_apply(iterations: usize, queue: *mut c_void, block: *mut c_void);
}

/// A payload whose drop is observable.
struct DropSpy(Arc<AtomicUsize>);

impl Drop for DropSpy {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

fn queue() -> *mut c_void {
    // SAFETY: a NULL label and NULL attributes request an unlabelled serial queue.
    let q = unsafe { dispatch_queue_create(std::ptr::null(), std::ptr::null()) };
    assert!(!q.is_null());
    q
}

#[test]
fn libdispatch_invokes_an_arity_zero_block() {
    let hits = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&hits);
    // SAFETY: `dispatch_sync` calls a `void (^)(void)` block.
    let block = unsafe {
        RcBlock::new0(move || {
            seen.fetch_add(1, Ordering::SeqCst);
        })
    }
    .expect("_Block_copy produced a heap block");
    let q = queue();
    // SAFETY: `q` is a live queue and `block` a live heap block of the right signature.
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
    // SAFETY: `dispatch_apply` calls a `void (^)(size_t)` block.
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
    assert_eq!(sum.load(Ordering::SeqCst), 10);
}

#[test]
fn foundation_invokes_an_arity_two_block() {
    let lines = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let sink = Arc::clone(&lines);
    // SAFETY: prototype matches Foundation's `enumerateLinesUsingBlock:`.
    let block = unsafe {
        RcBlock::new2(move |line: Id, _stop: *mut aterm_objc::Bool| {
            sink.lock()
                .expect("uncontended")
                .push(aterm_objc::ns_string_to_rust(line));
        })
    }
    .expect("_Block_copy produced a heap block");

    let text = ns_string("alpha\nbeta\ngamma").expect("NSString");
    // SAFETY: `enumerateLinesUsingBlock:` is `-(void)` taking one block argument.
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
    // SAFETY: the first four fields of every block are fixed ABI.
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

#[test]
fn a_framework_block_is_called_through_its_header() {
    unsafe extern "C" {
        /// `dispatch_block_create(flags, block)`.
        fn dispatch_block_create(flags: usize, block: *mut c_void) -> *mut c_void;
        fn _Block_release(block: *const c_void);
    }
    let hits = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&hits);
    // SAFETY: `void (^)(void)`, the prototype libdispatch's wrapper calls.
    let ours = unsafe {
        RcBlock::new0(move || {
            seen.fetch_add(1, Ordering::SeqCst);
        })
    }
    .expect("heap block");
    // SAFETY: flags 0 and a live `dispatch_block_t`; the wrapper copies it.
    let made = unsafe { dispatch_block_create(0, ours.as_ptr()) };
    assert!(!made.is_null(), "libdispatch made a block");
    assert_ne!(made, ours.as_ptr(), "a block of its own, not ours");
    // SAFETY: `made` is a live `void (^)(void)` block; released once after.
    unsafe {
        aterm_objc::BlockPtr::from_ptr(made).call0();
        _Block_release(made);
    }
    assert_eq!(hits.load(Ordering::SeqCst), 1, "ours ran, once");

    let sum = Arc::new(AtomicUsize::new(0));
    let acc = Arc::clone(&sum);
    // SAFETY: `void (^)(NSUInteger)`, the prototype `call1` names below.
    let one = unsafe {
        RcBlock::new1(move |options: usize| {
            acc.fetch_add(options, Ordering::SeqCst);
        })
    }
    .expect("heap block");
    // SAFETY: a live `void (^)(NSUInteger)` block, and a nil one.
    unsafe {
        one.as_block_ptr().call1(26_usize);
        aterm_objc::BlockPtr::from_ptr(std::ptr::null_mut()).call0();
    }
    assert_eq!(sum.load(Ordering::SeqCst), 26);
}

#[test]
fn a_named_prototype_builds_and_calls_its_block() {
    /// `void (^)(NSUInteger, NSUInteger)`.
    type Pair = fn(usize, usize);
    /// `void (^)(void)`.
    type Done = fn();
    let sum = Arc::new(AtomicUsize::new(0));
    let acc = Arc::clone(&sum);
    // SAFETY: called below as exactly `Pair`.
    let pair = unsafe {
        RcBlock::of::<Pair, _>(move |a: usize, b: usize| {
            acc.fetch_add(a * 10 + b, Ordering::SeqCst);
        })
    }
    .expect("heap block");
    let hits = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&hits);
    // SAFETY: called below as exactly `Done`.
    let done = unsafe {
        RcBlock::of::<Done, _>(move || {
            seen.fetch_add(1, Ordering::SeqCst);
        })
    }
    .expect("heap block");
    // SAFETY: live blocks of exactly these prototypes, and a nil one.
    unsafe {
        pair.as_block_ptr().call::<Pair>((2, 6));
        done.as_block_ptr().call::<Done>(());
        aterm_objc::BlockPtr::from_ptr(std::ptr::null_mut()).call::<Done>(());
    }
    assert_eq!(sum.load(Ordering::SeqCst), 26);
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    assert_eq!(<Pair as aterm_objc::BlockFn>::types(), "v@?QQ");
    assert_eq!(<Done as aterm_objc::BlockFn>::types(), "v@?");
}
