// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/linux/wayland/types/mod.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

//! Wayland protocol implementation boilerplate. Modified by aterm: the viewporter, fractional
//! scale and xdg-activation objects are the window's own (`window/mod.rs`), on first-party
//! aterm-wayland's tables, and kwin_blur left with the `wayland-kwin-blur` feature (LW.7).

pub mod cursor;
