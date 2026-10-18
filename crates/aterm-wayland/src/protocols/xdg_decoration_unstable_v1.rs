// SPDX-License-Identifier: MIT
// Copyright 2026 Andrew Yates
// Copyright © 2018 Simon Ser
//
// Generated from protocols/xdg-decoration-unstable-v1.xml (MIT, see protocols/COPYING.wayland-protocols) by
// `targo --unverified run --release` in crates/aterm-wayland/scan. Do not edit.

//! The `xdg_decoration_unstable_v1` protocol.

pub use self::tables::*;

#[rustfmt::skip]
mod tables {
/// window decoration manager
pub mod zxdg_decoration_manager_v1 {
    #[allow(unused_imports)]
    use crate::iface::{Interface, Message, NULLS, TypeRef};
    #[allow(unused_imports)]
    use crate::proxy::{Arg, Fixed, Proxy, borrowed_array};
    #[allow(unused_imports)]
    use std::ffi::{CStr, c_int};

    /// The highest version this table describes.
    pub const VERSION: u32 = 1;
    /// The interface table.
    pub static INTERFACE: Interface = Interface::new(c"zxdg_decoration_manager_v1", 1, &REQUESTS, &EVENTS);
    static REQUESTS_GET_TOPLEVEL_DECORATION_TYPES: [TypeRef; 2] = [
        TypeRef::to(&crate::protocols::xdg_decoration_unstable_v1::zxdg_toplevel_decoration_v1::INTERFACE),
        TypeRef::to(&crate::protocols::xdg_shell::xdg_toplevel::INTERFACE),
    ];
    static REQUESTS: [Message; 2] = [
        Message::new(c"destroy", c"", &NULLS),
        Message::new(c"get_toplevel_decoration", c"no", &REQUESTS_GET_TOPLEVEL_DECORATION_TYPES),
    ];
    static EVENTS: [Message; 0] = [
    ];
    /// Request opcodes.
    pub mod req {
        pub const DESTROY: u32 = 0;
        pub const GET_TOPLEVEL_DECORATION: u32 = 1;
    }
    /// Event opcodes.
    pub mod evt {
    }
    /// destroy the decoration manager object
    ///
    /// # Safety
    /// `this` is a live `zxdg_decoration_manager_v1`, destroyed by this request and not used again; every object argument is live.
    pub unsafe fn destroy(this: Proxy) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(0, &mut [], None, true) };
    }
    /// create a new toplevel decoration object
    ///
    /// # Safety
    /// `this` is a live `zxdg_decoration_manager_v1`; every object argument is live.
    pub unsafe fn get_toplevel_decoration(this: Proxy, toplevel: Proxy) -> Option<Proxy> {
        // SAFETY: the caller's contract; the arguments follow the signature.
        unsafe { this.marshal(1, &mut [Arg::new_id(), Arg::object(Some(toplevel))], Some((&crate::protocols::xdg_decoration_unstable_v1::zxdg_toplevel_decoration_v1::INTERFACE, this.version())), false) }
    }
}

/// decoration object for a toplevel surface
pub mod zxdg_toplevel_decoration_v1 {
    #[allow(unused_imports)]
    use crate::iface::{Interface, Message, NULLS, TypeRef};
    #[allow(unused_imports)]
    use crate::proxy::{Arg, Fixed, Proxy, borrowed_array};
    #[allow(unused_imports)]
    use std::ffi::{CStr, c_int};

    /// The highest version this table describes.
    pub const VERSION: u32 = 1;
    /// The interface table.
    pub static INTERFACE: Interface = Interface::new(c"zxdg_toplevel_decoration_v1", 1, &REQUESTS, &EVENTS);
    static REQUESTS: [Message; 3] = [
        Message::new(c"destroy", c"", &NULLS),
        Message::new(c"set_mode", c"u", &NULLS),
        Message::new(c"unset_mode", c"", &NULLS),
    ];
    static EVENTS: [Message; 1] = [
        Message::new(c"configure", c"u", &NULLS),
    ];
    /// Request opcodes.
    pub mod req {
        pub const DESTROY: u32 = 0;
        pub const SET_MODE: u32 = 1;
        pub const UNSET_MODE: u32 = 2;
    }
    /// Event opcodes.
    pub mod evt {
        pub const CONFIGURE: u32 = 0;
    }
    /// The `error` enum.
    pub mod error {
        /// xdg_toplevel has a buffer attached before configure
        pub const UNCONFIGURED_BUFFER: u32 = 0x0;
        /// xdg_toplevel already has a decoration object
        pub const ALREADY_CONSTRUCTED: u32 = 0x1;
        /// xdg_toplevel destroyed before the decoration object
        pub const ORPHANED: u32 = 0x2;
        /// invalid mode
        pub const INVALID_MODE: u32 = 0x3;
    }
    /// The `mode` enum.
    pub mod mode {
        /// no server-side window decoration
        pub const CLIENT_SIDE: u32 = 0x1;
        /// server-side window decoration
        pub const SERVER_SIDE: u32 = 0x2;
    }
    /// destroy the decoration object
    ///
    /// # Safety
    /// `this` is a live `zxdg_toplevel_decoration_v1`, destroyed by this request and not used again; every object argument is live.
    pub unsafe fn destroy(this: Proxy) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(0, &mut [], None, true) };
    }
    /// set the decoration mode
    ///
    /// # Safety
    /// `this` is a live `zxdg_toplevel_decoration_v1`; every object argument is live.
    pub unsafe fn set_mode(this: Proxy, mode: u32) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(1, &mut [Arg::uint(mode)], None, false) };
    }
    /// unset the decoration mode
    ///
    /// # Safety
    /// `this` is a live `zxdg_toplevel_decoration_v1`; every object argument is live.
    pub unsafe fn unset_mode(this: Proxy) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(2, &mut [], None, false) };
    }
}
}
