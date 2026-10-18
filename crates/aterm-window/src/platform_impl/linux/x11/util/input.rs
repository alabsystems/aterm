// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/linux/x11/util/input.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

// Modified by the aterm project in 2026; see the repository NOTICE. (XInput 2's requests are
// aterm-x11's encoders, where upstream used x11rb's.)

use std::{slice, str};

use aterm_x11::proto::xinput;

use super::*;

pub const VIRTUAL_CORE_POINTER: u16 = 2;
pub const VIRTUAL_CORE_KEYBOARD: u16 = 3;

// A base buffer size of 1kB uses a negligible amount of RAM while preventing us from having to
// re-allocate (and make another round-trip) in the *vast* majority of cases.
// To test if `lookup_utf8` works correctly, set this to 1.
const TEXT_BUFFER_SIZE: usize = 1024;

impl XConnection {
    pub fn select_xinput_events(
        &self,
        window: xproto::Window,
        device_id: u16,
        mask: u32,
    ) -> Result<VoidCookie<'_>, X11Error> {
        self.send_void(xinput::xi_select_events(self.xinput_major(), window, &[
            xinput::EventMask { deviceid: device_id, mask: vec![mask] },
        ]))
    }

    pub fn select_xkb_events(&self, device_id: c_uint, mask: c_ulong) -> Result<bool, X11Error> {
        let status = unsafe { (self.xlib.XkbSelectEvents)(self.display, device_id, mask, mask) };

        if status == ffi::TRUE {
            self.flush_requests()?;
            Ok(true)
        } else {
            tracing::error!("Could not select XKB events: The XKB extension is not initialized!");
            Ok(false)
        }
    }

    pub fn query_pointer(
        &self,
        window: xproto::Window,
        device_id: u16,
    ) -> Result<xinput::XIQueryPointerReply, X11Error> {
        self.request(xinput::xi_query_pointer(self.xinput_major(), window, device_id))
    }

    fn lookup_utf8_inner(
        &self,
        ic: ffi::XIC,
        key_event: &mut ffi::XKeyEvent,
        buffer: *mut u8,
        size: usize,
    ) -> (ffi::KeySym, ffi::Status, c_int) {
        let mut keysym: ffi::KeySym = 0;
        let mut status: ffi::Status = 0;
        let count = unsafe {
            (self.xlib.Xutf8LookupString)(
                ic,
                key_event,
                buffer as *mut c_char,
                size as c_int,
                &mut keysym,
                &mut status,
            )
        };
        (keysym, status, count)
    }

    pub fn lookup_utf8(&self, ic: ffi::XIC, key_event: &mut ffi::XKeyEvent) -> String {
        // `assume_init` is safe here because the array consists of `MaybeUninit` values,
        // which do not require initialization.
        let mut buffer: [MaybeUninit<u8>; TEXT_BUFFER_SIZE] =
            unsafe { MaybeUninit::uninit().assume_init() };
        // If the buffer overflows, we'll make a new one on the heap.
        let mut vec;

        let (_, status, count) =
            self.lookup_utf8_inner(ic, key_event, buffer.as_mut_ptr() as *mut u8, buffer.len());

        let bytes = if status == ffi::X_BUFFER_OVERFLOW {
            vec = Vec::with_capacity(count as usize);
            let (_, _, new_count) =
                self.lookup_utf8_inner(ic, key_event, vec.as_mut_ptr(), vec.capacity());
            debug_assert_eq!(count, new_count);

            unsafe { vec.set_len(count as usize) };
            &vec[..count as usize]
        } else {
            unsafe { slice::from_raw_parts(buffer.as_ptr() as *const u8, count as usize) }
        };

        str::from_utf8(bytes).unwrap_or("").to_string()
    }
}
