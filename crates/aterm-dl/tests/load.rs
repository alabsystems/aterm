// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Dynamic loader tests against `libc.so.6` and refusal cases.

#![cfg_attr(not(any(target_os = "linux", target_os = "macos")), allow(dead_code))]

use std::ffi::CStr;

use aterm_dl::{Error, Library};

aterm_dl::table! {
    struct Libc {
        getpid = c"getpid": unsafe extern "C" fn() -> i32;
        strlen = c"strlen": unsafe extern "C" fn(*const core::ffi::c_char) -> usize;
        snprintf = c"snprintf": unsafe extern "C" fn(
            *mut core::ffi::c_char,
            usize,
            *const core::ffi::c_char,
            ...
        ) -> i32;
    }
    optional {
        absent = c"aterm_dl_no_such_symbol": unsafe extern "C" fn();
        getppid = c"getppid": unsafe extern "C" fn() -> i32;
    }
}

aterm_dl::table! {
    struct Broken {
        getpid = c"getpid": unsafe extern "C" fn() -> i32;
        missing = c"aterm_dl_missing_row": unsafe extern "C" fn();
    }
}

#[cfg(not(target_os = "macos"))]
static LIBC: &[&CStr] = &[c"libc.so.6"];
#[cfg(target_os = "macos")]
static LIBC: &[&CStr] = &[c"/usr/lib/libSystem.B.dylib"];
static NOWHERE: &[&CStr] = &[c"libaterm-dl-absent.so.9", c"libaterm-dl-absent.so"];

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn reads_libc_and_calls_through_the_table() {
    let lib = Library::open(LIBC).expect("the platform C library");
    assert_eq!(lib.soname(), LIBC[0]);
    // SAFETY: the rows transcribe glibc's prototypes.
    let fns = unsafe { Libc::load(&lib) }.expect("every required row resolves");
    // SAFETY: getpid takes nothing and cannot fail.
    let pid = unsafe { (fns.getpid)() };
    assert_eq!(u32::try_from(pid).ok(), Some(std::process::id()));
    // SAFETY: a NUL-terminated string.
    assert_eq!(unsafe { (fns.strlen)(c"aterm".as_ptr()) }, 5);
    let mut out = [0u8; 16];
    // SAFETY: the buffer's own length; the format names one int.
    let n = unsafe { (fns.snprintf)(out.as_mut_ptr().cast(), out.len(), c"%d!".as_ptr(), 42) };
    assert_eq!(n, 3, "a variadic row passes its extra argument");
    assert_eq!(&out[..4], b"42!\0");
    assert!(
        fns.absent.is_none(),
        "an optional row the library lacks is None"
    );
    assert!(fns.getppid.is_some(), "an optional row it has is read");
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn a_missing_symbol_is_named() {
    let lib = Library::open(LIBC).expect("libc.so.6");
    // SAFETY: the one row that resolves is glibc's prototype.
    let err = unsafe { Broken::load(&lib) }
        .err()
        .expect("negative control: a row the library lacks must refuse the table");
    assert_eq!(
        err,
        Error::MissingSymbol {
            library: LIBC[0],
            symbol: c"aterm_dl_missing_row",
        }
    );
    assert_eq!(
        err.to_string(),
        format!("{} lacks aterm_dl_missing_row", LIBC[0].to_string_lossy())
    );
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn a_missing_library_names_every_soname_tried() {
    let err = Library::open(NOWHERE).expect_err("no such library");
    let Error::NotFound { tried } = &err else {
        panic!("expected NotFound, got {err:?}");
    };
    let names: Vec<_> = tried.iter().map(|(name, _)| *name).collect();
    assert_eq!(names, NOWHERE, "each name, in the order tried");
    assert!(
        tried.iter().all(|(_, reason)| !reason.is_empty()),
        "each with the loader's reason: {tried:?}"
    );
    let text = err.to_string();
    assert!(
        text.contains("libaterm-dl-absent.so.9") && text.contains("libaterm-dl-absent.so ("),
        "{text}"
    );
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
#[test]
fn off_linux_nothing_loads() {
    assert_eq!(
        Library::open(LIBC).expect_err("no loader off Linux"),
        Error::Unsupported {
            library: c"libc.so.6"
        }
    );
    assert_eq!(
        Library::open(NOWHERE).expect_err("no loader").to_string(),
        "libaterm-dl-absent.so.9: no dynamic loader on this platform"
    );
}
