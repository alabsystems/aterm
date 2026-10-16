// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/linux/wayland/types/mod.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

//! Wayland protocol implementation boilerplate. Modified by aterm (kwin_blur is opt-in).

pub mod cursor;
#[cfg(feature = "wayland-kwin-blur")]
pub mod kwin_blur;
pub mod wp_fractional_scaling;
pub mod wp_viewporter;
pub mod xdg_activation;
