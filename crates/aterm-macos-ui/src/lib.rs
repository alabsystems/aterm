// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Native UI framework bindings, kept outside the foundational Objective-C runtime.
//!
//! AppKit, Carbon, ServiceManagement and AudioQueue bindings acquire their
//! frameworks only when a caller first needs their classes or exports.
//! Handles and exported addresses remain valid for the process lifetime. Missing required
//! frameworks or symbols panic with the loader's diagnostic rather than return fake values.

#![cfg(target_os = "macos")]

#[macro_use]
mod framework;
pub mod appkit;
pub mod audio;
pub mod carbon;
pub mod service_management;
