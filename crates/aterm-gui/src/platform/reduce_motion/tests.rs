// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::{
    DID_WAKE_NOTIFICATION, DISPLAY_OPTIONS_NOTIFICATION, POWER_STATE_SYMBOL, ReduceMotionTarget,
    THERMAL_STATE_NOTIFICATION, WILL_SLEEP_NOTIFICATION, new_target, optional_notification,
    register,
};

fn unregister(target: &aterm_objc::Retained<ReduceMotionTarget>) {
    for center in [super::workspace_center(), super::default_center()] {
        if center.is_null() {
            continue;
        }
        // SAFETY: `-removeObserver:` is `-(void)(id)`; `center` is a live centre and `target` a live observer registered on it.
        unsafe {
            let f: unsafe extern "C-unwind" fn(aterm_objc::Id, aterm_objc::Sel, aterm_objc::Id) =
                aterm_objc::checked_msg();
            f(center, aterm_objc::sel!(removeObserver:), target.as_id());
        }
    }
}

fn post_default(name: aterm_objc::Id) {
    aterm_objc::autoreleasepool(|_| {
        let center = super::default_center();
        assert!(!center.is_null(), "no default notification centre");
        // SAFETY: `-postNotificationName:object:` is `-(void)(id, id)`; `center` is live, callers keep the NSString name alive, nil sender is legal.
        unsafe {
            let f: unsafe extern "C-unwind" fn(
                aterm_objc::Id,
                aterm_objc::Sel,
                aterm_objc::Id,
                aterm_objc::Id,
            ) = aterm_objc::checked_msg();
            f(
                center,
                aterm_objc::sel!(postNotificationName:object:),
                name,
                aterm_objc::Id::NIL,
            );
        }
    });
}

fn post() {
    post_named(DISPLAY_OPTIONS_NOTIFICATION);
}

fn post_named(notification: &str) {
    aterm_objc::autoreleasepool(|_| {
        let center = super::workspace_center();
        assert!(!center.is_null(), "no workspace notification centre");
        let name = aterm_objc::ns_string(notification).expect("NSString");
        // SAFETY: `-postNotificationName:object:` is `-(void)(id, id)`; `center` is live, `name` a live +1 NSString, nil sender is legal.
        unsafe {
            let f: unsafe extern "C-unwind" fn(
                aterm_objc::Id,
                aterm_objc::Sel,
                aterm_objc::Id,
                aterm_objc::Id,
            ) = aterm_objc::checked_msg();
            f(
                center,
                aterm_objc::sel!(postNotificationName:object:),
                name.id(),
                aterm_objc::Id::NIL,
            );
        }
    });
}

#[test]
fn a_real_workspace_notification_reaches_the_declared_class() {
    use aterm_objc::{ClassType, class, class_name, superclass_of};

    // SAFETY: these symbols are either absent or immutable Foundation NSString
    // constants. The known thermal value controls the required pointer dereference.
    let power_available = unsafe {
        assert_eq!(
            optional_notification(c"aterm_missing_foundation_notification_58495"),
            None,
            "an unavailable notification must be omitted"
        );
        assert_eq!(
            optional_notification(c"NSProcessInfoThermalStateDidChangeNotification"),
            Some(THERMAL_STATE_NOTIFICATION),
            "the resolver reads the exported value, not its address"
        );
        optional_notification(POWER_STATE_SYMBOL).is_some()
    };
    let hits = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&hits);
    let wakes = Arc::new(AtomicUsize::new(0));
    let woke = Arc::clone(&wakes);
    let recolors = Arc::new(AtomicUsize::new(0));
    let recolored = Arc::clone(&recolors);
    let sleeps = Arc::new(AtomicUsize::new(0));
    let slept = Arc::clone(&sleeps);
    let target = new_target(
        crate::appkit::test_witness(),
        Box::new(move || {
            seen.fetch_add(1, Ordering::SeqCst);
        }),
        Box::new(move || {
            woke.fetch_add(1, Ordering::SeqCst);
        }),
        Box::new(move || {
            recolored.fetch_add(1, Ordering::SeqCst);
        }),
        Box::new(move || {
            slept.fetch_add(1, Ordering::SeqCst);
        }),
    )
    .expect("+alloc/-init produced a ReduceMotionTarget");

    let cls = ReduceMotionTarget::class();
    assert!(!cls.is_null());
    assert_eq!(
        class(c"ATermReduceMotionTarget"),
        cls,
        "objc_getClass disagrees with the registered pair"
    );
    // SAFETY: `cls` is the class this module just registered, so it and its superclass are live, immortal class objects.
    unsafe {
        assert_eq!(class_name(cls), c"ATermReduceMotionTarget");
        assert_eq!(class_name(superclass_of(cls)), c"NSObject");
    }
    // SAFETY: `target` is a live instance; `-respondsToSelector:` is a side-effect-free query.
    unsafe {
        let responds: unsafe extern "C-unwind" fn(
            aterm_objc::Id,
            aterm_objc::Sel,
            aterm_objc::Sel,
        ) -> aterm_objc::Bool = aterm_objc::checked_msg();
        assert!(
            responds(
                target.as_id(),
                aterm_objc::sel!(respondsToSelector:),
                aterm_objc::sel!(reduceMotionDidChange:)
            )
            .as_bool()
        );
    }

    assert!(register(&target), "workspace centre registration failed");
    post();
    post();
    // Foundation delivers these on the global dispatch queue. No AppKit work may
    // happen in the relay, and a quiet page needs no motion tick to receive it.
    std::thread::spawn(|| {
        // SAFETY: Foundation owns these immutable names for the process lifetime.
        unsafe {
            if let Some(name) = optional_notification(POWER_STATE_SYMBOL) {
                post_default(name);
            }
            post_default(THERMAL_STATE_NOTIFICATION);
        }
    })
    .join()
    .expect("power notification sender");
    post_named(DID_WAKE_NOTIFICATION);
    post_named(WILL_SLEEP_NOTIFICATION);
    let colors_name = aterm_objc::ns_string(super::SYSTEM_COLORS_NOTIFICATION).expect("NSString");
    post_default(colors_name.id());
    unregister(&target);
    post();
    post_named(DID_WAKE_NOTIFICATION);
    post_named(WILL_SLEEP_NOTIFICATION);
    post_default(colors_name.id());
    // SAFETY: Foundation owns these immutable names for the process lifetime.
    unsafe {
        if let Some(name) = optional_notification(POWER_STATE_SYMBOL) {
            post_default(name);
        }
        post_default(THERMAL_STATE_NOTIFICATION);
    }
    assert_eq!(
        recolors.load(Ordering::SeqCst),
        1,
        "NSSystemColorsDidChangeNotification did not reach the colours relay \
         exactly once"
    );

    assert_eq!(
        hits.load(Ordering::SeqCst),
        3 + usize::from(power_available),
        "Foundation/AppKit did not deliver policy notifications to the declared class \
         exactly as many times as it was posted"
    );
    assert_eq!(
        wakes.load(Ordering::SeqCst),
        1,
        "AppKit did not deliver NSWorkspaceDidWakeNotification to the wake \
         selector exactly once"
    );
    // The account claims' quiet before sleep (docs/DESIGN-fluid-swarm-2026-10-28.md F19).
    assert_eq!(
        sleeps.load(Ordering::SeqCst),
        1,
        "AppKit did not deliver NSWorkspaceWillSleepNotification to the sleep \
         selector exactly once"
    );
}

#[test]
fn dropping_the_target_drops_its_relay() {
    struct Spy(Arc<AtomicUsize>);
    impl Drop for Spy {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
    let drops = Arc::new(AtomicUsize::new(0));
    let spy = Spy(Arc::clone(&drops));
    let target = new_target(
        crate::appkit::test_witness(),
        Box::new(move || {
            let _ = &spy;
        }),
        Box::new(|| {}),
        Box::new(|| {}),
        Box::new(|| {}),
    )
    .expect("target");
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    drop(target);
    assert_eq!(
        drops.load(Ordering::SeqCst),
        1,
        "the generated -dealloc did not drop the relay"
    );
}
