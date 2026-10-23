// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Runtime-loaded `libxcb` and `libX11-xcb` entry points via `aterm-dl`.

use std::ffi::{CStr, c_char, c_int, c_uint, c_void};
use std::sync::OnceLock;

/// `xcb_connection_t`, opaque.
pub(crate) type XcbConnection = c_void;

/// `struct iovec`.
#[repr(C)]
pub(crate) struct Iovec {
    pub(crate) base: *mut c_void,
    pub(crate) len: usize,
}

/// `xcb_protocol_request_t`.
#[repr(C)]
pub(crate) struct ProtocolRequest {
    pub(crate) count: usize,
    pub(crate) ext: *mut c_void,
    pub(crate) opcode: u8,
    pub(crate) isvoid: u8,
}

/// `xcb_void_cookie_t`.
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct VoidCookie {
    pub(crate) sequence: c_uint,
}

/// `XCB_REQUEST_CHECKED`.
pub(crate) const REQUEST_CHECKED: c_int = 1 << 0;
/// `XCB_REQUEST_RAW`: the bytes are a whole request, opcode and length included.
pub(crate) const REQUEST_RAW: c_int = 1 << 1;

aterm_dl::table! {
    /// libxcb.
    pub(crate) struct Xcb {
        connect = c"xcb_connect":
            unsafe extern "C" fn(*const c_char, *mut c_int) -> *mut XcbConnection;
        disconnect = c"xcb_disconnect": unsafe extern "C" fn(*mut XcbConnection);
        has_error = c"xcb_connection_has_error": unsafe extern "C" fn(*mut XcbConnection) -> c_int;
        get_setup = c"xcb_get_setup": unsafe extern "C" fn(*mut XcbConnection) -> *const u8;
        generate_id = c"xcb_generate_id": unsafe extern "C" fn(*mut XcbConnection) -> u32;
        flush = c"xcb_flush": unsafe extern "C" fn(*mut XcbConnection) -> c_int;
        get_file_descriptor = c"xcb_get_file_descriptor":
            unsafe extern "C" fn(*mut XcbConnection) -> c_int;
        maximum_request_length = c"xcb_get_maximum_request_length":
            unsafe extern "C" fn(*mut XcbConnection) -> u32;
        send_request64 = c"xcb_send_request64": unsafe extern "C" fn(
            *mut XcbConnection,
            c_int,
            *mut Iovec,
            *const ProtocolRequest,
        ) -> u64;
        send_request_with_fds64 = c"xcb_send_request_with_fds64": unsafe extern "C" fn(
            *mut XcbConnection,
            c_int,
            *mut Iovec,
            *const ProtocolRequest,
            c_uint,
            *mut c_int,
        ) -> u64;
        wait_for_reply64 = c"xcb_wait_for_reply64":
            unsafe extern "C" fn(*mut XcbConnection, u64, *mut *mut u8) -> *mut u8;
        request_check = c"xcb_request_check":
            unsafe extern "C" fn(*mut XcbConnection, VoidCookie) -> *mut u8;
        discard_reply64 = c"xcb_discard_reply64": unsafe extern "C" fn(*mut XcbConnection, u64);
        wait_for_event = c"xcb_wait_for_event": unsafe extern "C" fn(*mut XcbConnection) -> *mut u8;
        poll_for_event = c"xcb_poll_for_event": unsafe extern "C" fn(*mut XcbConnection) -> *mut u8;
    }
}

aterm_dl::table! {
    /// libX11-xcb: the XCB connection under an Xlib `Display`.
    pub(crate) struct X11Xcb {
        get_xcb_connection = c"XGetXCBConnection":
            unsafe extern "C" fn(*mut c_void) -> *mut XcbConnection;
    }
}

static XCB_SONAMES: &[&CStr] = &[c"libxcb.so.1", c"libxcb.so"];
static X11_XCB_SONAMES: &[&CStr] = &[c"libX11-xcb.so.1", c"libX11-xcb.so"];

/// libxcb, loaded once per process.
pub(crate) fn xcb() -> Result<&'static Xcb, aterm_dl::Error> {
    static XCB: OnceLock<Result<Xcb, aterm_dl::Error>> = OnceLock::new();
    XCB.get_or_init(|| {
        let lib = aterm_dl::Library::open(XCB_SONAMES)?;
        // SAFETY: every row transcribes libxcb's prototype.
        unsafe { Xcb::load(&lib) }
    })
    .as_ref()
    .map_err(Clone::clone)
}

/// libX11-xcb, loaded once per process.
pub(crate) fn x11_xcb() -> Result<&'static X11Xcb, aterm_dl::Error> {
    static LIB: OnceLock<Result<X11Xcb, aterm_dl::Error>> = OnceLock::new();
    LIB.get_or_init(|| {
        let lib = aterm_dl::Library::open(X11_XCB_SONAMES)?;
        // SAFETY: the row transcribes Xlib-xcb.h's prototype.
        unsafe { X11Xcb::load(&lib) }
    })
    .as_ref()
    .map_err(Clone::clone)
}
