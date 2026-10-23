// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

#![cfg_attr(trust_verify, feature(register_tool))]
#![cfg_attr(trust_verify, register_tool(trust))]

//! Shared accessibility model and native platform publishers (NSAccessibility, UI Automation, AT-SPI).

#![deny(unsafe_code)]

#[cfg(any(target_os = "linux", all(test, unix)))]
pub mod atspi;
#[cfg(target_os = "macos")]
#[allow(unsafe_code, reason = "typed main-thread NSAccessibility boundary")]
pub mod macos;
pub mod model;
#[cfg(any(target_os = "macos", windows, test))]
mod platform;
#[cfg(any(target_os = "macos", target_os = "linux", windows, all(test, unix)))]
#[cfg_attr(
    not(any(target_os = "linux", all(test, unix))),
    allow(
        dead_code,
        reason = "shared consumer operations also serve the AT-SPI backend"
    )
)]
mod text;
#[cfg(any(target_os = "macos", target_os = "linux", windows, all(test, unix)))]
#[cfg_attr(
    not(any(target_os = "linux", all(test, unix))),
    allow(
        dead_code,
        reason = "shared consumer operations also serve the AT-SPI backend"
    )
)]
mod tree;
#[cfg(windows)]
#[allow(unsafe_code, reason = "Windows UI Automation COM boundary")]
pub mod uia;
#[cfg(any(windows, test))]
#[allow(unsafe_code, reason = "typed nullable COM output slots")]
mod uia_abi;
#[cfg(any(windows, test))]
mod uia_text;

#[cfg(any(windows, test))]
mod uia_changes;
