// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `rustix::path::Arg`: what a name argument may be.
//!
//! The consumers pass `&str` (sctk's shm name) and `&CStr` (sctk's memfd
//! name); the owned and borrowed `String` and `CString` forms complete the
//! pair. A name with an interior NUL is `Errno::INVAL`, as upstream's is.

use std::ffi::{CStr, CString};

use crate::io;

/// A value that can be handed to the kernel as a NUL-terminated name.
pub trait Arg {
    /// Calls `f` with `self` as a C string.
    fn into_with_c_str<T, F>(self, f: F) -> io::Result<T>
    where
        Self: Sized,
        F: FnOnce(&CStr) -> io::Result<T>;
}

/// `bytes` plus a terminating NUL, or `INVAL` if `bytes` holds one already.
fn with_bytes<T, F>(bytes: &[u8], f: F) -> io::Result<T>
where
    F: FnOnce(&CStr) -> io::Result<T>,
{
    let owned = CString::new(bytes).map_err(|_| io::Errno::INVAL)?;
    f(&owned)
}

impl Arg for &CStr {
    fn into_with_c_str<T, F>(self, f: F) -> io::Result<T>
    where
        F: FnOnce(&CStr) -> io::Result<T>,
    {
        f(self)
    }
}

impl Arg for &CString {
    fn into_with_c_str<T, F>(self, f: F) -> io::Result<T>
    where
        F: FnOnce(&CStr) -> io::Result<T>,
    {
        f(self)
    }
}

impl Arg for CString {
    fn into_with_c_str<T, F>(self, f: F) -> io::Result<T>
    where
        F: FnOnce(&CStr) -> io::Result<T>,
    {
        f(&self)
    }
}

impl Arg for &str {
    fn into_with_c_str<T, F>(self, f: F) -> io::Result<T>
    where
        F: FnOnce(&CStr) -> io::Result<T>,
    {
        with_bytes(self.as_bytes(), f)
    }
}

impl Arg for &String {
    fn into_with_c_str<T, F>(self, f: F) -> io::Result<T>
    where
        F: FnOnce(&CStr) -> io::Result<T>,
    {
        with_bytes(self.as_bytes(), f)
    }
}

impl Arg for String {
    fn into_with_c_str<T, F>(self, f: F) -> io::Result<T>
    where
        F: FnOnce(&CStr) -> io::Result<T>,
    {
        with_bytes(self.as_bytes(), f)
    }
}
