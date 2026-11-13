// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_x11::proto::xproto::{Depth, Format, Screen, Visualtype};

fn setup() -> Setup {
    let visual = |id| Visualtype {
        visual_id: id,
        class: visual_class::TRUE_COLOR,
        bits_per_rgb_value: 8,
        colormap_entries: 256,
        red_mask: 0x00ff_0000,
        green_mask: 0x0000_ff00,
        blue_mask: 0x0000_00ff,
    };
    Setup {
        status: 1,
        protocol_major_version: 11,
        protocol_minor_version: 0,
        release_number: 0,
        resource_id_base: 0,
        resource_id_mask: 0,
        motion_buffer_size: 0,
        maximum_request_length: 65535,
        image_byte_order: if cfg!(target_endian = "little") {
            image_order::LSB_FIRST
        } else {
            image_order::MSB_FIRST
        },
        bitmap_format_bit_order: 0,
        bitmap_format_scanline_unit: 32,
        bitmap_format_scanline_pad: 32,
        min_keycode: 8,
        max_keycode: 255,
        vendor: Vec::new(),
        pixmap_formats: vec![
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
        roots: vec![Screen {
            root: 1,
            default_colormap: 0,
            white_pixel: 0,
            black_pixel: 0,
            current_input_masks: 0,
            width_in_pixels: 80,
            height_in_pixels: 24,
            width_in_millimeters: 0,
            height_in_millimeters: 0,
            min_installed_maps: 0,
            max_installed_maps: 0,
            root_visual: 0x24,
            backing_stores: 0,
            save_unders: false,
            root_depth: 24,
            allowed_depths: vec![
                Depth {
                    depth: 24,
                    visuals: vec![visual(0x24)],
                },
                Depth {
                    depth: 32,
                    visuals: vec![visual(0x32)],
                },
            ],
        }],
    }
}

/// A post sends what changed only where the segment still holds the
/// last post; a fresh or resized one is sent whole.
#[test]
fn a_post_sends_its_damage_only_over_the_last_post() {
    let damage = Some([10, 40, 900, 70]);
    assert_eq!(
        post_rect(damage, true, (800, 600)),
        [10, 40, 800, 70],
        "the damage, inside the frame"
    );
    assert_eq!(
        post_rect(damage, false, (800, 600)),
        [0, 0, 800, 600],
        "a segment holding no post at this size is sent whole"
    );
    assert_eq!(post_rect(None, true, (800, 600)), [0, 0, 800, 600]);
    assert_eq!(
        post_rect(Some([0, 0, 0, 0]), true, (800, 600)),
        [0, 0, 0, 0],
        "nothing changed, nothing sent"
    );
}

/// Formats, visuals and the observed window depth must describe one layout.
#[test]
fn the_windows_depth_owns_its_pixel_format_and_visual() {
    let ordinary = setup();
    assert!(visual_supported(&ordinary, 0x24, 24));
    assert!(visual_supported(&ordinary, 0x32, 32));
    assert!(!visual_supported(&ordinary, 0xff, 24));
    let mut wrong_masks = ordinary.clone();
    wrong_masks.roots[0].allowed_depths[0].visuals[0].red_mask ^= 1;
    assert!(!visual_supported(&wrong_masks, 0x24, 24));
    let mut wrong_class = ordinary.clone();
    wrong_class.roots[0].allowed_depths[0].visuals[0].class = u8::MAX;
    assert!(!visual_supported(&wrong_class, 0x24, 24));
    let mut absent = ordinary.clone();
    absent.pixmap_formats.clear();
    assert!(!visual_supported(&absent, 0x24, 24));

    let mut admitted = Vec::new();
    let mut packed24 = ordinary.clone();
    packed24.pixmap_formats[0].bits_per_pixel = 24;
    if visual_supported(&packed24, 0x24, 24) {
        admitted.push("24-bit format borrowed from depth32");
    }
    let mut packed32 = ordinary.clone();
    packed32.pixmap_formats[1].bits_per_pixel = 24;
    if visual_supported(&packed32, 0x32, 32) {
        admitted.push("32-bit format borrowed from depth24");
    }
    if visual_supported(&ordinary, 0x24, 32) {
        admitted.push("visual from another depth");
    }
    if visual_supported(&ordinary, 0x24, 16) {
        admitted.push("unsupported window depth");
    }
    assert!(
        admitted.is_empty(),
        "X11_WINDOW_DEPTH_MUST_OWN_ITS_LAYOUT: {admitted:?}"
    );
}
