// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Darwin notification posted after [`crate::state::save`].

/// The notification's name.
pub const CHANGED: &str = "com.aterm.aterm.acaffeine.changed";

/// Tell a running mug the state file changed.
pub fn post_changed() {
    let _ = aterm_sysprobe::notify::post(CHANGED_C);
}

/// [`CHANGED`] as the C string a post takes.
const CHANGED_C: &std::ffi::CStr = c"com.aterm.aterm.acaffeine.changed";

/// Run `hook` on the main run loop whenever [`CHANGED`] is posted; `None` off the main thread.
#[cfg(target_os = "macos")]
pub(crate) fn observe_changed(
    hook: impl Fn() + 'static,
) -> Option<aterm_macos_ui::darwin_notify::Observer> {
    aterm_macos_ui::darwin_notify::Observer::register(CHANGED_C, hook)
}

#[cfg(all(test, target_os = "macos"))]
#[path = "notify_tests.rs"]
mod tests;
