// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The state file changed: a Darwin notification (`notify_post`) a writer posts after
//! [`crate::state::save`], which the running mug observes and answers by reading the file
//! again. It carries nothing, so a stray post costs one read of a file the user owns.
//!
//! The mug observes it through CoreFoundation's Darwin notification center, whose callbacks
//! arrive on the main thread's run loop in its common modes (`CFNotificationCenter.h`;
//! measured 2026-10-19: a post from another thread was called back with
//! `pthread_main_np() == 1`), where the menu-bar app's state lives. Darwin notifications are
//! system-wide, so another user's post is one more read of this user's file.

/// The notification's name.
pub const CHANGED: &str = "com.aterm.aterm.caffeine.changed";

/// Tell a running mug the state file changed. Does nothing off macOS.
pub fn post_changed() {
    #[cfg(target_os = "macos")]
    {
        // Nothing to do on a failure: the mug reads the file again at its next start, and
        // `notify_post` fails only when notifyd is unreachable.
        let _ = darwin::post(darwin::CHANGED_C);
    }
}

#[cfg(target_os = "macos")]
pub(crate) use darwin::ChangedObserver;

#[cfg(target_os = "macos")]
mod darwin {
    use std::ffi::{CStr, c_char, c_void};

    use aterm_objc::Obj;

    /// [`super::CHANGED`] as the C string `notify_post` takes.
    pub(super) const CHANGED_C: &CStr = c"com.aterm.aterm.caffeine.changed";

    /// The suspension behavior passed: none. The Darwin center always delivers immediately
    /// (what `CFNotificationSuspensionBehaviorDeliverImmediately`, 4, asks of the others) and
    /// ignores the argument, and `CFNotificationCenter.h` asks for 0 in every ignored
    /// argument "to ensure future compatibility".
    const IGNORED_SUSPENSION_BEHAVIOR: isize = 0;

    /// `CFNotificationCallback`: `void (*)(CFNotificationCenterRef center, void *observer,
    /// CFNotificationName name, const void *object, CFDictionaryRef userInfo)`.
    type Callback =
        extern "C" fn(*mut c_void, *mut c_void, *const c_void, *const c_void, *const c_void);

    // `<notify.h>`; libsystem_notify is part of libSystem, which every binary links.
    unsafe extern "C" {
        /// Post `name` to every process registered for it; `NOTIFY_STATUS_OK` (0) on success.
        fn notify_post(name: *const c_char) -> u32;
    }

    // `<CoreFoundation/CFNotificationCenter.h>`.
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        /// The process's Darwin notify center: immortal, never released.
        fn CFNotificationCenterGetDarwinNotifyCenter() -> *mut c_void;
        /// `(center, observer, callBack, name, object, suspensionBehavior)`; the Darwin
        /// center ignores `object` and `suspensionBehavior` (both passed as 0) and copies
        /// `name`, which may not be NULL.
        fn CFNotificationCenterAddObserver(
            center: *mut c_void,
            observer: *const c_void,
            call_back: Callback,
            name: *const c_void,
            object: *const c_void,
            suspension_behavior: isize,
        );
        /// `(center, observer, name, object)`.
        fn CFNotificationCenterRemoveObserver(
            center: *mut c_void,
            observer: *const c_void,
            name: *const c_void,
            object: *const c_void,
        );
    }

    /// Post `name`; whether notifyd took it.
    pub(super) fn post(name: &CStr) -> bool {
        // SAFETY: `name` is a NUL-terminated string live for the call, which reads it only.
        unsafe { notify_post(name.as_ptr()) == 0 }
    }

    /// What a [`ChangedObserver`] runs; its address is the observer pointer the center
    /// hands back.
    struct Hook(Box<dyn Fn()>);

    /// A registered observer of [`super::CHANGED`]: its hook runs on the main thread each
    /// time a writer posts it, until this drops.
    pub(crate) struct ChangedObserver {
        /// The hook, boxed so its address (the observer pointer) does not move with `self`.
        hook: Box<Hook>,
        /// The name, an `NSString` toll-free bridged to the `CFStringRef` the center takes.
        name: Obj,
    }

    impl ChangedObserver {
        /// Run `hook` whenever [`super::CHANGED`] is posted, from the main run loop (the
        /// menu-bar app's `-run`). `None` off the main thread, where it is never registered:
        /// the hook is created, called and (this being `!Send`) dropped on the main thread
        /// alone, so no delivery can find it freed. `None` too when Foundation refuses the
        /// name.
        pub(crate) fn register(hook: impl Fn() + 'static) -> Option<Self> {
            aterm_objc::MainThread::new()?;
            let name = aterm_objc::ns_string(super::CHANGED)?;
            let this = Self {
                hook: Box::new(Hook(Box::new(hook))),
                name,
            };
            // SAFETY: the Darwin center is immortal; the observer pointer is `this.hook`'s
            // heap address, which stays valid until `Drop` has removed it; `on_changed` has
            // the `CFNotificationCallback` prototype; the name is a live `NSString`, which is
            // a `CFStringRef` (toll-free bridged) the center copies.
            unsafe {
                CFNotificationCenterAddObserver(
                    CFNotificationCenterGetDarwinNotifyCenter(),
                    this.observer(),
                    on_changed,
                    this.name.id().as_ptr().cast_const(),
                    std::ptr::null(),
                    IGNORED_SUSPENSION_BEHAVIOR,
                );
            }
            Some(this)
        }

        /// The observer pointer: the hook's address.
        fn observer(&self) -> *const c_void {
            std::ptr::from_ref::<Hook>(&*self.hook).cast()
        }
    }

    impl Drop for ChangedObserver {
        fn drop(&mut self) {
            // SAFETY: the pair `register` added; after this no callback names the hook,
            // which drops with `self` once this returns.
            unsafe {
                CFNotificationCenterRemoveObserver(
                    CFNotificationCenterGetDarwinNotifyCenter(),
                    self.observer(),
                    self.name.id().as_ptr().cast_const(),
                    std::ptr::null(),
                );
            }
        }
    }

    /// The center's callback. Never unwinds into CoreFoundation: a panic in the hook is
    /// caught here and the post is dropped.
    extern "C" fn on_changed(
        _center: *mut c_void,
        observer: *mut c_void,
        _name: *const c_void,
        _object: *const c_void,
        _user_info: *const c_void,
    ) {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            deliver(
                observer.cast_const(),
                aterm_objc::MainThread::new().is_some(),
            );
        }));
    }

    /// Run the hook `observer` names, when it names one and this is the main thread: where
    /// the center calls back, and the only thread a [`ChangedObserver`] lives on.
    fn deliver(observer: *const c_void, on_main: bool) {
        if observer.is_null() || !on_main {
            return;
        }
        // SAFETY: a non-null observer is the address of the `Hook` a live
        // `ChangedObserver` registered on this thread: its `Drop` removes the observer, on
        // this thread, before it frees the hook, so no callback runs between the two.
        let hook = unsafe { &*observer.cast::<Hook>() };
        (hook.0)();
    }

    #[cfg(test)]
    mod tests {
        use std::cell::Cell;
        use std::rc::Rc;

        use super::*;

        #[test]
        fn the_c_name_is_the_name() {
            assert_eq!(CHANGED_C.to_str(), Ok(super::super::CHANGED));
        }

        #[test]
        fn a_post_reaches_notifyd() {
            // A name nobody observes, so no running mug reads its file again.
            assert!(post(c"com.aterm.aterm.caffeine.test.unobserved"));
        }

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
            // The callback itself, off the main thread (libtest's) and with no observer.
            on_changed(
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
            );
            on_changed(
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
            // libtest never runs a test on the process main thread.
            assert!(aterm_objc::MainThread::new().is_none());
            assert!(ChangedObserver::register(|| {}).is_none());
        }
    }
}
