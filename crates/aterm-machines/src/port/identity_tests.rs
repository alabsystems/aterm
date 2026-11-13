// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{LOOKUP_CEILING, grown};

/// An entry larger than the first buffer is read with a doubled one, not refused.
/// NEGATIVE CONTROL: one that does not fit even at the ceiling stays an error, after the
/// buffer reached the ceiling and no further, and another error ends the lookup at once.
#[test]
fn a_lookup_that_does_not_fit_grows_its_buffer_up_to_the_ceiling() {
    let mut tried = Vec::new();
    let answer = grown(16 * 1024, |buf| {
        tried.push(buf.len());
        if tried.len() == 1 {
            Err(libc::ERANGE)
        } else {
            Ok(buf.len())
        }
    });
    assert_eq!(answer, Ok(32 * 1024));
    assert_eq!(tried, [16 * 1024, 32 * 1024]);

    let mut tried = Vec::new();
    let answer: Result<(), _> = grown(16 * 1024, |buf| {
        tried.push(buf.len());
        Err(libc::ERANGE)
    });
    assert_eq!(answer, Err(libc::ERANGE));
    assert_eq!(tried.first(), Some(&(16 * 1024)));
    assert_eq!(tried.last(), Some(&LOOKUP_CEILING));
    assert!(tried.windows(2).all(|w| w[1] == w[0] * 2), "{tried:?}");

    let mut calls = 0;
    let answer: Result<(), _> = grown(16 * 1024, |_| {
        calls += 1;
        Err(libc::EIO)
    });
    assert_eq!((answer, calls), (Err(libc::EIO), 1));
}

/// Read only: this process's own account and the host's facts, beside what `id`, `uname`
/// and `sw_vers` (elsewhere `uname -r`) print for them.
#[test]
fn the_live_identity_is_what_the_tools_print() {
    // SAFETY: `geteuid` reads this process's effective uid and cannot fail.
    let uid = unsafe { libc::geteuid() };
    let identity = super::read(uid).unwrap();
    let tool = |program: &str, arg: &str| {
        let out = aterm_uds::spawnfd::command(program)
            .arg(arg)
            .output()
            .unwrap();
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    };
    assert_eq!(identity.user.name, tool("/usr/bin/id", "-un"));
    assert_eq!(identity.user.uid, uid);
    assert_eq!(identity.arch, tool("/usr/bin/uname", "-m"));
    let version = if cfg!(target_os = "macos") {
        tool("/usr/bin/sw_vers", "-productVersion")
    } else {
        tool("/usr/bin/uname", "-r")
    };
    assert_eq!(identity.os_version, version);
    assert!(identity.user.home.starts_with('/'), "{identity:?}");
}
