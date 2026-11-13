// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The macOS Metal backend, the renderer's arm on macOS.

pub(crate) mod acquire_worker;
#[cfg(test)]
pub(crate) mod blit;
pub(crate) mod encoder;
pub(crate) mod fetch;
pub(crate) mod ffi;
pub(crate) mod loss;
pub(crate) mod pipelines;
pub(crate) mod present;
pub(crate) mod quiet_probe;
pub(crate) mod resources;
pub(crate) mod shaders;
pub(crate) mod swapchain;
pub(crate) mod tray_prims;
pub(crate) mod warm;

/// The helpers every GPU test module under `metal` shares.
#[cfg(test)]
#[path = "mod_test_support_tests.rs"]
pub(crate) mod test_support;

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;
