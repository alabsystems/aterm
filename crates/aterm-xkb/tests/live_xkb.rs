// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Declarations checked against the system's `libxkbcommon`.
//!
//! STOCK EXCEPTION: Linux only, and Trust has no Linux std, so only stock Rust builds this (the
//! Linux lane's guest pins its version: `aterm-lab/src/linux_guest.rs`).

#![cfg(target_os = "linux")]

use std::ffi::CString;

use aterm_xkb::keysym::{self, NAMES};
use aterm_xkb::{
    COMPOSE_NO_FLAGS, KEYMAP_COMPILE_NO_FLAGS, RuleNames, compose_feed, compose_status,
    context_flags, key_direction, keysym_flags, keysym_utf8, mod_name, state_component,
};

const KEY_A: u32 = 38;

#[test]
fn every_keysym_is_libxkbcommons() {
    let Ok(xkb) = aterm_xkb::xkb() else {
        eprintln!("SKIP: no libxkbcommon");
        return;
    };
    let mut wrong = Vec::new();
    for (rust, c_name, value) in NAMES {
        let c = CString::new(*c_name).unwrap();
        // SAFETY: a NUL-terminated name.
        let got = unsafe { (xkb.keysym_from_name)(c.as_ptr(), keysym_flags::NO_FLAGS) };
        if got != *value {
            wrong.push(format!(
                "{rust} ({c_name}): ours {value:#x}, libxkbcommon {got:#x}"
            ));
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    // SAFETY: a NUL-terminated name.
    let none = unsafe { (xkb.keysym_from_name)(c"aterm_no_such_keysym".as_ptr(), 0) };
    assert_eq!(none, aterm_xkb::NO_SYMBOL);
    assert_eq!(keysym_utf8(xkb, keysym::space).as_deref(), Some(" "));
    assert_eq!(
        keysym_utf8(xkb, keysym::F1),
        None,
        "a function key has no text"
    );
}

#[test]
fn a_us_keymap_types_and_shifts() {
    let Ok(xkb) = aterm_xkb::xkb() else {
        eprintln!("SKIP: no libxkbcommon");
        return;
    };
    // SAFETY: each object made here is unreffed once, below.
    unsafe {
        let ctx = (xkb.context_new)(context_flags::NO_ENVIRONMENT_NAMES);
        assert!(!ctx.is_null());
        let names = RuleNames {
            rules: c"evdev".as_ptr(),
            model: c"pc105".as_ptr(),
            layout: c"us".as_ptr(),
            variant: c"".as_ptr(),
            options: c"".as_ptr(),
        };
        let keymap = (xkb.keymap_new_from_names)(ctx, &names, KEYMAP_COMPILE_NO_FLAGS);
        if keymap.is_null() {
            (xkb.context_unref)(ctx);
            eprintln!("SKIP: no xkeyboard-config data for evdev/us");
            return;
        }
        assert_eq!((xkb.keymap_num_layouts)(keymap), 1);
        assert_eq!((xkb.keymap_key_repeats)(keymap, KEY_A), 1);
        let state = (xkb.state_new)(keymap);
        assert_eq!((xkb.state_key_get_one_sym)(state, KEY_A), 0x61);
        let mut buf = [0u8; 8];
        let n = (xkb.state_key_get_utf8)(state, KEY_A, buf.as_mut_ptr().cast(), buf.len());
        assert_eq!((n, &buf[..2]), (1, &b"a\0"[..]));
        let shift = (xkb.keymap_mod_get_index)(keymap, mod_name::SHIFT.as_ptr());
        assert_ne!(shift, aterm_xkb::INVALID);
        (xkb.state_update_mask)(state, 1 << shift, 0, 0, 0, 0, 0);
        assert_eq!((xkb.state_key_get_utf32)(state, KEY_A), u32::from('A'));
        assert_eq!(
            (xkb.state_mod_name_is_active)(
                state,
                mod_name::SHIFT.as_ptr(),
                state_component::MODS_EFFECTIVE
            ),
            1
        );
        assert_eq!(
            (xkb.state_serialize_mods)(state, state_component::MODS_DEPRESSED),
            1 << shift
        );
        (xkb.state_update_mask)(state, 0, 0, 0, 0, 0, 0);
        (xkb.state_update_key)(state, 50, key_direction::DOWN); // Shift_L
        assert_eq!((xkb.state_key_get_one_sym)(state, KEY_A), 0x41);
        let mut syms: *const u32 = std::ptr::null();
        let count = (xkb.keymap_key_get_syms_by_level)(keymap, KEY_A, 0, 1, &mut syms);
        assert_eq!((count, *syms), (1, 0x41), "level 1 of the a key is A");
        (xkb.state_unref)(state);
        (xkb.keymap_unref)(keymap);
        (xkb.context_unref)(ctx);
    }
}

#[test]
fn dead_acute_then_e_composes() {
    let (Ok(xkb), Ok(c)) = (aterm_xkb::xkb(), aterm_xkb::compose()) else {
        eprintln!("SKIP: no libxkbcommon");
        return;
    };
    // SAFETY: each object made here is unreffed once, below.
    unsafe {
        let ctx = (xkb.context_new)(context_flags::NO_FLAGS);
        let table = (c.table_new_from_locale)(ctx, c"en_US.UTF-8".as_ptr(), COMPOSE_NO_FLAGS);
        if table.is_null() {
            (xkb.context_unref)(ctx);
            eprintln!("SKIP: no compose table for en_US.UTF-8");
            return;
        }
        let state = (c.state_new)(table, COMPOSE_NO_FLAGS);
        assert_eq!((c.state_feed)(state, 0xfe51), compose_feed::ACCEPTED); // dead_acute
        assert_eq!((c.state_get_status)(state), compose_status::COMPOSING);
        (c.state_feed)(state, 0x65); // e
        assert_eq!((c.state_get_status)(state), compose_status::COMPOSED);
        let mut buf = [0u8; 8];
        let n = (c.state_get_utf8)(state, buf.as_mut_ptr().cast(), buf.len());
        assert_eq!(&buf[..usize::try_from(n).unwrap()], "é".as_bytes());
        assert_eq!((c.state_get_one_sym)(state), 0xe9);
        (c.state_reset)(state);
        assert_eq!((c.state_get_status)(state), compose_status::NOTHING);
        (c.state_unref)(state);
        (c.table_unref)(table);
        (xkb.context_unref)(ctx);
    }
}

#[test]
fn an_x_servers_keymap_and_state() {
    if std::env::var_os("DISPLAY").is_none() {
        eprintln!("SKIP: no DISPLAY");
        return;
    }
    let (Ok(xkb), Ok(x11)) = (aterm_xkb::xkb(), aterm_xkb::x11()) else {
        eprintln!("SKIP: no libxkbcommon-x11");
        return;
    };
    let (conn, _) = aterm_x11::Connection::connect(None).expect("DISPLAY is set");
    let raw = conn.as_raw().as_ptr();
    // SAFETY: a live connection; each object made here is unreffed once.
    unsafe {
        let (mut major, mut minor, mut event, mut error) = (0u16, 0u16, 0u8, 0u8);
        let ok = (x11.setup_xkb_extension)(
            raw,
            aterm_xkb::X11_MIN_XKB_VERSION.0,
            aterm_xkb::X11_MIN_XKB_VERSION.1,
            aterm_xkb::X11_SETUP_NO_FLAGS,
            &mut major,
            &mut minor,
            &mut event,
            &mut error,
        );
        assert_eq!(ok, 1, "XKB set up (server {major}.{minor})");
        let device = (x11.get_core_keyboard_device_id)(raw);
        assert!(device >= 0);
        let ctx = (xkb.context_new)(context_flags::NO_FLAGS);
        let keymap = (x11.keymap_new_from_device)(ctx, raw, device, KEYMAP_COMPILE_NO_FLAGS);
        assert!(!keymap.is_null());
        let state = (x11.state_new_from_device)(keymap, raw, device);
        assert!(!state.is_null());
        assert_ne!(
            (xkb.state_key_get_one_sym)(state, KEY_A),
            aterm_xkb::NO_SYMBOL
        );
        (xkb.state_unref)(state);
        (xkb.keymap_unref)(keymap);
        (xkb.context_unref)(ctx);
    }
}
