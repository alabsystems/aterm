// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Native UI framework bindings, kept outside the foundational Objective-C runtime.
//!
//! AppKit, Carbon, ServiceManagement, WebKit and AudioQueue bindings acquire their
//! frameworks only when a caller first needs their classes or exports.
//! `status_button` is a menu-bar item's click split, pop-up and template face, and
//! `darwin_notify` the observer of a state file's change notification.
//! The native restoration policy acquires Foundation before application creation.
//! Handles and exported addresses remain valid for the process lifetime. Missing required
//! frameworks or symbols panic with the loader's diagnostic rather than return fake values.

#![cfg(target_os = "macos")]

#[macro_use]
mod framework;
pub mod appkit;
pub mod audio;
pub mod carbon;
pub mod darwin_notify;
pub mod restoration;
pub mod service_management;
pub mod status_button;
pub mod webkit;
