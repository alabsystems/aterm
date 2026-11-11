// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;

fn apc(control: &str, payload: &[u8]) -> Vec<u8> {
    let mut v = b"G".to_vec();
    v.extend_from_slice(control.as_bytes());
    if !payload.is_empty() {
        v.push(b';');
        v.extend_from_slice(
            aterm_codec::base64::encode(payload)
                .expect("encode")
                .as_bytes(),
        );
    }
    v
}

#[test]
fn transmit_and_display_png_with_id() {
    let c = parse_kitty_command(&apc("a=T,f=100,i=7", b"hello")).expect("parses");
    assert_eq!(c.action, KittyAction::TransmitAndDisplay);
    assert_eq!(c.format, KittyFormat::Png);
    assert_eq!(c.id, Some(7));
    assert_eq!(c.payload, b"hello");
}

#[test]
fn defaults_when_keys_absent() {
    let c = parse_kitty_command(&apc("", b"raw")).expect("parses");
    assert_eq!(c.action, KittyAction::Transmit);
    assert_eq!(c.format, KittyFormat::Rgba);
    assert_eq!(c.medium, KittyMedium::Direct);
    assert_eq!(c.quiet, 0);
    assert!(!c.more && !c.compressed);
    assert_eq!(c.payload, b"raw");
}

#[test]
fn a_loadable_size_is_kittys() {
    let cases = [
        ("f=32,s=1,v=1", true),
        ("f=24,s=10000,v=10000", true),
        ("f=32", false),
        ("f=32,s=2", false),
        ("f=32,v=2", false),
        ("f=32,s=0,v=2", false),
        ("f=24,s=2,v=0", false),
        ("f=32,s=10001,v=1", false),
        ("f=24,s=1,v=10001", false),
        ("f=100", true),
        ("f=100,s=2,v=2", true),
        ("f=100,s=0", true),
        ("f=100,s=10000,v=10000", true),
        ("f=100,s=10001,v=1", false),
        ("f=100,v=10001", false),
    ];
    for (control, loadable) in cases {
        let c = parse_kitty_command(&apc(control, b"")).expect("parses");
        assert_eq!(c.declares_loadable_size(), loadable, "{control}");
    }
}

#[test]
fn raw_rgb_dimensions_and_medium() {
    let c = parse_kitty_command(&apc("f=24,s=10,v=20,t=f", b"")).expect("parses");
    assert_eq!(c.format, KittyFormat::Rgb);
    assert_eq!((c.width, c.height), (Some(10), Some(20)));
    assert_eq!(c.medium, KittyMedium::File);
    assert!(c.payload.is_empty());
}

#[test]
fn placement_z_index_and_chunking() {
    let c = parse_kitty_command(&apc("a=p,p=3,c=5,r=2,z=-1,m=1,o=z", b"")).expect("parses");
    assert_eq!(c.action, KittyAction::Display);
    assert_eq!(c.placement, Some(3));
    assert_eq!((c.columns, c.rows), (Some(5), Some(2)));
    assert_eq!(c.z_index, Some(-1));
    assert!(c.more);
    assert!(c.compressed);
}

#[test]
fn source_rect_cursor_and_virtual_keys() {
    let c = parse_kitty_command(&apc("a=p,i=4,x=3,y=5,w=7,h=9,C=1,U=1", b"")).expect("parses");
    assert_eq!((c.x, c.y), (Some(3), Some(5)));
    assert_eq!((c.crop_width, c.crop_height), (Some(7), Some(9)));
    assert!(c.cursor_stays && c.virtual_placement);
    let c = parse_kitty_command(&apc("a=p,i=4", b"")).expect("parses");
    assert_eq!(
        (c.x, c.y, c.crop_width, c.crop_height),
        (None, None, None, None)
    );
    assert!(!c.cursor_stays && !c.virtual_placement);
}

#[test]
fn cell_offset_and_relative_placement_keys() {
    let c = parse_kitty_command(&apc("a=p,i=4,X=3,Y=5,P=7,Q=9,H=-2,V=4", b"")).expect("parses");
    assert_eq!((c.cell_x_offset, c.cell_y_offset), (Some(3), Some(5)));
    assert_eq!((c.parent_id, c.parent_placement), (Some(7), Some(9)));
    assert_eq!((c.parent_dx, c.parent_dy), (Some(-2), Some(4)));
    let c = parse_kitty_command(&apc("a=p,i=4,P=0,Q=0", b"")).expect("parses");
    assert_eq!(
        (
            c.parent_id,
            c.parent_placement,
            c.parent_dx,
            c.cell_x_offset
        ),
        (None, None, None, None),
        "P=0/Q=0 name no parent, as i=0 names no image"
    );
}

#[test]
fn zero_ids_mean_unspecified() {
    let c = parse_kitty_command(&apc("a=p,i=0,I=0,p=0", b"")).expect("parses");
    assert_eq!((c.id, c.number, c.placement), (None, None, None));
}

#[test]
fn unknown_keys_ignored_last_value_wins() {
    let c = parse_kitty_command(&apc("zz=99,i=1,i=2,bogus", b"")).expect("parses");
    assert_eq!(c.id, Some(2), "duplicate key takes the last value");
}

#[test]
fn non_g_payload_is_none() {
    assert!(parse_kitty_command(b"X a=t").is_none());
    assert!(parse_kitty_command(b"").is_none());
}

#[test]
fn bad_base64_keeps_command_with_empty_payload() {
    let c = parse_kitty_command(b"Ga=T,i=9;!!!not-base64!!!").expect("parses control");
    assert_eq!(c.action, KittyAction::TransmitAndDisplay);
    assert_eq!(c.id, Some(9));
    assert!(
        c.payload.is_empty(),
        "bad payload -> empty, not a parse failure"
    );
}

#[test]
fn non_utf8_control_is_none() {
    assert!(parse_kitty_command(b"Ga=\xfft").is_none());
}

#[test]
fn compose_rgba_copies_blends_and_clips() {
    let mut under = [0, 0, 255, 255].repeat(4);
    compose_rgba(
        &mut under,
        2,
        (1, 1),
        &[255, 0, 0, 128],
        1,
        (0, 0, 1, 1),
        true,
    );
    assert_eq!(
        &under[12..16],
        &[128, 0, 127, 255],
        "source over an opaque pixel"
    );
    assert_eq!(&under[..4], &[0, 0, 255, 255], "the rest untouched");
    compose_rgba(
        &mut under,
        2,
        (0, 0),
        &[255, 0, 0, 128],
        1,
        (0, 0, 1, 1),
        false,
    );
    assert_eq!(&under[..4], &[255, 0, 0, 128]);
    let mut clear = [0u8; 4];
    compose_rgba(
        &mut clear,
        1,
        (0, 0),
        &[10, 20, 30, 40],
        1,
        (0, 0, 1, 1),
        true,
    );
    assert_eq!(clear, [10, 20, 30, 40]);
    let before = under.clone();
    compose_rgba(&mut under, 2, (2, 0), &[1; 16], 2, (0, 0, 9, 9), false);
    compose_rgba(&mut under, 2, (0, 0), &[1; 16], 2, (2, 2, 9, 9), false);
    assert_eq!(under, before, "nothing lands outside either raster");
    compose_rgba(&mut under, 2, (1, 0), &[9; 16], 2, (0, 0, 9, 9), false);
    assert_eq!(
        &under[4..8],
        &[9; 4],
        "the in-bounds part of a large rectangle lands"
    );
    let after = under.clone();
    compose_rgba(
        &mut under,
        usize::MAX,
        (0, 0),
        &[7; 16],
        usize::MAX,
        (0, 0, 4, 4),
        false,
    );
    assert_eq!(under, after, "overflowing widths clip cleanly");
}

#[test]
fn png_dimensions_reads_ihdr() {
    let mut png = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    png.extend_from_slice(&[0, 0, 0, 13]); // IHDR length
    png.extend_from_slice(b"IHDR");
    png.extend_from_slice(&3u32.to_be_bytes()); // width
    png.extend_from_slice(&5u32.to_be_bytes()); // height
    assert_eq!(png_dimensions(&png), Some((3, 5)));
    assert_eq!(png_dimensions(b"not a png"), None);
    assert_eq!(png_dimensions(b""), None);
}

#[test]
fn rgb_to_rgba_inserts_opaque_alpha() {
    let rgba = rgb_to_rgba(&[1, 2, 3, 4, 5, 6]);
    assert_eq!(rgba, vec![1, 2, 3, 0xff, 4, 5, 6, 0xff]);
    assert_eq!(rgb_to_rgba(&[9, 9]), Vec::<u8>::new());
}
