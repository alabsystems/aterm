// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The aterm-wayland integration files as modules of one test binary, so the crate links one test
//! executable instead of one per file.

#[allow(dead_code, reason = "the generator's binary entry is not used here")]
#[path = "../scan/src/generate.rs"]
mod generate;
#[allow(dead_code, reason = "as above")]
#[path = "../scan/src/xml.rs"]
mod xml;

mod cursor_layout;
mod full_socket;
mod generated;
mod live_wayland;
mod tables;
