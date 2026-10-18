// SPDX-License-Identifier: MIT
// Copyright 2026 Andrew Yates
// Copyright © 2013-2016 Collabora, Ltd.
//
// Generated from protocols/viewporter.xml (MIT, see protocols/COPYING.wayland-protocols) by
// `targo --unverified run --release` in crates/aterm-wayland/scan. Do not edit.

//! The `viewporter` protocol.

pub use self::tables::*;

#[rustfmt::skip]
mod tables {
/// surface cropping and scaling
pub mod wp_viewporter {
    #[allow(unused_imports)]
    use crate::iface::{Interface, Message, NULLS, TypeRef};
    #[allow(unused_imports)]
    use crate::proxy::{Arg, Fixed, Proxy, borrowed_array};
    #[allow(unused_imports)]
    use std::ffi::{CStr, c_int};

    /// The highest version this table describes.
    pub const VERSION: u32 = 1;
    /// The interface table.
    pub static INTERFACE: Interface = Interface::new(c"wp_viewporter", 1, &REQUESTS, &EVENTS);
    static REQUESTS_GET_VIEWPORT_TYPES: [TypeRef; 2] = [
        TypeRef::to(&crate::protocols::viewporter::wp_viewport::INTERFACE),
        TypeRef::to(&crate::protocols::wayland::wl_surface::INTERFACE),
    ];
    static REQUESTS: [Message; 2] = [
        Message::new(c"destroy", c"", &NULLS),
        Message::new(c"get_viewport", c"no", &REQUESTS_GET_VIEWPORT_TYPES),
    ];
    static EVENTS: [Message; 0] = [
    ];
    /// Request opcodes.
    pub mod req {
        pub const DESTROY: u32 = 0;
        pub const GET_VIEWPORT: u32 = 1;
    }
    /// Event opcodes.
    pub mod evt {
    }
    /// The `error` enum.
    pub mod error {
        /// the surface already has a viewport object associated
        pub const VIEWPORT_EXISTS: u32 = 0x0;
    }
    /// unbind from the cropping and scaling interface
    ///
    /// # Safety
    /// `this` is a live `wp_viewporter`, destroyed by this request and not used again; every object argument is live.
    pub unsafe fn destroy(this: Proxy) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(0, &mut [], None, true) };
    }
    /// extend surface interface for crop and scale
    ///
    /// # Safety
    /// `this` is a live `wp_viewporter`; every object argument is live.
    pub unsafe fn get_viewport(this: Proxy, surface: Proxy) -> Option<Proxy> {
        // SAFETY: the caller's contract; the arguments follow the signature.
        unsafe { this.marshal(1, &mut [Arg::new_id(), Arg::object(Some(surface))], Some((&crate::protocols::viewporter::wp_viewport::INTERFACE, this.version())), false) }
    }
}

/// crop and scale interface to a wl_surface
pub mod wp_viewport {
    #[allow(unused_imports)]
    use crate::iface::{Interface, Message, NULLS, TypeRef};
    #[allow(unused_imports)]
    use crate::proxy::{Arg, Fixed, Proxy, borrowed_array};
    #[allow(unused_imports)]
    use std::ffi::{CStr, c_int};

    /// The highest version this table describes.
    pub const VERSION: u32 = 1;
    /// The interface table.
    pub static INTERFACE: Interface = Interface::new(c"wp_viewport", 1, &REQUESTS, &EVENTS);
    static REQUESTS: [Message; 3] = [
        Message::new(c"destroy", c"", &NULLS),
        Message::new(c"set_source", c"ffff", &NULLS),
        Message::new(c"set_destination", c"ii", &NULLS),
    ];
    static EVENTS: [Message; 0] = [
    ];
    /// Request opcodes.
    pub mod req {
        pub const DESTROY: u32 = 0;
        pub const SET_SOURCE: u32 = 1;
        pub const SET_DESTINATION: u32 = 2;
    }
    /// Event opcodes.
    pub mod evt {
    }
    /// The `error` enum.
    pub mod error {
        /// negative or zero values in width or height
        pub const BAD_VALUE: u32 = 0x0;
        /// destination size is not integer
        pub const BAD_SIZE: u32 = 0x1;
        /// source rectangle extends outside of the content area
        pub const OUT_OF_BUFFER: u32 = 0x2;
        /// the wl_surface was destroyed
        pub const NO_SURFACE: u32 = 0x3;
    }
    /// remove scaling and cropping from the surface
    ///
    /// # Safety
    /// `this` is a live `wp_viewport`, destroyed by this request and not used again; every object argument is live.
    pub unsafe fn destroy(this: Proxy) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(0, &mut [], None, true) };
    }
    /// set the source rectangle for cropping
    ///
    /// # Safety
    /// `this` is a live `wp_viewport`; every object argument is live.
    pub unsafe fn set_source(this: Proxy, x: Fixed, y: Fixed, width: Fixed, height: Fixed) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(1, &mut [Arg::fixed(x), Arg::fixed(y), Arg::fixed(width), Arg::fixed(height)], None, false) };
    }
    /// set the surface size for scaling
    ///
    /// # Safety
    /// `this` is a live `wp_viewport`; every object argument is live.
    pub unsafe fn set_destination(this: Proxy, width: i32, height: i32) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(2, &mut [Arg::int(width), Arg::int(height)], None, false) };
    }
}
}
