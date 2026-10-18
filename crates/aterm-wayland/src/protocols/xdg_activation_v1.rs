// SPDX-License-Identifier: MIT
// Copyright 2026 Andrew Yates
// Copyright © 2020 Aleix Pol Gonzalez <aleixpol@kde.org>
// Copyright © 2020 Carlos Garnacho <carlosg@gnome.org>
//
// Generated from protocols/xdg-activation-v1.xml (MIT, see protocols/COPYING.wayland-protocols) by
// `targo --unverified run --release` in crates/aterm-wayland/scan. Do not edit.

//! The `xdg_activation_v1` protocol.

pub use self::tables::*;

#[rustfmt::skip]
mod tables {
/// interface for activating surfaces
pub mod xdg_activation_v1 {
    #[allow(unused_imports)]
    use crate::iface::{Interface, Message, NULLS, TypeRef};
    #[allow(unused_imports)]
    use crate::proxy::{Arg, Fixed, Proxy, borrowed_array};
    #[allow(unused_imports)]
    use std::ffi::{CStr, c_int};

    /// The highest version this table describes.
    pub const VERSION: u32 = 1;
    /// The interface table.
    pub static INTERFACE: Interface = Interface::new(c"xdg_activation_v1", 1, &REQUESTS, &EVENTS);
    static REQUESTS_GET_ACTIVATION_TOKEN_TYPES: [TypeRef; 1] = [
        TypeRef::to(&crate::protocols::xdg_activation_v1::xdg_activation_token_v1::INTERFACE),
    ];
    static REQUESTS_ACTIVATE_TYPES: [TypeRef; 2] = [
        TypeRef::NULL,
        TypeRef::to(&crate::protocols::wayland::wl_surface::INTERFACE),
    ];
    static REQUESTS: [Message; 3] = [
        Message::new(c"destroy", c"", &NULLS),
        Message::new(c"get_activation_token", c"n", &REQUESTS_GET_ACTIVATION_TOKEN_TYPES),
        Message::new(c"activate", c"so", &REQUESTS_ACTIVATE_TYPES),
    ];
    static EVENTS: [Message; 0] = [
    ];
    /// Request opcodes.
    pub mod req {
        pub const DESTROY: u32 = 0;
        pub const GET_ACTIVATION_TOKEN: u32 = 1;
        pub const ACTIVATE: u32 = 2;
    }
    /// Event opcodes.
    pub mod evt {
    }
    /// destroy the xdg_activation object
    ///
    /// # Safety
    /// `this` is a live `xdg_activation_v1`, destroyed by this request and not used again; every object argument is live.
    pub unsafe fn destroy(this: Proxy) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(0, &mut [], None, true) };
    }
    /// requests a token
    ///
    /// # Safety
    /// `this` is a live `xdg_activation_v1`; every object argument is live.
    pub unsafe fn get_activation_token(this: Proxy) -> Option<Proxy> {
        // SAFETY: the caller's contract; the arguments follow the signature.
        unsafe { this.marshal(1, &mut [Arg::new_id()], Some((&crate::protocols::xdg_activation_v1::xdg_activation_token_v1::INTERFACE, this.version())), false) }
    }
    /// notify new interaction being available
    ///
    /// # Safety
    /// `this` is a live `xdg_activation_v1`; every object argument is live.
    pub unsafe fn activate(this: Proxy, token: &CStr, surface: Proxy) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(2, &mut [Arg::string(Some(token)), Arg::object(Some(surface))], None, false) };
    }
}

/// an exported activation handle
pub mod xdg_activation_token_v1 {
    #[allow(unused_imports)]
    use crate::iface::{Interface, Message, NULLS, TypeRef};
    #[allow(unused_imports)]
    use crate::proxy::{Arg, Fixed, Proxy, borrowed_array};
    #[allow(unused_imports)]
    use std::ffi::{CStr, c_int};

    /// The highest version this table describes.
    pub const VERSION: u32 = 1;
    /// The interface table.
    pub static INTERFACE: Interface = Interface::new(c"xdg_activation_token_v1", 1, &REQUESTS, &EVENTS);
    static REQUESTS_SET_SERIAL_TYPES: [TypeRef; 2] = [
        TypeRef::NULL,
        TypeRef::to(&crate::protocols::wayland::wl_seat::INTERFACE),
    ];
    static REQUESTS_SET_SURFACE_TYPES: [TypeRef; 1] = [
        TypeRef::to(&crate::protocols::wayland::wl_surface::INTERFACE),
    ];
    static REQUESTS: [Message; 5] = [
        Message::new(c"set_serial", c"uo", &REQUESTS_SET_SERIAL_TYPES),
        Message::new(c"set_app_id", c"s", &NULLS),
        Message::new(c"set_surface", c"o", &REQUESTS_SET_SURFACE_TYPES),
        Message::new(c"commit", c"", &NULLS),
        Message::new(c"destroy", c"", &NULLS),
    ];
    static EVENTS: [Message; 1] = [
        Message::new(c"done", c"s", &NULLS),
    ];
    /// Request opcodes.
    pub mod req {
        pub const SET_SERIAL: u32 = 0;
        pub const SET_APP_ID: u32 = 1;
        pub const SET_SURFACE: u32 = 2;
        pub const COMMIT: u32 = 3;
        pub const DESTROY: u32 = 4;
    }
    /// Event opcodes.
    pub mod evt {
        pub const DONE: u32 = 0;
    }
    /// The `error` enum.
    pub mod error {
        /// The token has already been used previously
        pub const ALREADY_USED: u32 = 0x0;
    }
    /// specifies the seat and serial of the activating event
    ///
    /// # Safety
    /// `this` is a live `xdg_activation_token_v1`; every object argument is live.
    pub unsafe fn set_serial(this: Proxy, serial: u32, seat: Proxy) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(0, &mut [Arg::uint(serial), Arg::object(Some(seat))], None, false) };
    }
    /// specifies the application being activated
    ///
    /// # Safety
    /// `this` is a live `xdg_activation_token_v1`; every object argument is live.
    pub unsafe fn set_app_id(this: Proxy, app_id: &CStr) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(1, &mut [Arg::string(Some(app_id))], None, false) };
    }
    /// specifies the surface requesting activation
    ///
    /// # Safety
    /// `this` is a live `xdg_activation_token_v1`; every object argument is live.
    pub unsafe fn set_surface(this: Proxy, surface: Proxy) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(2, &mut [Arg::object(Some(surface))], None, false) };
    }
    /// issues the token request
    ///
    /// # Safety
    /// `this` is a live `xdg_activation_token_v1`; every object argument is live.
    pub unsafe fn commit(this: Proxy) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(3, &mut [], None, false) };
    }
    /// destroy the xdg_activation_token_v1 object
    ///
    /// # Safety
    /// `this` is a live `xdg_activation_token_v1`, destroyed by this request and not used again; every object argument is live.
    pub unsafe fn destroy(this: Proxy) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(4, &mut [], None, true) };
    }
}
}
