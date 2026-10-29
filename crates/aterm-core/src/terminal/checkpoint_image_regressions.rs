// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Live image state at replay's payload and clock boundaries.

use super::*;
use crate::terminal::{ClockReading, Terminal};
use std::sync::Arc;

#[test]
fn retained_variant_sources_count_once_after_live_frame_edits_and_deletes() {
    let mut mismatches = Vec::new();
    for (action, command, expected) in [
        (
            "edit",
            &b"\x1b_Ga=f,q=2,f=32,s=1,v=1,i=1,r=1;/wAA/w==\x1b\\"[..],
            20,
        ),
        ("delete", &b"\x1b_Ga=d,q=2,d=f,i=1,r=1\x1b\\"[..], 16),
    ] {
        let mut live = Terminal::new(4, 20);
        live.set_cell_pixel_size(10, 20);
        live.process(
            b"\x1b_Ga=t,q=2,f=32,s=1,v=1,i=1;/////w==\x1b\\\
              \x1b_Ga=f,q=2,f=32,s=1,v=1,i=1;AAAA/w==\x1b\\\
              \x1b_Ga=p,q=2,U=1,i=1,p=1,c=2,r=1\x1b\\",
        );
        assert_eq!(live.transient.kitty_frames[&1].frames.len(), 2);
        assert_eq!(live.transient.kitty_variants[&1].len(), 1);
        let source = Arc::clone(&live.transient.kitty_frames[&1].frames[0].image);
        assert_eq!(source.payload_bytes(), 4);
        assert_eq!(live.capture_replay_carry().retained_image_bytes(), 12);

        live.process(command);
        assert!(
            live.transient.kitty_frames[&1]
                .frames
                .iter()
                .all(|frame| !Arc::ptr_eq(&frame.image, &source)),
            "{action}: the old source is no longer a stored frame"
        );
        assert_eq!(live.transient.kitty_variants[&1].len(), 2);
        assert!(Arc::ptr_eq(
            &live.transient.kitty_variants[&1][0].source,
            &source
        ));
        let carry = live.capture_replay_carry();
        assert!(
            Arc::strong_count(&source) >= 3,
            "carry retains the old source"
        );
        let actual = carry.retained_image_bytes();
        if actual != expected {
            mismatches.push((action, actual, expected));
        }
    }
    assert!(
        mismatches.is_empty(),
        "retained image bytes: {mismatches:?}"
    );
}

fn png_over_kitty_limit() -> Vec<u8> {
    let mut png = Vec::new();
    {
        let mut encoder = aterm_png::Encoder::new(&mut png, 1, 1);
        encoder.set_color(aterm_png::ColorType::Rgba);
        let mut writer = encoder.write_header().expect("PNG header");
        writer
            .write_image_data(&[255, 0, 0, 255])
            .expect("PNG pixels");
        writer.finish().expect("PNG end");
    }
    let end = png.split_off(png.len() - 12);
    assert_eq!(&end[4..8], b"IEND");
    let padding = vec![0u8; super::super::handler::MAX_KITTY_IMAGE_BYTES];
    png.extend_from_slice(
        &u32::try_from(padding.len())
            .expect("chunk size")
            .to_be_bytes(),
    );
    png.extend_from_slice(b"paDd");
    png.extend_from_slice(&padding);
    let mut crc = aterm_codec::crc32::Crc32::new();
    crc.update(b"paDd");
    crc.update(&padding);
    png.extend_from_slice(&crc.finish().to_be_bytes());
    png.extend_from_slice(&end);
    png
}

#[test]
fn a_live_png_above_the_kitty_limit_roundtrips_its_loose_carry() {
    let png = png_over_kitty_limit();
    let encoded = aterm_codec::base64::encode(&png).expect("base64 PNG");
    let mut live = Terminal::new(4, 20);
    live.set_cell_pixel_size(10, 20);
    live.process(format!("\x1b]1337;File=inline=1;width=1;height=1:{encoded}\x1b\\").as_bytes());
    let carry = PaneStepCarry::capture(&live, aterm_time::Instant::now(), 1000);
    assert_eq!(carry.loose_images.len(), 1, "live OSC admitted the image");
    assert_eq!(carry.loose_images[0].format, ImageFormat::Png);
    assert_eq!(carry.loose_images[0].bytes, png);
    assert!(carry.loose_images[0].bytes.len() > super::super::handler::MAX_KITTY_IMAGE_BYTES);
    let bytes = carry.canonical();
    let decoded = PaneStepCarry::from_canonical(&bytes).expect("live loose PNG carry");
    assert_eq!(decoded, carry);
}

#[test]
fn a_live_sixel_above_the_kitty_limit_roundtrips_its_loose_carry() {
    let mut live = Terminal::new(4, 20);
    live.set_cell_pixel_size(10, 20);
    live.process(b"\x1bP0;0;0q\"1;1;1025;1025#1;2;100;0;0~\x1b\\");
    let carry = PaneStepCarry::capture(&live, aterm_time::Instant::now(), 1000);
    assert_eq!(
        carry.loose_images.len(),
        1,
        "live Sixel admitted the raster"
    );
    assert_eq!(
        carry.loose_images[0].format,
        ImageFormat::RawRgba8 {
            width: 1025,
            height: 1025,
        }
    );
    assert_eq!(carry.loose_images[0].bytes.len(), 1025 * 1025 * 4);
    assert!(carry.loose_images[0].bytes.len() > super::super::handler::MAX_KITTY_IMAGE_BYTES);
    let bytes = carry.canonical();
    let decoded = PaneStepCarry::from_canonical(&bytes).expect("live loose Sixel carry");
    assert_eq!(decoded, carry);
}

#[test]
fn a_quiet_animation_keeps_its_start_before_a_later_chain_origin() {
    use std::time::Duration;

    let start = aterm_time::Instant::now();
    let clock = |ms| ClockReading {
        monotonic: start + Duration::from_millis(ms),
        wall_ms: None,
    };
    let mut live = Terminal::new(4, 20);
    live.set_cell_pixel_size(10, 20);
    live.process_at(
        b"\x1b_Ga=t,q=2,f=32,s=1,v=1,i=1;/////w==\x1b\\\
          \x1b_Ga=f,q=2,f=32,s=1,v=1,i=1,z=40;AAAA/w==\x1b\\\
          \x1b_Ga=p,q=2,i=1,c=1,r=1\x1b\\\
          \x1b_Ga=a,q=2,i=1,r=1,z=40,s=3\x1b\\",
        clock(0),
    );
    assert!(live.kitty_animation_playing());
    assert!(
        live.take_response().is_none(),
        "quiet transfer seeds no reply clock"
    );
    let checkpoint = live.checkpoint_pane_step(1000).expect("ground");
    let replay_carry = live.capture_replay_carry();
    let first_slice = clock(50);
    let origin = replay_carry
        .earliest_instant()
        .map_or(first_slice.monotonic, |at| at.min(first_slice.monotonic));
    let carry = PaneStepCarry::from_replay_carry(
        &replay_carry,
        &checkpoint,
        &TerminalConfig::default(),
        None,
        origin,
    );
    let decoded = PaneStepCarry::from_canonical(&carry.canonical()).expect("quiet carry");
    let mut replayed = Terminal::replay_from_checkpoint(&checkpoint);
    replayed.set_cell_pixel_size(10, 20);
    replayed.apply_pane_step_carry(&decoded, origin);
    live.process_at(b"\x1b[5n", first_slice);
    replayed.process_at(b"\x1b[5n", first_slice);
    assert_eq!(replayed.take_response(), live.take_response());
    let expected = live.advance_kitty_animations(clock(55).monotonic);
    assert!(expected.advanced, "the original 40ms frame is due");
    assert_eq!(
        replayed.advance_kitty_animations(clock(55).monotonic),
        expected,
        "restoration must retain the original animation start"
    );
    assert_eq!(replay_carry.earliest_instant(), Some(start));
}

#[test]
fn hot_tier_scrollback_inline_images_roundtrip_pane_step_carry_and_unscroll() {
    let origin = aterm_time::Instant::now();
    let mut live = Terminal::new(20, 20);
    live.set_cell_pixel_size(10, 20);
    live.process(b"\x1bP0;0;0q\"1;1;10;20#1;2;100;0;0~\x1b\\img-row\r\n");
    for idx in 0..1_520 {
        live.process(format!("fill-{idx:04}\r\n").as_bytes());
    }
    let checkpoint = live.checkpoint_pane_step(2_048).expect("ground");
    let carry = PaneStepCarry::capture(&live, origin, 2_048);
    assert!(
        carry
            .active_images
            .history_rev
            .iter()
            .any(|&(rev, ref spans)| (1_000..2_000).contains(&rev) && !spans.is_empty()),
        "hot-tier history image in 1000..2000 must be captured: {:?}",
        carry.active_images.history_rev
    );
    let decoded =
        PaneStepCarry::from_canonical(&carry.canonical()).expect("hot-tier history image carry");
    assert_eq!(decoded, carry);

    let mut replayed = Terminal::replay_from_checkpoint(&checkpoint);
    replayed.set_cell_pixel_size(10, 20);
    replayed.apply_pane_step_carry(&decoded, origin);
    for _ in 0..76 {
        live.process(b"\x1b[20+T");
        replayed.process(b"\x1b[20+T");
    }
    assert_eq!(replayed.checkpoint_visible(), live.checkpoint_visible());
    let live_after = PaneStepCarry::capture(&live, origin, 2_048);
    let replayed_after = PaneStepCarry::capture(&replayed, origin, 2_048);
    assert!(
        !replayed_after.active_images.visible.is_empty(),
        "unscrolled hot-tier image must reappear on the visible viewport"
    );
    assert_eq!(
        replayed_after.active_images.visible,
        live_after.active_images.visible
    );
}
