// SPDX-License-Identifier: MIT
// Copyright 2026 Andrew Yates
// Copyright © 2008-2013 Kristian Høgsberg
// Copyright © 2013      Rafael Antognolli
// Copyright © 2013      Jasper St. Pierre
// Copyright © 2010-2013 Intel Corporation
// Copyright © 2015-2017 Samsung Electronics Co., Ltd
// Copyright © 2015-2017 Red Hat Inc.
//
// Generated from protocols/xdg-shell.xml (MIT, see protocols/COPYING.wayland-protocols) by
// `targo --unverified run --release` in crates/aterm-wayland/scan. Do not edit.

//! The `xdg_shell` protocol.

pub use self::tables::*;

#[rustfmt::skip]
mod tables {
/// create desktop-style surfaces
pub mod xdg_wm_base {
    #[allow(unused_imports)]
    use crate::iface::{Interface, Message, NULLS, TypeRef};
    #[allow(unused_imports)]
    use crate::proxy::{Arg, Fixed, Proxy, borrowed_array};
    #[allow(unused_imports)]
    use std::ffi::{CStr, c_int};

    /// The highest version this table describes.
    pub const VERSION: u32 = 7;
    /// The interface table.
    pub static INTERFACE: Interface = Interface::new(c"xdg_wm_base", 7, &REQUESTS, &EVENTS);
    static REQUESTS_CREATE_POSITIONER_TYPES: [TypeRef; 1] = [
        TypeRef::to(&crate::protocols::xdg_shell::xdg_positioner::INTERFACE),
    ];
    static REQUESTS_GET_XDG_SURFACE_TYPES: [TypeRef; 2] = [
        TypeRef::to(&crate::protocols::xdg_shell::xdg_surface::INTERFACE),
        TypeRef::to(&crate::protocols::wayland::wl_surface::INTERFACE),
    ];
    static REQUESTS: [Message; 4] = [
        Message::new(c"destroy", c"", &NULLS),
        Message::new(c"create_positioner", c"n", &REQUESTS_CREATE_POSITIONER_TYPES),
        Message::new(c"get_xdg_surface", c"no", &REQUESTS_GET_XDG_SURFACE_TYPES),
        Message::new(c"pong", c"u", &NULLS),
    ];
    static EVENTS: [Message; 1] = [
        Message::new(c"ping", c"u", &NULLS),
    ];
    /// Request opcodes.
    pub mod req {
        pub const DESTROY: u32 = 0;
        pub const CREATE_POSITIONER: u32 = 1;
        pub const GET_XDG_SURFACE: u32 = 2;
        pub const PONG: u32 = 3;
    }
    /// Event opcodes.
    pub mod evt {
        pub const PING: u32 = 0;
    }
    /// The `error` enum.
    pub mod error {
        /// given wl_surface has another role
        pub const ROLE: u32 = 0x0;
        /// xdg_wm_base was destroyed before children
        pub const DEFUNCT_SURFACES: u32 = 0x1;
        /// the client tried to map or destroy a non-topmost popup
        pub const NOT_THE_TOPMOST_POPUP: u32 = 0x2;
        /// the client specified an invalid popup parent surface
        pub const INVALID_POPUP_PARENT: u32 = 0x3;
        /// the client provided an invalid surface state
        pub const INVALID_SURFACE_STATE: u32 = 0x4;
        /// the client provided an invalid positioner
        pub const INVALID_POSITIONER: u32 = 0x5;
        /// the client didn’t respond to a ping event in time
        pub const UNRESPONSIVE: u32 = 0x6;
    }
    /// destroy xdg_wm_base
    ///
    /// # Safety
    /// `this` is a live `xdg_wm_base`, destroyed by this request and not used again; every object argument is live.
    pub unsafe fn destroy(this: Proxy) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(0, &mut [], None, true) };
    }
    /// create a positioner object
    ///
    /// # Safety
    /// `this` is a live `xdg_wm_base`; every object argument is live.
    pub unsafe fn create_positioner(this: Proxy) -> Option<Proxy> {
        // SAFETY: the caller's contract; the arguments follow the signature.
        unsafe { this.marshal(1, &mut [Arg::new_id()], Some((&crate::protocols::xdg_shell::xdg_positioner::INTERFACE, this.version())), false) }
    }
    /// create a shell surface from a surface
    ///
    /// # Safety
    /// `this` is a live `xdg_wm_base`; every object argument is live.
    pub unsafe fn get_xdg_surface(this: Proxy, surface: Proxy) -> Option<Proxy> {
        // SAFETY: the caller's contract; the arguments follow the signature.
        unsafe { this.marshal(2, &mut [Arg::new_id(), Arg::object(Some(surface))], Some((&crate::protocols::xdg_shell::xdg_surface::INTERFACE, this.version())), false) }
    }
    /// respond to a ping event
    ///
    /// # Safety
    /// `this` is a live `xdg_wm_base`; every object argument is live.
    pub unsafe fn pong(this: Proxy, serial: u32) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(3, &mut [Arg::uint(serial)], None, false) };
    }
}

/// child surface positioner
pub mod xdg_positioner {
    #[allow(unused_imports)]
    use crate::iface::{Interface, Message, NULLS, TypeRef};
    #[allow(unused_imports)]
    use crate::proxy::{Arg, Fixed, Proxy, borrowed_array};
    #[allow(unused_imports)]
    use std::ffi::{CStr, c_int};

    /// The highest version this table describes.
    pub const VERSION: u32 = 7;
    /// The interface table.
    pub static INTERFACE: Interface = Interface::new(c"xdg_positioner", 7, &REQUESTS, &EVENTS);
    static REQUESTS: [Message; 10] = [
        Message::new(c"destroy", c"", &NULLS),
        Message::new(c"set_size", c"ii", &NULLS),
        Message::new(c"set_anchor_rect", c"iiii", &NULLS),
        Message::new(c"set_anchor", c"u", &NULLS),
        Message::new(c"set_gravity", c"u", &NULLS),
        Message::new(c"set_constraint_adjustment", c"u", &NULLS),
        Message::new(c"set_offset", c"ii", &NULLS),
        Message::new(c"set_reactive", c"3", &NULLS),
        Message::new(c"set_parent_size", c"3ii", &NULLS),
        Message::new(c"set_parent_configure", c"3u", &NULLS),
    ];
    static EVENTS: [Message; 0] = [
    ];
    /// Request opcodes.
    pub mod req {
        pub const DESTROY: u32 = 0;
        pub const SET_SIZE: u32 = 1;
        pub const SET_ANCHOR_RECT: u32 = 2;
        pub const SET_ANCHOR: u32 = 3;
        pub const SET_GRAVITY: u32 = 4;
        pub const SET_CONSTRAINT_ADJUSTMENT: u32 = 5;
        pub const SET_OFFSET: u32 = 6;
        pub const SET_REACTIVE: u32 = 7;
        pub const SET_PARENT_SIZE: u32 = 8;
        pub const SET_PARENT_CONFIGURE: u32 = 9;
    }
    /// Event opcodes.
    pub mod evt {
    }
    /// The `error` enum.
    pub mod error {
        /// invalid input provided
        pub const INVALID_INPUT: u32 = 0x0;
    }
    /// The `anchor` enum.
    pub mod anchor {
        pub const NONE: u32 = 0x0;
        pub const TOP: u32 = 0x1;
        pub const BOTTOM: u32 = 0x2;
        pub const LEFT: u32 = 0x3;
        pub const RIGHT: u32 = 0x4;
        pub const TOP_LEFT: u32 = 0x5;
        pub const BOTTOM_LEFT: u32 = 0x6;
        pub const TOP_RIGHT: u32 = 0x7;
        pub const BOTTOM_RIGHT: u32 = 0x8;
    }
    /// The `gravity` enum.
    pub mod gravity {
        pub const NONE: u32 = 0x0;
        pub const TOP: u32 = 0x1;
        pub const BOTTOM: u32 = 0x2;
        pub const LEFT: u32 = 0x3;
        pub const RIGHT: u32 = 0x4;
        pub const TOP_LEFT: u32 = 0x5;
        pub const BOTTOM_LEFT: u32 = 0x6;
        pub const TOP_RIGHT: u32 = 0x7;
        pub const BOTTOM_RIGHT: u32 = 0x8;
    }
    /// The `constraint_adjustment` enum.
    pub mod constraint_adjustment {
        /// don't move the child surface when constrained
        pub const NONE: u32 = 0x0;
        /// move along the x axis until unconstrained
        pub const SLIDE_X: u32 = 0x1;
        /// move along the y axis until unconstrained
        pub const SLIDE_Y: u32 = 0x2;
        /// invert the anchor and gravity on the x axis
        pub const FLIP_X: u32 = 0x4;
        /// invert the anchor and gravity on the y axis
        pub const FLIP_Y: u32 = 0x8;
        /// horizontally resize the surface
        pub const RESIZE_X: u32 = 0x10;
        /// vertically resize the surface
        pub const RESIZE_Y: u32 = 0x20;
    }
    /// destroy the xdg_positioner object
    ///
    /// # Safety
    /// `this` is a live `xdg_positioner`, destroyed by this request and not used again; every object argument is live.
    pub unsafe fn destroy(this: Proxy) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(0, &mut [], None, true) };
    }
    /// set the size of the to-be positioned rectangle
    ///
    /// # Safety
    /// `this` is a live `xdg_positioner`; every object argument is live.
    pub unsafe fn set_size(this: Proxy, width: i32, height: i32) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(1, &mut [Arg::int(width), Arg::int(height)], None, false) };
    }
    /// set the anchor rectangle within the parent surface
    ///
    /// # Safety
    /// `this` is a live `xdg_positioner`; every object argument is live.
    pub unsafe fn set_anchor_rect(this: Proxy, x: i32, y: i32, width: i32, height: i32) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(2, &mut [Arg::int(x), Arg::int(y), Arg::int(width), Arg::int(height)], None, false) };
    }
    /// set anchor rectangle anchor
    ///
    /// # Safety
    /// `this` is a live `xdg_positioner`; every object argument is live.
    pub unsafe fn set_anchor(this: Proxy, anchor: u32) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(3, &mut [Arg::uint(anchor)], None, false) };
    }
    /// set child surface gravity
    ///
    /// # Safety
    /// `this` is a live `xdg_positioner`; every object argument is live.
    pub unsafe fn set_gravity(this: Proxy, gravity: u32) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(4, &mut [Arg::uint(gravity)], None, false) };
    }
    /// set the adjustment to be done when constrained
    ///
    /// # Safety
    /// `this` is a live `xdg_positioner`; every object argument is live.
    pub unsafe fn set_constraint_adjustment(this: Proxy, constraint_adjustment: u32) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(5, &mut [Arg::uint(constraint_adjustment)], None, false) };
    }
    /// set surface position offset
    ///
    /// # Safety
    /// `this` is a live `xdg_positioner`; every object argument is live.
    pub unsafe fn set_offset(this: Proxy, x: i32, y: i32) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(6, &mut [Arg::int(x), Arg::int(y)], None, false) };
    }
    /// continuously reconstrain the surface
    ///
    /// # Safety
    /// `this` is a live `xdg_positioner`; every object argument is live.
    pub unsafe fn set_reactive(this: Proxy) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(7, &mut [], None, false) };
    }
    /// # Safety
    /// `this` is a live `xdg_positioner`; every object argument is live.
    pub unsafe fn set_parent_size(this: Proxy, parent_width: i32, parent_height: i32) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(8, &mut [Arg::int(parent_width), Arg::int(parent_height)], None, false) };
    }
    /// set parent configure this is a response to
    ///
    /// # Safety
    /// `this` is a live `xdg_positioner`; every object argument is live.
    pub unsafe fn set_parent_configure(this: Proxy, serial: u32) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(9, &mut [Arg::uint(serial)], None, false) };
    }
}

/// desktop user interface surface base interface
pub mod xdg_surface {
    #[allow(unused_imports)]
    use crate::iface::{Interface, Message, NULLS, TypeRef};
    #[allow(unused_imports)]
    use crate::proxy::{Arg, Fixed, Proxy, borrowed_array};
    #[allow(unused_imports)]
    use std::ffi::{CStr, c_int};

    /// The highest version this table describes.
    pub const VERSION: u32 = 7;
    /// The interface table.
    pub static INTERFACE: Interface = Interface::new(c"xdg_surface", 7, &REQUESTS, &EVENTS);
    static REQUESTS_GET_TOPLEVEL_TYPES: [TypeRef; 1] = [
        TypeRef::to(&crate::protocols::xdg_shell::xdg_toplevel::INTERFACE),
    ];
    static REQUESTS_GET_POPUP_TYPES: [TypeRef; 3] = [
        TypeRef::to(&crate::protocols::xdg_shell::xdg_popup::INTERFACE),
        TypeRef::to(&crate::protocols::xdg_shell::xdg_surface::INTERFACE),
        TypeRef::to(&crate::protocols::xdg_shell::xdg_positioner::INTERFACE),
    ];
    static REQUESTS: [Message; 5] = [
        Message::new(c"destroy", c"", &NULLS),
        Message::new(c"get_toplevel", c"n", &REQUESTS_GET_TOPLEVEL_TYPES),
        Message::new(c"get_popup", c"n?oo", &REQUESTS_GET_POPUP_TYPES),
        Message::new(c"set_window_geometry", c"iiii", &NULLS),
        Message::new(c"ack_configure", c"u", &NULLS),
    ];
    static EVENTS: [Message; 1] = [
        Message::new(c"configure", c"u", &NULLS),
    ];
    /// Request opcodes.
    pub mod req {
        pub const DESTROY: u32 = 0;
        pub const GET_TOPLEVEL: u32 = 1;
        pub const GET_POPUP: u32 = 2;
        pub const SET_WINDOW_GEOMETRY: u32 = 3;
        pub const ACK_CONFIGURE: u32 = 4;
    }
    /// Event opcodes.
    pub mod evt {
        pub const CONFIGURE: u32 = 0;
    }
    /// The `error` enum.
    pub mod error {
        /// Surface was not fully constructed
        pub const NOT_CONSTRUCTED: u32 = 0x1;
        /// Surface was already constructed
        pub const ALREADY_CONSTRUCTED: u32 = 0x2;
        /// Attaching a buffer to an unconfigured surface
        pub const UNCONFIGURED_BUFFER: u32 = 0x3;
        /// Invalid serial number when acking a configure event
        pub const INVALID_SERIAL: u32 = 0x4;
        /// Width or height was zero or negative
        pub const INVALID_SIZE: u32 = 0x5;
        /// Surface was destroyed before its role object
        pub const DEFUNCT_ROLE_OBJECT: u32 = 0x6;
    }
    /// destroy the xdg_surface
    ///
    /// # Safety
    /// `this` is a live `xdg_surface`, destroyed by this request and not used again; every object argument is live.
    pub unsafe fn destroy(this: Proxy) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(0, &mut [], None, true) };
    }
    /// assign the xdg_toplevel surface role
    ///
    /// # Safety
    /// `this` is a live `xdg_surface`; every object argument is live.
    pub unsafe fn get_toplevel(this: Proxy) -> Option<Proxy> {
        // SAFETY: the caller's contract; the arguments follow the signature.
        unsafe { this.marshal(1, &mut [Arg::new_id()], Some((&crate::protocols::xdg_shell::xdg_toplevel::INTERFACE, this.version())), false) }
    }
    /// assign the xdg_popup surface role
    ///
    /// # Safety
    /// `this` is a live `xdg_surface`; every object argument is live.
    pub unsafe fn get_popup(this: Proxy, parent: Option<Proxy>, positioner: Proxy) -> Option<Proxy> {
        // SAFETY: the caller's contract; the arguments follow the signature.
        unsafe { this.marshal(2, &mut [Arg::new_id(), Arg::object(parent), Arg::object(Some(positioner))], Some((&crate::protocols::xdg_shell::xdg_popup::INTERFACE, this.version())), false) }
    }
    /// set the new window geometry
    ///
    /// # Safety
    /// `this` is a live `xdg_surface`; every object argument is live.
    pub unsafe fn set_window_geometry(this: Proxy, x: i32, y: i32, width: i32, height: i32) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(3, &mut [Arg::int(x), Arg::int(y), Arg::int(width), Arg::int(height)], None, false) };
    }
    /// ack a configure event
    ///
    /// # Safety
    /// `this` is a live `xdg_surface`; every object argument is live.
    pub unsafe fn ack_configure(this: Proxy, serial: u32) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(4, &mut [Arg::uint(serial)], None, false) };
    }
}

/// toplevel surface
pub mod xdg_toplevel {
    #[allow(unused_imports)]
    use crate::iface::{Interface, Message, NULLS, TypeRef};
    #[allow(unused_imports)]
    use crate::proxy::{Arg, Fixed, Proxy, borrowed_array};
    #[allow(unused_imports)]
    use std::ffi::{CStr, c_int};

    /// The highest version this table describes.
    pub const VERSION: u32 = 7;
    /// The interface table.
    pub static INTERFACE: Interface = Interface::new(c"xdg_toplevel", 7, &REQUESTS, &EVENTS);
    static REQUESTS_SET_PARENT_TYPES: [TypeRef; 1] = [
        TypeRef::to(&crate::protocols::xdg_shell::xdg_toplevel::INTERFACE),
    ];
    static REQUESTS_SHOW_WINDOW_MENU_TYPES: [TypeRef; 4] = [
        TypeRef::to(&crate::protocols::wayland::wl_seat::INTERFACE),
        TypeRef::NULL,
        TypeRef::NULL,
        TypeRef::NULL,
    ];
    static REQUESTS_MOVE_TYPES: [TypeRef; 2] = [
        TypeRef::to(&crate::protocols::wayland::wl_seat::INTERFACE),
        TypeRef::NULL,
    ];
    static REQUESTS_RESIZE_TYPES: [TypeRef; 3] = [
        TypeRef::to(&crate::protocols::wayland::wl_seat::INTERFACE),
        TypeRef::NULL,
        TypeRef::NULL,
    ];
    static REQUESTS_SET_FULLSCREEN_TYPES: [TypeRef; 1] = [
        TypeRef::to(&crate::protocols::wayland::wl_output::INTERFACE),
    ];
    static REQUESTS: [Message; 14] = [
        Message::new(c"destroy", c"", &NULLS),
        Message::new(c"set_parent", c"?o", &REQUESTS_SET_PARENT_TYPES),
        Message::new(c"set_title", c"s", &NULLS),
        Message::new(c"set_app_id", c"s", &NULLS),
        Message::new(c"show_window_menu", c"ouii", &REQUESTS_SHOW_WINDOW_MENU_TYPES),
        Message::new(c"move", c"ou", &REQUESTS_MOVE_TYPES),
        Message::new(c"resize", c"ouu", &REQUESTS_RESIZE_TYPES),
        Message::new(c"set_max_size", c"ii", &NULLS),
        Message::new(c"set_min_size", c"ii", &NULLS),
        Message::new(c"set_maximized", c"", &NULLS),
        Message::new(c"unset_maximized", c"", &NULLS),
        Message::new(c"set_fullscreen", c"?o", &REQUESTS_SET_FULLSCREEN_TYPES),
        Message::new(c"unset_fullscreen", c"", &NULLS),
        Message::new(c"set_minimized", c"", &NULLS),
    ];
    static EVENTS: [Message; 4] = [
        Message::new(c"configure", c"iia", &NULLS),
        Message::new(c"close", c"", &NULLS),
        Message::new(c"configure_bounds", c"4ii", &NULLS),
        Message::new(c"wm_capabilities", c"5a", &NULLS),
    ];
    /// Request opcodes.
    pub mod req {
        pub const DESTROY: u32 = 0;
        pub const SET_PARENT: u32 = 1;
        pub const SET_TITLE: u32 = 2;
        pub const SET_APP_ID: u32 = 3;
        pub const SHOW_WINDOW_MENU: u32 = 4;
        pub const MOVE: u32 = 5;
        pub const RESIZE: u32 = 6;
        pub const SET_MAX_SIZE: u32 = 7;
        pub const SET_MIN_SIZE: u32 = 8;
        pub const SET_MAXIMIZED: u32 = 9;
        pub const UNSET_MAXIMIZED: u32 = 10;
        pub const SET_FULLSCREEN: u32 = 11;
        pub const UNSET_FULLSCREEN: u32 = 12;
        pub const SET_MINIMIZED: u32 = 13;
    }
    /// Event opcodes.
    pub mod evt {
        pub const CONFIGURE: u32 = 0;
        pub const CLOSE: u32 = 1;
        pub const CONFIGURE_BOUNDS: u32 = 2;
        pub const WM_CAPABILITIES: u32 = 3;
    }
    /// The `error` enum.
    pub mod error {
        /// provided value is not a valid variant of the resize_edge enum
        pub const INVALID_RESIZE_EDGE: u32 = 0x0;
        /// invalid parent toplevel
        pub const INVALID_PARENT: u32 = 0x1;
        /// client provided an invalid min or max size
        pub const INVALID_SIZE: u32 = 0x2;
    }
    /// The `resize_edge` enum.
    pub mod resize_edge {
        pub const NONE: u32 = 0x0;
        pub const TOP: u32 = 0x1;
        pub const BOTTOM: u32 = 0x2;
        pub const LEFT: u32 = 0x4;
        pub const TOP_LEFT: u32 = 0x5;
        pub const BOTTOM_LEFT: u32 = 0x6;
        pub const RIGHT: u32 = 0x8;
        pub const TOP_RIGHT: u32 = 0x9;
        pub const BOTTOM_RIGHT: u32 = 0xa;
    }
    /// The `state` enum.
    pub mod state {
        /// the surface is maximized
        pub const MAXIMIZED: u32 = 0x1;
        /// the surface is fullscreen
        pub const FULLSCREEN: u32 = 0x2;
        /// the surface is being resized
        pub const RESIZING: u32 = 0x3;
        /// the surface is now activated
        pub const ACTIVATED: u32 = 0x4;
        /// the surface’s left edge is tiled
        pub const TILED_LEFT: u32 = 0x5;
        /// the surface’s right edge is tiled
        pub const TILED_RIGHT: u32 = 0x6;
        /// the surface’s top edge is tiled
        pub const TILED_TOP: u32 = 0x7;
        /// the surface’s bottom edge is tiled
        pub const TILED_BOTTOM: u32 = 0x8;
        /// surface repaint is suspended
        pub const SUSPENDED: u32 = 0x9;
        /// the surface’s left edge is constrained
        pub const CONSTRAINED_LEFT: u32 = 0xa;
        /// the surface’s right edge is constrained
        pub const CONSTRAINED_RIGHT: u32 = 0xb;
        /// the surface’s top edge is constrained
        pub const CONSTRAINED_TOP: u32 = 0xc;
        /// the surface’s bottom edge is constrained
        pub const CONSTRAINED_BOTTOM: u32 = 0xd;
    }
    /// The `wm_capabilities` enum.
    pub mod wm_capabilities {
        /// show_window_menu is available
        pub const WINDOW_MENU: u32 = 0x1;
        /// set_maximized and unset_maximized are available
        pub const MAXIMIZE: u32 = 0x2;
        /// set_fullscreen and unset_fullscreen are available
        pub const FULLSCREEN: u32 = 0x3;
        /// set_minimized is available
        pub const MINIMIZE: u32 = 0x4;
    }
    /// destroy the xdg_toplevel
    ///
    /// # Safety
    /// `this` is a live `xdg_toplevel`, destroyed by this request and not used again; every object argument is live.
    pub unsafe fn destroy(this: Proxy) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(0, &mut [], None, true) };
    }
    /// set the parent of this surface
    ///
    /// # Safety
    /// `this` is a live `xdg_toplevel`; every object argument is live.
    pub unsafe fn set_parent(this: Proxy, parent: Option<Proxy>) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(1, &mut [Arg::object(parent)], None, false) };
    }
    /// set surface title
    ///
    /// # Safety
    /// `this` is a live `xdg_toplevel`; every object argument is live.
    pub unsafe fn set_title(this: Proxy, title: &CStr) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(2, &mut [Arg::string(Some(title))], None, false) };
    }
    /// set application ID
    ///
    /// # Safety
    /// `this` is a live `xdg_toplevel`; every object argument is live.
    pub unsafe fn set_app_id(this: Proxy, app_id: &CStr) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(3, &mut [Arg::string(Some(app_id))], None, false) };
    }
    /// show the window menu
    ///
    /// # Safety
    /// `this` is a live `xdg_toplevel`; every object argument is live.
    pub unsafe fn show_window_menu(this: Proxy, seat: Proxy, serial: u32, x: i32, y: i32) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(4, &mut [Arg::object(Some(seat)), Arg::uint(serial), Arg::int(x), Arg::int(y)], None, false) };
    }
    /// start an interactive move
    ///
    /// # Safety
    /// `this` is a live `xdg_toplevel`; every object argument is live.
    pub unsafe fn r#move(this: Proxy, seat: Proxy, serial: u32) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(5, &mut [Arg::object(Some(seat)), Arg::uint(serial)], None, false) };
    }
    /// start an interactive resize
    ///
    /// # Safety
    /// `this` is a live `xdg_toplevel`; every object argument is live.
    pub unsafe fn resize(this: Proxy, seat: Proxy, serial: u32, edges: u32) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(6, &mut [Arg::object(Some(seat)), Arg::uint(serial), Arg::uint(edges)], None, false) };
    }
    /// set the maximum size
    ///
    /// # Safety
    /// `this` is a live `xdg_toplevel`; every object argument is live.
    pub unsafe fn set_max_size(this: Proxy, width: i32, height: i32) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(7, &mut [Arg::int(width), Arg::int(height)], None, false) };
    }
    /// set the minimum size
    ///
    /// # Safety
    /// `this` is a live `xdg_toplevel`; every object argument is live.
    pub unsafe fn set_min_size(this: Proxy, width: i32, height: i32) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(8, &mut [Arg::int(width), Arg::int(height)], None, false) };
    }
    /// maximize the window
    ///
    /// # Safety
    /// `this` is a live `xdg_toplevel`; every object argument is live.
    pub unsafe fn set_maximized(this: Proxy) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(9, &mut [], None, false) };
    }
    /// unmaximize the window
    ///
    /// # Safety
    /// `this` is a live `xdg_toplevel`; every object argument is live.
    pub unsafe fn unset_maximized(this: Proxy) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(10, &mut [], None, false) };
    }
    /// set the window as fullscreen on an output
    ///
    /// # Safety
    /// `this` is a live `xdg_toplevel`; every object argument is live.
    pub unsafe fn set_fullscreen(this: Proxy, output: Option<Proxy>) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(11, &mut [Arg::object(output)], None, false) };
    }
    /// unset the window as fullscreen
    ///
    /// # Safety
    /// `this` is a live `xdg_toplevel`; every object argument is live.
    pub unsafe fn unset_fullscreen(this: Proxy) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(12, &mut [], None, false) };
    }
    /// set the window as minimized
    ///
    /// # Safety
    /// `this` is a live `xdg_toplevel`; every object argument is live.
    pub unsafe fn set_minimized(this: Proxy) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(13, &mut [], None, false) };
    }
}

/// short-lived, popup surfaces for menus
pub mod xdg_popup {
    #[allow(unused_imports)]
    use crate::iface::{Interface, Message, NULLS, TypeRef};
    #[allow(unused_imports)]
    use crate::proxy::{Arg, Fixed, Proxy, borrowed_array};
    #[allow(unused_imports)]
    use std::ffi::{CStr, c_int};

    /// The highest version this table describes.
    pub const VERSION: u32 = 7;
    /// The interface table.
    pub static INTERFACE: Interface = Interface::new(c"xdg_popup", 7, &REQUESTS, &EVENTS);
    static REQUESTS_GRAB_TYPES: [TypeRef; 2] = [
        TypeRef::to(&crate::protocols::wayland::wl_seat::INTERFACE),
        TypeRef::NULL,
    ];
    static REQUESTS_REPOSITION_TYPES: [TypeRef; 2] = [
        TypeRef::to(&crate::protocols::xdg_shell::xdg_positioner::INTERFACE),
        TypeRef::NULL,
    ];
    static REQUESTS: [Message; 3] = [
        Message::new(c"destroy", c"", &NULLS),
        Message::new(c"grab", c"ou", &REQUESTS_GRAB_TYPES),
        Message::new(c"reposition", c"3ou", &REQUESTS_REPOSITION_TYPES),
    ];
    static EVENTS: [Message; 3] = [
        Message::new(c"configure", c"iiii", &NULLS),
        Message::new(c"popup_done", c"", &NULLS),
        Message::new(c"repositioned", c"3u", &NULLS),
    ];
    /// Request opcodes.
    pub mod req {
        pub const DESTROY: u32 = 0;
        pub const GRAB: u32 = 1;
        pub const REPOSITION: u32 = 2;
    }
    /// Event opcodes.
    pub mod evt {
        pub const CONFIGURE: u32 = 0;
        pub const POPUP_DONE: u32 = 1;
        pub const REPOSITIONED: u32 = 2;
    }
    /// The `error` enum.
    pub mod error {
        /// tried to grab after being mapped
        pub const INVALID_GRAB: u32 = 0x0;
    }
    /// remove xdg_popup interface
    ///
    /// # Safety
    /// `this` is a live `xdg_popup`, destroyed by this request and not used again; every object argument is live.
    pub unsafe fn destroy(this: Proxy) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(0, &mut [], None, true) };
    }
    /// make the popup take an explicit grab
    ///
    /// # Safety
    /// `this` is a live `xdg_popup`; every object argument is live.
    pub unsafe fn grab(this: Proxy, seat: Proxy, serial: u32) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(1, &mut [Arg::object(Some(seat)), Arg::uint(serial)], None, false) };
    }
    /// recalculate the popup's location
    ///
    /// # Safety
    /// `this` is a live `xdg_popup`; every object argument is live.
    pub unsafe fn reposition(this: Proxy, positioner: Proxy, token: u32) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(2, &mut [Arg::object(Some(positioner)), Arg::uint(token)], None, false) };
    }
}
}
