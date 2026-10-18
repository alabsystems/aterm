// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A holder's lock on a file that carries a record other processes read while it lives.

use std::fs::File;

/// A holder's exclusive lock on a file that CARRIES A RECORD other processes read while
/// the holder lives — a lease's "who" line ([`crate::lease`]), atpkg's head watch seat's
/// pid. Released on drop, like atpkg's `lock::Flock`.
///
/// Unix uses the advisory whole-file lock. Windows byte-range locks also deny
/// ordinary reads from other handles, so the record remains outside the locked
/// range `[record, u64::MAX)`. Whole-file liveness probes still overlap that range.
#[derive(Debug)]
pub struct RecordLock {
    file: File,
    /// The record's length: where the held range starts (Windows).
    #[cfg(windows)]
    record: u64,
}

impl RecordLock {
    /// Take `file`'s exclusive lock without blocking, leaving its first `record` bytes
    /// readable by other handles on Windows ([`RecordLock`]).
    ///
    /// # Errors
    ///
    /// `WouldBlock` while another handle holds it; the OS error otherwise.
    pub fn try_lock(file: File, record: u64) -> Result<Self, std::fs::TryLockError> {
        // The census's File evidence, as in atpkg's `Flock::try_lock`.
        let file: std::fs::File = file;
        #[cfg(windows)]
        {
            record_range::try_lock(&file, record)?;
            Ok(Self { file, record })
        }
        #[cfg(not(windows))]
        {
            // `flock` has no ranges, and needs none: it is advisory.
            let _ = record;
            #[expect(
                clippy::disallowed_methods,
                reason = "the record guard's own take: its Drop releases by LOCK_UN"
            )]
            file.try_lock()?;
            Ok(Self { file })
        }
    }

    /// Let the lock go, keeping the file open — what the kernel does for a holder that
    /// dies. The drop does it too.
    pub fn unlock(&self) {
        #[cfg(windows)]
        record_range::unlock(&self.file, self.record);
        #[cfg(not(windows))]
        {
            let _ = self.file.unlock();
        }
    }
}

impl std::ops::Deref for RecordLock {
    type Target = File;
    fn deref(&self) -> &File {
        &self.file
    }
}

impl std::ops::DerefMut for RecordLock {
    fn deref_mut(&mut self) -> &mut File {
        &mut self.file
    }
}

impl Drop for RecordLock {
    fn drop(&mut self) {
        self.unlock();
    }
}

/// [`RecordLock`]'s Windows range: `LockFileEx`/`UnlockFileEx` over `[record, u64::MAX)`.
#[cfg(windows)]
mod record_range {
    use std::ffi::c_void;
    use std::fs::File;
    use std::os::windows::io::AsRawHandle as _;

    /// `OVERLAPPED`: the offset names the range; the API may also update its status fields.
    #[repr(C)]
    struct Overlapped {
        internal: usize,
        internal_high: usize,
        offset: u32,
        offset_high: u32,
        event: *mut c_void,
    }

    const LOCKFILE_FAIL_IMMEDIATELY: u32 = 0x0000_0001;
    const LOCKFILE_EXCLUSIVE_LOCK: u32 = 0x0000_0002;
    /// `LockFileEx`'s answer when another handle holds part of the range.
    const ERROR_LOCK_VIOLATION: i32 = 33;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn LockFileEx(
            file: *mut c_void,
            flags: u32,
            reserved: u32,
            bytes_low: u32,
            bytes_high: u32,
            overlapped: *mut Overlapped,
        ) -> i32;
        fn UnlockFileEx(
            file: *mut c_void,
            reserved: u32,
            bytes_low: u32,
            bytes_high: u32,
            overlapped: *mut Overlapped,
        ) -> i32;
    }

    /// The `OVERLAPPED` naming `record` as the start, and the range's length split into
    /// its low and high halves.
    fn range(record: u64) -> (Overlapped, u32, u32) {
        let len = u64::MAX - record;
        let overlapped = Overlapped {
            internal: 0,
            internal_high: 0,
            offset: record as u32,
            offset_high: (record >> 32) as u32,
            event: std::ptr::null_mut(),
        };
        (overlapped, len as u32, (len >> 32) as u32)
    }

    pub(super) fn try_lock(file: &File, record: u64) -> Result<(), std::fs::TryLockError> {
        let (mut overlapped, low, high) = range(record);
        // SAFETY: `file` is an open handle for the call; `overlapped` is a live, initialised
        // `OVERLAPPED` available for the call's input/output. The handle is synchronous
        // and FAIL_IMMEDIATELY refuses contention rather than parking a request.
        let ok = unsafe {
            LockFileEx(
                file.as_raw_handle(),
                LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
                0,
                low,
                high,
                &mut overlapped,
            )
        };
        if ok != 0 {
            return Ok(());
        }
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() == Some(ERROR_LOCK_VIOLATION) {
            Err(std::fs::TryLockError::WouldBlock)
        } else {
            Err(std::fs::TryLockError::Error(err))
        }
    }

    pub(super) fn unlock(file: &File, record: u64) {
        let (mut overlapped, low, high) = range(record);
        // SAFETY: as in `try_lock`. A range this handle does not hold is refused, which
        // changes nothing.
        let _ = unsafe { UnlockFileEx(file.as_raw_handle(), 0, low, high, &mut overlapped) };
    }
}
