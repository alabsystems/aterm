// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Request encoding conformance tests against `x11rb-protocol` recordings.

#![cfg(target_endian = "little")]

use std::collections::BTreeMap;

use crate::support::*;
use aterm_x11::proto::event::SelectionNotify;
use aterm_x11::proto::xproto::{self, PropMode, config, cw, event_mask, gc, image_format};
use aterm_x11::proto::{Request, ValueList, randr, shm, sync, xinput};

const RECORDING: &str = "x11rb-protocol-0.13.2-requests.tsv";

fn hx<R>(r: Request<R>) -> String {
    hex(r.bytes())
}

fn cases() -> BTreeMap<String, String> {
    let mut c = BTreeMap::new();
    let mut put = |name: &str, hex: String| {
        assert!(c.insert(name.to_owned(), hex).is_none(), "{name} twice");
    };
    let window = ValueList::new()
        .set(cw::BACK_PIXEL, 0x00ff_8800)
        .set(cw::OVERRIDE_REDIRECT, 1)
        .set(
            cw::EVENT_MASK,
            event_mask::PROPERTY_CHANGE | event_mask::STRUCTURE_NOTIFY,
        )
        .set(cw::COLORMAP, 0x20);
    put(
        "create_window",
        hx(xproto::create_window(
            24,
            0x0060_0001,
            0x123,
            -5,
            7,
            640,
            480,
            0,
            xproto::window_class::INPUT_OUTPUT,
            0x21,
            &window,
        )),
    );
    put(
        "create_window_clipboard",
        hx(xproto::create_window(
            0,
            0x0040_0002,
            0x4e3,
            0,
            0,
            1,
            1,
            0,
            xproto::window_class::INPUT_OUTPUT,
            0x21,
            &ValueList::new().set(cw::EVENT_MASK, event_mask::PROPERTY_CHANGE),
        )),
    );
    put(
        "change_window_attributes",
        hx(xproto::change_window_attributes(
            0x0060_0001,
            &ValueList::new()
                .set(
                    cw::EVENT_MASK,
                    event_mask::FOCUS_CHANGE | event_mask::KEY_PRESS,
                )
                .set(cw::CURSOR, 0x0060_0009),
        )),
    );
    put(
        "get_window_attributes",
        hx(xproto::get_window_attributes(0x0060_0001)),
    );
    put("destroy_window", hx(xproto::destroy_window(0x0060_0001)));
    put(
        "reparent_window",
        hx(xproto::reparent_window(0x0060_0001, 0x123, -5, 7)),
    );
    put("map_window", hx(xproto::map_window(0x0060_0001)));
    put("unmap_window", hx(xproto::unmap_window(0x0060_0001)));
    put(
        "configure_window",
        hx(xproto::configure_window(
            0x0060_0001,
            &ValueList::new()
                .set(config::X, (-20i32).cast_unsigned())
                .set(config::Y, 30)
                .set(config::WIDTH, 800)
                .set(config::HEIGHT, 600)
                .set(config::STACK_MODE, 0),
        )),
    );
    put("get_geometry", hx(xproto::get_geometry(0x0060_0001)));
    put("query_tree", hx(xproto::query_tree(0x4e3)));
    put(
        "intern_atom_clipboard",
        hx(xproto::intern_atom(false, b"CLIPBOARD")),
    );
    put(
        "intern_atom_only_if_exists",
        hx(xproto::intern_atom(true, b"UTF8_STRING")),
    );
    put("intern_atom_empty", hx(xproto::intern_atom(false, b"")));
    put("intern_atom_word", hx(xproto::intern_atom(false, b"TEXT")));
    put("get_atom_name", hx(xproto::get_atom_name(0x1a2)));
    put(
        "change_property8",
        hx(xproto::change_property(
            PropMode::Replace,
            0x0040_0002,
            0x1b0,
            0x1a3,
            8,
            "héllo".as_bytes(),
        )),
    );
    put(
        "change_property32",
        hx(xproto::change_property32(
            PropMode::Replace,
            0x0050_0007,
            0x1b1,
            4,
            &[0x1a4, 0x1a3, 0x1a5, 31, 0x1a6],
        )),
    );
    put(
        "change_property_append_empty",
        hx(xproto::change_property(
            PropMode::Append,
            0x0050_0007,
            0x1b1,
            0x1a3,
            8,
            &[],
        )),
    );
    let card16: Vec<u8> = [1u16, 0xfffe, 300]
        .iter()
        .flat_map(|v| v.to_ne_bytes())
        .collect();
    put(
        "change_property16",
        hx(xproto::change_property(
            PropMode::Prepend,
            0x0050_0007,
            0x1b2,
            6,
            16,
            &card16,
        )),
    );
    put(
        "delete_property",
        hx(xproto::delete_property(0x0040_0002, 0x1b0)),
    );
    put(
        "get_property_paste",
        hx(xproto::get_property(
            true,
            0x0040_0002,
            0x1b0,
            0,
            0,
            u32::MAX,
        )),
    );
    put(
        "get_property_resource_manager",
        hx(xproto::get_property(false, 0x4e3, 23, 31, 0, 100_000_000)),
    );
    put(
        "set_selection_owner",
        hx(xproto::set_selection_owner(0x0040_0002, 0x1a1, 0)),
    );
    put("get_selection_owner", hx(xproto::get_selection_owner(1)));
    put(
        "convert_selection",
        hx(xproto::convert_selection(
            0x0040_0002,
            0x1a1,
            0x1a3,
            0x1b0,
            0,
        )),
    );
    let notify = SelectionNotify {
        sequence: 0,
        time: 123_456,
        requestor: 0x0050_0007,
        selection: 0x1a1,
        target: 0x1a3,
        property: 0x1b1,
    };
    put(
        "send_event_selection_notify",
        hx(xproto::send_event(
            false,
            0x0050_0007,
            event_mask::NO_EVENT,
            &notify.to_bytes(),
        )),
    );
    let grab_mask =
        event_mask::BUTTON_PRESS | event_mask::BUTTON_RELEASE | event_mask::POINTER_MOTION;
    put(
        "grab_pointer",
        hx(xproto::grab_pointer(
            true,
            0x0060_0001,
            u16::try_from(grab_mask).unwrap(),
            xproto::grab_mode::ASYNC,
            xproto::grab_mode::ASYNC,
            0x0060_0001,
            0,
            0,
        )),
    );
    put("ungrab_pointer", hx(xproto::ungrab_pointer(77)));
    put("query_pointer", hx(xproto::query_pointer(0x0060_0001)));
    put(
        "translate_coordinates",
        hx(xproto::translate_coordinates(0x0060_0001, 0x4e3, -3, 9)),
    );
    put(
        "warp_pointer",
        hx(xproto::warp_pointer(0, 0x0060_0001, 0, 0, 0, 0, 100, -2)),
    );
    put(
        "set_input_focus",
        hx(xproto::set_input_focus(
            xproto::input_focus::PARENT,
            0x0060_0001,
            0,
        )),
    );
    put("get_input_focus", hx(xproto::get_input_focus()));
    put("query_keymap", hx(xproto::query_keymap()));
    put(
        "create_gc_presenter",
        hx(xproto::create_gc(
            0x0060_0003,
            0x0060_0001,
            &ValueList::new().set(gc::GRAPHICS_EXPOSURES, 0),
        )),
    );
    put(
        "create_gc_colors",
        hx(xproto::create_gc(
            0x0060_0004,
            0x0060_0001,
            &ValueList::new()
                .set(gc::FUNCTION, 3)
                .set(gc::FOREGROUND, 0x00ff_ffff)
                .set(gc::BACKGROUND, 0)
                .set(gc::SUBWINDOW_MODE, 1),
        )),
    );
    put(
        "create_pixmap",
        hx(xproto::create_pixmap(
            24,
            0x0060_0007,
            0x0060_0001,
            300,
            200,
        )),
    );
    put("free_pixmap", hx(xproto::free_pixmap(0x0060_0007)));
    put("free_gc", hx(xproto::free_gc(0x0060_0003)));
    put(
        "create_colormap",
        hx(xproto::create_colormap(
            xproto::colormap_alloc::NONE,
            0x0060_000a,
            0x4e3,
            0x5e,
        )),
    );
    let confine = event_mask::BUTTON_PRESS
        | event_mask::BUTTON_RELEASE
        | event_mask::ENTER_WINDOW
        | event_mask::LEAVE_WINDOW
        | event_mask::POINTER_MOTION
        | event_mask::POINTER_MOTION_HINT
        | event_mask::BUTTON1_MOTION
        | event_mask::BUTTON2_MOTION
        | event_mask::BUTTON3_MOTION
        | event_mask::BUTTON4_MOTION
        | event_mask::BUTTON5_MOTION
        | event_mask::KEYMAP_STATE;
    put(
        "grab_pointer_confine",
        hx(xproto::grab_pointer(
            true,
            0x0060_0001,
            u16::try_from(confine).expect("a 16-bit pointer mask"),
            xproto::grab_mode::ASYNC,
            xproto::grab_mode::ASYNC,
            0x0060_0001,
            0,
            0,
        )),
    );
    let pixels: Vec<u8> = (0u8..16).collect();
    put(
        "put_image_2x2",
        hx(xproto::put_image(
            image_format::Z_PIXMAP,
            0x0060_0001,
            0x0060_0003,
            2,
            2,
            0,
            0,
            0,
            24,
            &pixels,
        )),
    );
    put(
        "put_image_odd",
        hx(xproto::put_image(
            image_format::Z_PIXMAP,
            0x0060_0001,
            0x0060_0003,
            5,
            1,
            -1,
            3,
            0,
            8,
            &[9, 8, 7, 6, 5],
        )),
    );
    let big: Vec<u8> = (0..262_144u32).map(|i| (i % 251) as u8).collect();
    let big = xproto::put_image(
        image_format::Z_PIXMAP,
        0x0060_0001,
        0x0060_0003,
        256,
        256,
        0,
        0,
        0,
        24,
        &big,
    );
    put(
        "put_image_big_head",
        format!("{}\t{}", hex(&big.bytes()[..24]), big.bytes().len()),
    );
    put(
        "get_image",
        hx(xproto::get_image(
            image_format::Z_PIXMAP,
            0x0060_0001,
            0,
            -4,
            64,
            32,
            u32::MAX,
        )),
    );
    put(
        "query_extension_shm",
        hx(xproto::query_extension(b"MIT-SHM")),
    );
    put(
        "query_extension_big_requests",
        hx(xproto::query_extension(b"BIG-REQUESTS")),
    );
    put("get_modifier_mapping", hx(xproto::get_modifier_mapping()));
    put("shm_query_version", hx(shm::query_version(130)));
    put(
        "shm_attach_fd_ro",
        hx(shm::attach_fd(130, 0x0060_0005, true)),
    );
    put(
        "shm_attach_fd_rw",
        hx(shm::attach_fd(131, 0x0060_0006, false)),
    );
    put("shm_detach", hx(shm::detach(130, 0x0060_0005)));
    put(
        "shm_put_image",
        hx(shm::put_image(
            130,
            0x0060_0001,
            0x0060_0003,
            1920,
            1080,
            8,
            16,
            300,
            20,
            8,
            -16,
            24,
            2,
            false,
            0x0060_0005,
            4096,
        )),
    );
    put("randr_query_version", hx(randr::query_version(140, 1, 5)));
    put(
        "randr_select_input",
        hx(randr::select_input(
            140,
            0x4e3,
            randr::notify_mask::SCREEN_CHANGE
                | randr::notify_mask::CRTC_CHANGE
                | randr::notify_mask::OUTPUT_CHANGE,
        )),
    );
    put(
        "randr_get_screen_resources",
        hx(randr::get_screen_resources(140, 0x4e3)),
    );
    put(
        "randr_get_screen_resources_current",
        hx(randr::get_screen_resources_current(140, 0x4e3)),
    );
    put(
        "randr_get_output_info",
        hx(randr::get_output_info(140, 0x42, 1234)),
    );
    put(
        "randr_get_crtc_info",
        hx(randr::get_crtc_info(140, 0x3f, 0)),
    );
    put(
        "randr_set_crtc_config",
        hx(randr::set_crtc_config(
            140,
            0x3f,
            0,
            1234,
            0,
            -10,
            0x45,
            randr::rotation::ROTATE_0,
            &[0x42, 0x43],
        )),
    );
    put(
        "randr_set_crtc_config_off",
        hx(randr::set_crtc_config(
            140,
            0x3f,
            0,
            1234,
            0,
            0,
            0,
            randr::rotation::ROTATE_0,
            &[],
        )),
    );
    put(
        "randr_get_output_primary",
        hx(randr::get_output_primary(140, 0x4e3)),
    );
    put("xi_query_version", hx(xinput::xi_query_version(131, 2, 3)));
    put(
        "xi_select_events",
        hx(xinput::xi_select_events(
            131,
            0x0060_0001,
            &[
                xinput::EventMask {
                    deviceid: 1,
                    mask: vec![
                        xinput::event_mask::MOTION
                            | xinput::event_mask::BUTTON_PRESS
                            | xinput::event_mask::TOUCH_END,
                    ],
                },
                xinput::EventMask {
                    deviceid: 0,
                    mask: vec![xinput::event_mask::HIERARCHY, 0],
                },
            ],
        )),
    );
    put(
        "xi_query_pointer",
        hx(xinput::xi_query_pointer(131, 0x0060_0001, 2)),
    );
    put("sync_initialize", hx(sync::initialize(134, 3, 1)));
    put(
        "sync_create_counter",
        hx(sync::create_counter(134, 0x0060_0007, 0)),
    );
    put(
        "sync_set_counter",
        hx(sync::set_counter(134, 0x0060_0007, (5 << 32) | 0x8000_0001)),
    );
    put(
        "sync_set_counter_negative",
        hx(sync::set_counter(134, 0x0060_0007, -2)),
    );
    put(
        "sync_query_counter",
        hx(sync::query_counter(134, 0x0060_0007)),
    );
    put(
        "sync_destroy_counter",
        hx(sync::destroy_counter(134, 0x0060_0007)),
    );
    c
}

#[test]
fn every_request_is_x11rb_protocols_bytes() {
    let recorded: BTreeMap<String, String> = by_case(RECORDING)
        .into_iter()
        .map(|(case, rest)| (case, rest.join("\t")))
        .collect();
    assert!(
        recorded.len() >= 71,
        "the corpus is whole: {}",
        recorded.len()
    );
    same_cases(RECORDING, &recorded, &cases());
}

#[test]
fn a_request_one_argument_away_misses_its_recording() {
    let recorded = by_case(RECORDING);
    let want = |case: &str| recorded[case][0].clone();
    assert_ne!(
        hex(xproto::intern_atom(true, b"CLIPBOARD").bytes()),
        want("intern_atom_clipboard")
    );
    assert_ne!(
        hex(xproto::get_property(false, 0x0040_0002, 0x1b0, 0, 0, u32::MAX).bytes()),
        want("get_property_paste")
    );
    assert_ne!(
        hex(shm::attach_fd(130, 0x0060_0005, false).bytes()),
        want("shm_attach_fd_ro")
    );
    assert_ne!(
        hex(sync::set_counter(134, 0x0060_0007, (0x8000_0001 << 32) | 5).bytes()),
        want("sync_set_counter"),
        "an INT64's halves in the other order"
    );
    assert_ne!(
        hex(xproto::put_image(
            image_format::Z_PIXMAP,
            0x0060_0001,
            0x0060_0003,
            5,
            1,
            -1,
            3,
            0,
            8,
            &[9, 8, 7, 6]
        )
        .bytes()),
        want("put_image_odd"),
        "a dropped byte changes the length and the padding"
    );
    let window = ValueList::new()
        .set(cw::EVENT_MASK, event_mask::PROPERTY_CHANGE)
        .set(cw::OVERRIDE_REDIRECT, 0);
    assert_ne!(
        hex(xproto::create_window(0, 0x0040_0002, 0x4e3, 0, 0, 1, 1, 0, 1, 0x21, &window).bytes()),
        want("create_window_clipboard"),
        "one more value-list member"
    );
}
