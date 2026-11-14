// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Establish a process fixture's image before reading its arguments or environment.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Wait under a one-minute hang detector for `pid` to exec `image`. Both the kernel's exec
/// flag and its executable path must agree: a fork still maps its parent's image, and an
/// intermediate `env` may have exec'd before the intended program has. The process's start
/// record ([`crate::process::read_process_start`], which Linux and macOS both keep) must stay the
/// same throughout the wait, and its thread must have run before an argument reader can ask the
/// kernel for that image's initial environment. The wait ends only once the kernel also hands
/// over the image's argument record ([`crate::process::read_process_argv`]): XNU sets the exec
/// flag and the path, and counts the thread's first user time, while `KERN_PROCARGS2` still
/// answers `EINVAL`, for around a millisecond in about one exec in 1,400 on a busy Mac
/// (macOS 27.2), and in each such exec read it answered from then on. A reader that takes an
/// unread record for a refusal (`cohort_gone`, which keeps a group live on one) otherwise
/// judges the fixture in a state that ends by itself.
///
/// This establishes kernel image identity only. A copied Mach-O must also prove it ran by
/// output ([`super::prints`], [`super::cat_echoes`]) before its file can be removed.
/// # Panics
/// `image` cannot be canonicalized, `pid`'s start cannot be read or changes, or the expected
/// exec, its running thread and its argument record do not become readable within a minute.
#[track_caller]
pub fn execd(pid: i32, image: &Path) {
    wait_execd(pid, image, || {}, crate::process::read_process_argv);
}

#[track_caller]
fn wait_execd(
    pid: i32,
    image: &Path,
    mut waiting: impl FnMut(),
    mut argv: impl FnMut(i32) -> Option<Vec<std::ffi::OsString>>,
) {
    let image = std::fs::canonicalize(image).expect("the fixture's image");
    let start = crate::process::read_process_start(pid).expect("the fixture's start");
    let give_up = Instant::now() + Duration::from_secs(60);
    loop {
        // The argument record is asked last, and only of the image: before the exec it is the
        // parent's, which reads whole and says nothing of this one.
        let ran = names_image(
            crate::process::read_process_execd(pid),
            crate::process::read_process_path(pid),
            &image,
        ) && crate::process::thread_ran(pid) == Some(true)
            && argv(pid).is_some();
        let now = crate::process::read_process_start(pid);
        assert!(
            now.is_none_or(|now| now == start),
            "pid {pid} no longer names the fixture"
        );
        if ran && now == Some(start) {
            return;
        }
        assert!(
            Instant::now() < give_up,
            "pid {pid} never exec'd {}",
            image.display()
        );
        waiting();
        super::poll_yield();
    }
}

fn names_image(execd: Option<bool>, path: Option<PathBuf>, image: &Path) -> bool {
    execd == Some(true) && path.as_deref() == Some(image)
}

#[cfg(test)]
#[path = "execd_tests.rs"]
mod tests;
