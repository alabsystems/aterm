// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Thread-scoped macOS I/O policy helpers ([`MaterializePolicy::On`]).
//! Provides [`main_thread_never_materializes`] and [`set_walker_thread_policy`].

/// Whether reads on the CALLING thread may block to download a dataless file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MaterializePolicy {
    /// Inherit the process policy (the kernel default is to materialize).
    Default,
    /// Never wait: a dataless read fails immediately with `EDEADLK`.
    Off,
    /// Wait for the download to finish.
    On,
}

/// Set the calling thread's dataless-file policy.
pub fn set_thread_materialize_policy(policy: MaterializePolicy) -> Result<(), i32> {
    #[cfg(target_vendor = "apple")]
    {
        set_thread_policy_raw(
            sys::IOPOL_TYPE_VFS_MATERIALIZE_DATALESS_FILES,
            sys::raw(policy),
        )
    }
    #[cfg(not(target_vendor = "apple"))]
    {
        let _ = policy;
        Ok(())
    }
}

/// Run once on the GUI's main thread at startup so the event loop never waits on a download.
#[inline]
pub fn main_thread_never_materializes() -> Result<(), i32> {
    set_thread_materialize_policy(MaterializePolicy::Off)
}

/// Read back the calling thread's dataless-file policy.
#[must_use]
pub fn thread_materialize_policy() -> Option<MaterializePolicy> {
    #[cfg(target_vendor = "apple")]
    {
        // SAFETY: Plain FFI query with two integer arguments; see the declaration.
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
    #[cfg(not(target_vendor = "apple"))]
    {
        None
    }
}

/// What the policy calls answered: one errno per call that failed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PolicyOutcome {
    /// `(which, errno)` for each call that failed.
    pub failed: Vec<(&'static str, i32)>,
}

/// Set the calling thread's walker I/O policy.
#[must_use]
pub fn set_walker_thread_policy() -> PolicyOutcome {
    #[cfg(target_vendor = "apple")]
    {
        let mut out = PolicyOutcome::default();
        for (which, iotype, policy) in [
            (
                "materialize-dataless-off",
                sys::IOPOL_TYPE_VFS_MATERIALIZE_DATALESS_FILES,
                sys::IOPOL_MATERIALIZE_DATALESS_FILES_OFF,
            ),
            (
                "atime-updates-off",
                sys::IOPOL_TYPE_VFS_ATIME_UPDATES,
                sys::IOPOL_ATIME_UPDATES_OFF,
            ),
            ("disk-throttle", sys::IOPOL_TYPE_DISK, sys::IOPOL_THROTTLE),
        ] {
            if let Err(errno) = set_thread_policy_raw(iotype, policy) {
                out.failed.push((which, errno));
            }
        }
        out
    }
    #[cfg(not(target_vendor = "apple"))]
    {
        PolicyOutcome::default()
    }
}

/// One `setiopolicy_np` at thread scope.
#[cfg(target_vendor = "apple")]
fn set_thread_policy_raw(iotype: libc::c_int, policy: libc::c_int) -> Result<(), i32> {
    // SAFETY: Integer-only FFI call that changes only the calling thread's policy.
    if unsafe { sys::setiopolicy_np(iotype, sys::IOPOL_SCOPE_THREAD, policy) } == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error().raw_os_error().unwrap_or(-1))
    }
}

#[cfg(target_vendor = "apple")]
pub(crate) mod sys {
    use libc::c_int;

    pub(crate) const IOPOL_TYPE_DISK: c_int = 0;
    pub(crate) const IOPOL_TYPE_VFS_ATIME_UPDATES: c_int = 2;
    pub(crate) const IOPOL_TYPE_VFS_MATERIALIZE_DATALESS_FILES: c_int = 3;
    pub(crate) const IOPOL_SCOPE_THREAD: c_int = 1;
    pub(crate) const IOPOL_THROTTLE: c_int = 3;
    pub(crate) const IOPOL_ATIME_UPDATES_OFF: c_int = 1;
    pub(crate) const IOPOL_MATERIALIZE_DATALESS_FILES_DEFAULT: c_int = 0;
    pub(crate) const IOPOL_MATERIALIZE_DATALESS_FILES_OFF: c_int = 1;
    pub(crate) const IOPOL_MATERIALIZE_DATALESS_FILES_ON: c_int = 2;

    // SAFETY: `libSystem` exports both symbols with these C signatures on macOS.
    unsafe extern "C" {
        pub(crate) fn setiopolicy_np(iotype: c_int, scope: c_int, policy: c_int) -> c_int;
        pub(crate) fn getiopolicy_np(iotype: c_int, scope: c_int) -> c_int;
    }

    pub(crate) const fn raw(policy: super::MaterializePolicy) -> c_int {
        match policy {
            super::MaterializePolicy::Default => IOPOL_MATERIALIZE_DATALESS_FILES_DEFAULT,
            super::MaterializePolicy::Off => IOPOL_MATERIALIZE_DATALESS_FILES_OFF,
            super::MaterializePolicy::On => IOPOL_MATERIALIZE_DATALESS_FILES_ON,
        }
    }
}

#[cfg(all(test, target_vendor = "apple"))]
mod tests {
    use super::*;

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
