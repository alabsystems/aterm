// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! LAB SEAM (debug builds, macOS): the processes of the lab jail a debug aterm runs in.
//!
//! aterm-lab runs its instance in a sandbox profile (its jail) that hides the owner's home, and
//! marks the jail by a Mach name nobody serves that the profile alone denies (`lab.jail.…`). From
//! inside it the owner's own processes, whose working directories and files lie in that home,
//! cannot be read, and a scan that counts an unread process as a possible user of a path
//! (`aterm harness disk free`'s, every disk tier's) rules out none: in a lab it would refuse
//! every path whatever the lab laid. Where the lab names its mark in [`JAIL_SEAM`], such a scan
//! reads the jail's processes alone ([`lab_jail_pids`]), as the lab is the instance's machine,
//! as its scratch home is its home. A release build reads no such variable.

use std::ffi::{CString, c_char, c_int};
use std::io;

use super::JAIL_SEAM;

/// `SANDBOX_FILTER_NONE`: whether a process is sandboxed at all.
const FILTER_NONE: c_int = 0;

/// `SANDBOX_FILTER_GLOBAL_NAME`: a Mach global name.
const FILTER_GLOBAL_NAME: c_int = 2;

unsafe extern "C" {
    fn sandbox_check(pid: i32, operation: *const c_char, kind: c_int, ...) -> c_int;
    static SANDBOX_CHECK_NO_REPORT: c_int;
}

/// The processes of the lab jail [`JAIL_SEAM`] names, or `None` when it names none.
///
/// # Errors
/// The variable names no jail mark, this process is not in the jail it names (the seam answers
/// only from inside its jail, so it narrows no scan outside one), or the process listing fails.
#[cfg_attr(trust_verify, trust::skip)]
#[must_use]
pub fn lab_jail_pids() -> Option<io::Result<Vec<u32>>> {
    let mark = std::env::var(JAIL_SEAM).ok()?;
    Some(jail_pids(&mark))
}

/// Every process of this uid in the jail `mark` names, this one among them.
#[cfg_attr(trust_verify, trust::skip)]
fn jail_pids(mark: &str) -> io::Result<Vec<u32>> {
    let refused =
        |why: String| io::Error::new(io::ErrorKind::InvalidInput, format!("{JAIL_SEAM}: {why}"));
    if !jail_mark(mark) {
        return Err(refused(format!("{mark:?} is no lab jail mark")));
    }
    let name = CString::new(mark).map_err(|_| refused("a NUL in the mark".to_owned()))?;
    let none = CString::new(format!("{mark}.none"))
        .map_err(|_| refused("a NUL in the mark".to_owned()))?;
    // SAFETY: a data symbol of libSystem, read once.
    let no_report = unsafe { SANDBOX_CHECK_NO_REPORT };
    let member = |pid: u32| {
        let Ok(pid) = i32::try_from(pid) else {
            return false;
        };
        // SAFETY: `sandbox_check` reads only its arguments; the operation is a null pointer
        // under `SANDBOX_FILTER_NONE`, which takes no further argument.
        let sandboxed = unsafe { sandbox_check(pid, std::ptr::null(), FILTER_NONE) };
        // SAFETY: as above; the operation is a C string literal and the one variadic argument
        // is the name, a valid C string that outlives the call.
        let denies = |n: &CString| unsafe {
            sandbox_check(
                pid,
                c"mach-lookup".as_ptr(),
                FILTER_GLOBAL_NAME | no_report,
                n.as_ptr(),
            )
        };
        sandboxed == 1 && denies(&name) == 1 && denies(&none) == 0
    };
    if !member(std::process::id()) {
        return Err(refused(format!("this process is not in the jail {mark}")));
    }
    Ok(members(super::uid_pids()?, member))
}

/// `pids` that `member` keeps, in their order.
fn members(pids: Vec<u32>, member: impl Fn(u32) -> bool) -> Vec<u32> {
    pids.into_iter().filter(|&pid| member(pid)).collect()
}

/// Whether `mark` is a lab jail's mark: `lab.jail.` and then letters, digits, `.`, `_` and `-`.
fn jail_mark(mark: &str) -> bool {
    mark.strip_prefix("lab.jail.").is_some_and(|rest| {
        !rest.is_empty()
            && rest
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A jail mark is aterm-lab's spelling; anything else names no jail.
    #[test]
    fn a_jail_mark_is_the_labs_spelling() {
        assert!(jail_mark("lab.jail.wk3.3a8ec08aab215af4"));
        assert!(jail_mark("lab.jail.probe_1-a"));
        for bad in [
            "",
            "lab.jail.",
            "lab.jailwk3",
            "com.apple.lsd.open",
            "lab.jail.a b",
            "lab.jail.a/b",
        ] {
            assert!(!jail_mark(bad), "{bad:?}");
        }
    }

    /// The seam keeps the jail's members in the listing's order, and answers only from inside the
    /// jail it names: this test process is in none, so it narrows nothing here. CONTROL: a name
    /// that is no mark is refused before any process is read.
    #[test]
    fn the_seam_keeps_the_jails_members_and_answers_only_inside_it() {
        assert_eq!(members(vec![4, 9, 12, 30], |p| p % 3 == 0), [9, 12, 30]);
        let outside = jail_pids("lab.jail.t.0123456789abcdef").expect_err("no jail here");
        assert!(outside.to_string().contains("not in the jail"), "{outside}");
        let bad = jail_pids("com.apple.lsd.open").expect_err("no mark");
        assert!(bad.to_string().contains("is no lab jail mark"), "{bad}");
    }
}
