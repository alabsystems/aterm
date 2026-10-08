// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Thread-scoped dataless-file policy (macOS).
//!
//! An evicted iCloud Drive item or other File Provider placeholder is "dataless":
//! `lstat`, `open(2)`, `fstat` and `realpath` succeed at once, and the first
//! `read(2)` blocks until the whole download finishes (`O_NONBLOCK` does not apply).
//! An unreachable SMB/NFS volume behaves the same until its network timeout.
//!
//! `setiopolicy_np(IOPOL_TYPE_VFS_MATERIALIZE_DATALESS_FILES, IOPOL_SCOPE_THREAD, …)`
//! controls this per thread: with the policy off, only `read(2)` of a dataless file
//! fails immediately with `EDEADLK`; with it on, the thread may wait.
//!
//! * The main thread never materializes: [`main_thread_never_materializes`] runs once
//!   at GUI startup, and a dataless read there fails fast as
//!   [`crate::native_document_host::DocumentHostError::NotDownloaded`] instead of
//!   stalling the event loop.
//! * A document admission worker (`aterm-document-admit`, see
//!   `App::begin_document_admission`) sets [`MaterializePolicy::On`] for itself, since
//!   the user asked for the file and a short-lived worker may wait.
//!
//! `setiopolicy_np` is declared here because the generated `libc` shim must match the
//! pinned reference libc 0.2.186, which does not export it.

/// Whether reads on the CALLING thread may block to download a dataless file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MaterializePolicy {
    /// Inherit the process policy (the kernel default is to materialize). Only
    /// the tests restore it; the shipped callers pick `Off` or `On`.
    #[cfg(test)]
    Default,
    /// Never wait: a dataless read fails immediately with `EDEADLK`.
    Off,
    /// Wait for the download to finish.
    On,
}

#[cfg(target_os = "macos")]
mod sys {
    use std::os::raw::c_int;

    // `<sys/resource.h>` ABI values (stable since 10.15), checked by the round-trip
    // test through `getiopolicy_np`.
    pub(super) const IOPOL_TYPE_VFS_MATERIALIZE_DATALESS_FILES: c_int = 3;
    pub(super) const IOPOL_SCOPE_THREAD: c_int = 1;
    #[cfg(test)]
    pub(super) const IOPOL_MATERIALIZE_DATALESS_FILES_DEFAULT: c_int = 0;
    pub(super) const IOPOL_MATERIALIZE_DATALESS_FILES_OFF: c_int = 1;
    pub(super) const IOPOL_MATERIALIZE_DATALESS_FILES_ON: c_int = 2;

    // SAFETY (declaration): libSystem exports both on every supported macOS with
    // these C signatures (`int` arguments, `int` return, errno on failure).
    unsafe extern "C" {
        pub(super) fn setiopolicy_np(iotype: c_int, scope: c_int, policy: c_int) -> c_int;
        #[cfg(test)]
        pub(super) fn getiopolicy_np(iotype: c_int, scope: c_int) -> c_int;
    }

    pub(super) const fn raw(policy: super::MaterializePolicy) -> c_int {
        match policy {
            #[cfg(test)]
            super::MaterializePolicy::Default => IOPOL_MATERIALIZE_DATALESS_FILES_DEFAULT,
            super::MaterializePolicy::Off => IOPOL_MATERIALIZE_DATALESS_FILES_OFF,
            super::MaterializePolicy::On => IOPOL_MATERIALIZE_DATALESS_FILES_ON,
        }
    }
}

/// Set the calling thread's dataless-file policy. `Err` carries the kernel errno;
/// off macOS this is a no-op that succeeds.
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
    // SAFETY: integer-only FFI call that changes only the calling thread's policy;
    // signature per the declaration above.
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

/// Read back the calling thread's dataless-file policy (`None` when the kernel
/// refuses the query); lets the tests check the SDK constants against the kernel.
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

/// Run once on the main thread at GUI startup so the event loop never waits on a
/// download. Returns the errno on failure for the caller to log; startup proceeds
/// either way.
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
