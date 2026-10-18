// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Test support (test and debug builds, never release ones): whether a macOS service a test
//! needs answers.
//!
//! * [`trust_service_silent`] asks once per process, through the shipped [`super::TRUST`]
//!   gate: one `SecStaticCodeCheckValidity` of the Apple-signed `/usr/bin/true`, whose chain
//!   `trustd` evaluates. It hangs exactly while `trustd` is wedged (else about 0.24 s); only
//!   whether there is a verdict matters. A silent service costs the first ask
//!   [`PROBE_DEADLINE`], and from then on every call through the gate is refused at once.
//! * [`disk_image_service_silent`] judges an `hdiutil` run the test already made.
//! * [`notice_once`] prints the skip line.
//!
//! Only silence skips: any other probe failure is an answer, and the tests run.

use std::io::Write as _;
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::Duration;

use super::{Refused, TRUST};

// `SKIPPED` and `DISK_IMAGE_SKIPPED`: their own file, which aterm-verify includes too.
include!("skipped.rs");

/// How long the probe waits for `trustd`: about a hundred times its honest answer, and half a
/// test's one-minute hang detector.
pub const PROBE_DEADLINE: Duration = Duration::from_secs(30);

/// Prints `line` once per test binary straight to stderr: libtest discards a passing test's
/// captured `eprintln!`, and a skipped test passes.
pub fn notice_once(line: &'static str) {
    static SAID: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());
    #[cfg(test)]
    ASKED.with(|n| n.set(n.get() + 1));
    let mut said = SAID.lock().unwrap_or_else(PoisonError::into_inner);
    if !said.contains(&line) {
        said.push(line);
        let _ = writeln!(std::io::stderr(), "{line}");
    }
}

#[cfg(test)]
thread_local! {
    /// How many notices this thread asked [`notice_once`] for, printed or not.
    static ASKED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// `true`, after [`notice_once`] of [`SKIPPED`], when macOS's `trustd` does
/// not answer; the caller then returns without asserting anything. Always
/// `false` off Apple platforms.
#[must_use]
pub fn trust_service_silent() -> bool {
    static SILENT: OnceLock<bool> = OnceLock::new();
    let silent = *SILENT.get_or_init(|| {
        cfg!(target_vendor = "apple")
            && TRUST.run(PROBE_DEADLINE, PROBE_DEADLINE, probe) == Err(Refused::Unanswered)
    });
    if silent {
        notice_once(SKIPPED);
    }
    silent
}

/// `true`, after [`notice_once`] of [`DISK_IMAGE_SKIPPED`], when `out` is an `hdiutil` run that
/// failed with `Operation timed out` (a wedged disk-image service, after about 121 s); the
/// caller then returns without asserting anything. Any other failure is an answer.
#[must_use]
pub fn disk_image_service_silent(out: &std::process::Output) -> bool {
    let silent = timed_out(out);
    if silent {
        notice_once(DISK_IMAGE_SKIPPED);
    }
    silent
}

/// [`disk_image_service_silent`]'s judgment without its notice. Tests of the judgment call
/// this: the notice is the line `verify test` counts as a host skip (aterm-verify
/// `libtest::host_skips`), so printing it for a made-up run reports a skip, and stops a
/// `verify release` as COULD NOT RUN, on a host whose service answers.
fn timed_out(out: &std::process::Output) -> bool {
    !out.status.success() && String::from_utf8_lossy(&out.stderr).contains("Operation timed out")
}

/// How long a test's own `codesign` may run: a hang detector, never a budget.
pub const CODESIGN_HANG: Duration = Duration::from_secs(60);

/// Sign `path` ad hoc (`codesign --force --sign -`), as a test does to a copy of an Apple
/// binary so the kernel lets it run off the system volume. Under [`CODESIGN_HANG`]; ask
/// [`trust_service_silent`] first. Panics, naming why, unless it signed.
pub fn sign_ad_hoc(path: &std::path::Path) {
    let out = super::output_until(
        crate::spawnfd::command("/usr/bin/codesign")
            .args(["--force", "--sign", "-"])
            .arg(path),
        std::time::Instant::now() + CODESIGN_HANG,
    )
    .unwrap_or_else(|e| panic!("codesign {}: {e}", path.display()));
    assert!(
        out.status.success(),
        "codesign {}: {}",
        path.display(),
        String::from_utf8_lossy(&out.stderr).trim()
    );
}

/// One certificate-chained code check; its verdict is discarded.
#[cfg(target_vendor = "apple")]
// Skip: bottoms out at Security.framework FFI.
#[cfg_attr(trust_verify, trust::skip)]
fn probe() {
    use core::ffi::c_void;
    type CFTypeRef = *const c_void;

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFRelease(cf: CFTypeRef);
        fn CFURLCreateFromFileSystemRepresentation(
            alloc: CFTypeRef,
            buffer: *const u8,
            len: isize,
            is_directory: u8,
        ) -> CFTypeRef;
    }
    #[link(name = "Security", kind = "framework")]
    unsafe extern "C" {
        fn SecStaticCodeCreateWithPath(path: CFTypeRef, flags: u32, out: *mut CFTypeRef) -> i32;
        fn SecStaticCodeCheckValidity(code: CFTypeRef, flags: u32, req: CFTypeRef) -> i32;
    }

    const PATH: &[u8] = b"/usr/bin/true";
    // SAFETY: `PATH` is `len` live bytes, copied by the call.
    let url = unsafe {
        CFURLCreateFromFileSystemRepresentation(
            std::ptr::null(),
            PATH.as_ptr(),
            PATH.len() as isize,
            0,
        )
    };
    if url.is_null() {
        return;
    }
    let mut code: CFTypeRef = std::ptr::null();
    // SAFETY: `url` is a live CFURL; an out-parameter for one new reference.
    let created = unsafe { SecStaticCodeCreateWithPath(url, 0, &mut code) };
    if created == 0 && !code.is_null() {
        // SAFETY: `code` is a live static code object; no requirement.
        let _ = unsafe { SecStaticCodeCheckValidity(code, 0, std::ptr::null()) };
        // SAFETY: we own exactly one reference.
        unsafe { CFRelease(code) };
    }
    // SAFETY: we own exactly one reference.
    unsafe { CFRelease(url) };
}

#[cfg(not(target_vendor = "apple"))]
fn probe() {}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::process::ExitStatusExt as _;
    use std::process::{ExitStatus, Output};

    fn run(code: i32, stderr: &str) -> Output {
        Output {
            status: ExitStatus::from_raw(code << 8),
            stdout: Vec::new(),
            stderr: stderr.as_bytes().to_vec(),
        }
    }

    /// Only a failed run that timed out is the service's silence.
    #[test]
    fn only_a_timed_out_failure_is_a_silent_disk_image_service() {
        // The judgment alone: `disk_image_service_silent` would print the skip line.
        use super::timed_out as silent;
        let asked = super::ASKED.with(std::cell::Cell::get);
        assert!(silent(&run(
            1,
            "hdiutil: create failed - Operation timed out\n"
        )));
        assert!(!silent(&run(
            1,
            "hdiutil: create failed - No space left on device\n"
        )));
        assert!(!silent(&run(1, "")));
        assert!(!silent(&run(0, "Operation timed out\n")));
        assert_eq!(
            super::ASKED.with(std::cell::Cell::get),
            asked,
            "the judgment asked for the skip line: the gate would count a made-up run as a \
             host check this binary skipped"
        );
    }
}
