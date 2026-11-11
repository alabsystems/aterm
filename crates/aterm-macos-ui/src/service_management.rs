// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! ServiceManagement's classes, loaded when a native login-items feature needs them.

use crate::framework::Framework;

static SERVICE_MANAGEMENT: Framework =
    Framework::new(&[c"/System/Library/Frameworks/ServiceManagement.framework/ServiceManagement"]);

/// Look up a ServiceManagement class after acquiring its process-lifetime framework.
/// Classes absent on the running macOS version still return nil.
///
/// # Panics
/// If the system framework cannot be opened.
pub fn class(name: &'static std::ffi::CStr) -> aterm_objc::ClassPtr {
    SERVICE_MANAGEMENT.class(name)
}

#[cfg(test)]
#[path = "service_management_tests.rs"]
mod tests;
