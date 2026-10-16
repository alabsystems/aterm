// Copyright (c) Microsoft Corporation.
// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from windows-sys 0.61.2 src/Windows/Win32/Security/mod.rs. GENERATED
// by aterm-win32-oracle: the 1 of its 637 names that aterm's Windows cells read
// or that those name, with their feature gates dropped and their paths pointed
// into this crate. Do not edit; regenerate. See ../../LICENSE-MIT and
// ../../LICENSE-APACHE for the upstream licenses.

//! `Win32::Security`.

pub mod Cryptography;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct SECURITY_ATTRIBUTES {
    pub nLength: u32,
    pub lpSecurityDescriptor: *mut core::ffi::c_void,
    pub bInheritHandle: crate::core::BOOL,
}
impl Default for SECURITY_ATTRIBUTES {
    fn default() -> Self {
        unsafe { core::mem::zeroed() }
    }
}
