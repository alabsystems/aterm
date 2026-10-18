// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! aterm-x11's reply, setup and event parsers against x11rb-protocol
//! 0.13.2's, recorded in tests/recorded/: each packet parses to the fields
//! x11rb-protocol read from it, and each packet it refused is refused.
//!
//! Negative controls: every recording has a case and every case a
//! recording; every packet that parses stops parsing one byte short, and
//! stops being a reply with its packet type changed.

#![cfg(target_endian = "little")]

mod support;

use std::collections::BTreeMap;

use aterm_x11::proto::event::Event;
use aterm_x11::proto::xproto::*;
use aterm_x11::proto::{ParseError, Reply, randr, shm, xinput};
use support::*;

const REPLIES: &str = "x11rb-protocol-0.13.2-replies.tsv";
const EVENTS: &str = "x11rb-protocol-0.13.2-events.tsv";

fn list(v: &[u32]) -> String {
    if v.is_empty() {
        return "-".to_owned();
    }
    v.iter().map(u32::to_string).collect::<Vec<_>>().join(",")
}

fn render_modes(modes: &[randr::ModeInfo]) -> String {
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
                m.mode_flags
            )
        })
        .collect::<Vec<_>>()
        .join(",")
}

fn render<R: Reply>(bytes: &[u8], f: impl Fn(&R) -> String) -> String {
    R::parse(bytes).map_or_else(|_| "ERR".to_owned(), |r| f(&r))
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
        s.image_byte_order,
        s.bitmap_format_bit_order,
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
            sc.current_input_masks,
            sc.width_in_pixels,
            sc.height_in_pixels,
            sc.width_in_millimeters,
            sc.height_in_millimeters,
            sc.min_installed_maps,
            sc.max_installed_maps,
            sc.root_visual,
            sc.backing_stores,
            b(sc.save_unders),
            sc.root_depth
        ));
        for d in &sc.allowed_depths {
            out.push_str(&format!(" depth={}", d.depth));
            for v in &d.visuals {
                out.push_str(&format!(
                    " visual={}:{}:{}:{}:{}:{}:{}",
                    v.visual_id,
                    v.class,
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

/// What aterm-x11 reads from the packet a reply case names.
fn parse_reply(case: &str, bytes: &[u8]) -> String {
    let atom = |r: &InternAtomReply| format!("atom={}", r.atom);
    match case {
        "intern_atom"
        | "intern_atom_short"
        | "intern_atom_not_a_reply"
        | "intern_atom_length_past_end" => render(bytes, atom),
        c if c.starts_with("get_property") => render(bytes, render_property),
        "get_geometry" => render(bytes, |r: &GetGeometryReply| {
            format!(
                "depth={} root={} x={} y={} width={} height={} border_width={}",
                r.depth, r.root, r.x, r.y, r.width, r.height, r.border_width
            )
        }),
        "get_window_attributes" => render(bytes, |r: &GetWindowAttributesReply| {
            format!(
                "backing_store={} visual={} class={} bit_gravity={} win_gravity={} \
                 backing_planes={} backing_pixel={} save_under={} map_is_installed={} \
                 map_state={} override_redirect={} colormap={} all_event_masks={} \
                 your_event_mask={} do_not_propagate_mask={}",
                r.backing_store,
                r.visual,
                r.class,
                r.bit_gravity,
                r.win_gravity,
                r.backing_planes,
                r.backing_pixel,
                b(r.save_under),
                b(r.map_is_installed),
                r.map_state,
                b(r.override_redirect),
                r.colormap,
                r.all_event_masks,
                r.your_event_mask,
                r.do_not_propagate_mask
            )
        }),
        "query_tree" => render(bytes, |r: &QueryTreeReply| {
            format!(
                "root={} parent={} children={}",
                r.root,
                r.parent,
                list(&r.children)
            )
        }),
        "get_atom_name" => render(bytes, |r: &GetAtomNameReply| {
            format!("name={}", hex(&r.name))
        }),
        "get_selection_owner" => render(bytes, |r: &GetSelectionOwnerReply| {
            format!("owner={}", r.owner)
        }),
        "grab_pointer" => render(bytes, |r: &GrabPointerReply| format!("status={}", r.status)),
        "query_pointer" => render(bytes, |r: &QueryPointerReply| {
            format!(
                "same_screen={} root={} child={} root_x={} root_y={} win_x={} win_y={} mask={}",
                b(r.same_screen),
                r.root,
                r.child,
                r.root_x,
                r.root_y,
                r.win_x,
                r.win_y,
                r.mask
            )
        }),
        "translate_coordinates" => render(bytes, |r: &TranslateCoordinatesReply| {
            format!(
                "same_screen={} child={} dst_x={} dst_y={}",
                b(r.same_screen),
                r.child,
                r.dst_x,
                r.dst_y
            )
        }),
        "get_input_focus" => render(bytes, |r: &GetInputFocusReply| {
            format!("revert_to={} focus={}", r.revert_to, r.focus)
        }),
        "query_keymap" => render(bytes, |r: &QueryKeymapReply| {
            format!("keys={}", hex(&r.keys))
        }),
        "get_image" => render(bytes, |r: &GetImageReply| {
            format!(
                "depth={} visual={} data={}",
                r.depth,
                r.visual,
                hex(&r.data)
            )
        }),
        "query_extension_present" | "query_extension_absent" => {
            render(bytes, |r: &QueryExtensionReply| {
                format!(
                    "present={} major_opcode={} first_event={} first_error={}",
                    b(r.present),
                    r.major_opcode,
                    r.first_event,
                    r.first_error
                )
            })
        }
        "get_modifier_mapping" => render(bytes, |r: &GetModifierMappingReply| {
            format!(
                "keycodes_per_modifier={} keycodes={}",
                r.keycodes_per_modifier,
                hex(&r.keycodes)
            )
        }),
        "shm_query_version" => render(bytes, |r: &shm::QueryVersionReply| {
            format!(
                "shared_pixmaps={} major_version={} minor_version={} uid={} gid={} pixmap_format={}",
                b(r.shared_pixmaps),
                r.major_version,
                r.minor_version,
                r.uid,
                r.gid,
                r.pixmap_format
            )
        }),
        "randr_query_version" => render(bytes, |r: &randr::QueryVersionReply| {
            format!(
                "major_version={} minor_version={}",
                r.major_version, r.minor_version
            )
        }),
        "randr_get_screen_resources"
        | "randr_screen_resources_short"
        | "randr_get_screen_resources_current" => {
            render(bytes, |r: &randr::ScreenResourcesReply| {
                format!(
                    "timestamp={} config_timestamp={} crtcs={} outputs={} modes={} names={}",
                    r.timestamp,
                    r.config_timestamp,
                    list(&r.crtcs),
                    list(&r.outputs),
                    render_modes(&r.modes),
                    hex(&r.names)
                )
            })
        }
        "randr_get_output_info" => render(bytes, |r: &randr::GetOutputInfoReply| {
            format!(
                "status={} timestamp={} crtc={} mm={}x{} connection={} subpixel={} num_preferred={} \
                 crtcs={} modes={} clones={} name={}",
                r.status,
                r.timestamp,
                r.crtc,
                r.mm_width,
                r.mm_height,
                r.connection,
                r.subpixel_order,
                r.num_preferred,
                list(&r.crtcs),
                list(&r.modes),
                list(&r.clones),
                hex(&r.name)
            )
        }),
        "randr_get_crtc_info" => render(bytes, |r: &randr::GetCrtcInfoReply| {
            format!(
                "status={} timestamp={} x={} y={} width={} height={} mode={} rotation={} \
                 rotations={} outputs={} possible={}",
                r.status,
                r.timestamp,
                r.x,
                r.y,
                r.width,
                r.height,
                r.mode,
                r.rotation,
                r.rotations,
                list(&r.outputs),
                list(&r.possible)
            )
        }),
        "randr_set_crtc_config" => render(bytes, |r: &randr::SetCrtcConfigReply| {
            format!("status={} timestamp={}", r.status, r.timestamp)
        }),
        "randr_get_output_primary" => render(bytes, |r: &randr::GetOutputPrimaryReply| {
            format!("output={}", r.output)
        }),
        "xi_query_version" => render(bytes, |r: &xinput::XIQueryVersionReply| {
            format!(
                "major_version={} minor_version={}",
                r.major_version, r.minor_version
            )
        }),
        "xi_query_pointer" => render(bytes, |r: &xinput::XIQueryPointerReply| {
            format!(
                "root={} child={} root_x={} root_y={} win_x={} win_y={} same_screen={} \
                 mods={},{},{},{} group={},{},{},{} buttons={}",
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
        }),
        c if c.starts_with("setup") => {
            Setup::parse(bytes).map_or_else(|_| "ERR".to_owned(), |s| render_setup(&s))
        }
        other => format!("NO CASE for {other}"),
    }
}

fn parse_event(case: &str, bytes: &[u8]) -> String {
    let Ok(event) = Event::parse(bytes) else {
        return "ERR".to_owned();
    };
    match (case, event) {
        ("selection_request" | "selection_request_sent", Event::SelectionRequest(e)) => format!(
            "sequence={} time={} owner={} requestor={} selection={} target={} property={}",
            e.sequence, e.time, e.owner, e.requestor, e.selection, e.target, e.property
        ),
        ("selection_notify", Event::SelectionNotify(e)) => format!(
            "sequence={} time={} requestor={} selection={} target={} property={}",
            e.sequence, e.time, e.requestor, e.selection, e.target, e.property
        ),
        ("selection_clear", Event::SelectionClear(e)) => format!(
            "sequence={} time={} owner={} selection={}",
            e.sequence, e.time, e.owner, e.selection
        ),
        ("property_notify_new" | "property_notify_delete", Event::PropertyNotify(e)) => format!(
            "sequence={} window={} atom={} time={} state={}",
            e.sequence, e.window, e.atom, e.time, e.state
        ),
        (
            "client_message32" | "client_message8" | "client_message_short",
            Event::ClientMessage(e),
        ) => {
            format!(
                "format={} sequence={} window={} type={} data={}",
                e.format,
                e.sequence,
                e.window,
                e.type_,
                hex(&e.data)
            )
        }
        (case, event) => format!("{case} parsed as {event:?}"),
    }
}

fn recorded(name: &str) -> BTreeMap<String, (Vec<u8>, String)> {
    by_case(name)
        .into_iter()
        .map(|(case, rest)| (case, (unhex(&rest[0]), rest[1].clone())))
        .collect()
}

#[test]
fn every_reply_and_setup_parses_as_x11rb_protocol_parsed_it() {
    let rec = recorded(REPLIES);
    assert!(rec.len() >= 38, "the corpus is whole: {}", rec.len());
    let want: BTreeMap<_, _> = rec
        .iter()
        .map(|(c, (_, f))| (c.clone(), f.clone()))
        .collect();
    let got: BTreeMap<_, _> = rec
        .iter()
        .map(|(c, (bytes, _))| (c.clone(), parse_reply(c, bytes)))
        .collect();
    same_cases(REPLIES, &want, &got);
    assert!(
        want.values().filter(|f| *f == "ERR").count() >= 5,
        "the refusals are recorded too"
    );
}

#[test]
fn every_event_parses_as_x11rb_protocol_parsed_it() {
    let rec = recorded(EVENTS);
    assert!(rec.len() >= 9, "the corpus is whole: {}", rec.len());
    let want: BTreeMap<_, _> = rec
        .iter()
        .map(|(c, (_, f))| (c.clone(), f.clone()))
        .collect();
    let got: BTreeMap<_, _> = rec
        .iter()
        .map(|(c, (bytes, _))| (c.clone(), parse_event(c, bytes)))
        .collect();
    same_cases(EVENTS, &want, &got);
}

/// Negative control: the parsers check what they are given. Every packet
/// that parses is refused one byte short, and a reply is refused under any
/// packet type but 1.
#[test]
fn a_packet_one_byte_short_or_retyped_is_refused() {
    for (case, (bytes, fields)) in recorded(REPLIES) {
        if fields == "ERR" {
            continue;
        }
        let short = &bytes[..bytes.len() - 1];
        assert_eq!(
            parse_reply(&case, short),
            "ERR",
            "{case}: one byte short must not parse"
        );
        if !case.starts_with("setup") {
            let mut retyped = bytes.clone();
            retyped[0] = 2;
            assert_eq!(parse_reply(&case, &retyped), "ERR", "{case}: type 2");
        }
    }
    for (case, (bytes, fields)) in recorded(EVENTS) {
        if fields == "ERR" {
            continue;
        }
        assert_eq!(
            Event::parse(&bytes[..31]).err(),
            Some(ParseError::Short),
            "{case}"
        );
    }
}

#[test]
fn an_error_packet_reads_its_request() {
    let mut packet = [0u8; 32];
    packet[1] = 3; // BadWindow
    packet[2..4].copy_from_slice(&41u16.to_ne_bytes());
    packet[4..8].copy_from_slice(&0x0060_0001u32.to_ne_bytes());
    packet[8..10].copy_from_slice(&0u16.to_ne_bytes());
    packet[10] = 20; // GetProperty
    let Event::Error(e) = Event::parse(&packet).expect("an error packet") else {
        panic!("not an error");
    };
    assert_eq!(
        (
            e.error_code,
            e.sequence,
            e.bad_value,
            e.minor_opcode,
            e.major_opcode
        ),
        (3, 41, 0x0060_0001, 0, 20)
    );
}
