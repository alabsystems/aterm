// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//! The Win32 FFI vocabulary the shell-integration modules share, declared once:
//! the COM half (`Guid`, the owned [`Com`] pointer, [`co_create`], the
//! [`Apartment`]) behind the file picker, the imaging decoder and the jump
//! list, and the `advapi32` registry half behind the Explorer verb and the
//! DefTerm registration.
//!
//! The COM half is aterm-win32's (`aterm_win32::com`, moved there by WB.2 of
//! docs/AUDIT-third-party-2026-10-15.md, where DXGI shares it); the registry
//! half is hand-rolled flat-C FFI, the house style. The interface vtables stay
//! beside the module that calls them.
#![cfg(windows)]

use std::ffi::c_void;

pub(crate) use aterm_win32::Win32::System::Com::{
    COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize,
};
pub(crate) use aterm_win32::com::{Apartment, Com, StepError, check, co_create};
pub(crate) use aterm_win32::core::GUID as Guid;

/// NUL-terminated UTF-16, the `PCWSTR` shape every call takes.
pub(crate) fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

// ---------------------------------------------------------------------------
// Registry (advapi32)
// ---------------------------------------------------------------------------

pub(crate) type Hkey = isize;
pub(crate) const HKEY_CURRENT_USER: Hkey = 0x8000_0001u32 as Hkey;
pub(crate) const REG_SZ: u32 = 1;
pub(crate) const KEY_WRITE: u32 = 0x0002_0006;
pub(crate) const REG_OPTION_NON_VOLATILE: u32 = 0;

#[link(name = "advapi32")]
unsafe extern "system" {
    pub(crate) fn RegCreateKeyExW(
        hkey: Hkey,
        subkey: *const u16,
        reserved: u32,
        class: *const u16,
        options: u32,
        sam: u32,
        security: *const c_void,
        result: *mut Hkey,
        disposition: *mut u32,
    ) -> i32;
    pub(crate) fn RegSetValueExW(
        hkey: Hkey,
        name: *const u16,
        reserved: u32,
        ty: u32,
        data: *const u8,
        cb: u32,
    ) -> i32;
    pub(crate) fn RegCloseKey(hkey: Hkey) -> i32;
}
