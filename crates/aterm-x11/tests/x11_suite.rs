// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The aterm-x11 integration files as modules of one test binary, so the crate links one test
//! executable instead of one per file.

#[cfg(target_endian = "little")]
mod support;

mod icccm_xrm;
mod live_x11;
mod replies;
mod requests;
mod xlib_layout;
