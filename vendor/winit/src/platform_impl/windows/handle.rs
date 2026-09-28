//! Typed conversions between windows-sys handle types (opaque pointers since
//! 0.59) and the integer handle values Win32 message parameters and winit's
//! public API carry — in place of `as` casts, so a future change of a handle's
//! type is a compile error, not a silent reinterpretation (aterm fork,
//! 2026-09-25). A handle is an opaque id, never dereferenced: no provenance.

use std::ffi::c_void;

pub(crate) fn from_isize(value: isize) -> *mut c_void {
    std::ptr::without_provenance_mut(value as usize)
}

pub(crate) fn from_usize(value: usize) -> *mut c_void {
    std::ptr::without_provenance_mut(value)
}

pub(crate) fn to_isize(handle: *mut c_void) -> isize {
    handle.addr() as isize
}

pub(crate) fn to_usize(handle: *mut c_void) -> usize {
    handle.addr()
}

/// A handle a `move` closure can take to the window's thread: the value names a
/// process-global object and winit marshals owner-thread calls itself. Read it
/// with [`SendHandle::get`], so the closure captures this wrapper, not the
/// pointer inside it.
#[derive(Clone, Copy)]
pub(crate) struct SendHandle(*mut c_void);

// SAFETY: see the type docs — an opaque id, not memory.
unsafe impl Send for SendHandle {}
unsafe impl Sync for SendHandle {}

impl SendHandle {
    pub(crate) fn new(handle: *mut c_void) -> Self {
        Self(handle)
    }

    pub(crate) fn get(self) -> *mut c_void {
        self.0
    }
}
