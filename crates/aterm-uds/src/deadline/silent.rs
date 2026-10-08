// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! TEST SUPPORT (the `test-support` feature, dev-dependencies only): how a
//! test that needs macOS's `trustd` to ANSWER finds out whether it will.
//!
//! * [`trust_service_silent`] asks ONCE per process, through the shipped
//!   [`super::TRUST`] gate: one `SecStaticCodeCheckValidity` of
//!   `/usr/bin/true`, Apple-signed, so its chain is evaluated by `trustd`.
//!   MEASURED 2026-10-01: that call hangs exactly while `trustd` is wedged and
//!   answers in about 0.24 s otherwise; the verdict is not the question, only
//!   whether there is one. A silent service costs the first ask
//!   [`PROBE_DEADLINE`]; every other ask returns the cached answer, and every
//!   shipped call through the gate is refused at once from then on.
//! * [`notice_once`] is how a skip reaches the output at all: libtest captures
//!   `eprintln!` and discards it for a test that passes, and a skipped test
//!   passes. It writes to the process's stderr directly, once per line per
//!   binary.
//!
//! The skip is ONLY for the service not answering: any other failure of the
//! probe (an error status, a missing file) is an answer, and the tests run.

use std::io::Write as _;
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::Duration;

use super::{Refused, TRUST};

// `SKIPPED`: its own file, which aterm-verify includes too.
include!("skipped.rs");

/// How long the probe waits for `trustd`: about a hundred times its honest
/// answer, and half the minute past which a wait reads as a hang.
pub const PROBE_DEADLINE: Duration = Duration::from_secs(30);

/// Prints `line` once per test binary, straight to the process's stderr, which
/// libtest does not capture — so a skip is visible in a default run even
/// though the skipped tests pass.
pub fn notice_once(line: &'static str) {
    static SAID: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());
    let mut said = SAID.lock().unwrap_or_else(PoisonError::into_inner);
    if !said.contains(&line) {
        said.push(line);
        let _ = writeln!(std::io::stderr(), "{line}");
    }
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

/// How long a test's own `codesign` may run: a hang detector, never a budget.
pub const CODESIGN_HANG: Duration = Duration::from_secs(60);

/// Sign `path` ad hoc (`codesign --force --sign -`), as a test does to a copy
/// of an Apple binary so the kernel lets it run off the system volume (AGENTS.md
/// rule 6). Under [`CODESIGN_HANG`], so a service that stops answering
/// mid-run fails the test in a minute instead of hanging the suite; ask
/// [`trust_service_silent`] first. Panics, naming why, unless it signed.
pub fn sign_ad_hoc(path: &std::path::Path) {
    let out = super::output_until(
        crate::spawnfd::command("/usr/bin/codesign")
            .args(["--force", "--sign", "-"])
            .arg(path),
        // wasm-clock-guard: allow — test support only (the `test-support` feature,
        // dev-dependencies), never compiled into a wasm bundle.
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
