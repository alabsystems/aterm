// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/linux/common/xkb/compose.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

//! Modified by the aterm project in 2026; see the repository NOTICE. (`SmolStr` from
//! `crate::keyboard`. libxkbcommon's compose entry points are first-party aterm-xkb's, where
//! upstream used xkbcommon-dl's: docs/AUDIT-third-party-2026-10-15.md, LW.7.)
//!
//! XKB compose handling.

use std::env;
use std::ffi::CString;
use std::ops::Deref;
use std::os::unix::ffi::OsStringExt;
use std::ptr::NonNull;

use super::{xkbch, XkbContext};
use crate::keyboard::SmolStr;
use aterm_xkb::{
    compose_feed, Keysym as xkb_keysym_t, XkbComposeState as xkb_compose_state,
    XkbComposeTable as xkb_compose_table, COMPOSE_NO_FLAGS,
};

/// `enum xkb_compose_status`, as libxkbcommon returns it.
type ComposeStatusCode = u32;

#[derive(Debug)]
pub struct XkbComposeTable {
    table: NonNull<xkb_compose_table>,
}

impl XkbComposeTable {
    pub fn new(context: &XkbContext) -> Option<Self> {
        let locale = env::var_os("LC_ALL")
            .filter(|v| !v.is_empty())
            .or_else(|| env::var_os("LC_CTYPE"))
            .filter(|v| !v.is_empty())
            .or_else(|| env::var_os("LANG"))
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| "C".into());
        let locale = CString::new(locale.into_vec()).unwrap();

        let table = unsafe {
            (xkbch().table_new_from_locale)(context.as_ptr(), locale.as_ptr(), COMPOSE_NO_FLAGS)
        };

        let table = NonNull::new(table)?;
        Some(Self { table })
    }

    /// Create new state with the given compose table.
    pub fn new_state(&self) -> Option<XkbComposeState> {
        let state = unsafe { (xkbch().state_new)(self.table.as_ptr(), COMPOSE_NO_FLAGS) };

        let state = NonNull::new(state)?;
        Some(XkbComposeState { state })
    }
}

impl Deref for XkbComposeTable {
    type Target = NonNull<xkb_compose_table>;

    fn deref(&self) -> &Self::Target {
        &self.table
    }
}

impl Drop for XkbComposeTable {
    fn drop(&mut self) {
        unsafe {
            (xkbch().table_unref)(self.table.as_ptr());
        }
    }
}

#[derive(Debug)]
pub struct XkbComposeState {
    state: NonNull<xkb_compose_state>,
}

impl XkbComposeState {
    pub fn get_string(&mut self, scratch_buffer: &mut Vec<u8>) -> Option<SmolStr> {
        super::make_string_with(scratch_buffer, |ptr, len| unsafe {
            (xkbch().state_get_utf8)(self.state.as_ptr(), ptr, len)
        })
    }

    #[inline]
    pub fn feed(&mut self, keysym: xkb_keysym_t) -> ComposeStatus {
        let feed_result = unsafe { (xkbch().state_feed)(self.state.as_ptr(), keysym) };
        match feed_result {
            compose_feed::ACCEPTED => ComposeStatus::Accepted(self.status()),
            // IGNORED, and any result a later libxkbcommon adds.
            _ => ComposeStatus::Ignored,
        }
    }

    #[inline]
    pub fn status(&mut self) -> ComposeStatusCode {
        unsafe { (xkbch().state_get_status)(self.state.as_ptr()) }
    }
}

impl Drop for XkbComposeState {
    fn drop(&mut self) {
        unsafe {
            (xkbch().state_unref)(self.state.as_ptr());
        };
    }
}

#[derive(Copy, Clone, Debug)]
pub enum ComposeStatus {
    Accepted(ComposeStatusCode),
    Ignored,
    None,
}
