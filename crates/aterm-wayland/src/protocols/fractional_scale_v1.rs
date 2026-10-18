// SPDX-License-Identifier: MIT
// Copyright 2026 Andrew Yates
// Copyright © 2022 Kenny Levinsen
//
// Generated from protocols/fractional-scale-v1.xml (MIT, see protocols/COPYING.wayland-protocols) by
// `targo --unverified run --release` in crates/aterm-wayland/scan. Do not edit.

//! The `fractional_scale_v1` protocol.

pub use self::tables::*;

#[rustfmt::skip]
mod tables {
/// fractional surface scale information
pub mod wp_fractional_scale_manager_v1 {
    #[allow(unused_imports)]
    use crate::iface::{Interface, Message, NULLS, TypeRef};
    #[allow(unused_imports)]
    use crate::proxy::{Arg, Fixed, Proxy, borrowed_array};
    #[allow(unused_imports)]
    use std::ffi::{CStr, c_int};

    /// The highest version this table describes.
    pub const VERSION: u32 = 1;
    /// The interface table.
    pub static INTERFACE: Interface = Interface::new(c"wp_fractional_scale_manager_v1", 1, &REQUESTS, &EVENTS);
    static REQUESTS_GET_FRACTIONAL_SCALE_TYPES: [TypeRef; 2] = [
        TypeRef::to(&crate::protocols::fractional_scale_v1::wp_fractional_scale_v1::INTERFACE),
        TypeRef::to(&crate::protocols::wayland::wl_surface::INTERFACE),
    ];
    static REQUESTS: [Message; 2] = [
        Message::new(c"destroy", c"", &NULLS),
        Message::new(c"get_fractional_scale", c"no", &REQUESTS_GET_FRACTIONAL_SCALE_TYPES),
    ];
    static EVENTS: [Message; 0] = [
    ];
    /// Request opcodes.
    pub mod req {
        pub const DESTROY: u32 = 0;
        pub const GET_FRACTIONAL_SCALE: u32 = 1;
    }
    /// Event opcodes.
    pub mod evt {
    }
    /// The `error` enum.
    pub mod error {
        /// the surface already has a fractional_scale object associated
        pub const FRACTIONAL_SCALE_EXISTS: u32 = 0x0;
    }
    /// unbind the fractional surface scale interface
    ///
    /// # Safety
    /// `this` is a live `wp_fractional_scale_manager_v1`, destroyed by this request and not used again; every object argument is live.
    pub unsafe fn destroy(this: Proxy) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(0, &mut [], None, true) };
    }
    /// extend surface interface for scale information
    ///
    /// # Safety
    /// `this` is a live `wp_fractional_scale_manager_v1`; every object argument is live.
    pub unsafe fn get_fractional_scale(this: Proxy, surface: Proxy) -> Option<Proxy> {
        // SAFETY: the caller's contract; the arguments follow the signature.
        unsafe { this.marshal(1, &mut [Arg::new_id(), Arg::object(Some(surface))], Some((&crate::protocols::fractional_scale_v1::wp_fractional_scale_v1::INTERFACE, this.version())), false) }
    }
}

/// fractional scale interface to a wl_surface
pub mod wp_fractional_scale_v1 {
    #[allow(unused_imports)]
    use crate::iface::{Interface, Message, NULLS, TypeRef};
    #[allow(unused_imports)]
    use crate::proxy::{Arg, Fixed, Proxy, borrowed_array};
    #[allow(unused_imports)]
    use std::ffi::{CStr, c_int};

    /// The highest version this table describes.
    pub const VERSION: u32 = 1;
    /// The interface table.
    pub static INTERFACE: Interface = Interface::new(c"wp_fractional_scale_v1", 1, &REQUESTS, &EVENTS);
    static REQUESTS: [Message; 1] = [
        Message::new(c"destroy", c"", &NULLS),
    ];
    static EVENTS: [Message; 1] = [
        Message::new(c"preferred_scale", c"u", &NULLS),
    ];
    /// Request opcodes.
    pub mod req {
        pub const DESTROY: u32 = 0;
    }
    /// Event opcodes.
    pub mod evt {
        pub const PREFERRED_SCALE: u32 = 0;
    }
    /// remove surface scale information for surface
    ///
    /// # Safety
    /// `this` is a live `wp_fractional_scale_v1`, destroyed by this request and not used again; every object argument is live.
    pub unsafe fn destroy(this: Proxy) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(0, &mut [], None, true) };
    }
}
}
