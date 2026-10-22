// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The X11 oracle's recorder (LW.6 of docs/AUDIT-third-party-2026-10-15.md),
//! built outside the aterm workspace (see Cargo.toml): registry
//! x11rb-protocol 0.13.2 and x11rb 0.13.2 write the bytes and answers, and
//! aterm-x11 is held to the committed recordings with no x11rb in its graph.
//!
//!   requests   each request aterm-x11 encodes, as x11rb-protocol encodes it
//!   replies    reply packets x11rb-protocol serializes, and its parse of each
//!              (or ERR), malformed ones included
//!   events     event packets, and x11rb-protocol's parse of each
//!   icccm      WM_SIZE_HINTS / WM_HINTS / WM_CLASS as x11rb writes and reads them
//!   xrm        resource databases and x11rb-protocol's answer to each query
//!   xrm-include  resource files with `#include` lines, written to a scratch
//!              tree, and x11rb-protocol's answer to each query of each file
//!              read as a home file is (its own directory the base)
//!
//! A row is `<case>\t<hex>[\t<fields>]`. Fields are `name=value` pairs in a
//! fixed order, integers in decimal, byte strings in hex, `-` for None; the
//! test renders aterm-x11's answers the same way.

use std::borrow::Cow;
use std::fs::File;

use x11rb::properties::{
    AspectRatio, WmClass, WmHints, WmHintsState, WmSizeHints, WmSizeHintsSpecification,
};
use x11rb_protocol::protocol::shm;
use x11rb_protocol::protocol::xproto::*;
use x11rb_protocol::resource_manager::Database;
use x11rb_protocol::x11_utils::{Request, Serialize, TryParse};

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn req<R: Request>(name: &str, r: R) {
    req_major(name, 0, r);
}

fn req_major<R: Request>(name: &str, major: u8, r: R) {
    let (bytes, _fds) = r.serialize(major);
    println!("{name}\t{}", hex(&bytes));
}

/// A reply struct's bytes as they arrive: serialized, then padded to the
/// 32 bytes every reply is at least, plus its length field's words.
fn wire(mut bytes: Vec<u8>) -> Vec<u8> {
    let length = u32::from_ne_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]) as usize;
    let total = 32 + 4 * length;
    if bytes.len() < total {
        bytes.resize(total, 0);
    }
    bytes
}

fn reply<T: TryParse>(name: &str, bytes: &[u8], render: impl Fn(&T) -> String) {
    let fields = match T::try_parse(bytes) {
        Ok((v, _)) => render(&v),
        Err(_) => "ERR".to_owned(),
    };
    println!("{name}\t{}\t{fields}", hex(bytes));
}

fn b(v: bool) -> u8 {
    u8::from(v)
}

fn list(v: &[u32]) -> String {
    if v.is_empty() {
        return "-".to_owned();
    }
    v.iter().map(u32::to_string).collect::<Vec<_>>().join(",")
}

fn words(n: u32) -> u32 {
    n.div_ceil(4)
}

fn requests() {
    let aux = CreateWindowAux::new()
        .background_pixel(0x00ff_8800)
        .override_redirect(1)
        .event_mask(EventMask::PROPERTY_CHANGE | EventMask::STRUCTURE_NOTIFY)
        .colormap(0x20);
    req(
        "create_window",
        CreateWindowRequest {
            depth: 24,
            wid: 0x0060_0001,
            parent: 0x123,
            x: -5,
            y: 7,
            width: 640,
            height: 480,
            border_width: 0,
            class: WindowClass::INPUT_OUTPUT,
            visual: 0x21,
            value_list: Cow::Owned(aux),
        },
    );
    req(
        "create_window_clipboard",
        CreateWindowRequest {
            depth: 0,
            wid: 0x0040_0002,
            parent: 0x4e3,
            x: 0,
            y: 0,
            width: 1,
            height: 1,
            border_width: 0,
            class: WindowClass::INPUT_OUTPUT,
            visual: 0x21,
            value_list: Cow::Owned(CreateWindowAux::new().event_mask(EventMask::PROPERTY_CHANGE)),
        },
    );
    req(
        "change_window_attributes",
        ChangeWindowAttributesRequest {
            window: 0x0060_0001,
            value_list: Cow::Owned(
                ChangeWindowAttributesAux::new()
                    .event_mask(EventMask::FOCUS_CHANGE | EventMask::KEY_PRESS)
                    .cursor(0x0060_0009),
            ),
        },
    );
    req(
        "get_window_attributes",
        GetWindowAttributesRequest {
            window: 0x0060_0001,
        },
    );
    req(
        "destroy_window",
        DestroyWindowRequest {
            window: 0x0060_0001,
        },
    );
    req(
        "reparent_window",
        ReparentWindowRequest {
            window: 0x0060_0001,
            parent: 0x123,
            x: -5,
            y: 7,
        },
    );
    req(
        "map_window",
        MapWindowRequest {
            window: 0x0060_0001,
        },
    );
    req(
        "unmap_window",
        UnmapWindowRequest {
            window: 0x0060_0001,
        },
    );
    req(
        "configure_window",
        ConfigureWindowRequest {
            window: 0x0060_0001,
            value_list: Cow::Owned(
                ConfigureWindowAux::new()
                    .x(-20)
                    .y(30)
                    .width(800)
                    .height(600)
                    .stack_mode(StackMode::ABOVE),
            ),
        },
    );
    req(
        "get_geometry",
        GetGeometryRequest {
            drawable: 0x0060_0001,
        },
    );
    req("query_tree", QueryTreeRequest { window: 0x4e3 });
    for (name, only, atom) in [
        ("intern_atom_clipboard", false, &b"CLIPBOARD"[..]),
        ("intern_atom_only_if_exists", true, b"UTF8_STRING"),
        ("intern_atom_empty", false, b""),
        ("intern_atom_word", false, b"TEXT"),
    ] {
        req(
            name,
            InternAtomRequest {
                only_if_exists: only,
                name: Cow::Borrowed(atom),
            },
        );
    }
    req("get_atom_name", GetAtomNameRequest { atom: 0x1a2 });
    req(
        "change_property8",
        ChangePropertyRequest {
            mode: PropMode::REPLACE,
            window: 0x0040_0002,
            property: 0x1b0,
            type_: 0x1a3,
            format: 8,
            data_len: 6,
            data: Cow::Borrowed("héllo".as_bytes()),
        },
    );
    let atoms: Vec<u8> = [0x1a4u32, 0x1a3, 0x1a5, 31, 0x1a6]
        .iter()
        .flat_map(|v| v.to_ne_bytes())
        .collect();
    req(
        "change_property32",
        ChangePropertyRequest {
            mode: PropMode::REPLACE,
            window: 0x0050_0007,
            property: 0x1b1,
            type_: 4,
            format: 32,
            data_len: 5,
            data: Cow::Owned(atoms),
        },
    );
    req(
        "change_property_append_empty",
        ChangePropertyRequest {
            mode: PropMode::APPEND,
            window: 0x0050_0007,
            property: 0x1b1,
            type_: 0x1a3,
            format: 8,
            data_len: 0,
            data: Cow::Borrowed(&[]),
        },
    );
    req(
        "change_property16",
        ChangePropertyRequest {
            mode: PropMode::PREPEND,
            window: 0x0050_0007,
            property: 0x1b2,
            type_: 6,
            format: 16,
            data_len: 3,
            data: Cow::Owned(
                [1u16, 0xfffe, 300]
                    .iter()
                    .flat_map(|v| v.to_ne_bytes())
                    .collect(),
            ),
        },
    );
    req(
        "delete_property",
        DeletePropertyRequest {
            window: 0x0040_0002,
            property: 0x1b0,
        },
    );
    req(
        "get_property_paste",
        GetPropertyRequest {
            delete: true,
            window: 0x0040_0002,
            property: 0x1b0,
            type_: 0,
            long_offset: 0,
            long_length: u32::MAX,
        },
    );
    req(
        "get_property_resource_manager",
        GetPropertyRequest {
            delete: false,
            window: 0x4e3,
            property: 23,
            type_: 31,
            long_offset: 0,
            long_length: 100_000_000,
        },
    );
    req(
        "set_selection_owner",
        SetSelectionOwnerRequest {
            owner: 0x0040_0002,
            selection: 0x1a1,
            time: 0,
        },
    );
    req(
        "get_selection_owner",
        GetSelectionOwnerRequest { selection: 1 },
    );
    req(
        "convert_selection",
        ConvertSelectionRequest {
            requestor: 0x0040_0002,
            selection: 0x1a1,
            target: 0x1a3,
            property: 0x1b0,
            time: 0,
        },
    );
    let notify = SelectionNotifyEvent {
        response_type: SELECTION_NOTIFY_EVENT,
        sequence: 0,
        time: 123_456,
        requestor: 0x0050_0007,
        selection: 0x1a1,
        target: 0x1a3,
        property: 0x1b1,
    };
    let event: [u8; 32] = notify.into();
    req(
        "send_event_selection_notify",
        SendEventRequest {
            propagate: false,
            destination: 0x0050_0007,
            event_mask: EventMask::NO_EVENT,
            event: Cow::Owned(event),
        },
    );
    req(
        "grab_pointer",
        GrabPointerRequest {
            owner_events: true,
            grab_window: 0x0060_0001,
            event_mask: EventMask::BUTTON_PRESS
                | EventMask::BUTTON_RELEASE
                | EventMask::POINTER_MOTION,
            pointer_mode: GrabMode::ASYNC,
            keyboard_mode: GrabMode::ASYNC,
            confine_to: 0x0060_0001,
            cursor: 0,
            time: 0,
        },
    );
    req("ungrab_pointer", UngrabPointerRequest { time: 77 });
    req(
        "query_pointer",
        QueryPointerRequest {
            window: 0x0060_0001,
        },
    );
    req(
        "translate_coordinates",
        TranslateCoordinatesRequest {
            src_window: 0x0060_0001,
            dst_window: 0x4e3,
            src_x: -3,
            src_y: 9,
        },
    );
    req(
        "warp_pointer",
        WarpPointerRequest {
            src_window: 0,
            dst_window: 0x0060_0001,
            src_x: 0,
            src_y: 0,
            src_width: 0,
            src_height: 0,
            dst_x: 100,
            dst_y: -2,
        },
    );
    req(
        "set_input_focus",
        SetInputFocusRequest {
            revert_to: InputFocus::PARENT,
            focus: 0x0060_0001,
            time: 0,
        },
    );
    req("get_input_focus", GetInputFocusRequest);
    req("query_keymap", QueryKeymapRequest);
    req(
        "create_gc_presenter",
        CreateGCRequest {
            cid: 0x0060_0003,
            drawable: 0x0060_0001,
            value_list: Cow::Owned(CreateGCAux::new().graphics_exposures(0)),
        },
    );
    req(
        "create_gc_colors",
        CreateGCRequest {
            cid: 0x0060_0004,
            drawable: 0x0060_0001,
            value_list: Cow::Owned(
                CreateGCAux::new()
                    .function(GX::COPY)
                    .foreground(0x00ff_ffff)
                    .background(0)
                    .subwindow_mode(SubwindowMode::INCLUDE_INFERIORS),
            ),
        },
    );
    req(
        "create_pixmap",
        CreatePixmapRequest {
            depth: 24,
            pid: 0x0060_0007,
            drawable: 0x0060_0001,
            width: 300,
            height: 200,
        },
    );
    req(
        "free_pixmap",
        FreePixmapRequest {
            pixmap: 0x0060_0007,
        },
    );
    req("free_gc", FreeGCRequest { gc: 0x0060_0003 });
    // LW.7: the window system's own requests the rows above lacked.
    req(
        "create_colormap",
        CreateColormapRequest {
            alloc: ColormapAlloc::NONE,
            mid: 0x0060_000a,
            window: 0x4e3,
            visual: 0x5e,
        },
    );
    req(
        "grab_pointer_confine",
        GrabPointerRequest {
            owner_events: true,
            grab_window: 0x0060_0001,
            event_mask: EventMask::BUTTON_PRESS
                | EventMask::BUTTON_RELEASE
                | EventMask::ENTER_WINDOW
                | EventMask::LEAVE_WINDOW
                | EventMask::POINTER_MOTION
                | EventMask::POINTER_MOTION_HINT
                | EventMask::BUTTON1_MOTION
                | EventMask::BUTTON2_MOTION
                | EventMask::BUTTON3_MOTION
                | EventMask::BUTTON4_MOTION
                | EventMask::BUTTON5_MOTION
                | EventMask::KEYMAP_STATE,
            pointer_mode: GrabMode::ASYNC,
            keyboard_mode: GrabMode::ASYNC,
            confine_to: 0x0060_0001,
            cursor: 0,
            time: 0,
        },
    );
    let pixels: Vec<u8> = (0u8..16).collect();
    req(
        "put_image_2x2",
        PutImageRequest {
            format: ImageFormat::Z_PIXMAP,
            drawable: 0x0060_0001,
            gc: 0x0060_0003,
            width: 2,
            height: 2,
            dst_x: 0,
            dst_y: 0,
            left_pad: 0,
            depth: 24,
            data: Cow::Owned(pixels),
        },
    );
    req(
        "put_image_odd",
        PutImageRequest {
            format: ImageFormat::Z_PIXMAP,
            drawable: 0x0060_0001,
            gc: 0x0060_0003,
            width: 5,
            height: 1,
            dst_x: -1,
            dst_y: 3,
            left_pad: 0,
            depth: 8,
            data: Cow::Borrowed(&[9, 8, 7, 6, 5]),
        },
    );
    // Longer than a 16-bit length field says: x11rb-protocol writes 0 there,
    // which the transport rewrites in the BIG-REQUESTS form. Recorded as its
    // head and length (its body is the data, verbatim).
    let big: Vec<u8> = (0..262_144u32).map(|i| (i % 251) as u8).collect();
    let (bytes, _) = Request::serialize(
        PutImageRequest {
            format: ImageFormat::Z_PIXMAP,
            drawable: 0x0060_0001,
            gc: 0x0060_0003,
            width: 256,
            height: 256,
            dst_x: 0,
            dst_y: 0,
            left_pad: 0,
            depth: 24,
            data: Cow::Owned(big),
        },
        0,
    );
    println!("put_image_big_head\t{}\t{}", hex(&bytes[..24]), bytes.len());
    req(
        "get_image",
        GetImageRequest {
            format: ImageFormat::Z_PIXMAP,
            drawable: 0x0060_0001,
            x: 0,
            y: -4,
            width: 64,
            height: 32,
            plane_mask: u32::MAX,
        },
    );
    req(
        "query_extension_shm",
        QueryExtensionRequest {
            name: Cow::Borrowed(b"MIT-SHM"),
        },
    );
    req(
        "query_extension_big_requests",
        QueryExtensionRequest {
            name: Cow::Borrowed(b"BIG-REQUESTS"),
        },
    );
    req("get_modifier_mapping", GetModifierMappingRequest);
    // MIT-SHM, at the major opcode a server might assign.
    req_major("shm_query_version", 130, shm::QueryVersionRequest);
    let fd = || File::open("/dev/null").expect("/dev/null").into();
    req_major(
        "shm_attach_fd_ro",
        130,
        shm::AttachFdRequest {
            shmseg: 0x0060_0005,
            shm_fd: fd(),
            read_only: true,
        },
    );
    req_major(
        "shm_attach_fd_rw",
        131,
        shm::AttachFdRequest {
            shmseg: 0x0060_0006,
            shm_fd: fd(),
            read_only: false,
        },
    );
    req_major(
        "shm_detach",
        130,
        shm::DetachRequest {
            shmseg: 0x0060_0005,
        },
    );
    req_major(
        "shm_put_image",
        130,
        shm::PutImageRequest {
            drawable: 0x0060_0001,
            gc: 0x0060_0003,
            total_width: 1920,
            total_height: 1080,
            src_x: 8,
            src_y: 16,
            src_width: 300,
            src_height: 20,
            dst_x: 8,
            dst_y: -16,
            depth: 24,
            format: 2,
            send_event: false,
            shmseg: 0x0060_0005,
            offset: 4096,
        },
    );
    // RandR at major opcode 140, XInput at 131.
    use x11rb_protocol::protocol::{randr, xinput};
    req_major(
        "randr_query_version",
        140,
        randr::QueryVersionRequest {
            major_version: 1,
            minor_version: 5,
        },
    );
    req_major(
        "randr_select_input",
        140,
        randr::SelectInputRequest {
            window: 0x4e3,
            enable: randr::NotifyMask::SCREEN_CHANGE
                | randr::NotifyMask::CRTC_CHANGE
                | randr::NotifyMask::OUTPUT_CHANGE,
        },
    );
    req_major(
        "randr_get_screen_resources",
        140,
        randr::GetScreenResourcesRequest { window: 0x4e3 },
    );
    req_major(
        "randr_get_screen_resources_current",
        140,
        randr::GetScreenResourcesCurrentRequest { window: 0x4e3 },
    );
    req_major(
        "randr_get_output_info",
        140,
        randr::GetOutputInfoRequest {
            output: 0x42,
            config_timestamp: 1234,
        },
    );
    req_major(
        "randr_get_crtc_info",
        140,
        randr::GetCrtcInfoRequest {
            crtc: 0x3f,
            config_timestamp: 0,
        },
    );
    req_major(
        "randr_set_crtc_config",
        140,
        randr::SetCrtcConfigRequest {
            crtc: 0x3f,
            timestamp: 0,
            config_timestamp: 1234,
            x: 0,
            y: -10,
            mode: 0x45,
            rotation: randr::Rotation::ROTATE0,
            outputs: Cow::Owned(vec![0x42, 0x43]),
        },
    );
    req_major(
        "randr_set_crtc_config_off",
        140,
        randr::SetCrtcConfigRequest {
            crtc: 0x3f,
            timestamp: 0,
            config_timestamp: 1234,
            x: 0,
            y: 0,
            mode: 0,
            rotation: randr::Rotation::ROTATE0,
            outputs: Cow::Owned(vec![]),
        },
    );
    req_major(
        "randr_get_output_primary",
        140,
        randr::GetOutputPrimaryRequest { window: 0x4e3 },
    );
    req_major(
        "xi_query_version",
        131,
        xinput::XIQueryVersionRequest {
            major_version: 2,
            minor_version: 3,
        },
    );
    req_major(
        "xi_select_events",
        131,
        xinput::XISelectEventsRequest {
            window: 0x0060_0001,
            masks: Cow::Owned(vec![
                xinput::EventMask {
                    deviceid: 1,
                    mask: vec![
                        xinput::XIEventMask::MOTION
                            | xinput::XIEventMask::BUTTON_PRESS
                            | xinput::XIEventMask::TOUCH_END,
                    ],
                },
                xinput::EventMask {
                    deviceid: 0,
                    mask: vec![
                        xinput::XIEventMask::HIERARCHY,
                        xinput::XIEventMask::from(0u32),
                    ],
                },
            ]),
        },
    );
    req_major(
        "xi_query_pointer",
        131,
        xinput::XIQueryPointerRequest {
            window: 0x0060_0001,
            deviceid: 2,
        },
    );
    sync_requests();
}

/// An INT64 as x11rb-protocol's SYNC holds one: the high half, then the low.
fn int64(value: i64) -> x11rb_protocol::protocol::sync::Int64 {
    x11rb_protocol::protocol::sync::Int64 {
        hi: (value >> 32) as i32,
        lo: value as u32,
    }
}

/// SYNC's counter requests, at major opcode 134 (Xwayland's and Xorg's).
fn sync_requests() {
    use x11rb_protocol::protocol::sync;
    req_major(
        "sync_initialize",
        134,
        sync::InitializeRequest {
            desired_major_version: 3,
            desired_minor_version: 1,
        },
    );
    req_major(
        "sync_create_counter",
        134,
        sync::CreateCounterRequest {
            id: 0x0060_0007,
            initial_value: int64(0),
        },
    );
    req_major(
        "sync_set_counter",
        134,
        sync::SetCounterRequest {
            counter: 0x0060_0007,
            value: int64((5 << 32) | 0x8000_0001),
        },
    );
    req_major(
        "sync_set_counter_negative",
        134,
        sync::SetCounterRequest {
            counter: 0x0060_0007,
            value: int64(-2),
        },
    );
    req_major(
        "sync_query_counter",
        134,
        sync::QueryCounterRequest {
            counter: 0x0060_0007,
        },
    );
    req_major(
        "sync_destroy_counter",
        134,
        sync::DestroyCounterRequest {
            counter: 0x0060_0007,
        },
    );
}

fn render_property(r: &GetPropertyReply) -> String {
    format!(
        "format={} type={} bytes_after={} value_len={} value={}",
        r.format,
        r.type_,
        r.bytes_after,
        r.value_len,
        hex(&r.value)
    )
}

fn property(name: &str, format: u8, type_: u32, bytes_after: u32, value: &[u8]) {
    let value_len = if format == 0 {
        0
    } else {
        value.len() as u32 / (u32::from(format) / 8)
    };
    let r = GetPropertyReply {
        format,
        sequence: 9,
        length: words(value.len() as u32),
        type_,
        bytes_after,
        value_len,
        value: value.to_vec(),
    };
    reply::<GetPropertyReply>(name, &wire(r.serialize()), render_property);
}

fn replies() {
    let r = InternAtomReply {
        sequence: 7,
        length: 0,
        atom: 0x1a2,
    };
    reply::<InternAtomReply>("intern_atom", &wire(r.serialize().to_vec()), |r| {
        format!("atom={}", r.atom)
    });
    property("get_property_utf8", 8, 0x1a3, 0, "héllo wörld".as_bytes());
    let atoms: Vec<u8> = [0x1a4u32, 0x1a3, 31]
        .iter()
        .flat_map(|v| v.to_ne_bytes())
        .collect();
    property("get_property_atoms", 32, 4, 0, &atoms);
    property("get_property_absent", 0, 0, 0, &[]);
    property("get_property_card16", 16, 6, 12, &[1, 0, 2, 0]);
    property("get_property_incr", 32, 0x1a7, 0, &65_536u32.to_ne_bytes());
    // Malformed: value_len promises more than the packet holds.
    let mut lie = wire(
        GetPropertyReply {
            format: 8,
            sequence: 1,
            length: 1,
            type_: 31,
            bytes_after: 0,
            value_len: 4,
            value: vec![1, 2, 3, 4],
        }
        .serialize(),
    );
    lie[16..20].copy_from_slice(&64u32.to_ne_bytes());
    reply::<GetPropertyReply>("get_property_value_len_lies", &lie, render_property);
    let short = wire(
        InternAtomReply {
            sequence: 1,
            length: 0,
            atom: 5,
        }
        .serialize()
        .to_vec(),
    );
    reply::<InternAtomReply>("intern_atom_short", &short[..31], |r| {
        format!("atom={}", r.atom)
    });
    let mut not_reply = short.clone();
    not_reply[0] = 0;
    reply::<InternAtomReply>("intern_atom_not_a_reply", &not_reply, |r| {
        format!("atom={}", r.atom)
    });
    let mut long_field = short;
    long_field[4..8].copy_from_slice(&1u32.to_ne_bytes());
    reply::<InternAtomReply>("intern_atom_length_past_end", &long_field, |r| {
        format!("atom={}", r.atom)
    });
    let r = GetGeometryReply {
        depth: 24,
        sequence: 3,
        length: 0,
        root: 0x4e3,
        x: -12,
        y: 40,
        width: 1280,
        height: 800,
        border_width: 1,
    };
    reply::<GetGeometryReply>("get_geometry", &wire(r.serialize().to_vec()), |r| {
        format!(
            "depth={} root={} x={} y={} width={} height={} border_width={}",
            r.depth, r.root, r.x, r.y, r.width, r.height, r.border_width
        )
    });
    let r = GetWindowAttributesReply {
        backing_store: BackingStore::WHEN_MAPPED,
        sequence: 4,
        length: 3,
        visual: 0x21,
        class: WindowClass::INPUT_OUTPUT,
        bit_gravity: Gravity::NORTH_WEST,
        win_gravity: Gravity::STATIC,
        backing_planes: u32::MAX,
        backing_pixel: 0,
        save_under: false,
        map_is_installed: true,
        map_state: MapState::VIEWABLE,
        override_redirect: false,
        colormap: 0x20,
        all_event_masks: EventMask::from(0x00fa_8000u32),
        your_event_mask: EventMask::from(0x0040_0000u32),
        do_not_propagate_mask: EventMask::from(0x3fu32),
    };
    reply::<GetWindowAttributesReply>(
        "get_window_attributes",
        &wire(r.serialize().to_vec()),
        |r| {
            format!(
                "backing_store={} visual={} class={} bit_gravity={} win_gravity={} backing_planes={} \
             backing_pixel={} save_under={} map_is_installed={} map_state={} override_redirect={} \
             colormap={} all_event_masks={} your_event_mask={} do_not_propagate_mask={}",
                u32::from(r.backing_store),
                r.visual,
                u16::from(r.class),
                u32::from(r.bit_gravity),
                u32::from(r.win_gravity),
                r.backing_planes,
                r.backing_pixel,
                b(r.save_under),
                b(r.map_is_installed),
                u8::from(r.map_state),
                b(r.override_redirect),
                r.colormap,
                u32::from(r.all_event_masks),
                u32::from(r.your_event_mask),
                u32::from(r.do_not_propagate_mask)
            )
        },
    );
    let children = vec![0x0060_0001, 0x0070_0001, 0x0080_0004];
    let r = QueryTreeReply {
        sequence: 5,
        length: 3,
        root: 0x4e3,
        parent: 0x4e3,
        children,
    };
    reply::<QueryTreeReply>("query_tree", &wire(r.serialize()), |r| {
        format!(
            "root={} parent={} children={}",
            r.root,
            r.parent,
            list(&r.children)
        )
    });
    let r = GetAtomNameReply {
        sequence: 6,
        length: words(11),
        name: b"UTF8_STRING".to_vec(),
    };
    reply::<GetAtomNameReply>("get_atom_name", &wire(r.serialize()), |r| {
        format!("name={}", hex(&r.name))
    });
    let r = GetSelectionOwnerReply {
        sequence: 8,
        length: 0,
        owner: 0x0050_0007,
    };
    reply::<GetSelectionOwnerReply>("get_selection_owner", &wire(r.serialize().to_vec()), |r| {
        format!("owner={}", r.owner)
    });
    let r = GrabPointerReply {
        status: GrabStatus::ALREADY_GRABBED,
        sequence: 2,
        length: 0,
    };
    reply::<GrabPointerReply>("grab_pointer", &wire(r.serialize().to_vec()), |r| {
        format!("status={}", u8::from(r.status))
    });
    let r = QueryPointerReply {
        same_screen: true,
        sequence: 2,
        length: 0,
        root: 0x4e3,
        child: 0,
        root_x: 700,
        root_y: -1,
        win_x: -30,
        win_y: 400,
        mask: KeyButMask::SHIFT | KeyButMask::BUTTON1,
    };
    reply::<QueryPointerReply>("query_pointer", &wire(r.serialize().to_vec()), |r| {
        format!(
            "same_screen={} root={} child={} root_x={} root_y={} win_x={} win_y={} mask={}",
            b(r.same_screen),
            r.root,
            r.child,
            r.root_x,
            r.root_y,
            r.win_x,
            r.win_y,
            u16::from(r.mask)
        )
    });
    let r = TranslateCoordinatesReply {
        same_screen: false,
        sequence: 2,
        length: 0,
        child: 0x0060_0001,
        dst_x: -5,
        dst_y: 17,
    };
    reply::<TranslateCoordinatesReply>(
        "translate_coordinates",
        &wire(r.serialize().to_vec()),
        |r| {
            format!(
                "same_screen={} child={} dst_x={} dst_y={}",
                b(r.same_screen),
                r.child,
                r.dst_x,
                r.dst_y
            )
        },
    );
    let r = GetInputFocusReply {
        revert_to: InputFocus::POINTER_ROOT,
        sequence: 11,
        length: 0,
        focus: 0x0060_0001,
    };
    reply::<GetInputFocusReply>("get_input_focus", &wire(r.serialize().to_vec()), |r| {
        format!("revert_to={} focus={}", u8::from(r.revert_to), r.focus)
    });
    let mut keys = [0u8; 32];
    keys[1] = 0b1000_0001;
    keys[31] = 0x80;
    let r = QueryKeymapReply {
        sequence: 1,
        length: 2,
        keys,
    };
    reply::<QueryKeymapReply>("query_keymap", &wire(r.serialize().to_vec()), |r| {
        format!("keys={}", hex(&r.keys))
    });
    let r = GetImageReply {
        depth: 24,
        sequence: 12,
        visual: 0x21,
        data: vec![0x10, 0x20, 0x30, 0, 0x11, 0x21, 0x31, 0],
    };
    reply::<GetImageReply>("get_image", &wire(r.serialize()), |r| {
        format!(
            "depth={} visual={} data={}",
            r.depth,
            r.visual,
            hex(&r.data)
        )
    });
    for (name, present) in [
        ("query_extension_present", true),
        ("query_extension_absent", false),
    ] {
        let r = QueryExtensionReply {
            sequence: 2,
            length: 0,
            present,
            major_opcode: if present { 130 } else { 0 },
            first_event: if present { 65 } else { 0 },
            first_error: if present { 128 } else { 0 },
        };
        reply::<QueryExtensionReply>(name, &wire(r.serialize().to_vec()), |r| {
            format!(
                "present={} major_opcode={} first_event={} first_error={}",
                b(r.present),
                r.major_opcode,
                r.first_event,
                r.first_error
            )
        });
    }
    let r = GetModifierMappingReply {
        sequence: 2,
        length: 4,
        keycodes: (50u8..66).collect(),
    };
    reply::<GetModifierMappingReply>("get_modifier_mapping", &wire(r.serialize()), |r| {
        format!(
            "keycodes_per_modifier={} keycodes={}",
            r.keycodes_per_modifier(),
            hex(&r.keycodes)
        )
    });
    let r = shm::QueryVersionReply {
        shared_pixmaps: true,
        sequence: 2,
        length: 0,
        major_version: 1,
        minor_version: 2,
        uid: 1000,
        gid: 1000,
        pixmap_format: 2,
    };
    reply::<shm::QueryVersionReply>("shm_query_version", &wire(r.serialize().to_vec()), |r| {
        format!(
            "shared_pixmaps={} major_version={} minor_version={} uid={} gid={} pixmap_format={}",
            b(r.shared_pixmaps),
            r.major_version,
            r.minor_version,
            r.uid,
            r.gid,
            r.pixmap_format
        )
    });
    randr_xinput_replies();
    sync_replies();
    setups();
}

fn sync_replies() {
    use x11rb_protocol::protocol::sync;
    let v = sync::InitializeReply {
        sequence: 3,
        length: 0,
        major_version: 3,
        minor_version: 1,
    };
    reply::<sync::InitializeReply>("sync_initialize", &wire(v.serialize().to_vec()), |r| {
        format!(
            "major_version={} minor_version={}",
            r.major_version, r.minor_version
        )
    });
    let render = |r: &sync::QueryCounterReply| {
        format!(
            "counter_value={}",
            (i64::from(r.counter_value.hi) << 32) | i64::from(r.counter_value.lo)
        )
    };
    for (name, value) in [
        ("sync_query_counter", (5 << 32) | 0x8000_0001),
        ("sync_query_counter_negative", -2),
        ("sync_query_counter_min", i64::MIN),
        ("sync_query_counter_max", i64::MAX),
    ] {
        let q = sync::QueryCounterReply {
            sequence: 4,
            length: 0,
            counter_value: int64(value),
        };
        reply::<sync::QueryCounterReply>(name, &wire(q.serialize().to_vec()), render);
    }
}

fn render_modes(modes: &[x11rb_protocol::protocol::randr::ModeInfo]) -> String {
    if modes.is_empty() {
        return "-".to_owned();
    }
    modes
        .iter()
        .map(|m| {
            format!(
                "{}:{}x{}:{}:{},{},{},{}:{},{},{}:{}:{}",
                m.id,
                m.width,
                m.height,
                m.dot_clock,
                m.hsync_start,
                m.hsync_end,
                m.htotal,
                m.hskew,
                m.vsync_start,
                m.vsync_end,
                m.vtotal,
                m.name_len,
                u32::from(m.mode_flags)
            )
        })
        .collect::<Vec<_>>()
        .join(",")
}

fn randr_xinput_replies() {
    use x11rb_protocol::protocol::{randr, xinput};
    let r = randr::QueryVersionReply {
        sequence: 2,
        length: 0,
        major_version: 1,
        minor_version: 6,
    };
    reply::<randr::QueryVersionReply>("randr_query_version", &wire(r.serialize().to_vec()), |r| {
        format!(
            "major_version={} minor_version={}",
            r.major_version, r.minor_version
        )
    });
    let mode = |id, w, h, clock, name_len| randr::ModeInfo {
        id,
        width: w,
        height: h,
        dot_clock: clock,
        hsync_start: w + 48,
        hsync_end: w + 80,
        htotal: w + 160,
        hskew: 0,
        vsync_start: h + 3,
        vsync_end: h + 8,
        vtotal: h + 31,
        name_len,
        mode_flags: randr::ModeFlag::HSYNC_POSITIVE | randr::ModeFlag::VSYNC_NEGATIVE,
    };
    let names = b"1920x10801280x800".to_vec();
    let res = randr::GetScreenResourcesReply {
        sequence: 3,
        length: 0,
        timestamp: 777,
        config_timestamp: 1234,
        crtcs: vec![0x3f, 0x40],
        outputs: vec![0x42, 0x43, 0x44],
        modes: vec![
            mode(0x45, 1920, 1080, 148_500_000, 9),
            mode(0x46, 1280, 800, 71_000_000, 8),
        ],
        names: names.clone(),
    };
    let mut bytes = res.serialize();
    let len = ((bytes.len() - 32 + 3) / 4) as u32;
    bytes[4..8].copy_from_slice(&len.to_ne_bytes());
    let bytes = wire(bytes);
    let render_res = |r: &randr::GetScreenResourcesReply| {
        format!(
            "timestamp={} config_timestamp={} crtcs={} outputs={} modes={} names={}",
            r.timestamp,
            r.config_timestamp,
            list(&r.crtcs),
            list(&r.outputs),
            render_modes(&r.modes),
            hex(&r.names)
        )
    };
    reply::<randr::GetScreenResourcesReply>("randr_get_screen_resources", &bytes, render_res);
    reply::<randr::GetScreenResourcesReply>(
        "randr_screen_resources_short",
        &bytes[..bytes.len() - 4],
        render_res,
    );
    let cur = randr::GetScreenResourcesCurrentReply {
        sequence: 4,
        length: 0,
        timestamp: 778,
        config_timestamp: 1234,
        crtcs: vec![0x3f],
        outputs: vec![],
        modes: vec![],
        names: vec![],
    };
    let mut bytes = cur.serialize();
    let len = ((bytes.len() - 32 + 3) / 4) as u32;
    bytes[4..8].copy_from_slice(&len.to_ne_bytes());
    reply::<randr::GetScreenResourcesCurrentReply>(
        "randr_get_screen_resources_current",
        &wire(bytes),
        |r| {
            format!(
                "timestamp={} config_timestamp={} crtcs={} outputs={} modes={} names={}",
                r.timestamp,
                r.config_timestamp,
                list(&r.crtcs),
                list(&r.outputs),
                render_modes(&r.modes),
                hex(&r.names)
            )
        },
    );
    let out = randr::GetOutputInfoReply {
        status: randr::SetConfig::SUCCESS,
        sequence: 5,
        length: 0,
        timestamp: 777,
        crtc: 0x3f,
        mm_width: 344,
        mm_height: 193,
        connection: randr::Connection::CONNECTED,
        subpixel_order: x11rb_protocol::protocol::render::SubPixel::UNKNOWN,
        num_preferred: 1,
        crtcs: vec![0x3f, 0x40],
        modes: vec![0x45, 0x46],
        clones: vec![],
        name: b"Virtual-1".to_vec(),
    };
    let mut bytes = out.serialize();
    let len = ((bytes.len() - 32 + 3) / 4) as u32;
    bytes[4..8].copy_from_slice(&len.to_ne_bytes());
    reply::<randr::GetOutputInfoReply>("randr_get_output_info", &wire(bytes), |r| {
        format!(
            "status={} timestamp={} crtc={} mm={}x{} connection={} subpixel={} num_preferred={} crtcs={} modes={} clones={} name={}",
            u8::from(r.status),
            r.timestamp,
            r.crtc,
            r.mm_width,
            r.mm_height,
            u8::from(r.connection),
            u32::from(r.subpixel_order),
            r.num_preferred,
            list(&r.crtcs),
            list(&r.modes),
            list(&r.clones),
            hex(&r.name)
        )
    });
    let crtc = randr::GetCrtcInfoReply {
        status: randr::SetConfig::SUCCESS,
        sequence: 6,
        length: 0,
        timestamp: 777,
        x: -5,
        y: 10,
        width: 1920,
        height: 1080,
        mode: 0x45,
        rotation: randr::Rotation::ROTATE0,
        rotations: randr::Rotation::ROTATE0 | randr::Rotation::REFLECT_X,
        outputs: vec![0x42],
        possible: vec![0x42, 0x43],
    };
    let mut bytes = crtc.serialize();
    let len = ((bytes.len() - 32 + 3) / 4) as u32;
    bytes[4..8].copy_from_slice(&len.to_ne_bytes());
    reply::<randr::GetCrtcInfoReply>("randr_get_crtc_info", &wire(bytes), |r| {
        format!(
            "status={} timestamp={} x={} y={} width={} height={} mode={} rotation={} rotations={} outputs={} possible={}",
            u8::from(r.status),
            r.timestamp,
            r.x,
            r.y,
            r.width,
            r.height,
            r.mode,
            u16::from(r.rotation),
            u16::from(r.rotations),
            list(&r.outputs),
            list(&r.possible)
        )
    });
    let set = randr::SetCrtcConfigReply {
        status: randr::SetConfig::INVALID_CONFIG_TIME,
        sequence: 7,
        length: 0,
        timestamp: 999,
    };
    reply::<randr::SetCrtcConfigReply>(
        "randr_set_crtc_config",
        &wire(set.serialize().to_vec()),
        |r| format!("status={} timestamp={}", u8::from(r.status), r.timestamp),
    );
    let primary = randr::GetOutputPrimaryReply {
        sequence: 8,
        length: 0,
        output: 0x42,
    };
    reply::<randr::GetOutputPrimaryReply>(
        "randr_get_output_primary",
        &wire(primary.serialize().to_vec()),
        |r| format!("output={}", r.output),
    );
    let v = xinput::XIQueryVersionReply {
        sequence: 9,
        length: 0,
        major_version: 2,
        minor_version: 4,
    };
    reply::<xinput::XIQueryVersionReply>("xi_query_version", &wire(v.serialize().to_vec()), |r| {
        format!(
            "major_version={} minor_version={}",
            r.major_version, r.minor_version
        )
    });
    let p = xinput::XIQueryPointerReply {
        sequence: 10,
        length: 2,
        root: 0x4e3,
        child: 0x0060_0001,
        root_x: (100 << 16) | 0x8000,
        root_y: -(3 << 16),
        win_x: 42 << 16,
        win_y: 7 << 16,
        same_screen: true,
        mods: xinput::ModifierInfo {
            base: 1,
            latched: 0,
            locked: 2,
            effective: 3,
        },
        group: xinput::GroupInfo {
            base: 0,
            latched: 0,
            locked: 1,
            effective: 1,
        },
        buttons: vec![0b10, 0],
    };
    reply::<xinput::XIQueryPointerReply>("xi_query_pointer", &wire(p.serialize()), |r| {
        format!(
            "root={} child={} root_x={} root_y={} win_x={} win_y={} same_screen={} mods={},{},{},{} group={},{},{},{} buttons={}",
            r.root,
            r.child,
            r.root_x,
            r.root_y,
            r.win_x,
            r.win_y,
            b(r.same_screen),
            r.mods.base,
            r.mods.latched,
            r.mods.locked,
            r.mods.effective,
            r.group.base,
            r.group.latched,
            r.group.locked,
            r.group.effective,
            list(&r.buttons)
        )
    });
}

fn render_setup(s: &Setup) -> String {
    let mut out = format!(
        "status={} protocol={}.{} release={} id_base={} id_mask={} motion={} max_request={} \
         image_order={} bitmap_order={} scanline_unit={} scanline_pad={} keycodes={}-{} vendor={}",
        s.status,
        s.protocol_major_version,
        s.protocol_minor_version,
        s.release_number,
        s.resource_id_base,
        s.resource_id_mask,
        s.motion_buffer_size,
        s.maximum_request_length,
        u8::from(s.image_byte_order),
        u8::from(s.bitmap_format_bit_order),
        s.bitmap_format_scanline_unit,
        s.bitmap_format_scanline_pad,
        s.min_keycode,
        s.max_keycode,
        hex(&s.vendor)
    );
    for f in &s.pixmap_formats {
        out.push_str(&format!(
            " format={}/{}/{}",
            f.depth, f.bits_per_pixel, f.scanline_pad
        ));
    }
    for sc in &s.roots {
        out.push_str(&format!(
            " screen=root:{},cmap:{},white:{},black:{},masks:{},px:{}x{},mm:{}x{},maps:{}-{},visual:{},backing:{},save_unders:{},depth:{}",
            sc.root,
            sc.default_colormap,
            sc.white_pixel,
            sc.black_pixel,
            u32::from(sc.current_input_masks),
            sc.width_in_pixels,
            sc.height_in_pixels,
            sc.width_in_millimeters,
            sc.height_in_millimeters,
            sc.min_installed_maps,
            sc.max_installed_maps,
            sc.root_visual,
            u32::from(sc.backing_stores),
            b(sc.save_unders),
            sc.root_depth
        ));
        for d in &sc.allowed_depths {
            out.push_str(&format!(" depth={}", d.depth));
            for v in &d.visuals {
                out.push_str(&format!(
                    " visual={}:{}:{}:{}:{}:{}:{}",
                    v.visual_id,
                    u8::from(v.class),
                    v.bits_per_rgb_value,
                    v.colormap_entries,
                    v.red_mask,
                    v.green_mask,
                    v.blue_mask
                ));
            }
        }
    }
    out
}

fn setups() {
    let visual = |id, class, bits, entries, r, g, b| Visualtype {
        visual_id: id,
        class,
        bits_per_rgb_value: bits,
        colormap_entries: entries,
        red_mask: r,
        green_mask: g,
        blue_mask: b,
    };
    let screen = Screen {
        root: 0x4e3,
        default_colormap: 0x20,
        white_pixel: 0x00ff_ffff,
        black_pixel: 0,
        current_input_masks: EventMask::PROPERTY_CHANGE | EventMask::SUBSTRUCTURE_REDIRECT,
        width_in_pixels: 1280,
        height_in_pixels: 800,
        width_in_millimeters: 338,
        height_in_millimeters: 211,
        min_installed_maps: 1,
        max_installed_maps: 1,
        root_visual: 0x21,
        backing_stores: BackingStore::WHEN_MAPPED,
        save_unders: false,
        root_depth: 24,
        allowed_depths: vec![
            Depth {
                depth: 24,
                visuals: vec![
                    visual(
                        0x21,
                        VisualClass::TRUE_COLOR,
                        8,
                        256,
                        0xff_0000,
                        0xff00,
                        0xff,
                    ),
                    visual(
                        0x22,
                        VisualClass::DIRECT_COLOR,
                        8,
                        256,
                        0xff_0000,
                        0xff00,
                        0xff,
                    ),
                ],
            },
            Depth {
                depth: 1,
                visuals: vec![],
            },
            Depth {
                depth: 32,
                visuals: vec![visual(
                    0x5c,
                    VisualClass::TRUE_COLOR,
                    8,
                    256,
                    0xff_0000,
                    0xff00,
                    0xff,
                )],
            },
        ],
    };
    let mut setup = Setup {
        status: 1,
        protocol_major_version: 11,
        protocol_minor_version: 0,
        length: 0,
        release_number: 12_101_011,
        resource_id_base: 0x0060_0000,
        resource_id_mask: 0x001f_ffff,
        motion_buffer_size: 256,
        maximum_request_length: 65535,
        image_byte_order: ImageOrder::LSB_FIRST,
        bitmap_format_bit_order: ImageOrder::LSB_FIRST,
        bitmap_format_scanline_unit: 32,
        bitmap_format_scanline_pad: 32,
        min_keycode: 8,
        max_keycode: 255,
        vendor: b"The X.Org Foundation".to_vec(),
        pixmap_formats: vec![
            Format {
                depth: 1,
                bits_per_pixel: 1,
                scanline_pad: 32,
            },
            Format {
                depth: 24,
                bits_per_pixel: 32,
                scanline_pad: 32,
            },
            Format {
                depth: 32,
                bits_per_pixel: 32,
                scanline_pad: 32,
            },
        ],
        roots: vec![screen.clone()],
    };
    let bytes = setup.serialize();
    setup.length = ((bytes.len() - 8) / 4) as u16;
    let bytes = setup.serialize();
    reply::<Setup>("setup_xorg", &bytes, render_setup);
    let mut odd = setup.clone();
    odd.vendor = b"Vendor".to_vec();
    odd.roots = vec![
        screen.clone(),
        Screen {
            root: 0x7e4,
            allowed_depths: vec![],
            ..screen
        },
    ];
    let bytes = odd.serialize();
    odd.length = ((bytes.len() - 8) / 4) as u16;
    let bytes = odd.serialize();
    reply::<Setup>("setup_two_screens_odd_vendor", &bytes, render_setup);
    reply::<Setup>("setup_truncated", &bytes[..bytes.len() - 5], render_setup);
}

fn render_selection_request(e: &SelectionRequestEvent) -> String {
    format!(
        "sequence={} time={} owner={} requestor={} selection={} target={} property={}",
        e.sequence, e.time, e.owner, e.requestor, e.selection, e.target, e.property
    )
}

fn event_row<E: TryParse>(name: &str, bytes: &[u8], render: impl Fn(&E) -> String) {
    reply::<E>(name, bytes, render);
}

fn events() {
    let e = SelectionRequestEvent {
        response_type: SELECTION_REQUEST_EVENT,
        sequence: 77,
        time: 999,
        owner: 0x0040_0002,
        requestor: 0x0050_0007,
        selection: 0x1a1,
        target: 0x1a3,
        property: 0x1b1,
    };
    let bytes: [u8; 32] = e.into();
    event_row::<SelectionRequestEvent>("selection_request", &bytes, render_selection_request);
    let mut sent = bytes;
    sent[0] |= 0x80;
    event_row::<SelectionRequestEvent>("selection_request_sent", &sent, render_selection_request);
    let e = SelectionNotifyEvent {
        response_type: SELECTION_NOTIFY_EVENT,
        sequence: 78,
        time: 1000,
        requestor: 0x0040_0002,
        selection: 0x1a1,
        target: 0x1a3,
        property: 0,
    };
    let bytes: [u8; 32] = e.into();
    event_row::<SelectionNotifyEvent>("selection_notify", &bytes, |e| {
        format!(
            "sequence={} time={} requestor={} selection={} target={} property={}",
            e.sequence, e.time, e.requestor, e.selection, e.target, e.property
        )
    });
    let e = SelectionClearEvent {
        response_type: SELECTION_CLEAR_EVENT,
        sequence: 79,
        time: 1001,
        owner: 0x0040_0002,
        selection: 1,
    };
    let bytes: [u8; 32] = e.into();
    event_row::<SelectionClearEvent>("selection_clear", &bytes, |e| {
        format!(
            "sequence={} time={} owner={} selection={}",
            e.sequence, e.time, e.owner, e.selection
        )
    });
    for (name, state) in [
        ("property_notify_new", Property::NEW_VALUE),
        ("property_notify_delete", Property::DELETE),
    ] {
        let e = PropertyNotifyEvent {
            response_type: PROPERTY_NOTIFY_EVENT,
            sequence: 80,
            window: 0x0040_0002,
            atom: 0x1b0,
            time: 1002,
            state,
        };
        let bytes: [u8; 32] = e.into();
        event_row::<PropertyNotifyEvent>(name, &bytes, |e| {
            format!(
                "sequence={} window={} atom={} time={} state={}",
                e.sequence,
                e.window,
                e.atom,
                e.time,
                u8::from(e.state)
            )
        });
    }
    let render_cm = |e: &ClientMessageEvent| {
        let data = match e.format {
            8 => hex(&e.data.as_data8()),
            16 => hex(&e
                .data
                .as_data16()
                .iter()
                .flat_map(|v| v.to_ne_bytes())
                .collect::<Vec<_>>()),
            _ => hex(&e
                .data
                .as_data32()
                .iter()
                .flat_map(|v| v.to_ne_bytes())
                .collect::<Vec<_>>()),
        };
        format!(
            "format={} sequence={} window={} type={} data={}",
            e.format, e.sequence, e.window, e.type_, data
        )
    };
    let e = ClientMessageEvent::new(32, 0x0060_0001, 0x1c0u32, [0x1c1, 1234, 0, 0, 7]);
    let bytes: [u8; 32] = e.into();
    event_row::<ClientMessageEvent>("client_message32", &bytes, render_cm);
    let e = ClientMessageEvent::new(8, 0x0060_0001, 0x1c2u32, *b"twenty bytes of data");
    let bytes: [u8; 32] = e.into();
    event_row::<ClientMessageEvent>("client_message8", &bytes, render_cm);
    event_row::<ClientMessageEvent>("client_message_short", &bytes[..31], render_cm);
}

fn render_size_hints(h: &WmSizeHints) -> String {
    let spec = |s: &WmSizeHintsSpecification| match s {
        WmSizeHintsSpecification::UserSpecified => "user",
        WmSizeHintsSpecification::ProgramSpecified => "program",
    };
    let pair = |p: Option<(i32, i32)>| p.map_or("-".to_owned(), |(a, b)| format!("{a},{b}"));
    format!(
        "position={} size={} min={} max={} inc={} aspect={} base={} gravity={}",
        h.position
            .map_or("-".to_owned(), |(s, x, y)| format!("{}:{x},{y}", spec(&s))),
        h.size
            .map_or("-".to_owned(), |(s, x, y)| format!("{}:{x},{y}", spec(&s))),
        pair(h.min_size),
        pair(h.max_size),
        pair(h.size_increment),
        h.aspect.map_or("-".to_owned(), |(a, b)| format!(
            "{},{}/{},{}",
            a.numerator, a.denominator, b.numerator, b.denominator
        )),
        pair(h.base_size),
        h.win_gravity
            .map_or("-".to_owned(), |g| u32::from(g).to_string()),
    )
}

fn render_hints(h: &WmHints) -> String {
    let opt = |v: Option<u32>| v.map_or("-".to_owned(), |v| v.to_string());
    format!(
        "input={} state={} icon_pixmap={} icon_window={} icon_position={} icon_mask={} group={} urgent={}",
        h.input.map_or("-".to_owned(), |v| b(v).to_string()),
        h.initial_state.map_or("-", |s| match s {
            WmHintsState::Normal => "normal",
            WmHintsState::Iconic => "iconic",
        }),
        opt(h.icon_pixmap),
        opt(h.icon_window),
        h.icon_position
            .map_or("-".to_owned(), |(x, y)| format!("{x},{y}")),
        opt(h.icon_mask),
        opt(h.window_group),
        b(h.urgent)
    )
}

fn icccm() {
    let full = WmSizeHints {
        position: Some((WmSizeHintsSpecification::ProgramSpecified, 10, -20)),
        size: Some((WmSizeHintsSpecification::UserSpecified, 800, 600)),
        min_size: Some((100, 50)),
        max_size: Some((4000, 3000)),
        size_increment: Some((9, 18)),
        aspect: Some((AspectRatio::new(1, 2), AspectRatio::new(3, 1))),
        base_size: Some((20, 40)),
        win_gravity: Some(Gravity::STATIC),
    };
    let sparse = WmSizeHints {
        min_size: Some((200, 100)),
        size_increment: Some((7, 15)),
        base_size: Some((4, 6)),
        ..WmSizeHints::default()
    };
    for (name, h) in [
        ("size_hints_full", full),
        ("size_hints_sparse", sparse),
        ("size_hints_empty", WmSizeHints::new()),
    ] {
        let bytes = h.serialize();
        println!(
            "{name}\t{}\t{}",
            hex(&bytes),
            render_size_hints(&WmSizeHints::try_parse(&bytes).unwrap().0)
        );
    }
    // What a reader makes of bytes it did not write: the pre-ICCCM-1 short
    // form (60 bytes), and a truncated property.
    let legacy = &full.serialize()[..60];
    reply::<WmSizeHints>("size_hints_legacy", legacy, render_size_hints);
    reply::<WmSizeHints>(
        "size_hints_truncated",
        &full.serialize()[..64],
        render_size_hints,
    );
    let hints = WmHints {
        input: Some(true),
        initial_state: Some(WmHintsState::Iconic),
        icon_pixmap: Some(0x0060_0010),
        icon_window: None,
        icon_position: Some((-4, 8)),
        icon_mask: None,
        window_group: Some(0x0060_0001),
        urgent: true,
    };
    let minimal = WmHints {
        input: Some(false),
        ..WmHints::new()
    };
    for (name, h) in [("hints_full", hints), ("hints_input", minimal)] {
        let bytes = h.serialize();
        println!(
            "{name}\t{}\t{}",
            hex(&bytes),
            render_hints(&WmHints::try_parse(&bytes).unwrap().0)
        );
    }
    reply::<WmHints>("hints_legacy", &hints.serialize()[..32], render_hints);
    let mut bad_state = hints.serialize();
    bad_state[8..12].copy_from_slice(&2u32.to_ne_bytes());
    reply::<WmHints>("hints_bad_state", &bad_state, render_hints);
    for (name, value) in [
        ("class_both", &b"aterm\0ATerm\0"[..]),
        ("class_unterminated", b"aterm\0ATerm"),
        ("class_instance_only", b"aterm"),
        ("class_empty", b""),
        ("class_double_nul", b"\0\0"),
    ] {
        let r = GetPropertyReply {
            format: 8,
            sequence: 0,
            length: 0,
            type_: 31,
            bytes_after: 0,
            value_len: value.len() as u32,
            value: value.to_vec(),
        };
        let c = WmClass::from_reply(r).unwrap().unwrap();
        println!(
            "{name}\t{}\tinstance={} class={}",
            hex(value),
            hex(c.instance()),
            hex(c.class())
        );
    }
}

fn xrm() {
    let databases: &[(&str, &[u8])] = &[
        ("xrdb", b"Xft.antialias:\t1\nXft.dpi:\t144\nXft.hinting:\t1\nXft.rgba:\trgb\n*customization:\t-color\n"),
        (
            "precedence",
            b"*dpi: 1\nXft*dpi: 2\nXft.dpi: 3\n?.dpi: 4\nxft.Dpi: 5\nXft.?: 6\n*Xft.dpi: 7\nXft.dpi: 8\n",
        ),
        (
            "classes",
            b"aterm.Background: class\naterm.background: instance\nATerm*Foreground: loose-class\n*foreground: loose\nATerm.font: tight-class\n",
        ),
        (
            "escapes",
            b"a.b: \\  leading space\na.c: tab\\there\na.d: new\\nline\na.e: back\\\\slash\na.f: octal\\101\\1x\\777\na.g: cont \\\ninued\na.h:\\\n   wrapped\na.i: trailing\\",
        ),
        (
            "malformed",
            b"no colon here\n: empty\n?: wildcard last\n.a: ok\n!comment: no\n#include \"nowhere\"\n  a.b  :  spaced\nx.y:\n*.*z: dots\n",
        ),
        ("bytes", b"a.b: \xff\xfe raw\na.c: \xc3\xa9t\xc3\xa9\n"),
        ("empty", b""),
    ];
    let queries: &[(&str, &str)] = &[
        ("Xft.dpi", ""),
        ("Xft.dpi", "Xft.Dpi"),
        ("xft.dpi", "Xft.Dpi"),
        ("Xft.antialias", ""),
        ("Xft.rgba", ""),
        ("Xft.missing", ""),
        ("aterm.customization", "ATerm.Customization"),
        ("aterm.background", "ATerm.Background"),
        ("aterm.foreground", "ATerm.Foreground"),
        ("other.foreground", "Other.Foreground"),
        ("aterm.font", "ATerm.font"),
        ("a.b", ""),
        ("a.c", ""),
        ("a.d", ""),
        ("a.e", ""),
        ("a.f", ""),
        ("a.g", ""),
        ("a.h", ""),
        ("a.i", ""),
        ("a", ""),
        ("x.y", ""),
        ("q.r.z", ""),
        ("", ""),
        ("bad*query", ""),
        ("Xft.?", ""),
    ];
    for (db_name, data) in databases {
        println!("db\t{db_name}\t{}", hex(data));
        let db = Database::new_from_data(data);
        for (name, class) in queries {
            let answer = db
                .get_bytes(name, class)
                .map_or("-".to_owned(), |v| format!("={}", hex(v)));
            println!("q\t{db_name}\t{name}\t{class}\t{answer}");
        }
    }
}

/// The tree `xrm_include` writes: `(path, text)`, `@ROOT@` standing for the
/// tree's absolute path (the replay writes it where it likes).
fn include_tree() -> Vec<(String, String)> {
    let mut files: Vec<(String, String)> = [
        (".Xresources", "Xft.antialias: 0\n#include \"xres/dpi\"\nXft.rgba: bgr\n"),
        ("xres/dpi", "Xft.dpi: 144\n#include \"../shared\"\nXft.antialias: 1\n"),
        ("shared", "*rgba: rgb\nXft.hinting: 1\n"),
        ("order-before", "Xft.dpi: 96\n#include \"xres/dpi\"\n"),
        ("order-after", "#include \"xres/dpi\"\nXft.dpi: 96\n"),
        ("loop", "a.count: once\n#include \"loop\"\na.last: loop\n"),
        ("missing", "#include \"no-such-file\"\nXft.dpi: 120\n"),
        ("spaced", "#  include   \"xres/dpi\"   and the rest\n"),
        (
            "malformed",
            "#include xres/dpi\n#include \"xres/dpi\n#includes \"shared\"\n#include\"shared\"\n#define x\n",
        ),
        ("a-directory", "#include \"xres\"\nXft.dpi: 72\n"),
        ("absolute", "#include \"@ROOT@/shared\"\na.abs: yes\n"),
        ("nested-dir", "#include \"xres/up\"\n"),
        ("xres/up", "#include \"dpi\"\n"),
        ("d000", "a.depth: 0\n#include \"d001\"\n"),
    ]
    .iter()
    .map(|(p, t)| ((*p).to_owned(), (*t).to_owned()))
    .collect();
    // A chain past Xlib's nesting limit: d<k> sets a.depth to k and includes
    // d<k+1>, so the answer names the deepest file followed.
    for k in 1..=102 {
        files.push((
            format!("d{k:03}"),
            format!("a.depth: {k}\n#include \"d{:03}\"\n", k + 1),
        ));
    }
    files
}

fn xrm_include() {
    let root = std::env::temp_dir().join(format!("aterm-x11-recorder-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let root_text = root.to_str().expect("a UTF-8 scratch path");
    for (path, text) in include_tree() {
        let file = root.join(&path);
        std::fs::create_dir_all(file.parent().expect("a parent")).expect("the tree's directory");
        std::fs::write(&file, text.replace("@ROOT@", root_text)).expect("a tree file");
        println!("file\t{path}\t{}", hex(text.as_bytes()));
    }
    let roots = [
        ".Xresources",
        "xres/dpi",
        "order-before",
        "order-after",
        "loop",
        "missing",
        "spaced",
        "malformed",
        "a-directory",
        "absolute",
        "nested-dir",
        "d000",
    ];
    let queries: &[(&str, &str)] = &[
        ("Xft.dpi", ""),
        ("Xft.antialias", ""),
        ("Xft.rgba", ""),
        ("Xft.hinting", ""),
        ("a.depth", ""),
        ("a.count", ""),
        ("a.last", ""),
        ("a.abs", ""),
    ];
    for name in roots {
        let file = root.join(name);
        let data = std::fs::read(&file).expect("a root file");
        let base = file.parent().expect("a parent");
        let db = Database::new_from_data_with_base_directory(&data, base);
        println!("root\t{name}");
        for (q, class) in queries {
            let answer = db
                .get_bytes(q, class)
                .map_or("-".to_owned(), |v| format!("={}", hex(v)));
            println!("q\t{name}\t{q}\t{class}\t{answer}");
        }
    }
    std::fs::remove_dir_all(&root).expect("the scratch tree removed");
}

fn main() {
    match std::env::args().nth(1).as_deref() {
        Some("requests") => requests(),
        Some("replies") => replies(),
        Some("events") => events(),
        Some("icccm") => icccm(),
        Some("xrm") => xrm(),
        Some("xrm-include") => xrm_include(),
        _ => {
            eprintln!("usage: aterm-x11-recorder requests|replies|events|icccm|xrm|xrm-include");
            std::process::exit(2);
        }
    }
}
