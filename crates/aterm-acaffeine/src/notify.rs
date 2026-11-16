// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Darwin notification posted after [`crate::state::save`] and [`crate::state::save_lid`],
//! named for the state home the mug's files are under, so only a mug of that home hears it.

use std::ffi::{CStr, CString};

use crate::paths::Paths;

/// The base of [`changed`]'s name.
const CHANGED: &CStr = c"com.aterm.aterm.acaffeine.changed";

/// The notification about the mug's files `paths` names: [`CHANGED`] named for their state home
/// (`aterm_types::state_notify::name` of [`Paths::state_dir`]), so a mug hears a change of the
/// files it reads and of no other home's.
fn changed(paths: &Paths) -> CString {
    aterm_types::state_notify::name(CHANGED, &paths.state_dir)
}

/// Tell a running mug whose files are `paths` that its state file changed.
pub fn post_changed(paths: &Paths) {
    let _ = aterm_sysprobe::notify::post(&changed(paths));
}

/// Run `hook` on the main run loop whenever a change of the files `paths` names is posted
/// ([`post_changed`]); `None` off the main thread.
#[cfg(target_os = "macos")]
pub(crate) fn observe_changed(
    paths: &Paths,
    hook: impl Fn() + 'static,
) -> Option<aterm_macos_ui::darwin_notify::Observer> {
    aterm_macos_ui::darwin_notify::Observer::register(&changed(paths), hook)
}

#[cfg(all(test, target_os = "macos"))]
#[path = "notify_tests.rs"]
mod tests;
