// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/linux/x11/ffi.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

// Modified by the aterm project in 2026; see the repository NOTICE. (The Xlib, XKB, XIM,
// XInput 2 and Xcursor declarations are first-party `aterm-x11`'s, read at run time, where
// upstream re-exported x11-dl's: docs/AUDIT-third-party-2026-10-15.md, LW.7.)

pub use aterm_x11::xlib::*;
