// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `windows-sys` 0.61: a first-party replacement for the crates.io package of
//! the same name (the manifest says why the directory name differs).
//!
//! It is a re-export and nothing else. The declarations are
//! [`aterm_win32`]'s, generated from windows-sys 0.61.2 at upstream's module
//! paths, so `windows_sys::Win32::Foundation::HWND` and
//! `windows_sys::core::BOOL` name what they named upstream; that crate's
//! documentation says which declarations exist, how they are harvested from
//! the consumers, how the oracle checks them, and where they diverge. The
//! features gate nothing (the manifest says why).

#![no_std]

pub use aterm_win32::{Win32, core};
