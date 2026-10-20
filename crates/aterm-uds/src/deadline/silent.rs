// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Test support (test and debug builds, never release ones): whether a macOS service a test
//! needs answers, and the end of a test that needs one that does not.
//!
//! * [`trust_service_silent`] asks once per process, through the shipped [`super::TRUST`]
//!   gate: one `SecStaticCodeCheckValidity` of the Apple-signed `/usr/bin/true`, whose chain
//!   `trustd` evaluates. It hangs exactly while `trustd` is wedged (else about 0.24 s); only
//!   whether there is a verdict matters. A silent service costs the first ask
//!   [`PROBE_DEADLINE`], and from then on every call through the gate is refused at once.
//! * [`require_trust_service`] ends the calling test COULD NOT RUN on that silence, and
//!   [`require_disk_image_service`] on an `hdiutil` run the test made that timed out.
//!
//! Only silence refuses: any other probe failure is an answer, and the tests run.

use std::sync::OnceLock;
use std::time::Duration;

use super::{Refused, TRUST};
use crate::host::could_not_run;

/// How long the probe waits for `trustd`: about a hundred times its honest answer, and half a
/// test's one-minute hang detector.
pub const PROBE_DEADLINE: Duration = Duration::from_secs(30);

/// Whether macOS's `trustd` does not answer (asked once per process). Always `false` off
/// Apple platforms. A test that needs it calls [`require_trust_service`].
#[must_use]
pub fn trust_service_silent() -> bool {
    static SILENT: OnceLock<bool> = OnceLock::new();
    *SILENT.get_or_init(|| {
        cfg!(target_vendor = "apple")
            && TRUST.run(PROBE_DEADLINE, PROBE_DEADLINE, probe) == Err(Refused::Unanswered)
    })
}

/// End the calling test COULD NOT RUN when macOS's `trustd` does not answer
/// ([`trust_service_silent`]): every code-signing and TLS check waits on it, so the silence
/// is the machine's, not the code's.
#[track_caller]
pub fn require_trust_service() {
    if trust_service_silent() {
        could_not_run(
            "macOS's trust service (trustd) did not answer, so no code-signing or TLS check \
             ran here; rebooting the Mac clears a wedged trustd",
        );
    }
}

/// End the calling test COULD NOT RUN when `out` is an `hdiutil` run that failed with
/// `Operation timed out` (a wedged disk-image service, after about 121 s). Any other
/// failure is an answer, and returns.
#[track_caller]
pub fn require_disk_image_service(out: &std::process::Output) {
    if timed_out(out) {
        could_not_run(
            "macOS's disk-image service (hdiutil) did not answer, so no disk-image check ran \
             here; rebooting the Mac clears a wedged disk-image service",
        );
    }
}

/// Whether `out` is a failed `hdiutil` run that timed out.
fn timed_out(out: &std::process::Output) -> bool {
    !out.status.success() && String::from_utf8_lossy(&out.stderr).contains("Operation timed out")
}

/// How long a test's own `codesign` may run: a hang detector, never a budget.
pub const CODESIGN_HANG: Duration = Duration::from_secs(60);

/// Sign `path` ad hoc (`codesign --force --sign -`), as a test does to a copy of an Apple
/// binary so the kernel lets it run off the system volume. Under [`CODESIGN_HANG`]; ask
/// [`require_trust_service`] first. Panics, naming why (codesign's exit status, a signal
/// among them, and its stderr), unless it signed.
#[track_caller]
pub fn sign_ad_hoc(path: &std::path::Path) {
    let out = super::output_until(
        crate::spawnfd::command("/usr/bin/codesign")
            .args(["--force", "--sign", "-"])
            .arg(path),
        std::time::Instant::now() + CODESIGN_HANG,
    )
    .unwrap_or_else(|e| panic!("codesign --force --sign - {}: {e}", path.display()));
    assert!(
        out.status.success(),
        "codesign --force --sign - {}: {}: {}",
        path.display(),
        out.status,
        String::from_utf8_lossy(&out.stderr).trim()
    );
}

/// Copy the Apple binary `from` to `to` and sign the copy ad hoc ([`sign_ad_hoc`]): a test's
/// stand-in that runs off the system volume under its own identity (the kernel kills a copy
/// that keeps Apple's signature, and hides a platform binary's environment from other
/// processes). Re-signing reads the copy's Apple signature through `trustd`, so
/// [`require_trust_service`] is asked first. Off Apple platforms the copy is all. Panics,
/// naming why, unless the copy is made and signed.
#[track_caller]
pub fn ad_hoc_copy(from: &std::path::Path, to: &std::path::Path) {
    if cfg!(target_vendor = "apple") {
        require_trust_service();
    }
    std::fs::copy(from, to)
        .unwrap_or_else(|e| panic!("copy {} to {}: {e}", from.display(), to.display()));
    if cfg!(target_vendor = "apple") {
        sign_ad_hoc(to);
    }
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
        use super::timed_out as silent;
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
    }

    /// The silence ends the test COULD NOT RUN; an answer returns.
    #[test]
    fn a_silent_disk_image_service_ends_the_test_could_not_run() {
        super::require_disk_image_service(&run(1, "hdiutil: No space left on device\n"));
        let refused = std::panic::catch_unwind(|| {
            super::require_disk_image_service(&run(1, "hdiutil: Operation timed out\n"));
        })
        .expect_err("a timed-out run ends the test");
        let why = refused
            .downcast_ref::<String>()
            .expect("could_not_run panics with its words");
        assert!(why.starts_with(crate::host::COULD_NOT_RUN), "{why}");
    }
}
