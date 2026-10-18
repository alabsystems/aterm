// SPDX-License-Identifier: MIT
// Copyright 2026 Andrew Yates
// Copyright © 2015, 2016 Red Hat
//
// Generated from protocols/primary-selection-unstable-v1.xml (MIT, see protocols/COPYING.wayland-protocols) by
// `targo --unverified run --release` in crates/aterm-wayland/scan. Do not edit.

//! The `wp_primary_selection_unstable_v1` protocol.

pub use self::tables::*;

#[rustfmt::skip]
mod tables {
/// X primary selection emulation
pub mod zwp_primary_selection_device_manager_v1 {
    #[allow(unused_imports)]
    use crate::iface::{Interface, Message, NULLS, TypeRef};
    #[allow(unused_imports)]
    use crate::proxy::{Arg, Fixed, Proxy, borrowed_array};
    #[allow(unused_imports)]
    use std::ffi::{CStr, c_int};

    /// The highest version this table describes.
    pub const VERSION: u32 = 1;
    /// The interface table.
    pub static INTERFACE: Interface = Interface::new(c"zwp_primary_selection_device_manager_v1", 1, &REQUESTS, &EVENTS);
    static REQUESTS_CREATE_SOURCE_TYPES: [TypeRef; 1] = [
        TypeRef::to(&crate::protocols::wp_primary_selection_unstable_v1::zwp_primary_selection_source_v1::INTERFACE),
    ];
    static REQUESTS_GET_DEVICE_TYPES: [TypeRef; 2] = [
        TypeRef::to(&crate::protocols::wp_primary_selection_unstable_v1::zwp_primary_selection_device_v1::INTERFACE),
        TypeRef::to(&crate::protocols::wayland::wl_seat::INTERFACE),
    ];
    static REQUESTS: [Message; 3] = [
        Message::new(c"create_source", c"n", &REQUESTS_CREATE_SOURCE_TYPES),
        Message::new(c"get_device", c"no", &REQUESTS_GET_DEVICE_TYPES),
        Message::new(c"destroy", c"", &NULLS),
    ];
    static EVENTS: [Message; 0] = [
    ];
    /// Request opcodes.
    pub mod req {
        pub const CREATE_SOURCE: u32 = 0;
        pub const GET_DEVICE: u32 = 1;
        pub const DESTROY: u32 = 2;
    }
    /// Event opcodes.
    pub mod evt {
    }
    /// create a new primary selection source
    ///
    /// # Safety
    /// `this` is a live `zwp_primary_selection_device_manager_v1`; every object argument is live.
    pub unsafe fn create_source(this: Proxy) -> Option<Proxy> {
        // SAFETY: the caller's contract; the arguments follow the signature.
        unsafe { this.marshal(0, &mut [Arg::new_id()], Some((&crate::protocols::wp_primary_selection_unstable_v1::zwp_primary_selection_source_v1::INTERFACE, this.version())), false) }
    }
    /// create a new primary selection device
    ///
    /// # Safety
    /// `this` is a live `zwp_primary_selection_device_manager_v1`; every object argument is live.
    pub unsafe fn get_device(this: Proxy, seat: Proxy) -> Option<Proxy> {
        // SAFETY: the caller's contract; the arguments follow the signature.
        unsafe { this.marshal(1, &mut [Arg::new_id(), Arg::object(Some(seat))], Some((&crate::protocols::wp_primary_selection_unstable_v1::zwp_primary_selection_device_v1::INTERFACE, this.version())), false) }
    }
    /// destroy the primary selection device manager
    ///
    /// # Safety
    /// `this` is a live `zwp_primary_selection_device_manager_v1`, destroyed by this request and not used again; every object argument is live.
    pub unsafe fn destroy(this: Proxy) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(2, &mut [], None, true) };
    }
}

pub mod zwp_primary_selection_device_v1 {
    #[allow(unused_imports)]
    use crate::iface::{Interface, Message, NULLS, TypeRef};
    #[allow(unused_imports)]
    use crate::proxy::{Arg, Fixed, Proxy, borrowed_array};
    #[allow(unused_imports)]
    use std::ffi::{CStr, c_int};

    /// The highest version this table describes.
    pub const VERSION: u32 = 1;
    /// The interface table.
    pub static INTERFACE: Interface = Interface::new(c"zwp_primary_selection_device_v1", 1, &REQUESTS, &EVENTS);
    static REQUESTS_SET_SELECTION_TYPES: [TypeRef; 2] = [
        TypeRef::to(&crate::protocols::wp_primary_selection_unstable_v1::zwp_primary_selection_source_v1::INTERFACE),
        TypeRef::NULL,
    ];
    static REQUESTS: [Message; 2] = [
        Message::new(c"set_selection", c"?ou", &REQUESTS_SET_SELECTION_TYPES),
        Message::new(c"destroy", c"", &NULLS),
    ];
    static EVENTS_DATA_OFFER_TYPES: [TypeRef; 1] = [
        TypeRef::to(&crate::protocols::wp_primary_selection_unstable_v1::zwp_primary_selection_offer_v1::INTERFACE),
    ];
    static EVENTS_SELECTION_TYPES: [TypeRef; 1] = [
        TypeRef::to(&crate::protocols::wp_primary_selection_unstable_v1::zwp_primary_selection_offer_v1::INTERFACE),
    ];
    static EVENTS: [Message; 2] = [
        Message::new(c"data_offer", c"n", &EVENTS_DATA_OFFER_TYPES),
        Message::new(c"selection", c"?o", &EVENTS_SELECTION_TYPES),
    ];
    /// Request opcodes.
    pub mod req {
        pub const SET_SELECTION: u32 = 0;
        pub const DESTROY: u32 = 1;
    }
    /// Event opcodes.
    pub mod evt {
        pub const DATA_OFFER: u32 = 0;
        pub const SELECTION: u32 = 1;
    }
    /// set the primary selection
    ///
    /// # Safety
    /// `this` is a live `zwp_primary_selection_device_v1`; every object argument is live.
    pub unsafe fn set_selection(this: Proxy, source: Option<Proxy>, serial: u32) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(0, &mut [Arg::object(source), Arg::uint(serial)], None, false) };
    }
    /// destroy the primary selection device
    ///
    /// # Safety
    /// `this` is a live `zwp_primary_selection_device_v1`, destroyed by this request and not used again; every object argument is live.
    pub unsafe fn destroy(this: Proxy) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(1, &mut [], None, true) };
    }
}

/// offer to transfer primary selection contents
pub mod zwp_primary_selection_offer_v1 {
    #[allow(unused_imports)]
    use crate::iface::{Interface, Message, NULLS, TypeRef};
    #[allow(unused_imports)]
    use crate::proxy::{Arg, Fixed, Proxy, borrowed_array};
    #[allow(unused_imports)]
    use std::ffi::{CStr, c_int};

    /// The highest version this table describes.
    pub const VERSION: u32 = 1;
    /// The interface table.
    pub static INTERFACE: Interface = Interface::new(c"zwp_primary_selection_offer_v1", 1, &REQUESTS, &EVENTS);
    static REQUESTS: [Message; 2] = [
        Message::new(c"receive", c"sh", &NULLS),
        Message::new(c"destroy", c"", &NULLS),
    ];
    static EVENTS: [Message; 1] = [
        Message::new(c"offer", c"s", &NULLS),
    ];
    /// Request opcodes.
    pub mod req {
        pub const RECEIVE: u32 = 0;
        pub const DESTROY: u32 = 1;
    }
    /// Event opcodes.
    pub mod evt {
        pub const OFFER: u32 = 0;
    }
    /// request that the data is transferred
    ///
    /// # Safety
    /// `this` is a live `zwp_primary_selection_offer_v1`; every object argument is live.
    pub unsafe fn receive(this: Proxy, mime_type: &CStr, fd: c_int) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(0, &mut [Arg::string(Some(mime_type)), Arg::fd(fd)], None, false) };
    }
    /// destroy the primary selection offer
    ///
    /// # Safety
    /// `this` is a live `zwp_primary_selection_offer_v1`, destroyed by this request and not used again; every object argument is live.
    pub unsafe fn destroy(this: Proxy) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(1, &mut [], None, true) };
    }
}

/// offer to replace the contents of the primary selection
pub mod zwp_primary_selection_source_v1 {
    #[allow(unused_imports)]
    use crate::iface::{Interface, Message, NULLS, TypeRef};
    #[allow(unused_imports)]
    use crate::proxy::{Arg, Fixed, Proxy, borrowed_array};
    #[allow(unused_imports)]
    use std::ffi::{CStr, c_int};

    /// The highest version this table describes.
    pub const VERSION: u32 = 1;
    /// The interface table.
    pub static INTERFACE: Interface = Interface::new(c"zwp_primary_selection_source_v1", 1, &REQUESTS, &EVENTS);
    static REQUESTS: [Message; 2] = [
        Message::new(c"offer", c"s", &NULLS),
        Message::new(c"destroy", c"", &NULLS),
    ];
    static EVENTS: [Message; 2] = [
        Message::new(c"send", c"sh", &NULLS),
        Message::new(c"cancelled", c"", &NULLS),
    ];
    /// Request opcodes.
    pub mod req {
        pub const OFFER: u32 = 0;
        pub const DESTROY: u32 = 1;
    }
    /// Event opcodes.
    pub mod evt {
        pub const SEND: u32 = 0;
        pub const CANCELLED: u32 = 1;
    }
    /// add an offered mime type
    ///
    /// # Safety
    /// `this` is a live `zwp_primary_selection_source_v1`; every object argument is live.
    pub unsafe fn offer(this: Proxy, mime_type: &CStr) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(0, &mut [Arg::string(Some(mime_type))], None, false) };
    }
    /// destroy the primary selection source
    ///
    /// # Safety
    /// `this` is a live `zwp_primary_selection_source_v1`, destroyed by this request and not used again; every object argument is live.
    pub unsafe fn destroy(this: Proxy) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(1, &mut [], None, true) };
    }
}
}
