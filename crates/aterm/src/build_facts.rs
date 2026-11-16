// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A read-only compile record and its pre-startup, headless reply.

use std::ffi::OsString;
use std::process::ExitCode;

// Derived once from the binary's existing compile identity, never its patchable product stamp.
#[used]
#[cfg_attr(target_vendor = "apple", unsafe(link_section = "__TEXT,__aterm_facts"))]
#[cfg_attr(
    all(not(target_vendor = "apple"), not(windows)),
    unsafe(link_section = ".aterm_facts")
)]
#[cfg_attr(windows, unsafe(link_section = ".afacts"))]
static RECORD: [u8; aterm_stamp::facts::LEN] = aterm_stamp::facts::encode(super::COMPILED);

pub(crate) fn entry(argv: &[OsString]) -> Option<ExitCode> {
    if argv.get(1).is_none_or(|arg| arg != aterm_stamp::facts::ARG) {
        return None;
    }
    if argv.len() != 2 {
        eprintln!("aterm: --build-facts takes no arguments");
        return Some(ExitCode::from(2));
    }
    Some(if aterm_uds::stdout::write_all(&RECORD).is_ok() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}
