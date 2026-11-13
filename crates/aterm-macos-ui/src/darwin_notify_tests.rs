// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::cell::Cell;
use std::rc::Rc;

use super::*;

#[test]
fn a_delivery_runs_the_hook_only_on_the_main_thread_and_for_an_observer() {
    let hits = Rc::new(Cell::new(0_u32));
    let seen = Rc::clone(&hits);
    let hook = Hook(Box::new(move || seen.set(seen.get() + 1)));
    let observer = std::ptr::from_ref(&hook).cast::<c_void>();
    deliver(observer, true);
    assert_eq!(hits.get(), 1);
    deliver(observer, false);
    deliver(std::ptr::null(), true);
    assert_eq!(hits.get(), 1);
    on_posted(
        std::ptr::null_mut(),
        std::ptr::null_mut(),
        std::ptr::null(),
        std::ptr::null(),
        std::ptr::null(),
    );
    on_posted(
        std::ptr::null_mut(),
        observer.cast_mut(),
        std::ptr::null(),
        std::ptr::null(),
        std::ptr::null(),
    );
    assert_eq!(hits.get(), 1);
}

#[test]
fn an_observer_is_registered_only_on_the_main_thread() {
    assert!(aterm_objc::MainThread::new().is_none());
    assert!(Observer::register(c"com.aterm.aterm.test.unobserved", || {}).is_none());
}

#[test]
fn the_center_this_registers_with_is_the_darwin_one() {
    // SAFETY: the accessor takes nothing and answers the immortal center.
    let center = unsafe { cf::darwin_center() };
    assert!(!center.is_null());
    // SAFETY: as above; it answers the same center every time.
    assert_eq!(center, unsafe { cf::darwin_center() });
}
