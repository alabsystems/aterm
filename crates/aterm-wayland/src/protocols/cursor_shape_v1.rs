// SPDX-License-Identifier: MIT
// Copyright 2026 Andrew Yates
// Copyright 2018 The Chromium Authors
// Copyright 2023 Simon Ser
//
// Generated from protocols/cursor-shape-v1.xml (MIT, see protocols/COPYING.wayland-protocols) by
// `targo --unverified run --release` in crates/aterm-wayland/scan. Do not edit.

//! The `cursor_shape_v1` protocol.

pub use self::tables::*;

#[rustfmt::skip]
mod tables {
/// cursor shape manager
pub mod wp_cursor_shape_manager_v1 {
    #[allow(unused_imports)]
    use crate::iface::{Interface, Message, NULLS, TypeRef};
    #[allow(unused_imports)]
    use crate::proxy::{Arg, Fixed, Proxy, borrowed_array};
    #[allow(unused_imports)]
    use std::ffi::{CStr, c_int};

    /// The highest version this table describes.
    pub const VERSION: u32 = 2;
    /// The interface table.
    pub static INTERFACE: Interface = Interface::new(c"wp_cursor_shape_manager_v1", 2, &REQUESTS, &EVENTS);
    static REQUESTS_GET_POINTER_TYPES: [TypeRef; 2] = [
        TypeRef::to(&crate::protocols::cursor_shape_v1::wp_cursor_shape_device_v1::INTERFACE),
        TypeRef::to(&crate::protocols::wayland::wl_pointer::INTERFACE),
    ];
    static REQUESTS_GET_TABLET_TOOL_V2_TYPES: [TypeRef; 2] = [
        TypeRef::to(&crate::protocols::cursor_shape_v1::wp_cursor_shape_device_v1::INTERFACE),
        TypeRef::to(&crate::protocols::stubs::zwp_tablet_tool_v2::INTERFACE),
    ];
    static REQUESTS: [Message; 3] = [
        Message::new(c"destroy", c"", &NULLS),
        Message::new(c"get_pointer", c"no", &REQUESTS_GET_POINTER_TYPES),
        Message::new(c"get_tablet_tool_v2", c"no", &REQUESTS_GET_TABLET_TOOL_V2_TYPES),
    ];
    static EVENTS: [Message; 0] = [
    ];
    /// Request opcodes.
    pub mod req {
        pub const DESTROY: u32 = 0;
        pub const GET_POINTER: u32 = 1;
        pub const GET_TABLET_TOOL_V2: u32 = 2;
    }
    /// Event opcodes.
    pub mod evt {
    }
    /// destroy the manager
    ///
    /// # Safety
    /// `this` is a live `wp_cursor_shape_manager_v1`, destroyed by this request and not used again; every object argument is live.
    pub unsafe fn destroy(this: Proxy) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(0, &mut [], None, true) };
    }
    /// manage the cursor shape of a pointer device
    ///
    /// # Safety
    /// `this` is a live `wp_cursor_shape_manager_v1`; every object argument is live.
    pub unsafe fn get_pointer(this: Proxy, pointer: Proxy) -> Option<Proxy> {
        // SAFETY: the caller's contract; the arguments follow the signature.
        unsafe { this.marshal(1, &mut [Arg::new_id(), Arg::object(Some(pointer))], Some((&crate::protocols::cursor_shape_v1::wp_cursor_shape_device_v1::INTERFACE, this.version())), false) }
    }
    /// manage the cursor shape of a tablet tool device
    ///
    /// # Safety
    /// `this` is a live `wp_cursor_shape_manager_v1`; every object argument is live.
    pub unsafe fn get_tablet_tool_v2(this: Proxy, tablet_tool: Proxy) -> Option<Proxy> {
        // SAFETY: the caller's contract; the arguments follow the signature.
        unsafe { this.marshal(2, &mut [Arg::new_id(), Arg::object(Some(tablet_tool))], Some((&crate::protocols::cursor_shape_v1::wp_cursor_shape_device_v1::INTERFACE, this.version())), false) }
    }
}

/// cursor shape for a device
pub mod wp_cursor_shape_device_v1 {
    #[allow(unused_imports)]
    use crate::iface::{Interface, Message, NULLS, TypeRef};
    #[allow(unused_imports)]
    use crate::proxy::{Arg, Fixed, Proxy, borrowed_array};
    #[allow(unused_imports)]
    use std::ffi::{CStr, c_int};

    /// The highest version this table describes.
    pub const VERSION: u32 = 2;
    /// The interface table.
    pub static INTERFACE: Interface = Interface::new(c"wp_cursor_shape_device_v1", 2, &REQUESTS, &EVENTS);
    static REQUESTS: [Message; 2] = [
        Message::new(c"destroy", c"", &NULLS),
        Message::new(c"set_shape", c"uu", &NULLS),
    ];
    static EVENTS: [Message; 0] = [
    ];
    /// Request opcodes.
    pub mod req {
        pub const DESTROY: u32 = 0;
        pub const SET_SHAPE: u32 = 1;
    }
    /// Event opcodes.
    pub mod evt {
    }
    /// The `shape` enum.
    pub mod shape {
        /// default cursor
        pub const DEFAULT: u32 = 0x1;
        /// a context menu is available for the object under the cursor
        pub const CONTEXT_MENU: u32 = 0x2;
        /// help is available for the object under the cursor
        pub const HELP: u32 = 0x3;
        /// pointer that indicates a link or another interactive element
        pub const POINTER: u32 = 0x4;
        /// progress indicator
        pub const PROGRESS: u32 = 0x5;
        /// program is busy, user should wait
        pub const WAIT: u32 = 0x6;
        /// a cell or set of cells may be selected
        pub const CELL: u32 = 0x7;
        /// simple crosshair
        pub const CROSSHAIR: u32 = 0x8;
        /// text may be selected
        pub const TEXT: u32 = 0x9;
        /// vertical text may be selected
        pub const VERTICAL_TEXT: u32 = 0xa;
        /// drag-and-drop: alias of/shortcut to something is to be created
        pub const ALIAS: u32 = 0xb;
        /// drag-and-drop: something is to be copied
        pub const COPY: u32 = 0xc;
        /// drag-and-drop: something is to be moved
        pub const MOVE: u32 = 0xd;
        /// drag-and-drop: the dragged item cannot be dropped at the current cursor location
        pub const NO_DROP: u32 = 0xe;
        /// drag-and-drop: the requested action will not be carried out
        pub const NOT_ALLOWED: u32 = 0xf;
        /// drag-and-drop: something can be grabbed
        pub const GRAB: u32 = 0x10;
        /// drag-and-drop: something is being grabbed
        pub const GRABBING: u32 = 0x11;
        /// resizing: the east border is to be moved
        pub const E_RESIZE: u32 = 0x12;
        /// resizing: the north border is to be moved
        pub const N_RESIZE: u32 = 0x13;
        /// resizing: the north-east corner is to be moved
        pub const NE_RESIZE: u32 = 0x14;
        /// resizing: the north-west corner is to be moved
        pub const NW_RESIZE: u32 = 0x15;
        /// resizing: the south border is to be moved
        pub const S_RESIZE: u32 = 0x16;
        /// resizing: the south-east corner is to be moved
        pub const SE_RESIZE: u32 = 0x17;
        /// resizing: the south-west corner is to be moved
        pub const SW_RESIZE: u32 = 0x18;
        /// resizing: the west border is to be moved
        pub const W_RESIZE: u32 = 0x19;
        /// resizing: the east and west borders are to be moved
        pub const EW_RESIZE: u32 = 0x1a;
        /// resizing: the north and south borders are to be moved
        pub const NS_RESIZE: u32 = 0x1b;
        /// resizing: the north-east and south-west corners are to be moved
        pub const NESW_RESIZE: u32 = 0x1c;
        /// resizing: the north-west and south-east corners are to be moved
        pub const NWSE_RESIZE: u32 = 0x1d;
        /// resizing: that the item/column can be resized horizontally
        pub const COL_RESIZE: u32 = 0x1e;
        /// resizing: that the item/row can be resized vertically
        pub const ROW_RESIZE: u32 = 0x1f;
        /// something can be scrolled in any direction
        pub const ALL_SCROLL: u32 = 0x20;
        /// something can be zoomed in
        pub const ZOOM_IN: u32 = 0x21;
        /// something can be zoomed out
        pub const ZOOM_OUT: u32 = 0x22;
        /// drag-and-drop: the user will select which action will be carried out (non-css value)
        pub const DND_ASK: u32 = 0x23;
        /// resizing: something can be moved or resized in any direction (non-css value)
        pub const ALL_RESIZE: u32 = 0x24;
    }
    /// The `error` enum.
    pub mod error {
        /// the specified shape value is invalid
        pub const INVALID_SHAPE: u32 = 0x1;
    }
    /// destroy the cursor shape device
    ///
    /// # Safety
    /// `this` is a live `wp_cursor_shape_device_v1`, destroyed by this request and not used again; every object argument is live.
    pub unsafe fn destroy(this: Proxy) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(0, &mut [], None, true) };
    }
    /// set device cursor to the shape
    ///
    /// # Safety
    /// `this` is a live `wp_cursor_shape_device_v1`; every object argument is live.
    pub unsafe fn set_shape(this: Proxy, serial: u32, shape: u32) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(1, &mut [Arg::uint(serial), Arg::uint(shape)], None, false) };
    }
}
}
