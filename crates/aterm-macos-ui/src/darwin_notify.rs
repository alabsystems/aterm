// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! An observer of a Darwin notification (`notify_post(3)`'s names), run on the main run loop.
//!
//! A command line that changed a state file posts the file's notification
//! (`aterm_sysprobe::notify::post`); the running app that shows that state registers an
//! [`Observer`] and reads the file again. The mug's mode (`aterm acaffeine on|off`) and this
//! Mac's fleet access (`aterm fleet access`) both travel this way. A notification carries no
//! payload and no sender: the hook reads the truth from the file.

use std::ffi::{CStr, c_void};

use aterm_objc::Obj;

use crate::framework::Framework;

static CORE_FOUNDATION: Framework =
    Framework::new(&[c"/System/Library/Frameworks/CoreFoundation.framework/CoreFoundation"]);

/// `CFNotificationSuspensionBehavior`, which the Darwin center ignores.
const IGNORED_SUSPENSION_BEHAVIOR: isize = 0;

/// `CFNotificationCallback`.
type Callback =
    extern "C" fn(*mut c_void, *mut c_void, *const c_void, *const c_void, *const c_void);

/// CoreFoundation's notification-center exports.
mod cf {
    use std::ffi::c_void;

    use super::{CORE_FOUNDATION, Callback};

    framework_functions! { CORE_FOUNDATION, "C";
        /// The process's Darwin notify center.
        /// # Safety
        /// None: it answers the immortal center.
        fn darwin_center() -> *mut c_void = c"CFNotificationCenterGetDarwinNotifyCenter";
        /// `(center, observer, callBack, name, object, suspensionBehavior)`.
        /// # Safety
        /// `center` is a live center, `observer` stays valid until removed, and `name` is a
        /// live `CFStringRef`.
        fn add_observer(
            center: *mut c_void,
            observer: *const c_void,
            call_back: Callback,
            name: *const c_void,
            object: *const c_void,
            suspension_behavior: isize,
        ) = c"CFNotificationCenterAddObserver";
        /// `(center, observer, name, object)`.
        /// # Safety
        /// `center` is a live center and `name` a live `CFStringRef`.
        fn remove_observer(
            center: *mut c_void,
            observer: *const c_void,
            name: *const c_void,
            object: *const c_void,
        ) = c"CFNotificationCenterRemoveObserver";
    }
}

/// What an [`Observer`] runs.
struct Hook(Box<dyn Fn()>);

/// A registered observer of one Darwin notification; dropping it stops the hook.
pub struct Observer {
    /// The hook, boxed so its address (the observer pointer) does not move with `self`.
    hook: Box<Hook>,
    /// The name, an `NSString` toll-free bridged to the `CFStringRef` the center takes.
    name: Obj,
}

impl Observer {
    /// Run `hook` on the main run loop whenever the notification `name` is posted; `None` off
    /// the main thread, or where `name` is no UTF-8 string.
    pub fn register(name: &CStr, hook: impl Fn() + 'static) -> Option<Self> {
        aterm_objc::MainThread::new()?;
        let name = aterm_objc::ns_string(name.to_str().ok()?)?;
        let this = Self {
            hook: Box::new(Hook(Box::new(hook))),
            name,
        };
        // SAFETY: the Darwin center is immortal; the observer (`this.hook`'s address) stays
        // valid until `Drop` removes it; `on_posted` has the `CFNotificationCallback`
        // prototype; the name is a live `NSString`, a `CFStringRef` the center copies.
        unsafe {
            cf::add_observer(
                cf::darwin_center(),
                this.observer(),
                on_posted,
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

impl Drop for Observer {
    fn drop(&mut self) {
        // SAFETY: the pair `register` added; after this no callback names the hook, which drops
        // with `self` once this returns.
        unsafe {
            cf::remove_observer(
                cf::darwin_center(),
                self.observer(),
                self.name.id().as_ptr().cast_const(),
                std::ptr::null(),
            );
        }
    }
}

/// The center's callback.
extern "C" fn on_posted(
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

/// Run the hook `observer` names on the main thread ([`Observer`]).
fn deliver(observer: *const c_void, on_main: bool) {
    if observer.is_null() || !on_main {
        return;
    }
    // SAFETY: a non-null `observer` is the `Hook` of a live `Observer` on this thread, whose
    // `Drop` removes the observer before the hook is freed.
    let hook = unsafe { &*observer.cast::<Hook>() };
    (hook.0)();
}

#[cfg(test)]
#[path = "darwin_notify_tests.rs"]
mod tests;
