// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A bounded write to a Windows capture child's stdin ([`super::CaptureInput::write_all`]).
//!
//! std makes its end of a child's stdin pipe an overlapped (asynchronous) handle, so a write
//! that the pipe cannot take at once need not block the thread that asked for it: each write
//! here starts with its own event and is waited on in slices, and before each slice the
//! caller's look says whether the write may go on and for how long. A write the look stops is
//! cancelled (`CancelIoEx`) and its end awaited before its buffer and `OVERLAPPED` leave scope,
//! as the kernel may still read the one and write the other until then. A handle opened for
//! synchronous I/O would turn the same call into a wait nothing can bound, so such a handle is
//! refused before anything is written.

use std::ffi::c_void;
use std::io;
use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle, RawHandle};
use std::time::Duration;

const ERROR_IO_PENDING: i32 = 997;
const WAIT_OBJECT_0: u32 = 0;
const WAIT_TIMEOUT: u32 = 258;
/// `FILE_INFORMATION_CLASS::FileModeInformation`.
const FILE_MODE_INFORMATION: u32 = 16;
const FILE_SYNCHRONOUS_IO_ALERT: u32 = 0x10;
const FILE_SYNCHRONOUS_IO_NONALERT: u32 = 0x20;

/// `OVERLAPPED`, its offset union spelled as the two offset words (a pipe reads neither).
#[repr(C)]
struct Overlapped {
    internal: usize,
    internal_high: usize,
    offset: u32,
    offset_high: u32,
    event: RawHandle,
}

/// `IO_STATUS_BLOCK`.
#[repr(C)]
struct IoStatusBlock {
    status: usize,
    information: usize,
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateEventW(
        attributes: *const c_void,
        manual_reset: i32,
        initial_state: i32,
        name: *const u16,
    ) -> RawHandle;
    fn WriteFile(
        file: RawHandle,
        buffer: *const c_void,
        len: u32,
        written: *mut u32,
        overlapped: *mut Overlapped,
    ) -> i32;
    fn GetOverlappedResult(
        file: RawHandle,
        overlapped: *const Overlapped,
        transferred: *mut u32,
        wait: i32,
    ) -> i32;
    fn CancelIoEx(file: RawHandle, overlapped: *const Overlapped) -> i32;
    fn WaitForSingleObject(handle: RawHandle, millis: u32) -> u32;
}

#[link(name = "ntdll")]
unsafe extern "system" {
    fn NtQueryInformationFile(
        file: RawHandle,
        status: *mut IoStatusBlock,
        information: *mut c_void,
        len: u32,
        class: u32,
    ) -> i32;
}

/// Write all of `bytes` to `pipe`, std's overlapped end of a child's stdin. `look` is asked
/// before each write and before each slice of a wait: it answers how long the next slice may
/// be, or the error that stops the write (cancellation, the deadline).
///
/// # Errors
///
/// What `look` answered, the handle is not open for overlapped I/O, or the write failed (a
/// closed pipe reads as `BrokenPipe`). An error can follow partial delivery.
pub(super) fn write_all(
    pipe: RawHandle,
    mut bytes: &[u8],
    mut look: impl FnMut() -> io::Result<Duration>,
) -> io::Result<()> {
    if synchronous(pipe)? {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "the capture's stdin is open for synchronous writes, which nothing could bound",
        ));
    }
    let event = manual_event()?;
    while !bytes.is_empty() {
        look()?;
        let len = u32::try_from(bytes.len()).unwrap_or(u32::MAX);
        let mut overlapped = Overlapped {
            internal: 0,
            internal_high: 0,
            offset: 0,
            offset_high: 0,
            event: event.as_raw_handle(),
        };
        // SAFETY: `pipe` is the capture's open stdin handle, borrowed for this call; `bytes`
        // holds `len` readable bytes and `overlapped` is a live, zeroed OVERLAPPED whose event
        // this call owns. Both stay in scope until the write has ended: below, on every path,
        // its result is read only once it completed, or it is cancelled and waited for.
        let started = unsafe {
            WriteFile(
                pipe,
                bytes.as_ptr().cast(),
                len,
                std::ptr::null_mut(),
                &raw mut overlapped,
            )
        };
        if started == 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(ERROR_IO_PENDING) {
                return Err(error);
            }
            if let Err(stop) = wait(&event, &mut look) {
                let mut moved = 0u32;
                // SAFETY: the same handle and the same live OVERLAPPED of the write in flight;
                // the cancel names that write alone, and the waiting result that follows
                // returns only once the kernel has finished with `bytes` and `overlapped`.
                unsafe {
                    CancelIoEx(pipe, &raw const overlapped);
                    GetOverlappedResult(pipe, &raw const overlapped, &raw mut moved, 1);
                }
                return Err(stop);
            }
        }
        let mut moved = 0u32;
        // SAFETY: the write completed (at once, or its event was signalled), so this reads its
        // result from the live OVERLAPPED without waiting.
        if unsafe { GetOverlappedResult(pipe, &raw const overlapped, &raw mut moved, 0) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let moved = usize::try_from(moved)
            .unwrap_or(usize::MAX)
            .min(bytes.len());
        if moved == 0 {
            return Err(io::ErrorKind::WriteZero.into());
        }
        bytes = &bytes[moved..];
    }
    Ok(())
}

/// Wait for `event`, a write's, in slices `look` sizes; its error stops the wait.
fn wait(event: &OwnedHandle, look: &mut impl FnMut() -> io::Result<Duration>) -> io::Result<()> {
    loop {
        let slice = look()?;
        // At least a millisecond, and never INFINITE (u32::MAX).
        let millis = u32::try_from(slice.as_millis())
            .unwrap_or(u32::MAX - 1)
            .clamp(1, u32::MAX - 1);
        // SAFETY: `event` is an open event handle this call owns.
        match unsafe { WaitForSingleObject(event.as_raw_handle(), millis) } {
            WAIT_OBJECT_0 => return Ok(()),
            WAIT_TIMEOUT => {}
            _ => return Err(io::Error::last_os_error()),
        }
    }
}

/// A manual-reset event, unsignalled, as an overlapped write's.
fn manual_event() -> io::Result<OwnedHandle> {
    // SAFETY: no attributes and no name; the answer is checked before it is owned.
    let raw = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
    if raw.is_null() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `raw` is a fresh event handle this call alone holds.
    Ok(unsafe { OwnedHandle::from_raw_handle(raw) })
}

/// Whether `handle` was opened for synchronous I/O, where an overlapped write would block.
fn synchronous(handle: RawHandle) -> io::Result<bool> {
    let mut status = IoStatusBlock {
        status: 0,
        information: 0,
    };
    let mut mode = 0u32;
    // SAFETY: `handle` is open for this call; `status` and `mode` are live and `mode` is the
    // four bytes FILE_MODE_INFORMATION holds.
    let answer = unsafe {
        NtQueryInformationFile(
            handle,
            &raw mut status,
            (&raw mut mode).cast(),
            4,
            FILE_MODE_INFORMATION,
        )
    };
    if answer < 0 {
        return Err(io::Error::other(format!(
            "the capture's stdin mode could not be read (NTSTATUS {answer:#x})"
        )));
    }
    Ok(mode & (FILE_SYNCHRONOUS_IO_ALERT | FILE_SYNCHRONOUS_IO_NONALERT) != 0)
}
