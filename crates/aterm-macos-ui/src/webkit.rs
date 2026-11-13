// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! WebKit's classes, loaded when a native feature first shows a web page (the sign-in window).

use crate::framework::Framework;

static WEBKIT: Framework = Framework::new(&[c"/System/Library/Frameworks/WebKit.framework/WebKit"]);

/// Look up a WebKit class after acquiring its process-lifetime framework.
/// Classes absent on the running macOS version still return nil.
///
/// # Panics
/// If the system framework cannot be opened.
pub fn class(name: &'static std::ffi::CStr) -> aterm_objc::ClassPtr {
    WEBKIT.class(name)
}

#[cfg(test)]
#[path = "webkit_tests.rs"]
mod tests;
