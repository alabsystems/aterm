// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Live X11 server transport, property, image, MIT-SHM, and selection tests.

#![cfg(target_os = "linux")]

use std::os::fd::{FromRawFd, OwnedFd};

use aterm_x11::proto::event::{Event, SelectionNotify};
use aterm_x11::proto::xproto::{self, PropMode, cw, event_mask, image_format};
use aterm_x11::proto::{ValueList, atom, shm};
use aterm_x11::{Connection, Error};

fn connect() -> Option<(Connection, usize)> {
    if std::env::var_os("DISPLAY").is_none() {
        eprintln!("SKIP: no DISPLAY");
        return None;
    }
    Some(Connection::connect(None).expect("DISPLAY is set: the connection must open"))
}

/// An unmapped 16x8 window on `screen`'s root, with PropertyChange events.
fn window(conn: &Connection, screen: usize) -> u32 {
    let s = &conn.setup().roots[screen];
    let wid = conn.generate_id().unwrap();
    conn.send_void(xproto::create_window(
        s.root_depth,
        wid,
        s.root,
        0,
        0,
        16,
        8,
        0,
        xproto::window_class::INPUT_OUTPUT,
        s.root_visual,
        &ValueList::new()
            .set(cw::OVERRIDE_REDIRECT, 1)
            .set(cw::EVENT_MASK, event_mask::PROPERTY_CHANGE),
    ))
    .unwrap()
    .check()
    .expect("CreateWindow");
    wid
}

#[test]
fn a_server_answers_and_its_replies_parse() {
    let Some((conn, screen)) = connect() else {
        return;
    };
    let setup = conn.setup();
    assert_eq!(setup.protocol_major_version, 11);
    let s = &setup.roots[screen];
    assert!(s.width_in_pixels > 0 && s.height_in_pixels > 0);
    assert!(
        s.allowed_depths
            .iter()
            .any(|d| d.visuals.iter().any(|v| v.visual_id == s.root_visual)),
        "the root visual is among the screen's visuals"
    );
    let geometry = conn
        .send(xproto::get_geometry(s.root))
        .unwrap()
        .reply()
        .unwrap();
    assert_eq!(
        (geometry.width, geometry.height, geometry.depth),
        (s.width_in_pixels, s.height_in_pixels, s.root_depth)
    );
    let names: [&[u8]; 3] = [b"ATERM_X11_LIVE", b"UTF8_STRING", b"CLIPBOARD"];
    let atoms = conn.intern_atoms(&names).unwrap();
    for (name, atom) in names.iter().zip(&atoms) {
        let back = conn
            .send(xproto::get_atom_name(*atom))
            .unwrap()
            .reply()
            .unwrap();
        assert_eq!(&back.name[..], *name);
    }
    let absent = conn
        .send(xproto::intern_atom(true, b"ATERM_X11_NEVER_INTERNED_8f3a"))
        .unwrap()
        .reply()
        .unwrap();
    assert_eq!(absent.atom, 0, "only_if_exists answers None");
}

#[test]
fn a_property_makes_the_round_trip() {
    let Some((conn, screen)) = connect() else {
        return;
    };
    let win = window(&conn, screen);
    let [prop, utf8] = conn
        .intern_atoms(&[b"ATERM_X11_PROP", b"UTF8_STRING"])
        .unwrap()[..]
    else {
        unreachable!()
    };
    let text = "aterm ✓ 你好 😀".as_bytes();
    conn.send_void(xproto::change_property(
        PropMode::Replace,
        win,
        prop,
        utf8,
        8,
        text,
    ))
    .unwrap()
    .check()
    .unwrap();
    let got = conn
        .send(xproto::get_property(
            false,
            win,
            prop,
            atom::ANY,
            0,
            u32::MAX,
        ))
        .unwrap()
        .reply()
        .unwrap();
    assert_eq!((got.format, got.type_, &got.value[..]), (8, utf8, text));
    let list = [atom::STRING, utf8, prop];
    conn.send_void(xproto::change_property32(
        PropMode::Replace,
        win,
        prop,
        atom::ATOM,
        &list,
    ))
    .unwrap()
    .check()
    .unwrap();
    let got = conn
        .send(xproto::get_property(true, win, prop, atom::ATOM, 0, 2))
        .unwrap()
        .reply()
        .unwrap();
    assert_eq!(got.value32().as_deref(), Some(&list[..2]));
    assert_eq!(got.bytes_after, 4, "one item left unread");
    conn.send_void(xproto::destroy_window(win))
        .unwrap()
        .check()
        .unwrap();
}

#[test]
fn an_error_comes_back_to_its_request() {
    let Some((conn, _)) = connect() else {
        return;
    };
    let bogus = conn.setup().resource_id_base | 0x1f_fff0;
    match conn.send(xproto::get_geometry(bogus)).unwrap().reply() {
        Err(Error::X11(e)) => {
            assert_eq!((e.error_code, e.major_opcode, e.bad_value), (9, 14, bogus));
        }
        other => panic!("expected BadDrawable, got {other:?}"),
    }
    match conn.send_void(xproto::map_window(bogus)).unwrap().check() {
        Err(Error::X11(e)) => assert_eq!((e.error_code, e.major_opcode), (3, 8)),
        other => panic!("expected BadWindow, got {other:?}"),
    }
    conn.send(xproto::get_input_focus())
        .unwrap()
        .reply()
        .unwrap();
}

#[test]
fn an_image_and_an_shm_segment_draw() {
    let Some((conn, screen)) = connect() else {
        return;
    };
    let s = conn.setup().roots[screen].clone();
    if s.root_depth != 24 {
        eprintln!("SKIP: root depth {} is not 24", s.root_depth);
        return;
    }
    let win = window(&conn, screen);
    conn.send_void(xproto::map_window(win))
        .unwrap()
        .check()
        .unwrap();
    let gc = conn.generate_id().unwrap();
    conn.send_void(xproto::create_gc(gc, win, &ValueList::new()))
        .unwrap()
        .check()
        .unwrap();
    let pixels: Vec<u32> = (0..16 * 8).map(|i| 0x0010_2030 + i * 0x0001_0101).collect();
    let bytes: Vec<u8> = pixels.iter().flat_map(|p| p.to_ne_bytes()).collect();
    conn.send_void(xproto::put_image(
        image_format::Z_PIXMAP,
        win,
        gc,
        16,
        8,
        0,
        0,
        0,
        24,
        &bytes,
    ))
    .unwrap()
    .check()
    .unwrap();
    let read = |conn: &Connection| {
        conn.send(xproto::get_image(
            image_format::Z_PIXMAP,
            win,
            0,
            0,
            16,
            8,
            u32::MAX,
        ))
        .unwrap()
        .reply()
        .unwrap()
        .data
        .as_chunks::<4>()
        .0
        .iter()
        .map(|c| u32::from_ne_bytes(*c) & 0x00ff_ffff)
        .collect::<Vec<_>>()
    };
    assert_eq!(read(&conn), pixels, "PutImage then GetImage");

    let Some(ext) = conn.extension(shm::NAME).unwrap() else {
        eprintln!("SKIP: no MIT-SHM");
        return;
    };
    let version = conn
        .send(shm::query_version(ext.major_opcode))
        .unwrap()
        .reply()
        .unwrap();
    assert!(
        (version.major_version, version.minor_version) >= (1, 2),
        "AttachFd needs 1.2"
    );
    let len = 16 * 8 * 4;
    // SAFETY: a fresh memfd, sized and mapped for this test alone.
    let (fd, map) = unsafe {
        let raw = libc::memfd_create(c"aterm-x11-live".as_ptr(), libc::MFD_CLOEXEC);
        assert!(raw >= 0);
        assert_eq!(libc::ftruncate(raw, len as libc::off_t), 0);
        let map = libc::mmap(
            std::ptr::null_mut(),
            len,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_SHARED,
            raw,
            0,
        );
        assert_ne!(map, libc::MAP_FAILED);
        (OwnedFd::from_raw_fd(raw), map.cast::<u32>())
    };
    let inverted: Vec<u32> = pixels.iter().map(|p| !p & 0x00ff_ffff).collect();
    // SAFETY: `len` bytes mapped above, not yet shared with the server.
    unsafe { std::slice::from_raw_parts_mut(map, 16 * 8) }.copy_from_slice(&inverted);
    let seg = conn.generate_id().unwrap();
    conn.send_void_with_fds(shm::attach_fd(ext.major_opcode, seg, true), vec![fd])
        .unwrap()
        .check()
        .expect("AttachFd");
    conn.send_void(shm::put_image(
        ext.major_opcode,
        win,
        gc,
        16,
        8,
        0,
        0,
        16,
        8,
        0,
        0,
        24,
        image_format::Z_PIXMAP,
        false,
        seg,
        0,
    ))
    .unwrap()
    .check()
    .expect("ShmPutImage");
    assert_eq!(read(&conn), inverted, "ShmPutImage then GetImage");
    conn.send_void(shm::detach(ext.major_opcode, seg))
        .unwrap()
        .check()
        .unwrap();
    // SAFETY: the mapping made above, unmapped once.
    unsafe { libc::munmap(map.cast(), len) };
    conn.send_void(xproto::free_gc(gc))
        .unwrap()
        .check()
        .unwrap();
    conn.send_void(xproto::destroy_window(win))
        .unwrap()
        .check()
        .unwrap();
}

#[test]
fn a_big_request_takes_the_big_form() {
    let Some((conn, screen)) = connect() else {
        return;
    };
    let s = conn.setup().roots[screen].clone();
    if s.root_depth != 24 || conn.maximum_request_bytes() <= 4 * 65535 + 64 {
        eprintln!("SKIP: no BIG-REQUESTS or not depth 24");
        return;
    }
    let win = window(&conn, screen);
    let gc = conn.generate_id().unwrap();
    conn.send_void(xproto::create_gc(gc, win, &ValueList::new()))
        .unwrap()
        .check()
        .unwrap();
    let data = vec![0x5au8; 300 * 300 * 4];
    let pixmap = conn.generate_id().unwrap();
    conn.send_void(xproto::create_pixmap(s.root_depth, pixmap, win, 300, 300))
        .unwrap()
        .check()
        .unwrap();
    conn.send_void(xproto::put_image(
        image_format::Z_PIXMAP,
        pixmap,
        gc,
        300,
        300,
        0,
        0,
        0,
        24,
        &data,
    ))
    .unwrap()
    .check()
    .expect("a BIG-REQUESTS PutImage");
    conn.send_void(xproto::free_pixmap(pixmap))
        .unwrap()
        .check()
        .unwrap();
    conn.send_void(xproto::free_gc(gc))
        .unwrap()
        .check()
        .unwrap();
    conn.send_void(xproto::destroy_window(win))
        .unwrap()
        .check()
        .unwrap();
}

#[test]
fn a_selection_moves_between_two_connections() {
    let Some((owner, screen)) = connect() else {
        return;
    };
    let (asker, _) = Connection::connect(None).unwrap();
    let owner_win = window(&owner, screen);
    let asker_win = window(&asker, screen);
    let name = format!("ATERM_X11_LIVE_SEL_{}", std::process::id());
    let [sel, target, prop] = owner
        .intern_atoms(&[name.as_bytes(), b"UTF8_STRING", b"ATERM_X11_RECV"])
        .unwrap()[..]
    else {
        unreachable!()
    };
    owner
        .send_void(xproto::set_selection_owner(owner_win, sel, 0))
        .unwrap()
        .check()
        .unwrap();
    let who = asker
        .send(xproto::get_selection_owner(sel))
        .unwrap()
        .reply()
        .unwrap();
    assert_eq!(who.owner, owner_win);
    asker
        .send_void(xproto::convert_selection(asker_win, sel, target, prop, 0))
        .unwrap()
        .check()
        .unwrap();
    let request = loop {
        let packet = owner.wait_for_event().unwrap();
        if let Event::SelectionRequest(r) = Event::parse(&packet).unwrap() {
            break r;
        }
    };
    assert_eq!(
        (
            request.requestor,
            request.selection,
            request.target,
            request.property
        ),
        (asker_win, sel, target, prop)
    );
    owner
        .send_void(xproto::change_property(
            PropMode::Replace,
            request.requestor,
            request.property,
            target,
            8,
            b"moved",
        ))
        .unwrap()
        .check()
        .unwrap();
    let notify = SelectionNotify {
        sequence: 0,
        time: request.time,
        requestor: request.requestor,
        selection: request.selection,
        target: request.target,
        property: request.property,
    };
    owner
        .send_void(xproto::send_event(
            false,
            request.requestor,
            event_mask::NO_EVENT,
            &notify.to_bytes(),
        ))
        .unwrap()
        .check()
        .unwrap();
    let got = loop {
        let packet = asker.wait_for_event().unwrap();
        if let Event::SelectionNotify(n) = Event::parse(&packet).unwrap() {
            break n;
        }
    };
    assert_eq!(got.property, prop);
    let value = asker
        .send(xproto::get_property(
            true,
            asker_win,
            prop,
            atom::ANY,
            0,
            u32::MAX,
        ))
        .unwrap()
        .reply()
        .unwrap();
    assert_eq!(&value.value[..], b"moved");
}

#[test]
fn randr_and_xinput_answer() {
    use aterm_x11::proto::{randr, xinput};
    let Some((conn, screen)) = connect() else {
        return;
    };
    let root = conn.setup().roots[screen].root;
    let Some(rr) = conn.extension(randr::NAME).unwrap() else {
        eprintln!("SKIP: no RANDR");
        return;
    };
    let v = conn
        .send(randr::query_version(rr.major_opcode, 1, 5))
        .unwrap()
        .reply()
        .unwrap();
    assert!((v.major_version, v.minor_version) >= (1, 2), "{v:?}");
    conn.send_void(randr::select_input(
        rr.major_opcode,
        root,
        randr::notify_mask::SCREEN_CHANGE,
    ))
    .unwrap()
    .check()
    .unwrap();
    let res = conn
        .send(randr::get_screen_resources_current(rr.major_opcode, root))
        .unwrap()
        .reply()
        .unwrap();
    let names: usize = res.modes.iter().map(|m| usize::from(m.name_len)).sum();
    assert_eq!(names, res.names.len(), "the mode names fill the name bytes");
    for &crtc in &res.crtcs {
        let info = conn
            .send(randr::get_crtc_info(
                rr.major_opcode,
                crtc,
                res.config_timestamp,
            ))
            .unwrap()
            .reply()
            .unwrap();
        assert!(info.mode == 0 || res.modes.iter().any(|m| m.id == info.mode));
    }
    for &output in &res.outputs {
        let info = conn
            .send(randr::get_output_info(
                rr.major_opcode,
                output,
                res.config_timestamp,
            ))
            .unwrap()
            .reply()
            .unwrap();
        assert!(!info.name.is_empty());
    }
    conn.send(randr::get_output_primary(rr.major_opcode, root))
        .unwrap()
        .reply()
        .unwrap();
    let Some(xi) = conn.extension(xinput::NAME).unwrap() else {
        eprintln!("SKIP: no XInputExtension");
        return;
    };
    let v = conn
        .send(xinput::xi_query_version(xi.major_opcode, 2, 3))
        .unwrap()
        .reply()
        .unwrap();
    assert!(v.major_version >= 2, "{v:?}");
    let p = conn
        .send(xinput::xi_query_pointer(xi.major_opcode, root, 2))
        .unwrap()
        .reply()
        .unwrap();
    assert_eq!(p.root, root);
    conn.send_void(xinput::xi_select_events(
        xi.major_opcode,
        root,
        &[xinput::EventMask {
            deviceid: xinput::ALL_DEVICES,
            mask: vec![xinput::event_mask::HIERARCHY],
        }],
    ))
    .unwrap()
    .check()
    .unwrap();
}

#[test]
fn xlib_xi_and_xcursor_load_and_answer() {
    use aterm_x11::xlib;
    if std::env::var_os("DISPLAY").is_none() {
        eprintln!("SKIP: no DISPLAY");
        return;
    }
    let x = xlib::xlib().expect("libX11 on a machine with a display");
    let xi = xlib::xi().expect("libXi");
    let cursors = xlib::xcursor().expect("libXcursor");
    // SAFETY: a display opened and closed here; every call takes it live.
    unsafe {
        let display = (x.XOpenDisplay)(std::ptr::null());
        assert!(!display.is_null());
        let screen = (x.XDefaultScreen)(display);
        let root = (x.XRootWindow)(display, screen);
        assert_eq!(root, (x.XDefaultRootWindow)(display));
        let conn =
            aterm_x11::Connection::from_xlib_display(std::ptr::NonNull::new(display).unwrap())
                .unwrap();
        let s = usize::try_from(screen).unwrap();
        assert_eq!(u64::from(conn.setup().roots[s].root), root);
        drop(conn);
        let mut n = 0;
        let devices = (xi.XIQueryDevice)(display, xlib::xi::ALL_DEVICES, &mut n);
        assert!(
            !devices.is_null() && n >= 2,
            "a master pointer and keyboard at least"
        );
        let all = std::slice::from_raw_parts(devices, usize::try_from(n).unwrap());
        assert!(all.iter().any(|d| d.use_ == xlib::xi::MASTER_POINTER));
        assert!(all.iter().any(|d| d.use_ == xlib::xi::MASTER_KEYBOARD));
        (xi.XIFreeDeviceInfo)(devices);
        let image = (cursors.XcursorImageCreate)(2, 2);
        assert!(!image.is_null());
        assert_eq!(((*image).width, (*image).height), (2, 2));
        (*image).xhot = 0;
        (*image).yhot = 0;
        std::slice::from_raw_parts_mut((*image).pixels, 4).fill(0xff00_0000);
        let cursor = (cursors.XcursorImageLoadCursor)(display, image);
        assert_ne!(cursor, 0, "a cursor from pixels");
        (x.XFreeCursor)(display, cursor);
        (cursors.XcursorImageDestroy)(image);
        let mut keys = [0 as std::ffi::c_char; 32];
        (x.XQueryKeymap)(display, keys.as_mut_ptr());
        let map = (x.XGetModifierMapping)(display);
        assert!(!map.is_null() && (*map).max_keypermod > 0);
        (x.XFreeModifiermap)(map);
        let mut state = xlib::XkbStateRec::default();
        assert_eq!(
            (x.XkbGetState)(display, xlib::XKB_USE_CORE_KBD, &mut state),
            0
        );
        (x.XSync)(display, xlib::FALSE);
        (x.XCloseDisplay)(display);
    }
}
