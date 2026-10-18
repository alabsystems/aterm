// SPDX-License-Identifier: MIT
// Copyright 2026 Andrew Yates
// Copyright © 2012, 2013 Intel Corporation
// Copyright © 2015, 2016 Jan Arne Petersen
// Copyright © 2017, 2018 Red Hat, Inc.
// Copyright © 2018       Purism SPC
//
// Generated from protocols/text-input-unstable-v3.xml (MIT, see protocols/COPYING.wayland-protocols) by
// `targo --unverified run --release` in crates/aterm-wayland/scan. Do not edit.

//! The `text_input_unstable_v3` protocol.

pub use self::tables::*;

#[rustfmt::skip]
mod tables {
/// text input
pub mod zwp_text_input_v3 {
    #[allow(unused_imports)]
    use crate::iface::{Interface, Message, NULLS, TypeRef};
    #[allow(unused_imports)]
    use crate::proxy::{Arg, Fixed, Proxy, borrowed_array};
    #[allow(unused_imports)]
    use std::ffi::{CStr, c_int};

    /// The highest version this table describes.
    pub const VERSION: u32 = 1;
    /// The interface table.
    pub static INTERFACE: Interface = Interface::new(c"zwp_text_input_v3", 1, &REQUESTS, &EVENTS);
    static REQUESTS: [Message; 8] = [
        Message::new(c"destroy", c"", &NULLS),
        Message::new(c"enable", c"", &NULLS),
        Message::new(c"disable", c"", &NULLS),
        Message::new(c"set_surrounding_text", c"sii", &NULLS),
        Message::new(c"set_text_change_cause", c"u", &NULLS),
        Message::new(c"set_content_type", c"uu", &NULLS),
        Message::new(c"set_cursor_rectangle", c"iiii", &NULLS),
        Message::new(c"commit", c"", &NULLS),
    ];
    static EVENTS_ENTER_TYPES: [TypeRef; 1] = [
        TypeRef::to(&crate::protocols::wayland::wl_surface::INTERFACE),
    ];
    static EVENTS_LEAVE_TYPES: [TypeRef; 1] = [
        TypeRef::to(&crate::protocols::wayland::wl_surface::INTERFACE),
    ];
    static EVENTS: [Message; 6] = [
        Message::new(c"enter", c"o", &EVENTS_ENTER_TYPES),
        Message::new(c"leave", c"o", &EVENTS_LEAVE_TYPES),
        Message::new(c"preedit_string", c"?sii", &NULLS),
        Message::new(c"commit_string", c"?s", &NULLS),
        Message::new(c"delete_surrounding_text", c"uu", &NULLS),
        Message::new(c"done", c"u", &NULLS),
    ];
    /// Request opcodes.
    pub mod req {
        pub const DESTROY: u32 = 0;
        pub const ENABLE: u32 = 1;
        pub const DISABLE: u32 = 2;
        pub const SET_SURROUNDING_TEXT: u32 = 3;
        pub const SET_TEXT_CHANGE_CAUSE: u32 = 4;
        pub const SET_CONTENT_TYPE: u32 = 5;
        pub const SET_CURSOR_RECTANGLE: u32 = 6;
        pub const COMMIT: u32 = 7;
    }
    /// Event opcodes.
    pub mod evt {
        pub const ENTER: u32 = 0;
        pub const LEAVE: u32 = 1;
        pub const PREEDIT_STRING: u32 = 2;
        pub const COMMIT_STRING: u32 = 3;
        pub const DELETE_SURROUNDING_TEXT: u32 = 4;
        pub const DONE: u32 = 5;
    }
    /// The `change_cause` enum.
    pub mod change_cause {
        /// input method caused the change
        pub const INPUT_METHOD: u32 = 0x0;
        /// something else than the input method caused the change
        pub const OTHER: u32 = 0x1;
    }
    /// The `content_hint` enum.
    pub mod content_hint {
        /// no special behavior
        pub const NONE: u32 = 0x0;
        /// suggest word completions
        pub const COMPLETION: u32 = 0x1;
        /// suggest word corrections
        pub const SPELLCHECK: u32 = 0x2;
        /// switch to uppercase letters at the start of a sentence
        pub const AUTO_CAPITALIZATION: u32 = 0x4;
        /// prefer lowercase letters
        pub const LOWERCASE: u32 = 0x8;
        /// prefer uppercase letters
        pub const UPPERCASE: u32 = 0x10;
        /// prefer casing for titles and headings (can be language dependent)
        pub const TITLECASE: u32 = 0x20;
        /// characters should be hidden
        pub const HIDDEN_TEXT: u32 = 0x40;
        /// typed text should not be stored
        pub const SENSITIVE_DATA: u32 = 0x80;
        /// just Latin characters should be entered
        pub const LATIN: u32 = 0x100;
        /// the text input is multiline
        pub const MULTILINE: u32 = 0x200;
    }
    /// The `content_purpose` enum.
    pub mod content_purpose {
        /// default input, allowing all characters
        pub const NORMAL: u32 = 0x0;
        /// allow only alphabetic characters
        pub const ALPHA: u32 = 0x1;
        /// allow only digits
        pub const DIGITS: u32 = 0x2;
        /// input a number (including decimal separator and sign)
        pub const NUMBER: u32 = 0x3;
        /// input a phone number
        pub const PHONE: u32 = 0x4;
        /// input an URL
        pub const URL: u32 = 0x5;
        /// input an email address
        pub const EMAIL: u32 = 0x6;
        /// input a name of a person
        pub const NAME: u32 = 0x7;
        /// input a password (combine with sensitive_data hint)
        pub const PASSWORD: u32 = 0x8;
        /// input is a numeric password (combine with sensitive_data hint)
        pub const PIN: u32 = 0x9;
        /// input a date
        pub const DATE: u32 = 0xa;
        /// input a time
        pub const TIME: u32 = 0xb;
        /// input a date and time
        pub const DATETIME: u32 = 0xc;
        /// input for a terminal
        pub const TERMINAL: u32 = 0xd;
    }
    /// Destroy the wp_text_input
    ///
    /// # Safety
    /// `this` is a live `zwp_text_input_v3`, destroyed by this request and not used again; every object argument is live.
    pub unsafe fn destroy(this: Proxy) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(0, &mut [], None, true) };
    }
    /// Request text input to be enabled
    ///
    /// # Safety
    /// `this` is a live `zwp_text_input_v3`; every object argument is live.
    pub unsafe fn enable(this: Proxy) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(1, &mut [], None, false) };
    }
    /// Disable text input on a surface
    ///
    /// # Safety
    /// `this` is a live `zwp_text_input_v3`; every object argument is live.
    pub unsafe fn disable(this: Proxy) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(2, &mut [], None, false) };
    }
    /// sets the surrounding text
    ///
    /// # Safety
    /// `this` is a live `zwp_text_input_v3`; every object argument is live.
    pub unsafe fn set_surrounding_text(this: Proxy, text: &CStr, cursor: i32, anchor: i32) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(3, &mut [Arg::string(Some(text)), Arg::int(cursor), Arg::int(anchor)], None, false) };
    }
    /// indicates the cause of surrounding text change
    ///
    /// # Safety
    /// `this` is a live `zwp_text_input_v3`; every object argument is live.
    pub unsafe fn set_text_change_cause(this: Proxy, cause: u32) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(4, &mut [Arg::uint(cause)], None, false) };
    }
    /// set content purpose and hint
    ///
    /// # Safety
    /// `this` is a live `zwp_text_input_v3`; every object argument is live.
    pub unsafe fn set_content_type(this: Proxy, hint: u32, purpose: u32) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(5, &mut [Arg::uint(hint), Arg::uint(purpose)], None, false) };
    }
    /// set cursor position
    ///
    /// # Safety
    /// `this` is a live `zwp_text_input_v3`; every object argument is live.
    pub unsafe fn set_cursor_rectangle(this: Proxy, x: i32, y: i32, width: i32, height: i32) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(6, &mut [Arg::int(x), Arg::int(y), Arg::int(width), Arg::int(height)], None, false) };
    }
    /// commit state
    ///
    /// # Safety
    /// `this` is a live `zwp_text_input_v3`; every object argument is live.
    pub unsafe fn commit(this: Proxy) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(7, &mut [], None, false) };
    }
}

/// text input manager
pub mod zwp_text_input_manager_v3 {
    #[allow(unused_imports)]
    use crate::iface::{Interface, Message, NULLS, TypeRef};
    #[allow(unused_imports)]
    use crate::proxy::{Arg, Fixed, Proxy, borrowed_array};
    #[allow(unused_imports)]
    use std::ffi::{CStr, c_int};

    /// The highest version this table describes.
    pub const VERSION: u32 = 1;
    /// The interface table.
    pub static INTERFACE: Interface = Interface::new(c"zwp_text_input_manager_v3", 1, &REQUESTS, &EVENTS);
    static REQUESTS_GET_TEXT_INPUT_TYPES: [TypeRef; 2] = [
        TypeRef::to(&crate::protocols::text_input_unstable_v3::zwp_text_input_v3::INTERFACE),
        TypeRef::to(&crate::protocols::wayland::wl_seat::INTERFACE),
    ];
    static REQUESTS: [Message; 2] = [
        Message::new(c"destroy", c"", &NULLS),
        Message::new(c"get_text_input", c"no", &REQUESTS_GET_TEXT_INPUT_TYPES),
    ];
    static EVENTS: [Message; 0] = [
    ];
    /// Request opcodes.
    pub mod req {
        pub const DESTROY: u32 = 0;
        pub const GET_TEXT_INPUT: u32 = 1;
    }
    /// Event opcodes.
    pub mod evt {
    }
    /// Destroy the wp_text_input_manager
    ///
    /// # Safety
    /// `this` is a live `zwp_text_input_manager_v3`, destroyed by this request and not used again; every object argument is live.
    pub unsafe fn destroy(this: Proxy) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(0, &mut [], None, true) };
    }
    /// create a new text input object
    ///
    /// # Safety
    /// `this` is a live `zwp_text_input_manager_v3`; every object argument is live.
    pub unsafe fn get_text_input(this: Proxy, seat: Proxy) -> Option<Proxy> {
        // SAFETY: the caller's contract; the arguments follow the signature.
        unsafe { this.marshal(1, &mut [Arg::new_id(), Arg::object(Some(seat))], Some((&crate::protocols::text_input_unstable_v3::zwp_text_input_v3::INTERFACE, this.version())), false) }
    }
}
}
