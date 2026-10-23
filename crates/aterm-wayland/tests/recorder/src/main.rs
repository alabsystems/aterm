// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Wayland oracle's recorder, built outside the aterm workspace (see Cargo.toml):
//! wayland-scanner 0.31.10 generates the C tables from aterm-wayland's protocol files, and this
//! prints them, one row per interface and one per message.

#![allow(non_upper_case_globals, dead_code, clippy::all)]

use std::ffi::CStr;

use wayland_backend::protocol::{wl_interface, wl_message};

mod core {
    wayland_scanner::generate_interfaces!("../../protocols/wayland.xml");
}
mod xdg_shell {
    use crate::core::*;
    wayland_scanner::generate_interfaces!("../../protocols/xdg-shell.xml");
}
mod xdg_decoration {
    use crate::xdg_shell::*;
    wayland_scanner::generate_interfaces!("../../protocols/xdg-decoration-unstable-v1.xml");
}
mod xdg_activation {
    use crate::core::*;
    wayland_scanner::generate_interfaces!("../../protocols/xdg-activation-v1.xml");
}
mod viewporter {
    use crate::core::*;
    wayland_scanner::generate_interfaces!("../../protocols/viewporter.xml");
}
mod fractional_scale {
    use crate::core::*;
    wayland_scanner::generate_interfaces!("../../protocols/fractional-scale-v1.xml");
}
mod tablet {
    use crate::core::*;
    wayland_scanner::generate_interfaces!("tablet-v2.xml");
}
mod cursor_shape {
    use crate::core::*;
    use crate::tablet::*;
    wayland_scanner::generate_interfaces!("../../protocols/cursor-shape-v1.xml");
}
mod text_input {
    use crate::core::*;
    wayland_scanner::generate_interfaces!("../../protocols/text-input-unstable-v3.xml");
}
mod primary_selection {
    use crate::core::*;
    wayland_scanner::generate_interfaces!("../../protocols/primary-selection-unstable-v1.xml");
}

fn sig_args(sig: &[u8]) -> usize {
    sig.iter()
        .filter(|c| matches!(c, b'i' | b'u' | b'f' | b's' | b'o' | b'n' | b'a' | b'h'))
        .count()
}

unsafe fn message(kind: &str, iface: &str, opcode: usize, m: &wl_message) {
    let name = unsafe { CStr::from_ptr(m.name) }.to_str().unwrap();
    let sig = unsafe { CStr::from_ptr(m.signature) }.to_bytes();
    let types: Vec<String> = (0..sig_args(sig))
        .map(|i| {
            let t = unsafe { *m.types.add(i) };
            if t.is_null() {
                "-".to_owned()
            } else {
                unsafe { CStr::from_ptr((*t).name) }
                    .to_str()
                    .unwrap()
                    .to_owned()
            }
        })
        .collect();
    let types = if types.is_empty() {
        "-".to_owned()
    } else {
        types.join(",")
    };
    println!(
        "{kind}\t{iface}\t{opcode}\t{name}\t{}\t{types}",
        String::from_utf8_lossy(sig)
    );
}

fn record(i: &wl_interface) {
    let name = unsafe { CStr::from_ptr(i.name) }.to_str().unwrap();
    println!(
        "iface\t{name}\t{}\t{}\t{}",
        i.version, i.request_count, i.event_count
    );
    for (k, (p, n)) in [
        ("req", (i.requests, i.request_count)),
        ("evt", (i.events, i.event_count)),
    ] {
        for op in 0..n as usize {
            unsafe { message(k, name, op, &*p.add(op)) };
        }
    }
}

fn main() {
    use crate::core::*;
    use crate::cursor_shape::*;
    use crate::fractional_scale::*;
    use crate::primary_selection::*;
    use crate::text_input::*;
    use crate::viewporter::*;
    use crate::xdg_activation::*;
    use crate::xdg_decoration::*;
    use crate::xdg_shell::*;
    for i in [
        &wl_display_interface,
        &wl_registry_interface,
        &wl_callback_interface,
        &wl_compositor_interface,
        &wl_shm_pool_interface,
        &wl_shm_interface,
        &wl_buffer_interface,
        &wl_data_offer_interface,
        &wl_data_source_interface,
        &wl_data_device_interface,
        &wl_data_device_manager_interface,
        &wl_shell_interface,
        &wl_shell_surface_interface,
        &wl_surface_interface,
        &wl_seat_interface,
        &wl_pointer_interface,
        &wl_keyboard_interface,
        &wl_touch_interface,
        &wl_output_interface,
        &wl_region_interface,
        &wl_subcompositor_interface,
        &wl_subsurface_interface,
        &wl_fixes_interface,
        &xdg_wm_base_interface,
        &xdg_positioner_interface,
        &xdg_surface_interface,
        &xdg_toplevel_interface,
        &xdg_popup_interface,
        &zxdg_decoration_manager_v1_interface,
        &zxdg_toplevel_decoration_v1_interface,
        &xdg_activation_v1_interface,
        &xdg_activation_token_v1_interface,
        &wp_viewporter_interface,
        &wp_viewport_interface,
        &wp_fractional_scale_manager_v1_interface,
        &wp_fractional_scale_v1_interface,
        &wp_cursor_shape_manager_v1_interface,
        &wp_cursor_shape_device_v1_interface,
        &zwp_text_input_v3_interface,
        &zwp_text_input_manager_v3_interface,
        &zwp_primary_selection_device_manager_v1_interface,
        &zwp_primary_selection_device_v1_interface,
        &zwp_primary_selection_offer_v1_interface,
        &zwp_primary_selection_source_v1_interface,
    ] {
        record(i);
    }
}
