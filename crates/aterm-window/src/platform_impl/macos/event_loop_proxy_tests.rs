// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/macos/event_loop.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

use super::*;

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFGetRetainCount(cf: *const c_void) -> CFIndex;
}

fn main_wakes() -> usize {
    MAIN_WAKES.with(Cell::get)
}

fn retains(source: CFRunLoopSourceRef) -> CFIndex {
    // SAFETY: the caller holds a proxy that retains `source`, so it is a live CF object.
    unsafe { CFGetRetainCount(source as *const c_void) }
}

#[test]
fn a_proxy_clone_wakes_nothing_and_a_send_through_it_wakes_the_loop_once() {
    let (sender, receiver) = wake_queue::channel::<u32>();
    let proxy = EventLoopProxy::new(sender);
    let source = proxy.source;
    let held = retains(source);
    let before = main_wakes();
    let clones: Vec<_> = (0..64).map(|_| proxy.clone()).collect();
    assert_eq!(main_wakes(), before, "a clone is no event, so it wakes nothing");
    assert!(clones.iter().all(|clone| clone.source == source), "a clone shares its source");
    assert_eq!(retains(source), held + 64, "each clone holds its own retain");
    let kept = clones[0].clone();
    drop(clones);
    drop(proxy);
    assert_eq!(retains(source), held, "each dropped proxy gave its retain back");
    kept.send_event(7).expect("the receiver is alive");
    assert_eq!(main_wakes(), before + 1, "a send wakes the loop once");
    assert_eq!(receiver.try_next(), Some(7));
}
