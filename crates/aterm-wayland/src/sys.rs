// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! libwayland-client entry points loaded at run time through `aterm-dl`.

use std::ffi::{CStr, c_char, c_int, c_void};
use std::sync::OnceLock;

use crate::iface::{Interface, Message};

/// `struct wl_display`, opaque.
pub type WlDisplay = c_void;
/// `struct wl_proxy`, opaque.
pub type WlProxy = c_void;
/// `struct wl_event_queue`, opaque.
pub type WlEventQueue = c_void;

/// `struct wl_array`.
#[repr(C)]
pub struct WlArray {
    pub size: usize,
    pub alloc: usize,
    pub data: *mut c_void,
}

/// `union wl_argument`.
#[repr(C)]
#[derive(Clone, Copy)]
pub union WlArgument {
    pub i: i32,
    pub u: u32,
    pub f: i32,
    pub s: *const c_char,
    pub o: *mut WlProxy,
    pub n: u32,
    pub a: *mut WlArray,
    pub h: i32,
}

/// `wl_dispatcher_func_t`.
pub type Dispatcher =
    unsafe extern "C" fn(*const c_void, *mut c_void, u32, *const Message, *mut WlArgument) -> c_int;

/// `WL_MARSHAL_FLAG_DESTROY`.
pub const MARSHAL_FLAG_DESTROY: u32 = 1 << 0;

aterm_dl::table! {
    /// libwayland-client.
    pub struct Client {
        display_connect = c"wl_display_connect":
            unsafe extern "C" fn(*const c_char) -> *mut WlDisplay;
        display_connect_to_fd = c"wl_display_connect_to_fd":
            unsafe extern "C" fn(c_int) -> *mut WlDisplay;
        display_disconnect = c"wl_display_disconnect": unsafe extern "C" fn(*mut WlDisplay);
        display_get_fd = c"wl_display_get_fd": unsafe extern "C" fn(*mut WlDisplay) -> c_int;
        display_get_error = c"wl_display_get_error": unsafe extern "C" fn(*mut WlDisplay) -> c_int;
        display_flush = c"wl_display_flush": unsafe extern "C" fn(*mut WlDisplay) -> c_int;
        display_create_queue = c"wl_display_create_queue":
            unsafe extern "C" fn(*mut WlDisplay) -> *mut WlEventQueue;
        event_queue_destroy = c"wl_event_queue_destroy": unsafe extern "C" fn(*mut WlEventQueue);
        display_roundtrip_queue = c"wl_display_roundtrip_queue":
            unsafe extern "C" fn(*mut WlDisplay, *mut WlEventQueue) -> c_int;
        display_dispatch_queue = c"wl_display_dispatch_queue":
            unsafe extern "C" fn(*mut WlDisplay, *mut WlEventQueue) -> c_int;
        display_dispatch_queue_pending = c"wl_display_dispatch_queue_pending":
            unsafe extern "C" fn(*mut WlDisplay, *mut WlEventQueue) -> c_int;
        display_prepare_read_queue = c"wl_display_prepare_read_queue":
            unsafe extern "C" fn(*mut WlDisplay, *mut WlEventQueue) -> c_int;
        display_cancel_read = c"wl_display_cancel_read": unsafe extern "C" fn(*mut WlDisplay);
        display_read_events = c"wl_display_read_events":
            unsafe extern "C" fn(*mut WlDisplay) -> c_int;
        proxy_marshal_array_flags = c"wl_proxy_marshal_array_flags": unsafe extern "C" fn(
            *mut WlProxy,
            u32,
            *const Interface,
            u32,
            u32,
            *mut WlArgument,
        ) -> *mut WlProxy;
        proxy_destroy = c"wl_proxy_destroy": unsafe extern "C" fn(*mut WlProxy);
        proxy_add_dispatcher = c"wl_proxy_add_dispatcher": unsafe extern "C" fn(
            *mut WlProxy,
            Dispatcher,
            *const c_void,
            *mut c_void,
        ) -> c_int;
        proxy_get_version = c"wl_proxy_get_version": unsafe extern "C" fn(*mut WlProxy) -> u32;
        proxy_get_id = c"wl_proxy_get_id": unsafe extern "C" fn(*mut WlProxy) -> u32;
        proxy_get_class = c"wl_proxy_get_class":
            unsafe extern "C" fn(*mut WlProxy) -> *const c_char;
        proxy_set_queue = c"wl_proxy_set_queue":
            unsafe extern "C" fn(*mut WlProxy, *mut WlEventQueue);
        proxy_create_wrapper = c"wl_proxy_create_wrapper":
            unsafe extern "C" fn(*mut c_void) -> *mut c_void;
        proxy_wrapper_destroy = c"wl_proxy_wrapper_destroy": unsafe extern "C" fn(*mut c_void);
    }
}

static CLIENT_SONAMES: &[&CStr] = &[c"libwayland-client.so.0", c"libwayland-client.so"];

/// libwayland-client, loaded once per process.
pub fn client() -> Result<&'static Client, aterm_dl::Error> {
    static LIB: OnceLock<Result<Client, aterm_dl::Error>> = OnceLock::new();
    LIB.get_or_init(|| {
        let lib = aterm_dl::Library::open(CLIENT_SONAMES)?;
        // SAFETY: every row transcribes wayland-client-core.h.
        unsafe { Client::load(&lib) }
    })
    .as_ref()
    .map_err(Clone::clone)
}
