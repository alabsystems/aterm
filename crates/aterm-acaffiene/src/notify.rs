// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Darwin notification posted after [`crate::state::save`].

/// The notification's name.
pub const CHANGED: &str = "com.aterm.aterm.acaffiene.changed";

/// Tell a running mug the state file changed.
pub fn post_changed() {
    #[cfg(target_os = "macos")]
    {
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
    pub(super) const CHANGED_C: &CStr = c"com.aterm.aterm.acaffiene.changed";

    /// The suspension behavior passed.
    const IGNORED_SUSPENSION_BEHAVIOR: isize = 0;

    /// `CFNotificationCallback`.
    type Callback =
        extern "C" fn(*mut c_void, *mut c_void, *const c_void, *const c_void, *const c_void);

    unsafe extern "C" {
        /// Post `name` to every process registered for it.
        fn notify_post(name: *const c_char) -> u32;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        /// The process's Darwin notify center.
        fn CFNotificationCenterGetDarwinNotifyCenter() -> *mut c_void;
        /// `(center, observer, callBack, name, object, suspensionBehavior)`.
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

    /// What a [`ChangedObserver`] runs.
    struct Hook(Box<dyn Fn()>);

    /// A registered observer of [`super::CHANGED`].
    pub(crate) struct ChangedObserver {
        /// The hook, boxed so its address (the observer pointer) does not move with `self`.
        hook: Box<Hook>,
        /// The name, an `NSString` toll-free bridged to the `CFStringRef` the center takes.
        name: Obj,
    }

    impl ChangedObserver {
        /// Run `hook` whenever [`super::CHANGED`] is posted, from the main run loop.
        pub(crate) fn register(hook: impl Fn() + 'static) -> Option<Self> {
            aterm_objc::MainThread::new()?;
            let name = aterm_objc::ns_string(super::CHANGED)?;
            let this = Self {
                hook: Box::new(Hook(Box::new(hook))),
                name,
            };
            // SAFETY: the Darwin center is immortal; the observer (`this.hook`'s address) stays
            // valid until `Drop` removes it; `on_changed` has the `CFNotificationCallback`
            // prototype; the name is a live `NSString`, a `CFStringRef` the center copies.
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

        /// The observer pointer.
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

    /// The center's callback.
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

    /// Run the hook `observer` names on the main thread ([`ChangedObserver`]).
    fn deliver(observer: *const c_void, on_main: bool) {
        if observer.is_null() || !on_main {
            return;
        }
        // SAFETY: a non-null `observer` is the `Hook` of a live `ChangedObserver` on this thread,
        // whose `Drop` removes the observer before the hook is freed.
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
            assert!(post(c"com.aterm.aterm.acaffiene.test.unobserved"));
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
            assert!(aterm_objc::MainThread::new().is_none());
            assert!(ChangedObserver::register(|| {}).is_none());
        }
    }
}
