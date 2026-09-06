// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Thread-scoped DATALESS-FILE policy (macOS).
//!
//! An evicted iCloud Drive item — or any File Provider placeholder — is a
//! "dataless" file: `lstat`, `open(2)`, `fstat` and `realpath` all succeed
//! instantly, and the FIRST `read(2)` blocks until the whole download has
//! finished. `O_NONBLOCK` does not help; it governs FIFOs and sockets, not
//! materialization. The same shape appears on an unreachable SMB/NFS volume,
//! where the read blocks until the network timeout.
//!
//! The kernel exposes one knob for this: `setiopolicy_np(
//! IOPOL_TYPE_VFS_MATERIALIZE_DATALESS_FILES, IOPOL_SCOPE_THREAD, …)`. With the
//! policy OFF, a read of a dataless file fails at once with `EDEADLK` instead of
//! waiting — MEASURED on this machine on 2026-09-03: open/lstat/fstat/realpath
//! unaffected, only `read(2)` fails, errno `EDEADLK`. With it ON, the thread may
//! wait for the download.
//!
//! aterm's rule, decided by the three-lens audit of the main-thread stall:
//!
//! * the MAIN THREAD never materializes — [`main_thread_never_materializes`]
//!   runs once at GUI startup, before any document can be opened; a dataless
//!   read reaching the main thread by any remaining path fails fast and is
//!   reported as [`crate::native_document_host::DocumentHostError::NotDownloaded`]
//!   rather than freezing the event loop into the watchdog;
//! * a DOCUMENT ADMISSION WORKER (`aterm-document-admit`, see
//!   `App::begin_document_admission`) sets its OWN policy to
//!   [`MaterializePolicy::On`] first, because a user who dropped the file
//!   really does want it, and a short-lived worker is the right place to wait.
//!
//! The generated first-party `libc` shim cannot carry `setiopolicy_np`: the
//! pinned reference libc 0.2.186 does not export it, and the shim's oracle
//! asserts that the two declare the same names. So the declaration lives here,
//! locally, with the SDK constants from `<sys/resource.h>`.

/// Whether reads on the CALLING thread may block to download a dataless file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MaterializePolicy {
    /// Inherit the process policy (the kernel default is to materialize).
    #[allow(
        dead_code,
        reason = "the third SDK value; read back by the diagnostic seam"
    )]
    Default,
    /// Never wait: a dataless read fails immediately with `EDEADLK`.
    Off,
    /// Wait for the download to finish.
    On,
}

#[cfg(target_os = "macos")]
mod sys {
    use std::os::raw::c_int;

    // `<sys/resource.h>`, macOS SDK. Values are ABI facts; they have not changed
    // since the policy was introduced (10.15) and are asserted by the round-trip
    // test below through `getiopolicy_np`.
    pub(super) const IOPOL_TYPE_VFS_MATERIALIZE_DATALESS_FILES: c_int = 3;
    #[allow(dead_code, reason = "documented sibling of the scope in use")]
    pub(super) const IOPOL_SCOPE_PROCESS: c_int = 0;
    pub(super) const IOPOL_SCOPE_THREAD: c_int = 1;
    pub(super) const IOPOL_MATERIALIZE_DATALESS_FILES_DEFAULT: c_int = 0;
    pub(super) const IOPOL_MATERIALIZE_DATALESS_FILES_OFF: c_int = 1;
    pub(super) const IOPOL_MATERIALIZE_DATALESS_FILES_ON: c_int = 2;

    // SAFETY (declaration): both functions are exported by libSystem on every
    // supported macOS (`man 3 setiopolicy_np`) with exactly these C signatures —
    // three/two `int` arguments, `int` return, errno on failure. They are declared
    // here rather than in the first-party `libc` shim because the shim is
    // generated from, and conformance-checked against, the pinned reference libc
    // 0.2.186, which does not export them.
    unsafe extern "C" {
        pub(super) fn setiopolicy_np(iotype: c_int, scope: c_int, policy: c_int) -> c_int;
        #[cfg(test)]
        pub(super) fn getiopolicy_np(iotype: c_int, scope: c_int) -> c_int;
    }

    pub(super) const fn raw(policy: super::MaterializePolicy) -> c_int {
        match policy {
            super::MaterializePolicy::Default => IOPOL_MATERIALIZE_DATALESS_FILES_DEFAULT,
            super::MaterializePolicy::Off => IOPOL_MATERIALIZE_DATALESS_FILES_OFF,
            super::MaterializePolicy::On => IOPOL_MATERIALIZE_DATALESS_FILES_ON,
        }
    }
}

/// Set the dataless-file policy of the CALLING thread only. `Err` carries the
/// errno the kernel returned; it never panics, and off macOS it is a no-op that
/// succeeds (no other supported platform has the concept).
pub(crate) fn set_thread_materialize_policy(policy: MaterializePolicy) -> Result<(), i32> {
    #[cfg(target_os = "macos")]
    {
        set_thread_policy_raw(sys::raw(policy))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = policy;
        Ok(())
    }
}

/// The raw-value seam under [`set_thread_materialize_policy`], kept separate so a
/// test can hand the kernel a value it must refuse and observe the errno path.
#[cfg(target_os = "macos")]
fn set_thread_policy_raw(policy: std::os::raw::c_int) -> Result<(), i32> {
    // SAFETY: plain FFI call with three integer arguments and no pointers; the
    // callee touches only the calling thread's own policy state. See the
    // declaration's SAFETY note for the signature guarantee.
    let rc = unsafe {
        sys::setiopolicy_np(
            sys::IOPOL_TYPE_VFS_MATERIALIZE_DATALESS_FILES,
            sys::IOPOL_SCOPE_THREAD,
            policy,
        )
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error().raw_os_error().unwrap_or(-1))
    }
}

/// Read back the calling thread's dataless-file policy (macOS only; `None` when
/// the kernel refuses the query). Test seam: it is how the tests prove the SDK
/// constants above name what the kernel thinks they name.
#[cfg(all(target_os = "macos", test))]
fn thread_materialize_policy() -> Option<MaterializePolicy> {
    // SAFETY: plain FFI query with two integer arguments; see the declaration.
    let raw = unsafe {
        sys::getiopolicy_np(
            sys::IOPOL_TYPE_VFS_MATERIALIZE_DATALESS_FILES,
            sys::IOPOL_SCOPE_THREAD,
        )
    };
    match raw {
        sys::IOPOL_MATERIALIZE_DATALESS_FILES_DEFAULT => Some(MaterializePolicy::Default),
        sys::IOPOL_MATERIALIZE_DATALESS_FILES_OFF => Some(MaterializePolicy::Off),
        sys::IOPOL_MATERIALIZE_DATALESS_FILES_ON => Some(MaterializePolicy::On),
        _ => None,
    }
}

/// GUI startup, main thread, once: the event-loop thread must never wait on a
/// download. Returns the errno on failure so the caller can log it; the caller
/// proceeds either way (a failure here degrades to the pre-fix behaviour, it
/// does not make anything less safe).
pub(crate) fn main_thread_never_materializes() -> Result<(), i32> {
    set_thread_materialize_policy(MaterializePolicy::Off)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setting_the_policy_never_panics_and_is_a_no_op_off_macos() {
        // Runs on a fresh thread so the test binary's other threads keep their
        // inherited policy.
        let outcome = std::thread::spawn(|| {
            let on = set_thread_materialize_policy(MaterializePolicy::On);
            let off = set_thread_materialize_policy(MaterializePolicy::Off);
            let default = set_thread_materialize_policy(MaterializePolicy::Default);
            (on, off, default)
        })
        .join()
        .expect("policy calls never panic");
        assert_eq!(outcome, (Ok(()), Ok(()), Ok(())));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn thread_policy_round_trips_through_the_kernel() {
        std::thread::spawn(|| {
            set_thread_materialize_policy(MaterializePolicy::On).unwrap();
            assert_eq!(thread_materialize_policy(), Some(MaterializePolicy::On));
            set_thread_materialize_policy(MaterializePolicy::Off).unwrap();
            assert_eq!(thread_materialize_policy(), Some(MaterializePolicy::Off));
            main_thread_never_materializes().unwrap();
            assert_eq!(thread_materialize_policy(), Some(MaterializePolicy::Off));
        })
        .join()
        .unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn an_invalid_policy_value_returns_its_errno_without_panicking() {
        let outcome = std::thread::spawn(|| set_thread_policy_raw(99))
            .join()
            .expect("an invalid value is refused, not a crash");
        assert_eq!(outcome, Err(libc::EINVAL), "{outcome:?}");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_thread_scoped_policy_does_not_leak_into_a_sibling_thread() {
        let sibling = std::thread::spawn(|| {
            set_thread_materialize_policy(MaterializePolicy::On).unwrap();
            thread_materialize_policy()
        })
        .join()
        .unwrap();
        assert_eq!(sibling, Some(MaterializePolicy::On));
        let fresh = std::thread::spawn(thread_materialize_policy)
            .join()
            .unwrap();
        assert_ne!(
            fresh,
            Some(MaterializePolicy::On),
            "a sibling's thread-scoped ON must not reach a thread that never set it"
        );
    }
}
