// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use crate::terminal::Terminal;
use crate::terminal::kitty_graphics::zlib_zeros;

#[test]
fn rgba_frame_size_rejects_oversize_and_32_bit_overflow_dimensions() {
    assert_eq!(super::kitty_rgba_len(1, 1), Ok(4));
    assert_eq!(super::kitty_rgba_len(1024, 1024), Ok(4 * 1024 * 1024));
    for (width, height) in [(1024, 1025), (32768, 32768), (u16::MAX, u16::MAX)] {
        assert_eq!(
            super::kitty_rgba_len(width, height),
            Err("ENOSPC:frame too large")
        );
    }
}

fn apc_g(control: &str, raw_payload: &[u8]) -> Vec<u8> {
    let mut v = b"\x1b_G".to_vec();
    v.extend_from_slice(control.as_bytes());
    if !raw_payload.is_empty() {
        v.push(b';');
        v.extend_from_slice(
            aterm_codec::base64::encode(raw_payload)
                .expect("encode")
                .as_bytes(),
        );
    }
    v.extend_from_slice(b"\x1b\\");
    v
}

fn one_cell_rgba() -> Vec<u8> {
    vec![0u8; 10 * 20 * 4]
}

fn shown_frame(term: &mut Terminal) -> Option<u8> {
    term.cell_frame(24, 80).images[0]
        .first()
        .map(|(_, iref)| iref.image.bytes[0])
}

fn three_frames(shown: u32) -> Terminal {
    let mut term = Terminal::new(24, 80);
    term.set_cell_pixel_size(10, 20);
    term.process(&apc_g("a=t,f=32,s=10,v=20,i=7", &[1u8; 800]));
    term.process(&apc_g("a=f,f=32,s=10,v=20,i=7", &[2u8; 800]));
    term.process(&apc_g("a=f,f=32,s=10,v=20,i=7", &[3u8; 800]));
    term.process(&apc_g(&format!("a=a,i=7,c={shown}"), b""));
    let mut seq = b"\x1b[38;5;7m".to_vec();
    let mut buf = [0u8; 4];
    for c in ['\u{10EEEE}', '\u{0305}', '\u{0305}'] {
        seq.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
    }
    term.process(&seq);
    term
}

fn placed_frame(term: &Terminal) -> Option<u8> {
    image_at(term, 0, 0).map(|image| image.image.bytes[0])
}

fn clock() -> impl Fn(u64) -> crate::terminal::ClockReading {
    let base = crate::terminal::ClockReading::now();
    move |ms: u64| crate::terminal::ClockReading {
        monotonic: base
            .monotonic
            .checked_add(std::time::Duration::from_millis(ms))
            .expect("clock"),
        wall_ms: None,
    }
}

fn film() -> (Terminal, impl Fn(u64) -> crate::terminal::ClockReading) {
    let at = clock();
    let mut term = Terminal::new(24, 80);
    term.set_cell_pixel_size(10, 20);
    for control in [
        "a=t,f=32,s=10,v=20,i=7,q=2",
        "a=f,f=32,s=10,v=20,i=7,z=100,q=2",
        "a=f,f=32,s=10,v=20,i=7,z=-1,q=2",
        "a=f,f=32,s=10,v=20,i=7,q=2",
    ] {
        let pixel = u8::try_from(
            term.transient
                .kitty_frames
                .get(&7)
                .map_or(0, |a| a.frames.len()),
        )
        .expect("few frames")
            + 1;
        term.process_at(&apc_g(control, &[pixel; 800]), at(0));
    }
    term.process_at(&apc_g("a=a,i=7,r=1,z=50", b""), at(0));
    term.process_at(&apc_g("a=p,i=7,C=1,q=2", b""), at(0));
    (term, at)
}

#[test]
fn a_running_animation_steps_its_frames_on_their_gaps() {
    use crate::terminal::KittyAnimationStep;
    let (mut term, at) = film();
    let step = |advanced, due: Option<u64>| KittyAnimationStep {
        advanced,
        next_frame_at: due.map(|ms| at(ms).monotonic),
    };
    assert_eq!(
        term.advance_kitty_animations(at(1000).monotonic),
        KittyAnimationStep::default(),
        "stopped: nothing plays, no wake"
    );
    assert_eq!(placed_frame(&term), Some(1));

    term.process_at(&apc_g("a=a,i=7,s=3", b""), at(1000));
    assert_eq!(reply(&mut term), "", "a=a is not answered");
    let mut play = |ms: u64| term.advance_kitty_animations(at(ms).monotonic);
    assert_eq!(play(1010), step(false, Some(1050)), "the root's 50 ms");
    assert_eq!(play(1050), step(true, Some(1150)), "frame 2, for 100 ms");
    assert_eq!(play(1149), step(false, Some(1150)));
    assert_eq!(play(1160), step(true, Some(1200)), "gapless 3 skipped: 4");
    assert_eq!(play(1200), step(true, Some(1250)), "round to the root");
    assert_eq!(placed_frame(&term), Some(1));

    term.process_at(&apc_g("a=a,i=7,s=1", b""), at(1210));
    assert_eq!(
        term.advance_kitty_animations(at(5000).monotonic),
        KittyAnimationStep::default()
    );
    assert_eq!(placed_frame(&term), Some(1));

    term.process_at(&apc_g("a=a,i=7,s=3", b""), at(6000));
    term.process_at(&apc_g("a=d,d=i,i=7", b""), at(6000));
    assert_eq!(
        term.advance_kitty_animations(at(9000).monotonic),
        KittyAnimationStep::default()
    );

    term.process_at(&apc_g("a=a,i=99,s=3", b""), at(9000));
    assert_eq!(reply(&mut term), "\x1b_Gi=99;ENOENT:no such image\x1b\\");
}

#[test]
fn loop_counts_and_loading_rest_on_the_last_frame() {
    use crate::terminal::KittyAnimationStep;
    let (mut term, at) = film();
    term.process_at(&apc_g("a=a,i=7,s=3,v=2", b""), at(0));
    let mut play = |ms: u64| term.advance_kitty_animations(at(ms).monotonic);
    assert!(play(50).advanced, "frame 2");
    assert!(play(150).advanced, "frame 4");
    let rest = play(190);
    assert_eq!(
        rest,
        KittyAnimationStep::default(),
        "one loop played: it rests on the last frame"
    );
    assert_eq!(placed_frame(&term), Some(4));

    term.process_at(&apc_g("a=a,i=7,s=2", b""), at(1000));
    let mut play = |ms: u64| term.advance_kitty_animations(at(ms).monotonic);
    assert!(!play(1010).advanced, "loading: still resting on the last");
    term.process_at(
        &apc_g("a=f,f=32,s=10,v=20,i=7,z=30,q=2", &[5u8; 800]),
        at(1020),
    );
    assert_eq!(
        term.advance_kitty_animations(at(1030).monotonic),
        KittyAnimationStep {
            advanced: true,
            next_frame_at: Some(at(1060).monotonic),
        },
        "the new frame plays"
    );
    assert_eq!(placed_frame(&term), Some(5));
}

#[test]
fn a_zero_state_changes_nothing() {
    use crate::terminal::KittyAnimationStep;
    let (mut term, at) = film();
    term.process_at(&apc_g("a=a,i=7,s=3,v=3", b""), at(0));
    let mut play = |ms: u64| term.advance_kitty_animations(at(ms).monotonic);
    for ms in [50, 150, 190] {
        assert!(play(ms).advanced, "the first time round, at {ms} ms");
    }
    term.process_at(&apc_g("a=a,i=7,s=0", b""), at(200));
    let mut play = |ms: u64| term.advance_kitty_animations(at(ms).monotonic);
    assert!(play(240).advanced && play(340).advanced, "the second");
    assert_eq!(play(380), KittyAnimationStep::default(), "then rests");
    assert_eq!(placed_frame(&term), Some(4));
}

#[test]
fn frame_and_animation_errors_name_the_commands_frame() {
    let mut term = Terminal::new(24, 80);
    term.process(&apc_g("a=a,i=99,r=2,z=50", b""));
    assert_eq!(
        reply(&mut term),
        "\x1b_Gi=99,r=2;ENOENT:no such image\x1b\\"
    );
    term.process(&apc_g("a=f,f=32,s=1,v=1,i=99,r=3", &[0u8; 4]));
    assert_eq!(
        reply(&mut term),
        "\x1b_Gi=99,r=3;ENOENT:no such image\x1b\\"
    );
    term.process(&apc_g("a=a,i=99,s=3", b""));
    assert_eq!(
        reply(&mut term),
        "\x1b_Gi=99;ENOENT:no such image\x1b\\",
        "no frame named, none answered"
    );
}

#[test]
fn an_animation_plays_through_while_its_drawn_frames_are_held() {
    let at = clock();
    let mut term = Terminal::new(24, 80);
    term.set_cell_pixel_size(10, 20);
    term.process_at(&apc_g("a=t,f=32,s=10,v=20,i=7,q=2", &[1u8; 800]), at(0));
    for pixel in 2..=20u8 {
        term.process_at(
            &apc_g("a=f,f=32,s=10,v=20,i=7,z=10,q=2", &[pixel; 800]),
            at(0),
        );
    }
    term.process_at(&apc_g("a=a,i=7,r=1,z=10", b""), at(0));
    term.process_at(&apc_g("a=p,i=7,c=2,r=1,C=1,q=2", b""), at(0));
    term.process_at(&apc_g("a=a,i=7,s=3", b""), at(0));
    let mut held = Vec::new();
    let mut shown = Vec::new();
    for step in 1..=40u64 {
        term.advance_kitty_animations(at(step * 10).monotonic);
        let placed = image_at(&term, 0, 0).expect("placed").image;
        shown.push(placed.bytes[0]);
        held.push(placed);
    }
    let expected: Vec<u8> = (1..=40u8).map(|step| step % 20 + 1).collect();
    assert_eq!(shown, expected, "every frame, round and round");
}

#[test]
fn frame_delete_removes_the_numbered_frame_as_kitty_does() {
    const FRAME: usize = 800;
    let mut term = three_frames(3);
    assert_eq!(shown_frame(&mut term), Some(3));
    let before = term.transient.kitty_total_bytes;
    term.process(&apc_g("a=d,d=f,i=7,r=2", b""));
    assert_eq!(
        term.transient.kitty_frames[&7].frames.len(),
        2,
        "frame 2 went"
    );
    assert_eq!(shown_frame(&mut term), Some(3), "frame 3 is still shown");
    assert_eq!(
        term.transient.kitty_total_bytes,
        before - FRAME,
        "the frame's bytes are returned"
    );
    term.process(&apc_g("a=a,i=7,c=2", b""));
    assert_eq!(
        shown_frame(&mut term),
        Some(3),
        "old frame 3 is frame 2 now"
    );

    let mut term = three_frames(2);
    term.process(&apc_g("a=d,d=f,i=7,r=2", b""));
    assert_eq!(shown_frame(&mut term), Some(3));

    let mut term = three_frames(3);
    term.process(&apc_g("a=d,d=f,i=7,r=99", b""));
    assert_eq!(shown_frame(&mut term), Some(2), "r= past the end: the last");

    let mut term = three_frames(1);
    term.process(&apc_g("a=d,d=f,i=7", b""));
    assert_eq!(shown_frame(&mut term), Some(2));
    term.process(&apc_g("a=d,d=f,i=7", b""));
    assert_eq!(term.transient.kitty_frames[&7].frames.len(), 1);
    assert_eq!(shown_frame(&mut term), Some(3));
    let lone = term.transient.kitty_total_bytes;
    assert_eq!(lone, 2 * FRAME, "one frame, charged in its two slots");

    term.process(&apc_g("a=d,d=f,i=7", b""));
    assert_eq!(shown_frame(&mut term), Some(3), "d=f keeps a lone frame");
    assert_eq!(term.transient.kitty_total_bytes, lone);
    term.process(&apc_g("a=d,d=F,i=7", b""));
    assert_eq!(shown_frame(&mut term), None, "d=F deleted the image");
    assert_eq!(term.transient.kitty_total_bytes, 0);
}

#[test]
fn animation_frame_keys_place_the_data_and_keep_the_root_layout() {
    let mut term = Terminal::new(24, 80);
    term.set_cell_pixel_size(10, 20);
    term.process(&apc_g("a=t,f=32,s=10,v=20,i=7", &one_cell_rgba()));
    let frame2 = vec![0xABu8; 10 * 20 * 4];
    term.process(&apc_g(
        "a=f,f=32,s=10,v=20,i=7,x=2,y=3,c=1,r=2,z=-40",
        &frame2,
    ));
    term.process(&apc_g("a=a,i=7,c=2", b""));
    let mut seq = b"\x1b[38;5;7m".to_vec();
    let mut buf = [0u8; 4];
    for c in ['\u{10EEEE}', '\u{0305}', '\u{0305}'] {
        seq.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
    }
    term.process(&seq);
    let f = term.cell_frame(24, 80);
    let shown = &f.images[0][0].1.image;
    assert_eq!(
        shown.bytes.len(),
        10 * 20 * 4,
        "the frame is the whole canvas"
    );
    assert_eq!(shown.bytes[0], 0, "left of x=2: frame 1's pixels (c=1)");
    let at = (3 * 10 + 2) * 4;
    assert_eq!(shown.bytes[at], 0xAB, "the data starts at (x=2, y=3)");
    assert_eq!(
        shown.source_rect, None,
        "x=/y= on a=f do not crop the frame"
    );
    assert_eq!(
        (shown.cols, shown.rows, shown.z_index, shown.scaling),
        (1, 1, 0, aterm_grid::ImageScaling::PixelExact),
        "c=/r=/z= on a=f do not lay the frame out: it takes frame 1's layout"
    );
}

#[test]
fn non_direct_mediums_skip_cleanly_never_garbage() {
    for medium in ["f", "t", "s"] {
        let mut term = Terminal::new(24, 80);
        term.set_cell_pixel_size(10, 20);
        let control = format!("a=T,f=32,s=10,v=20,t={medium}");
        term.process(&apc_g(&control, b"/tmp/some/image.png"));
        assert!(
            term.cell_frame(24, 80).images[0].is_empty(),
            "t={medium} (non-direct medium) must skip cleanly — no image placed"
        );
    }
}

#[test]
fn non_direct_mediums_place_only_what_the_host_resolver_serves() {
    use crate::terminal::kitty_graphics::KittyMedium;
    for (medium, name, served) in [
        ("f", "img.rgba", true),
        ("s", "/aterm-shm", true),
        ("f", "/etc/passwd", false),
        ("s", "img.rgba", false),
    ] {
        let mut term = Terminal::new(24, 80);
        term.set_cell_pixel_size(10, 20);
        term.set_kitty_file_resolver(|medium, name| {
            matches!(
                (medium, name),
                (KittyMedium::File, "img.rgba") | (KittyMedium::SharedMemory, "/aterm-shm")
            )
            .then(|| vec![0u8; 10 * 20 * 4])
        });
        term.process(&apc_g(
            &format!("a=T,f=32,s=10,v=20,t={medium}"),
            name.as_bytes(),
        ));
        assert_eq!(
            !term.cell_frame(24, 80).images[0].is_empty(),
            served,
            "t={medium} {name}"
        );
    }
}

#[test]
fn malformed_payloads_place_nothing() {
    for (control, payload) in [
        ("a=T,f=32,s=10,v=20,o=z", &[1u8, 2, 3, 4, 5, 6][..]),
        ("a=T,f=32,s=10,v=20", &[1, 2, 3, 4]),
    ] {
        let mut term = Terminal::new(24, 80);
        term.set_cell_pixel_size(10, 20);
        term.process(&apc_g(control, payload));
        assert!(term.cell_frame(24, 80).images[0].is_empty(), "{control}");
    }
}

#[test]
fn armed_alloc_fault_aborts_chunked_transfer_fail_closed() {
    use crate::fault;
    let mut term = Terminal::new(24, 80);
    term.set_cell_pixel_size(10, 20);
    let full = one_cell_rgba();
    fault::with_armed("kitty.chunk_alloc", || {
        term.process(&apc_g("a=T,f=32,s=10,v=20,m=1", &full[0..400]));
        term.process(&apc_g("m=1", &full[400..600]));
        term.process(&apc_g("m=0", &full[600..800]));
    });
    assert!(
        term.cell_frame(24, 80).images[0].is_empty(),
        "an armed alloc fault must abort the transfer (fail closed), placing nothing"
    );
    term.process(&apc_g("a=T,f=32,s=10,v=20,m=1", &full[0..400]));
    term.process(&apc_g("m=1", &full[400..600]));
    term.process(&apc_g("m=0", &full[600..800]));
    assert!(
        !term.cell_frame(24, 80).images[0].is_empty(),
        "after disarming, the accumulator recovers and places the assembled image"
    );
}

#[test]
fn query_answers_per_medium_availability() {
    let mut term = Terminal::new(24, 80);
    term.process(&apc_g("a=q,i=3,t=f", b""));
    let reply = term.take_response().unwrap_or_default();
    let reply = String::from_utf8_lossy(&reply).into_owned();
    assert!(
        reply.starts_with("\x1b_Gi=3;ENOTSUPPORTED"),
        "no resolver -> t=f errors: {reply:?}"
    );
    term.process(&apc_g("a=q,i=3", b""));
    assert_eq!(
        term.take_response().unwrap_or_default(),
        b"\x1b_Gi=3;OK\x1b\\",
        "direct (the default medium) is always real"
    );
    term.process(&apc_g("a=q,i=4,q=2", b""));
    assert!(term.take_response().is_none(), "q=2 suppresses the OK");
    term.set_kitty_file_resolver(|_, _| None);
    term.process(&apc_g("a=q,i=3,t=f", b""));
    assert_eq!(
        term.take_response().unwrap_or_default(),
        b"\x1b_Gi=3;OK\x1b\\",
        "with the resolver installed the same probe is OK"
    );
}

#[test]
fn transmit_over_a_disabled_medium_answers_as_the_probe_does() {
    let mut term = Terminal::new(24, 80);
    term.process(&apc_g("a=q,i=5,t=f", b"/no/such/file"));
    let probe = term.take_response().expect("the probe is answered");
    term.process(&apc_g("a=t,i=5,t=f", b"/no/such/file"));
    let transmit = term.take_response().expect("the transmit is answered");
    assert_eq!(
        String::from_utf8_lossy(&transmit),
        String::from_utf8_lossy(&probe),
        "a disabled medium: the transmit's verdict is the probe's"
    );
    term.set_kitty_file_resolver(|_, _| None);
    term.process(&apc_g("a=t,i=5,t=f", b"/no/such/file"));
    assert_eq!(
        term.take_response().unwrap_or_default(),
        b"\x1b_Gi=5;EINVAL:bad image data\x1b\\",
        "with the resolver installed, a refused path is bad data"
    );
}

#[test]
fn deletes_are_never_answered() {
    let mut term = Terminal::new(24, 80);
    term.set_cell_pixel_size(10, 20);
    term.process(&apc_g("a=t,f=32,s=10,v=20,i=4,q=1", &one_cell_rgba()));
    assert!(term.take_response().is_none(), "q=1 quiets the OK");
    for delete in ["a=d,d=i,i=4", "a=d,d=I,i=4", "a=d,d=i,i=99", "a=d,d=n,I=9"] {
        term.process(&apc_g(delete, b""));
        assert!(term.take_response().is_none(), "{delete} must not answer");
    }
}

#[test]
fn bulk_string_accumulators_cross_caps_like_per_byte() {
    use crate::parser::ActionSink;
    use crate::terminal::{DcsType, MAX_DCS_GLOBAL_BUDGET};
    use aterm_provenance::pty_wrap_ref;

    let cases: &[(Option<DcsType>, usize)] = &[
        (None, 4 * 1024 * 1024),
        (Some(DcsType::Decrqss), 256),
        (Some(DcsType::Xtgettcap), 1024),
        (
            Some(DcsType::Decrsps { discarded: false }),
            crate::terminal::handler::handler_decrsps::DATA_CAP,
        ),
        (Some(DcsType::Unknown), 0),
    ];
    let run: Vec<u8> = (0..300u32).map(|i| (i % 251) as u8).collect();
    let drive = |bulk: bool, kind: Option<DcsType>, cap: usize| {
        let mut term = Terminal::new(24, 80);
        let (_parser, mut h) = term.split_for_process();
        if let Some(dcs_type) = kind {
            h.dcs.dcs_type = dcs_type;
        }
        h.dcs.data = vec![0xAB; cap.saturating_sub(100)];
        h.dcs.total_bytes = MAX_DCS_GLOBAL_BUDGET - 200;
        h.dcs.sequence_bytes = MAX_DCS_GLOBAL_BUDGET - 200;
        for chunk in [&run[..150], &run[150..151], &run[151..]] {
            match (bulk, kind) {
                (true, None) => h.apc_put_bulk(pty_wrap_ref(chunk)),
                (true, Some(_)) => h.dcs_put_bulk(pty_wrap_ref(chunk)),
                (false, None) => chunk.iter().for_each(|&b| h.apc_put(b)),
                (false, Some(_)) => chunk.iter().for_each(|&b| h.dcs_put(b)),
            }
        }
        (h.dcs.data.clone(), h.dcs.total_bytes, h.dcs.sequence_bytes)
    };
    for &(kind, cap) in cases {
        let per_byte = drive(false, kind, cap);
        assert_eq!(per_byte, drive(true, kind, cap), "{kind:?}");
        assert_eq!(
            per_byte.1, MAX_DCS_GLOBAL_BUDGET,
            "{kind:?}: budget reached"
        );
        if cap > 0 {
            assert_eq!(per_byte.0.len(), cap, "{kind:?}: data cap reached");
        }
    }
}

#[test]
fn dcs_put_bulk_sixel_parity() {
    use crate::parser::ActionSink;
    use aterm_provenance::pty_wrap_ref;

    let mut body = b"\"1;1;60;24#0;2;100;0;0".to_vec();
    for _ in 0..4 {
        body.extend_from_slice(b"#0!60~-");
    }

    fn drive(bulk: bool, body: &[u8]) -> (usize, usize, usize) {
        let mut term = Terminal::new(24, 80);
        let (_parser, mut h) = term.split_for_process();
        h.dcs_hook_inner(&[], &[], b'q'); // Sixel
        if bulk {
            let mid = body.len() / 2;
            h.dcs_put_bulk(pty_wrap_ref(&body[..mid]));
            h.dcs_put_bulk(pty_wrap_ref(&body[mid..]));
        } else {
            for &b in body {
                h.dcs_put(b);
            }
        }
        (
            h.dcs.total_bytes,
            h.dcs.sequence_bytes,
            h.sixel.decoder.pixel_alloc_bytes(),
        )
    }

    let per_byte = drive(false, &body);
    let bulk = drive(true, &body);
    assert_eq!(per_byte, bulk, "sixel bulk/per-byte budget + alloc parity");
    assert!(bulk.2 > 0, "the stream must actually allocate pixels");
}

fn raw_rgba(w: usize, h: usize) -> Vec<u8> {
    vec![0u8; w * h * 4]
}

#[test]
fn ris_clears_kitty_store_and_budget() {
    let mut term = Terminal::new(24, 80);
    term.set_cell_pixel_size(10, 20);
    let img = raw_rgba(4, 4);

    term.process(&apc_g("a=t,f=32,s=4,v=4,i=1", &img));
    term.process(&apc_g("a=f,f=32,s=4,v=4,i=1", &img));
    term.process(&apc_g("a=t,f=32,s=4,v=4,i=2", &img));
    assert!(
        term.transient.kitty_total_bytes > 0,
        "images stored before RIS"
    );
    assert!(!term.transient.kitty_images.is_empty());

    term.process(b"\x1bc");

    assert_eq!(term.transient.kitty_total_bytes, 0, "RIS resets the budget");
    assert!(
        term.transient.kitty_images.is_empty(),
        "RIS clears stored images"
    );
    assert!(
        term.transient.kitty_frames.is_empty(),
        "RIS clears animation frames"
    );

    term.process(&apc_g("a=T,f=32,s=4,v=4,m=1", &img));
    assert!(term.transient.kitty_pending.is_some(), "precondition");
    term.process(b"\x1bc");
    assert!(term.transient.kitty_pending.is_none());
}

#[test]
fn kitty_store_global_budget_rejects_overflow() {
    let mut term = Terminal::new(24, 80);
    term.set_cell_pixel_size(10, 20);

    let img = raw_rgba(1000, 750); // 3_000_000 bytes
    let per_slot = img.len();
    let mut stored = 0usize;
    let mut id = 1u32;
    loop {
        let before = term.transient.kitty_total_bytes;
        term.process(&apc_g(&format!("a=t,f=32,s=1000,v=750,i={id}"), &img));
        if term.transient.kitty_images.contains_key(&id) {
            assert_eq!(term.transient.kitty_total_bytes, before + 2 * per_slot);
            stored += 1;
            id += 1;
            assert!(
                term.transient.kitty_total_bytes <= super::MAX_KITTY_STORE_BYTES,
                "the running budget must never exceed the global cap"
            );
        } else {
            assert_eq!(
                term.transient.kitty_total_bytes, before,
                "a rejected over-budget transfer must not change the counter"
            );
            assert!(
                before + 2 * per_slot > super::MAX_KITTY_STORE_BYTES,
                "rejection only when adding the image would exceed the cap"
            );
            break;
        }
        assert!(id < 100, "must reject before storing absurdly many images");
    }
    assert!(stored >= 1 && stored < super::MAX_KITTY_IMAGES);
    assert_eq!(term.transient.kitty_images.len(), stored);
}

#[test]
fn a_transmits_footprint_and_scaling_follow_its_keys() {
    use aterm_grid::ImageScaling::{Fit, PixelExact, Stretch};
    for ((w, h), keys, footprint, scaling) in [
        ((15, 25), "", (2, 2), PixelExact),
        ((15, 25), ",c=4,r=3", (4, 3), Stretch),
        ((4, 4), ",c=8,r=2", (8, 2), Stretch),
        ((20, 40), ",c=4", (4, 4), Fit),
        ((20, 40), ",r=1", (1, 1), Fit),
        ((20, 40), ",c=4,r=0", (4, 4), Fit),
        ((20, 40), ",c=0,r=0", (2, 2), PixelExact),
    ] {
        let mut term = Terminal::new(24, 80);
        term.set_cell_pixel_size(10, 20);
        term.process(&apc_g(
            &format!("a=T,f=32,s={w},v={h}{keys}"),
            &raw_rgba(w, h),
        ));
        let placed = image_at(&term, 0, 0).expect("a=T places an image");
        assert_eq!(
            ((placed.image.cols, placed.image.rows), placed.image.scaling),
            (footprint, scaling),
            "{w}x{h}{keys}"
        );
    }
}

fn image_at(term: &Terminal, row: usize, col: usize) -> Option<aterm_grid::ImageRef> {
    term.images_row(row)
        .into_iter()
        .find(|(c, _)| *c == col)
        .map(|(_, image)| image)
}

fn layers_at(term: &Terminal, row: usize, col: usize) -> Vec<u32> {
    term.images_row(row)
        .into_iter()
        .filter(|(c, _)| *c == col)
        .map(|(_, image)| image.kitty.map_or(0, |tag| tag.image_id))
        .collect()
}

#[test]
fn overlapping_placements_stack_by_z_then_image_then_placement_order() {
    let mut term = Terminal::new(24, 80);
    term.set_cell_pixel_size(10, 20);
    for id in 1..=3 {
        term.process(&apc_g(
            &format!("a=t,f=32,s=20,v=20,i={id},q=2"),
            &raw_rgba(20, 20),
        ));
    }
    term.process(b"\x1b[1;1H");
    term.process(&apc_g("a=p,i=2,q=2", b""));
    term.process(b"\x1b[1;2H");
    term.process(&apc_g("a=p,i=1,q=2", b""));
    assert_eq!(layers_at(&term, 0, 0), [2], "only image 2 reaches col 0");
    assert_eq!(
        layers_at(&term, 0, 1),
        [1, 2],
        "one z: the image sent first lies lower"
    );
    assert_eq!(layers_at(&term, 0, 2), [1]);

    term.process(b"\x1b[1;2H");
    term.process(&apc_g("a=p,i=3,z=-1,q=2", b""));
    term.process(b"\x1b[1;2H");
    term.process(&apc_g("a=p,i=3,z=4,q=2", b""));
    assert_eq!(layers_at(&term, 0, 1), [3, 1, 2, 3], "then by z");
    term.process(b"\x1b[1;1H");
    term.process(&apc_g("a=p,i=1,q=2", b""));
    assert_eq!(
        layers_at(&term, 0, 1),
        [3, 1, 1, 2, 3],
        "one image, one z: the newer placement above"
    );

    term.process(&apc_g("a=d,d=i,i=2", b""));
    assert_eq!(layers_at(&term, 0, 0), [1]);
    assert_eq!(layers_at(&term, 0, 1), [3, 1, 1, 3]);
    term.process(&apc_g("a=d,d=z,z=4", b""));
    assert_eq!(layers_at(&term, 0, 1), [3, 1, 1]);

    term.process(&[b'\n'; 30]);
    term.scroll_display(30);
    assert_eq!(term.grid().display_offset(), 7);
    assert_eq!(layers_at(&term, 0, 1), [3, 1, 1], "scrolled back");

    term.scroll_display(-30);
    term.process(b"\x1b[1;1H");
    term.process(&apc_g("a=p,i=1,q=2", b""));
    term.process(b"\x1b[1;1H");
    term.process(b"\x1b]1337;File=inline=1;width=1;height=1:AAAA\x07");
    assert_eq!(layers_at(&term, 0, 0), [0]);
}

#[test]
fn images_of_one_z_stack_in_the_order_they_were_first_sent() {
    let mut term = Terminal::new(24, 80);
    term.set_cell_pixel_size(10, 20);
    for id in [9, 4] {
        term.process(&apc_g(
            &format!("a=t,f=32,s=10,v=20,i={id},q=2"),
            &one_cell_rgba(),
        ));
    }
    for put in ["a=p,i=9,C=1,q=2", "a=p,i=4,C=1,q=2"] {
        term.process(b"\x1b[1;1H");
        term.process(&apc_g(put, b""));
    }
    term.process(&apc_g("a=T,f=32,s=10,v=20,C=1,q=2", &one_cell_rgba()));
    assert_eq!(layers_at(&term, 0, 0), [9, 4, 0]);

    term.process(&apc_g("a=t,f=32,s=10,v=20,i=9,q=2", &one_cell_rgba()));
    assert_eq!(layers_at(&term, 0, 0), [4, 0], "sent again: unplaced");
    term.process(&apc_g("a=p,i=9,C=1,q=2", b""));
    assert_eq!(layers_at(&term, 0, 0), [9, 4, 0], "and back in its place");
}

#[test]
fn a_redraw_in_place_replaces_the_placement_it_covers() {
    let mut term = Terminal::new(24, 80);
    term.set_cell_pixel_size(10, 20);
    for frame in 1..=20u8 {
        term.process(b"\x1b[1;1H");
        term.process(&apc_g("a=T,f=32,s=10,v=20,c=2,r=1,C=1,q=2", &[frame; 800]));
    }
    let shown: Vec<u8> = term
        .images_row(0)
        .into_iter()
        .filter(|(col, _)| *col == 0)
        .map(|(_, image)| image.image.bytes[0])
        .collect();
    assert_eq!(shown, [20], "one layer: the newest frame");

    term.process(&apc_g("a=t,f=32,s=20,v=20,i=1,q=2", &raw_rgba(20, 20)));
    term.process(&apc_g("a=t,f=32,s=10,v=20,i=3,q=2", &one_cell_rgba()));
    for x in 0..16 {
        term.process(b"\x1b[3;1H");
        term.process(&apc_g(&format!("a=p,i=1,x={x},w=5,h=5,C=1,q=2"), b""));
    }
    assert_eq!(layers_at(&term, 2, 0), [1], "a re-crop replaces too");
    term.process(&apc_g("a=p,i=3,P=1,H=1,q=2", b""));
    assert_eq!(layers_at(&term, 2, 1), [3], "a child of the redrawn one");
    term.process(b"\x1b[3;1H");
    term.process(&apc_g("a=p,i=1,x=7,w=5,h=5,C=1,q=2", b""));
    assert_eq!(layers_at(&term, 2, 0), [1]);
    assert_eq!(layers_at(&term, 2, 1), [3], "follows its redraw");
    term.process(&apc_g("a=d,d=i,i=1", b""));
    assert_eq!(
        layers_at(&term, 2, 1),
        Vec::<u32>::new(),
        "and goes with it"
    );

    term.process(b"\x1b[5;1H");
    term.process(&apc_g("a=p,i=1,C=1,q=2", b""));
    term.process(&apc_g("a=p,i=1,z=1,C=1,q=2", b""));
    assert_eq!(layers_at(&term, 4, 0), [1, 1], "another z-index stacks");
}

#[test]
fn floating_placements_are_capped_and_a_frame_keeps_each_cells_top_layers() {
    use aterm_grid::MAX_IMAGE_LAYERS;
    let cap = super::MAX_KITTY_FLOATING;
    let mut term = Terminal::new(24, 80);
    term.set_cell_pixel_size(10, 20);
    for id in (1..=2).chain(10..10 + MAX_IMAGE_LAYERS as u32) {
        term.process(&apc_g(
            &format!("a=t,f=32,s=10,v=20,i={id},q=2"),
            &one_cell_rgba(),
        ));
    }
    term.process(&apc_g("a=p,U=1,i=1,p=7,q=2", b""));
    term.process(b"\x1b[1;1H");
    term.process(&placeholder(1, 7, 0, 0));
    for _ in 0..40 {
        term.process(&apc_g("a=p,i=2,P=1,Q=7,c=20,r=4,z=1,q=2", b""));
    }
    assert_eq!(term.transient.kitty_floating.len(), 1, "a redraw replaces");
    let mut refused = 0;
    for h in 1..=cap + 3 {
        term.process(&apc_g(&format!("a=p,i=2,P=1,Q=7,H={h},c=20,r=4,z=1"), b""));
        if reply(&mut term).contains(";ENOSPC") {
            refused += 1;
        }
    }
    assert_eq!(term.transient.kitty_floating.len(), cap);
    assert_eq!(refused, 4, "every put past the cap");

    for id in 10..10 + MAX_IMAGE_LAYERS as u32 {
        term.process(b"\x1b[2;20H");
        term.process(&apc_g(&format!("a=p,i={id},C=1,q=2"), b""));
    }
    let top = vec![(2, 0); MAX_IMAGE_LAYERS];
    assert_eq!(tags_at(&term, 1, 19), top, "the top layers, per row");
    let frame = term.cell_frame(24, 80);
    let batch: Vec<(u32, u32)> = frame.images[1]
        .iter()
        .filter(|(col, _)| *col == 19)
        .filter_map(|(_, image)| image.kitty)
        .map(|tag| (tag.image_id, tag.placement_id))
        .collect();
    assert_eq!(batch, top, "and per frame");
}

#[test]
fn an_animation_drawn_nowhere_through_a_virtual_parent_does_not_play() {
    use crate::terminal::KittyAnimationStep;
    let at = clock();
    let mut term = Terminal::new(24, 80);
    term.set_cell_pixel_size(10, 20);
    for control in [
        "a=t,f=32,s=10,v=20,i=1,q=2",
        "a=t,f=32,s=10,v=20,i=2,q=2",
        "a=f,f=32,s=10,v=20,i=2,z=30,q=2",
    ] {
        term.process_at(&apc_g(control, &one_cell_rgba()), at(0));
    }
    term.process_at(&apc_g("a=a,i=2,r=1,z=30", b""), at(0));
    term.process_at(&apc_g("a=p,U=1,i=1,p=7,q=2", b""), at(0));
    term.process_at(b"\x1b[2;3H", at(0));
    term.process_at(&placeholder(1, 7, 0, 0), at(0));
    term.process_at(&apc_g("a=p,i=2,P=1,Q=7,H=1,V=1,q=2", b""), at(0));
    term.process_at(&apc_g("a=a,i=2,s=3", b""), at(0));
    assert!(term.advance_kitty_animations(at(30).monotonic).advanced);

    term.process_at(b"\x1b[H\x1b[2J", at(40));
    for ms in [60, 90, 120, 150] {
        assert_eq!(
            term.advance_kitty_animations(at(ms).monotonic),
            KittyAnimationStep::default(),
            "drawn nowhere at {ms} ms"
        );
    }
    term.process_at(b"\x1b[2;3H", at(160));
    term.process_at(&placeholder(1, 7, 0, 0), at(160));
    assert!(
        term.advance_kitty_animations(at(170).monotonic).advanced,
        "shown again: it plays on"
    );
}

#[test]
fn put_offsets_start_the_image_inside_its_first_cell() {
    let mut term = Terminal::new(24, 80);
    term.set_cell_pixel_size(10, 20);
    term.process(&apc_g("a=t,f=32,s=10,v=20,i=1,q=2", &raw_rgba(10, 20)));
    term.process(&apc_g("a=p,i=1,X=4,Y=6,q=2", b""));
    let placed = image_at(&term, 0, 0).expect("placed");
    assert_eq!(placed.image.cell_offset_px, (4, 6));
    assert_eq!(
        (placed.image.cols, placed.image.rows),
        (2, 2),
        "4+10 px wide and 6+20 tall need two cells each way"
    );
    let at = term.cursor();
    assert_eq!((at.row, at.col), (1, 2), "past the whole footprint");

    term.process(b"\x1b[5;1H");
    term.process(&apc_g("a=p,i=1,X=99,Y=99,q=2", b""));
    let clamped = image_at(&term, 4, 0).expect("placed");
    assert_eq!(clamped.image.cell_offset_px, (9, 19), "below a cell");

    term.process(b"\x1b[10;1H");
    term.process(&apc_g("a=p,i=1,c=2,X=5,q=2", b""));
    let boxed = image_at(&term, 9, 0).expect("placed");
    assert_eq!((boxed.image.cols, boxed.image.rows), (2, 3));

    term.process(&apc_g("a=p,U=1,i=1,p=3,X=4,Y=6,q=2", b""));
    assert_eq!(
        term.transient.kitty_virtual[&1][0].image.cell_offset_px,
        (0, 0),
        "a virtual placement takes none"
    );
}

#[test]
fn put_lays_the_image_out_by_its_own_keys() {
    let mut term = Terminal::new(24, 80);
    term.set_cell_pixel_size(10, 20);
    term.process(&apc_g("a=t,f=32,s=20,v=40,i=1,q=2", &raw_rgba(20, 40)));
    let stored = std::sync::Arc::clone(&term.transient.kitty_images[&1]);

    term.process(b"\x1b[1;1H");
    term.process(&apc_g("a=p,i=1,q=2", b""));
    let natural = image_at(&term, 0, 0).expect("placed");
    assert!(
        std::sync::Arc::ptr_eq(&natural.image, &stored),
        "a key-less put shows the stored image itself"
    );

    term.process(b"\x1b[5;1H");
    term.process(&apc_g("a=p,i=1,c=4,r=3,q=2", b""));
    let boxed = image_at(&term, 4, 0).expect("placed");
    assert_eq!(
        (boxed.image.cols, boxed.image.rows),
        (4, 3),
        "the put's cell box"
    );
    assert_eq!(boxed.image.scaling, aterm_grid::ImageScaling::Stretch);
    assert_eq!(
        (stored.cols, stored.rows),
        (2, 2),
        "the stored image keeps its own layout"
    );
    term.process(b"\x1b[10;1H");
    term.process(&apc_g("a=p,i=1,c=4,r=3,q=2", b""));
    let again = image_at(&term, 9, 0).expect("placed");
    assert!(
        std::sync::Arc::ptr_eq(&again.image, &boxed.image),
        "an identical put reuses the variant (one decode for both)"
    );

    term.process(b"\x1b[15;1H");
    term.process(&apc_g("a=p,i=1,x=10,y=20,w=10,h=20,q=2", b""));
    let cropped = image_at(&term, 14, 0).expect("placed");
    assert_eq!(
        cropped.image.source_rect,
        Some(aterm_grid::SourceRect {
            x: 10,
            y: 20,
            width: 10,
            height: 20,
        }),
        "the crop rides the placement to the renderer"
    );
    assert_eq!(
        (cropped.image.cols, cropped.image.rows),
        (1, 1),
        "an un-sized crop's footprint is the CROPPED raster's"
    );
    assert_eq!(cropped.image.scaling, aterm_grid::ImageScaling::PixelExact);

    term.process(&apc_g("a=p,i=1,x=99,y=0", b""));
    let reply = String::from_utf8(term.take_response().unwrap_or_default()).unwrap_or_default();
    assert!(reply.starts_with("\x1b_Gi=1;EINVAL"), "{reply:?}");
}

#[test]
fn put_variants_are_budgeted_recycled_and_capped() {
    let mut term = Terminal::new(40, 80);
    term.set_cell_pixel_size(10, 20);
    let img = raw_rgba(20, 40);
    term.process(&apc_g("a=t,f=32,s=20,v=40,i=1,q=2", &img));
    let base = term.transient.kitty_total_bytes;
    for x in 0..(2 * super::MAX_KITTY_VARIANTS as u32) {
        term.process(b"\x1b[1;1H");
        term.process(&apc_g(&format!("a=p,i=1,x={},w=1,h=1", x % 20), b""));
        let reply = term.take_response().unwrap_or_default();
        assert_eq!(reply, b"\x1b_Gi=1;OK\x1b\\", "put {x}");
    }
    assert!(
        term.transient.kitty_total_bytes <= base + super::MAX_KITTY_VARIANTS * img.len(),
        "variants stay charged within their cap"
    );

    let mut term = Terminal::new(40, 80);
    term.set_cell_pixel_size(10, 20);
    term.process(&apc_g("a=t,f=32,s=20,v=40,i=1,q=2", &img));
    let mut refused = None;
    for x in 0..=super::MAX_KITTY_VARIANTS as u32 {
        term.process(format!("\x1b[{};1H", x + 1).as_bytes());
        term.process(&apc_g(&format!("a=p,i=1,x={x},w=1,h=1"), b""));
        let reply = String::from_utf8(term.take_response().unwrap_or_default()).unwrap_or_default();
        if reply.contains("ENOSPC") {
            refused = Some(x);
            break;
        }
    }
    assert_eq!(
        refused,
        Some(super::MAX_KITTY_VARIANTS as u32),
        "the variant past the cap, with every earlier one still on screen, is refused"
    );
}

#[test]
fn number_transmit_assigns_an_id_and_answers_with_both() {
    let mut term = Terminal::new(24, 80);
    term.set_cell_pixel_size(10, 20);
    term.process(&apc_g("a=t,f=32,s=10,v=20,I=77", &one_cell_rgba()));
    assert_eq!(
        term.take_response().unwrap_or_default(),
        b"\x1b_Gi=1,I=77;OK\x1b\\"
    );
    term.process(&apc_g("a=p,I=77,q=1", b""));
    assert!(term.take_response().is_none(), "q=1 silences the OK");
    assert!(
        !term.cell_frame(24, 80).images[0].is_empty(),
        "placed by number"
    );

    term.process(&apc_g("a=t,f=32,s=10,v=20,I=78,i=3", &one_cell_rgba()));
    let reply = String::from_utf8(term.take_response().unwrap_or_default()).unwrap_or_default();
    assert!(reply.contains(";EINVAL"), "i and I together: {reply:?}");

    term.process(&apc_g("a=p,i=9", b""));
    assert_eq!(
        term.take_response().unwrap_or_default(),
        b"\x1b_Gi=9;ENOENT:no such image\x1b\\"
    );
    term.process(&apc_g("a=p,i=9,q=2", b""));
    assert!(term.take_response().is_none(), "q=2 silences errors too");
    term.process(&apc_g("a=T,f=32,s=10,v=20", &one_cell_rgba()));
    assert!(term.take_response().is_none());
}

#[test]
fn retransmitting_an_id_removes_its_old_placements() {
    let mut term = Terminal::new(24, 80);
    term.set_cell_pixel_size(10, 20);
    term.process(&apc_g("a=T,f=32,s=10,v=20,i=4,q=2", &one_cell_rgba()));
    assert!(image_at(&term, 0, 0).is_some());
    term.process(&apc_g("a=t,f=32,s=10,v=20,i=4,q=2", &one_cell_rgba()));
    assert!(
        image_at(&term, 0, 0).is_none(),
        "the old placement of id 4 went with its data"
    );
    term.process(&apc_g("a=t,f=32,s=10,v=20,i=5,q=2", &one_cell_rgba()));
    term.process(b"\x1b[3;1H");
    term.process(&apc_g("a=p,i=4,q=2", b""));
    term.process(&apc_g("a=t,f=32,s=10,v=20,i=5,q=2", &one_cell_rgba()));
    assert!(
        image_at(&term, 2, 0).is_some(),
        "control: another id's transmit leaves id 4's placement alone"
    );
}

#[test]
fn kitty_cursor_lands_after_the_image_and_c1_keeps_it() {
    let mut term = Terminal::new(24, 80);
    term.set_cell_pixel_size(10, 20);
    term.process(b"\x1b[2;3H");
    term.process(&apc_g("a=T,f=32,s=10,v=20,c=4,r=3,q=2", &one_cell_rgba()));
    let at = term.cursor();
    assert_eq!((at.row, at.col), (3, 6), "row 1 + 3 - 1, col 2 + 4");

    term.process(b"\x1b[10;78H");
    term.process(&apc_g("a=T,f=32,s=10,v=20,c=3,r=1,q=2", &one_cell_rgba()));
    let at = term.cursor();
    assert_eq!(
        (at.row, at.col),
        (10, 0),
        "past the right margin: next line"
    );

    term.process(b"\x1b[15;5H");
    term.process(&apc_g(
        "a=T,f=32,s=10,v=20,c=2,r=2,C=1,q=2",
        &one_cell_rgba(),
    ));
    let at = term.cursor();
    assert_eq!((at.row, at.col), (14, 4), "C=1: the cursor does not move");
    assert!(image_at(&term, 15, 5).is_some(), "but the image is placed");

    let png = [0x89u8, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0];
    let b64 = aterm_codec::base64::encode(&png).expect("encode");
    term.process(b"\x1b[20;1H");
    term.process(format!("\x1b]1337;File=inline=1;width=2;height=1:{b64}\x1b\\").as_bytes());
    let at = term.cursor();
    assert_eq!((at.row, at.col), (20, 0), "iTerm2: the line below");
}

#[test]
fn virtual_placement_stamps_nothing_and_keeps_the_cursor() {
    let mut term = Terminal::new(24, 80);
    term.set_cell_pixel_size(10, 20);
    term.process(b"\x1b[4;4H");
    term.process(&apc_g(
        "a=T,U=1,f=32,s=10,v=20,i=6,c=2,r=1,q=2",
        &one_cell_rgba(),
    ));
    assert!(
        (0..24).all(|row| term.images_row(row).is_empty()),
        "no direct placement"
    );
    let at = term.cursor();
    assert_eq!((at.row, at.col), (3, 3), "cursor untouched");
    assert!(
        term.transient.kitty_images.contains_key(&6),
        "stored for placeholders"
    );
    term.process(&apc_g(
        "a=T,f=32,s=10,v=20,i=7,c=2,r=1,q=2",
        &one_cell_rgba(),
    ));
    assert!(image_at(&term, 3, 3).is_some());
}

fn placeholder(id: u8, pid: u8, row: usize, col: usize) -> Vec<u8> {
    const MARKS: [char; 4] = ['\u{0305}', '\u{030D}', '\u{030E}', '\u{0310}'];
    let mut seq = format!("\x1b[38;5;{id}m").into_bytes();
    if pid != 0 {
        seq.extend_from_slice(format!("\x1b[58;5;{pid}m").as_bytes());
    }
    let mut buf = [0u8; 4];
    for c in ['\u{10EEEE}', MARKS[row], MARKS[col]] {
        seq.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
    }
    seq.extend_from_slice(b"\x1b[0m");
    seq
}

fn reply(term: &mut Terminal) -> String {
    String::from_utf8(term.take_response().unwrap_or_default()).unwrap_or_default()
}

#[test]
fn a_selected_frame_shows_in_every_placement_and_repaints_placeholders() {
    let mut term = Terminal::new(24, 80);
    term.set_cell_pixel_size(10, 20);
    term.process(&apc_g("a=t,f=32,s=10,v=20,i=7,q=2", &[1u8; 800]));
    term.process(&apc_g("a=f,f=32,s=10,v=20,i=7,q=2", &[2u8; 800]));
    term.process(&apc_g("a=p,i=7,c=2,r=1,q=2", b""));
    term.process(b"\x1b[6;1H");
    term.process(&placeholder(7, 0, 0, 0));
    term.process(b"\x1b[21;1H");
    let _ = term.cell_frame(24, 80);
    term.take_damage();

    term.process(&apc_g("a=a,i=7,c=2", b""));
    let placed = image_at(&term, 0, 0).expect("still placed");
    assert_eq!(
        placed.image.bytes[0], 2,
        "the direct placement shows frame 2"
    );
    assert_eq!(
        (placed.image.cols, placed.image.rows),
        (2, 1),
        "...in its own layout"
    );
    assert!(
        term.grid().damage().is_row_damaged(5),
        "the placeholder's row repaints"
    );
    assert_eq!(term.cell_frame(24, 80).images[5][0].1.image.bytes[0], 2);

    term.process(&apc_g("a=a,i=7,r=1,z=80", b""));
    assert_eq!(term.cell_frame(24, 80).images[5][0].1.image.bytes[0], 2);
}

#[test]
fn virtual_placements_carry_their_own_id_and_layout() {
    let mut term = Terminal::new(24, 80);
    term.set_cell_pixel_size(10, 20);
    term.process(&apc_g("a=t,f=32,s=20,v=40,i=5,q=2", &raw_rgba(20, 40)));
    term.process(&apc_g("a=p,U=1,i=5,p=3,c=4,r=1", b""));
    assert_eq!(reply(&mut term), "\x1b_Gi=5,p=3;OK\x1b\\");
    assert!(
        (0..24).all(|row| term.images_row(row).is_empty()),
        "a virtual placement stamps nothing"
    );
    term.process(b"\x1b[1;1H");
    term.process(&placeholder(5, 3, 0, 1));
    term.process(b"\x1b[2;1H");
    term.process(&placeholder(5, 9, 0, 0));
    term.process(b"\x1b[3;1H");
    term.process(&placeholder(5, 0, 0, 0));
    let frame = term.cell_frame(24, 80);
    let (_, named) = &frame.images[0][0];
    assert_eq!((named.image.cols, named.image.rows), (4, 1), "p=3's box");
    assert_eq!((named.cell_row, named.cell_col), (0, 1));
    assert!(
        frame.images[1].is_empty(),
        "no virtual placement 9: nothing"
    );
    let (_, any) = &frame.images[2][0];
    assert!(
        std::sync::Arc::ptr_eq(&any.image, &named.image),
        "no placement id: the newest virtual placement"
    );

    term.process(&apc_g("a=p,U=1,i=5,p=3,c=3,r=2,q=2", b""));
    let frame = term.cell_frame(24, 80);
    assert_eq!(
        (
            frame.images[0][0].1.image.cols,
            frame.images[0][0].1.image.rows
        ),
        (3, 2)
    );
    assert_eq!(term.transient.kitty_virtual[&5].len(), 1);
}

#[test]
fn deletes_reach_virtual_placements_by_id_only() {
    let mut term = Terminal::new(24, 80);
    term.set_cell_pixel_size(10, 20);
    term.process(&apc_g("a=t,f=32,s=20,v=40,i=5,q=2", &raw_rgba(20, 40)));
    term.process(&apc_g("a=p,U=1,i=5,p=3,c=4,r=1,q=2", b""));
    term.process(&apc_g("a=t,f=32,s=10,v=20,i=6,q=2", &one_cell_rgba()));
    term.process(&placeholder(5, 3, 0, 0));
    let cols = |term: &mut Terminal| {
        term.cell_frame(24, 80).images[0]
            .first()
            .map(|(_, image)| image.image.cols)
    };

    for selector in ["a", "c", "p,x=1,y=1", "x,x=1", "y,y=1", "z"] {
        term.process(&apc_g(&format!("a=d,d={selector}"), b""));
        assert_eq!(cols(&mut term), Some(4), "d={selector} leaves it");
    }
    term.process(&apc_g("a=d,d=A", b""));
    assert!(term.transient.kitty_images.contains_key(&5), "d=A keeps 5");
    assert!(!term.transient.kitty_images.contains_key(&6), "d=A frees 6");
    assert_eq!(cols(&mut term), Some(4));

    term.process(&apc_g("a=d,d=i,i=5,p=3", b""));
    assert_eq!(cols(&mut term), None, "d=i with p= deletes that one");
    assert!(
        term.transient.kitty_images.contains_key(&5),
        "lowercase keeps data"
    );
    term.process(&apc_g("a=p,U=1,i=5,p=3,c=4,r=1,q=2", b""));
    term.process(&apc_g("a=d,d=I,i=5", b""));
    assert!(
        !term.transient.kitty_images.contains_key(&5),
        "d=I frees it"
    );
    assert_eq!(term.transient.kitty_total_bytes, 0, "and its budget");
}

#[test]
fn relative_placements_follow_their_parent() {
    let mut term = Terminal::new(24, 80);
    term.set_cell_pixel_size(10, 20);
    for id in 1..=3 {
        term.process(&apc_g(
            &format!("a=t,f=32,s=10,v=20,i={id},q=2"),
            &one_cell_rgba(),
        ));
    }
    let tag_at = |term: &Terminal, row, col| {
        image_at(term, row, col)
            .and_then(|image| image.kitty)
            .map(|tag| (tag.image_id, tag.placement_id))
    };
    term.process(b"\x1b[3;4H");
    term.process(&apc_g("a=p,i=1,p=1,q=2", b""));
    term.process(b"\x1b[10;10H");
    term.process(&apc_g("a=p,i=2,p=5,P=1,Q=1,H=2,V=1", b""));
    assert_eq!(reply(&mut term), "\x1b_Gi=2,p=5;OK\x1b\\");
    assert_eq!(tag_at(&term, 3, 5), Some((2, 5)), "(2,3) + (V=1, H=2)");
    assert_eq!(tag_at(&term, 9, 9), None, "not at the cursor");
    let at = term.cursor();
    assert_eq!((at.row, at.col), (9, 9), "the cursor does not move");

    term.process(b"\x1b[6;1H");
    term.process(&apc_g("a=p,i=1,p=1,q=2", b""));
    assert_eq!(tag_at(&term, 2, 3), None, "the parent left (2,3)");
    assert_eq!(tag_at(&term, 3, 5), None, "the child left (3,5)");
    assert_eq!(tag_at(&term, 6, 2), Some((2, 5)), "(5,0) + (1, 2)");

    term.process(&apc_g("a=p,i=3,P=9", b""));
    assert!(reply(&mut term).contains(";ENOPARENT"));
    term.process(&apc_g("a=p,i=1,p=1,P=2,Q=5", b""));
    assert!(reply(&mut term).contains(";ECYCLE"));
    term.process(&apc_g("a=p,U=1,i=3,P=1", b""));
    assert!(
        reply(&mut term).contains(";EINVAL"),
        "a relative virtual placement"
    );
    assert_eq!(tag_at(&term, 6, 2), Some((2, 5)), "refusals moved nothing");

    term.process(&apc_g("a=d,d=i,i=1", b""));
    assert_eq!(tag_at(&term, 6, 2), None, "the child went with its parent");
    assert!(
        term.transient.kitty_images.contains_key(&1),
        "d=i keeps the parent's data"
    );
    assert!(
        !term.transient.kitty_images.contains_key(&2),
        "the child's image went"
    );
    assert!(term.transient.kitty_relations.is_empty());
}

fn tags_at(term: &Terminal, row: usize, col: usize) -> Vec<(u32, u32)> {
    term.images_row(row)
        .into_iter()
        .filter(|(c, _)| *c == col)
        .filter_map(|(_, image)| image.kitty)
        .map(|tag| (tag.image_id, tag.placement_id))
        .collect()
}

#[test]
fn a_placement_hanging_off_the_top_left_shows_exactly_its_on_screen_cells() {
    let mut term = Terminal::new(6, 20);
    term.set_cell_pixel_size(10, 20);
    for id in 1..=2 {
        term.process(&apc_g(
            &format!("a=t,f=32,s=10,v=20,i={id},q=2"),
            &one_cell_rgba(),
        ));
    }
    term.process(&apc_g("a=p,U=1,i=1,p=7,q=2", b""));
    term.process(b"\x1b[1;1H");
    term.process(&placeholder(1, 7, 0, 0));
    term.process(&apc_g("a=p,i=2,p=5,P=1,Q=7,H=-3,V=-2,c=10,r=4", b""));
    assert_eq!(reply(&mut term), "\x1b_Gi=2,p=5;OK\x1b\\");
    let shown: Vec<(usize, usize, u16, u16)> = (0..6)
        .flat_map(|row| {
            term.images_row(row)
                .into_iter()
                .filter(|(_, image)| image.kitty.is_some_and(|tag| tag.image_id == 2))
                .map(move |(col, image)| (row, col, image.cell_row, image.cell_col))
        })
        .collect();
    let want: Vec<(usize, usize, u16, u16)> = (0..2)
        .flat_map(|row| (0..7).map(move |col| (row, col, row as u16 + 2, col as u16 + 3)))
        .collect();
    assert_eq!(shown, want, "rows 2..4 and columns 3..10 of the footprint");

    term.process(&apc_g("a=p,i=2,p=5,P=1,Q=7,H=-20,V=-2,c=10,r=4,q=2", b""));
    assert!(
        (0..6).all(|row| term
            .images_row(row)
            .iter()
            .all(|(_, image)| image.kitty.is_none_or(|tag| tag.image_id != 2))),
        "a footprint wholly off the left edge shows nowhere"
    );
}

#[test]
fn placements_relative_to_a_virtual_placement_follow_its_placeholders() {
    let mut term = Terminal::new(24, 80);
    term.set_cell_pixel_size(10, 20);
    for id in 1..=3 {
        term.process(&apc_g(
            &format!("a=t,f=32,s=10,v=20,i={id},q=2"),
            &one_cell_rgba(),
        ));
    }
    term.process(&apc_g("a=p,U=1,i=1,p=7,q=2", b""));
    term.process(b"\x1b[4;6H");
    term.process(&placeholder(1, 7, 0, 1));
    term.process(b"\x1b[5;5H");
    term.process(&placeholder(1, 7, 1, 0));
    term.process(b"\x1b[20;20H");
    term.process(&apc_g("a=p,i=2,p=5,P=1,Q=7,H=2,V=1", b""));
    assert_eq!(reply(&mut term), "\x1b_Gi=2,p=5;OK\x1b\\");
    assert_eq!(
        tags_at(&term, 4, 6),
        [(2, 5)],
        "(top row 3, left col 4) + (V=1, H=2)"
    );
    let at = term.cursor();
    assert_eq!((at.row, at.col), (19, 19), "the cursor does not move");
    term.process(&apc_g("a=p,i=3,p=6,P=2,Q=5,V=2,q=2", b""));
    assert_eq!(tags_at(&term, 6, 6), [(3, 6)], "a chain follows in turn");

    term.process(b"\x1b[4;1H\x1b[2K\x1b[5;1H\x1b[2K\x1b[10;1H");
    term.process(&placeholder(1, 7, 0, 0));
    assert_eq!(tags_at(&term, 4, 6), Vec::<(u32, u32)>::new());
    assert_eq!(tags_at(&term, 10, 2), [(2, 5)], "(9, 0) + (1, 2)");
    assert_eq!(tags_at(&term, 12, 2), [(3, 6)]);

    term.process(b"\x1b[10;1H\x1b[2K");
    assert!(
        (0..24).all(|row| term.images_row(row).is_empty()),
        "nothing drawn"
    );
    assert_eq!(term.transient.kitty_floating.len(), 2, "but kept");
    term.process(b"\x1b[1;1H");
    term.process(&placeholder(1, 7, 0, 0));
    assert_eq!(tags_at(&term, 1, 2), [(2, 5)], "back with its parent");

    term.process(&apc_g("a=p,i=2,p=5,P=3,Q=6", b""));
    assert!(reply(&mut term).contains(";ECYCLE"));
    assert_eq!(tags_at(&term, 1, 2), [(2, 5)], "refusals moved nothing");

    term.process(&apc_g("a=d,d=p,x=3,y=4", b""));
    assert_eq!(
        tags_at(&term, 3, 2),
        Vec::<(u32, u32)>::new(),
        "(3, 6) went"
    );
    assert_eq!(tags_at(&term, 1, 2), [(2, 5)]);
    assert!(
        term.transient.kitty_images.contains_key(&3),
        "a lowercase delete keeps the data"
    );

    term.process(&apc_g("a=d,d=i,i=1,p=7", b""));
    assert!(term.transient.kitty_floating.is_empty());
    assert!(
        !term.transient.kitty_images.contains_key(&2),
        "deleted with its parent, with no placement left: its image went"
    );
    assert!(
        term.transient.kitty_images.contains_key(&1),
        "d=i keeps its own data"
    );
}

#[test]
fn relative_chains_are_bounded() {
    let mut term = Terminal::new(40, 80);
    term.set_cell_pixel_size(10, 20);
    term.process(&apc_g("a=t,f=32,s=10,v=20,i=1,q=2", &one_cell_rgba()));
    term.process(&apc_g("a=p,i=1,p=1,q=2", b""));
    for p in 2..=u32::try_from(super::MAX_KITTY_RELATIVE_DEPTH).unwrap_or(0) + 1 {
        term.process(&apc_g(&format!("a=p,i=1,p={p},P=1,Q={},V=1", p - 1), b""));
        assert_eq!(reply(&mut term), format!("\x1b_Gi=1,p={p};OK\x1b\\"));
    }
    let deepest = super::MAX_KITTY_RELATIVE_DEPTH + 1;
    term.process(&apc_g(&format!("a=p,i=1,p=99,P=1,Q={deepest}"), b""));
    assert!(reply(&mut term).contains(";ETOODEEP"));
}

#[test]
fn frames_are_composed_on_a_canvas() {
    const RED: [u8; 4] = [255, 0, 0, 255];
    const GREEN: [u8; 4] = [0, 255, 0, 255];
    const BLUE: [u8; 4] = [0, 0, 255, 255];
    let mut term = Terminal::new(24, 80);
    term.set_cell_pixel_size(10, 20);
    term.process(&apc_g("a=t,f=32,s=2,v=1,i=7,q=1", &[RED, GREEN].concat()));
    let frame = |term: &Terminal, n: usize| {
        term.transient.kitty_frames[&7].frames[n]
            .image
            .bytes
            .clone()
    };

    term.process(&apc_g("a=f,f=32,s=1,v=1,i=7,x=1", &BLUE));
    assert_eq!(reply(&mut term), "\x1b_Gi=7,r=2;OK\x1b\\");
    assert_eq!(
        frame(&term, 1),
        [[0; 4], BLUE].concat(),
        "over transparent black"
    );

    term.process(&apc_g("a=f,f=32,s=1,v=1,i=7,x=1,c=1,q=1", &BLUE));
    assert_eq!(frame(&term, 2), [RED, BLUE].concat(), "over frame 1");

    term.process(&apc_g(
        "a=f,f=32,s=1,v=1,i=7,Y=4278190335,q=1",
        &[0, 0, 0, 0],
    ));
    assert_eq!(
        frame(&term, 3),
        [RED, RED].concat(),
        "the background Y= shows through"
    );

    term.process(&apc_g("a=f,f=32,s=1,v=1,i=7,r=1,X=1", &[0, 0, 255, 0]));
    assert_eq!(reply(&mut term), "\x1b_Gi=7,r=1;OK\x1b\\");
    assert_eq!(
        frame(&term, 0),
        [[0, 0, 255, 0], GREEN].concat(),
        "X=1 overwrites"
    );
    assert_eq!(term.transient.kitty_images[&7].bytes, frame(&term, 0));
    assert_eq!(
        term.transient.kitty_frames[&7].frames.len(),
        4,
        "no frame was added"
    );

    term.process(&apc_g("a=f,f=32,s=3,v=1,i=7", &[0; 12]));
    assert!(reply(&mut term).contains(";EINVAL"), "wider than the image");
    let mut png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 13];
    png.extend_from_slice(b"IHDR");
    png.extend_from_slice(&1u32.to_be_bytes());
    png.extend_from_slice(&1u32.to_be_bytes());
    term.process(&apc_g("a=f,f=100,i=7,x=1", &png));
    assert!(
        reply(&mut term).contains(";EBADPNG"),
        "a truncated PNG patch"
    );
    term.process(&apc_g("a=f,f=32,s=1,v=1,i=7,c=9", &BLUE));
    assert!(
        reply(&mut term).contains(";EINVAL"),
        "no frame 9 to draw over"
    );
    assert_eq!(term.transient.kitty_frames[&7].frames.len(), 4);
}

fn png(w: u32, h: u32, pixels: &[[u8; 4]]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut enc = aterm_png::Encoder::new(&mut out, w, h);
    enc.set_color(aterm_png::ColorType::Rgba);
    enc.set_depth(aterm_png::BitDepth::Eight);
    let mut writer = enc.write_header().expect("png header");
    writer.write_image_data(&pixels.concat()).expect("png data");
    drop(writer);
    out
}

#[test]
fn png_frames_are_decoded_to_compose() {
    const RED: [u8; 4] = [255, 0, 0, 255];
    const GREEN: [u8; 4] = [0, 255, 0, 255];
    const BLUE: [u8; 4] = [0, 0, 255, 255];
    let mut term = Terminal::new(24, 80);
    term.set_cell_pixel_size(10, 20);
    term.process(&apc_g("a=t,f=100,i=7,q=1", &png(2, 1, &[RED, GREEN])));
    let frame = |term: &Terminal, n: usize| {
        term.transient.kitty_frames[&7].frames[n]
            .image
            .bytes
            .clone()
    };

    term.process(&apc_g("a=f,f=100,i=7,x=1", &png(1, 1, &[BLUE])));
    assert_eq!(reply(&mut term), "\x1b_Gi=7,r=2;OK\x1b\\");
    assert_eq!(frame(&term, 1), [[0; 4], BLUE].concat(), "PNG data");
    term.process(&apc_g("a=f,f=32,s=1,v=1,i=7,x=1,c=1", &BLUE));
    assert_eq!(reply(&mut term), "\x1b_Gi=7,r=3;OK\x1b\\");
    assert_eq!(frame(&term, 2), [RED, BLUE].concat(), "over the PNG root");
    term.process(&apc_g("a=f,f=100,i=7,r=1,X=1", &png(1, 1, &[BLUE])));
    assert_eq!(reply(&mut term), "\x1b_Gi=7,r=1;OK\x1b\\");
    assert_eq!(frame(&term, 0), [BLUE, GREEN].concat(), "the root edited");
    assert_eq!(
        term.transient.kitty_images[&7].bytes,
        frame(&term, 0),
        "the edited root is still the frame shown"
    );

    term.process(&apc_g("a=t,f=100,i=8,q=2", &png(2, 1, &[RED, GREEN])));
    term.process(&apc_g("a=f,f=100,i=8,q=2", &png(2, 1, &[BLUE, BLUE])));
    term.process(&apc_g("a=c,i=8,r=1,c=2,w=1,h=1,x=1", b""));
    assert_eq!(reply(&mut term), "\x1b_Gi=8;OK\x1b\\");
    assert_eq!(
        term.transient.kitty_frames[&8].frames[1].image.bytes,
        [BLUE, RED].concat(),
        "PNG frame 1's (0,0) onto PNG frame 2's (1,0)"
    );
}

#[test]
fn compose_copies_a_rectangle_between_frames() {
    const RED: [u8; 4] = [255, 0, 0, 255];
    const GREEN: [u8; 4] = [0, 255, 0, 255];
    let mut term = Terminal::new(24, 80);
    term.set_cell_pixel_size(10, 20);
    term.process(&apc_g("a=t,f=32,s=2,v=1,i=7,q=2", &[RED, GREEN].concat()));
    term.process(&apc_g("a=f,f=32,s=2,v=1,i=7,q=2", &[GREEN, GREEN].concat()));
    term.process(&apc_g("a=c,i=7,r=1,c=2,w=1,h=1,x=1", b""));
    assert_eq!(reply(&mut term), "\x1b_Gi=7;OK\x1b\\");
    assert_eq!(
        term.transient.kitty_frames[&7].frames[1].image.bytes,
        [GREEN, RED].concat(),
        "frame 1's (0,0) onto frame 2's (1,0)"
    );
    term.process(&apc_g("a=c,i=7,r=1,c=2,w=2,x=1", b""));
    assert!(reply(&mut term).contains(";EINVAL"), "out of bounds");
    term.process(&apc_g("a=c,i=7,r=9,c=2", b""));
    assert!(reply(&mut term).contains(";ENOENT"), "no frame 9");
    term.process(&apc_g("a=c,i=7,r=2,c=2,w=2", b""));
    assert!(reply(&mut term).contains(";EINVAL"), "overlapping itself");
}

#[test]
fn compose_refuses_its_arguments_before_it_decodes_and_decodes_a_frame_once() {
    const RED: [u8; 4] = [255, 0, 0, 255];
    const GREEN: [u8; 4] = [0, 255, 0, 255];
    let decodes = || super::KITTY_PNG_DECODES.with(std::cell::Cell::get);
    let mut term = Terminal::new(24, 80);
    term.set_cell_pixel_size(10, 20);
    // A 2x1 header and no image data: it is stored, and it never decodes.
    let mut header = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 13];
    header.extend_from_slice(b"IHDR");
    header.extend_from_slice(&2u32.to_be_bytes());
    header.extend_from_slice(&1u32.to_be_bytes());
    term.process(&apc_g("a=t,f=100,i=7,q=2", &header));
    let before = decodes();
    for (control, verdict) in [
        (
            "a=c,i=7,r=1,c=1",
            "EINVAL:the rectangles overlap in one frame",
        ),
        (
            "a=c,i=7,r=1,c=1,w=3",
            "EINVAL:the destination rectangle is out of bounds",
        ),
        (
            "a=c,i=7,r=1,c=1,w=1,X=2",
            "EINVAL:the source rectangle is out of bounds",
        ),
    ] {
        term.process(&apc_g(control, b""));
        assert_eq!(
            reply(&mut term),
            format!("\x1b_Gi=7;{verdict}\x1b\\"),
            "{control}"
        );
    }
    assert_eq!(decodes(), before, "a refused compose decodes nothing");
    for _ in 0..3 {
        term.process(&apc_g("a=c,i=7,r=1,c=1,w=1,x=1", b""));
        assert!(
            reply(&mut term).contains(";EBADPNG"),
            "the header alone does not decode"
        );
    }
    assert_eq!(
        decodes() - before,
        1,
        "a frame that did not decode is not decoded again"
    );

    term.process(&apc_g("a=t,f=100,i=8,q=2", &png(2, 1, &[RED, GREEN])));
    let before = decodes();
    term.process(&apc_g("a=c,i=8,r=1,c=1,w=1,x=1", b""));
    assert_eq!(reply(&mut term), "\x1b_Gi=8;OK\x1b\\");
    assert_eq!(
        decodes() - before,
        1,
        "a frame composed onto itself decodes once"
    );
    assert_eq!(
        term.transient.kitty_frames[&8].frames[0].image.bytes,
        [RED, RED].concat(),
        "the PNG frame's (0,0) onto its own (1,0)"
    );
}

#[test]
fn a_png_frame_is_decoded_once_however_many_commands_read_it() {
    const RED: [u8; 4] = [255, 0, 0, 255];
    const GREEN: [u8; 4] = [0, 255, 0, 255];
    const BLUE: [u8; 4] = [0, 0, 255, 255];
    let decodes = || super::KITTY_PNG_DECODES.with(std::cell::Cell::get);
    let mut term = Terminal::new(24, 80);
    term.set_cell_pixel_size(10, 20);
    term.process(&apc_g("a=T,f=100,i=7,q=2", &png(2, 1, &[RED, GREEN])));
    term.process(&apc_g("a=f,f=32,s=2,v=1,i=7,q=2", &[BLUE, BLUE].concat()));
    let before = decodes();
    for _ in 0..3 {
        term.process(&apc_g("a=c,i=7,r=1,c=2,w=1,x=1", b""));
        assert_eq!(reply(&mut term), "\x1b_Gi=7;OK\x1b\\");
    }
    for frame in 3..=5 {
        term.process(&apc_g("a=f,f=32,s=1,v=1,i=7,c=1,x=1", &BLUE));
        assert_eq!(reply(&mut term), format!("\x1b_Gi=7,r={frame};OK\x1b\\"));
    }
    assert_eq!(
        decodes() - before,
        1,
        "six commands read the PNG frame; the first decodes it"
    );
    let t = &term.transient;
    let frames = &t.kitty_frames[&7].frames;
    assert!(matches!(
        frames[0].image.format,
        aterm_grid::ImageFormat::RawRgba8 {
            width: 2,
            height: 1
        }
    ));
    assert_eq!(frames[0].image.bytes, [RED, GREEN].concat(), "same pixels");
    assert_eq!(frames[1].image.bytes, [BLUE, RED].concat());
    assert_eq!(frames[4].image.bytes, [RED, BLUE].concat());
    assert!(
        std::sync::Arc::ptr_eq(&t.kitty_images[&7], &frames[0].image),
        "the frame shown is the one stored decoded"
    );
    let shown = image_at(&term, 0, 0).expect("still placed");
    assert_eq!(shown.image.bytes, [RED, GREEN].concat());
    assert_eq!(
        term.transient.kitty_total_bytes,
        super::kitty_bytes_held(&term.transient, 7),
        "the store charges the decoded frame"
    );
}

#[test]
fn reading_the_shown_png_frame_keeps_the_animation_clock() {
    const RED: [u8; 4] = [255, 0, 0, 255];
    const BLUE: [u8; 4] = [0, 0, 255, 255];
    let at = clock();
    let mut term = Terminal::new(24, 80);
    term.set_cell_pixel_size(10, 20);
    for control in [
        "a=T,f=100,i=7,q=2",
        "a=f,f=32,s=2,v=1,i=7,z=100,q=2",
        "a=a,i=7,r=1,z=40",
        "a=a,i=7,s=3",
    ] {
        let payload = match control.split(',').nth(1) {
            Some("f=100") => png(2, 1, &[RED, RED]),
            Some("f=32") => [BLUE, BLUE].concat(),
            _ => Vec::new(),
        };
        term.process_at(&apc_g(control, &payload), at(0));
    }
    term.process_at(&apc_g("a=c,i=7,r=1,c=2,w=1,x=1,q=2", b""), at(35));
    assert!(
        matches!(
            term.transient.kitty_frames[&7].frames[0].image.format,
            aterm_grid::ImageFormat::RawRgba8 { .. }
        ),
        "the compose read the shown PNG frame and stored it decoded"
    );
    assert!(
        term.advance_kitty_animations(at(40).monotonic).advanced,
        "frame 1's 40 ms run from when it went up, not from the read"
    );
    assert_eq!(placed_frame(&term), Some(BLUE[0]));
}

#[test]
fn a_png_frame_that_does_not_decode_is_kept_and_refused_without_a_second_decode() {
    const BLUE: [u8; 4] = [0, 0, 255, 255];
    let decodes = || super::KITTY_PNG_DECODES.with(std::cell::Cell::get);
    let mut term = Terminal::new(24, 80);
    term.set_cell_pixel_size(10, 20);
    let mut header = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 13];
    header.extend_from_slice(b"IHDR");
    header.extend_from_slice(&2u32.to_be_bytes());
    header.extend_from_slice(&1u32.to_be_bytes());
    term.process(&apc_g("a=T,f=100,i=7,q=2", &header));
    let origin = clock()(0).monotonic;
    let before = decodes();
    for (control, payload) in [
        ("a=c,i=7,r=1,c=1,w=1,x=1", &[][..]),
        ("a=f,f=32,s=1,v=1,i=7,c=1,x=1", &BLUE[..]),
    ] {
        for _ in 0..2 {
            term.process(&apc_g(control, payload));
            assert!(reply(&mut term).contains(";EBADPNG"), "{control}");
            let carry = crate::terminal::PaneStepCarry::capture(&term, origin, 100);
            if let Err(fault) = crate::terminal::PaneStepCarry::from_canonical(&carry.canonical()) {
                panic!("after {control}, the pane carry does not decode: {fault}");
            }
        }
    }
    assert_eq!(decodes() - before, 1, "four reads, one decode");
    let root = &term.transient.kitty_frames[&7].frames[0].image;
    assert!(matches!(root.format, aterm_grid::ImageFormat::Png));
    assert_eq!(root.bytes, header, "the frame keeps its PNG");
    term.process(&apc_g("a=f,f=32,s=2,v=1,i=7", &[BLUE, BLUE].concat()));
    assert_eq!(
        reply(&mut term),
        "\x1b_Gi=7,r=2;OK\x1b\\",
        "a whole frame needs the root's size alone, which its header still gives"
    );
}

#[test]
fn huge_lengths_with_an_in_cell_offset_fill_the_grid() {
    let footprint = |control: &str, payload: &[u8]| {
        let mut term = Terminal::new(24, 80);
        term.set_cell_pixel_size(10, 20);
        term.process(&apc_g(control, payload));
        let placed = image_at(&term, 0, 0).expect("placed");
        (placed.image.cols, placed.image.rows)
    };
    let max = u32::MAX;
    for control in [
        format!("a=T,f=32,s=10,v=20,c={max},X=5,Y=5,C=1,q=2"),
        format!("a=T,f=32,s=10,v=20,r={max},X=5,Y=5,C=1,q=2"),
    ] {
        assert_eq!(footprint(&control, &one_cell_rgba()), (80, 24), "{control}");
    }
    let mut png = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    png.extend_from_slice(&13u32.to_be_bytes());
    png.extend_from_slice(b"IHDR");
    png.extend_from_slice(&max.to_be_bytes());
    png.extend_from_slice(&max.to_be_bytes());
    assert_eq!(footprint("a=T,f=100,X=5,Y=5,C=1,q=2", &png), (80, 24));
}

#[test]
fn a_whole_image_declaring_a_side_over_kittys_limit_is_refused() {
    let max = super::kitty_graphics::MAX_KITTY_DIMENSION;
    let side = usize::try_from(max).expect("fits");
    let one_px = png(1, 1, &[[0; 4]]);
    let cases: [(String, Vec<u8>, &str); 6] = [
        (
            format!("a=t,f=24,s={},v=1,i=7", max + 1),
            vec![0; (side + 1) * 3],
            "EINVAL",
        ),
        (format!("a=t,f=24,s={max},v=1,i=7"), vec![0; side * 3], "OK"),
        (
            format!("a=t,f=32,s=1,v={},i=7", max + 1),
            vec![0; (side + 1) * 4],
            "EINVAL",
        ),
        (format!("a=t,f=32,s=1,v={max},i=7"), vec![0; side * 4], "OK"),
        (
            format!("a=t,f=100,s={},v=1,i=7", max + 1),
            one_px.clone(),
            "EINVAL",
        ),
        (format!("a=t,f=100,s={max},v=1,i=7"), one_px, "OK"),
    ];
    for (control, payload, verdict) in cases {
        let mut term = Terminal::new(24, 80);
        term.set_cell_pixel_size(10, 20);
        term.process(&apc_g(&control, &payload));
        let answer = reply(&mut term);
        assert!(
            answer.starts_with(&format!("\x1b_Gi=7;{verdict}")),
            "{control}: {answer:?}"
        );
        assert_eq!(
            term.transient.kitty_images.contains_key(&7),
            verdict == "OK",
            "{control}: whether it was stored"
        );
    }
}

fn live_image_bytes(term: &Terminal) -> usize {
    let mut seen = std::collections::HashSet::new();
    let mut bytes = 0;
    for row in 0..usize::from(term.grid().rows()) {
        for (_, layer) in term.images_row(row) {
            if seen.insert(std::sync::Arc::as_ptr(&layer.image)) {
                bytes += layer.image.payload_bytes();
            }
        }
    }
    bytes
}

#[test]
fn unstored_kitty_images_stay_within_the_live_budget() {
    use crate::terminal::handler_osc_1337::MAX_LIVE_IMAGE_BYTES;
    let mut term = Terminal::new(24, 80);
    term.set_cell_pixel_size(10, 20);
    let side = 1024;
    let raster = side * side * 4;
    let zeros = zlib_zeros(raster);
    assert_eq!(
        aterm_codec::inflate::zlib_decompress(&zeros, raster).map(|out| out.len()),
        Ok(raster),
        "the helper writes a valid stream"
    );
    for col in 1..=40 {
        term.process(format!("\x1b[1;{col}H").as_bytes());
        let control = format!("a=T,f=32,s={side},v={side},o=z,c=1,r=1,C=1,q=2");
        term.process(&apc_g(&control, &zeros));
        assert!(
            image_at(&term, 0, col - 1).is_some(),
            "the image just sent shows"
        );
        let live = live_image_bytes(&term);
        assert!(
            live <= MAX_LIVE_IMAGE_BYTES + raster,
            "after {col} images the screen holds {live} bytes of them"
        );
    }
}

#[test]
fn kitty_images_freed_behind_a_parked_screen_stay_within_the_live_budget() {
    use crate::terminal::handler_osc_1337::MAX_LIVE_IMAGE_BYTES;
    let mut term = Terminal::new(24, 80);
    term.set_cell_pixel_size(10, 20);
    let side = 1024;
    let raster = side * side * 4;
    let zeros = zlib_zeros(raster);
    let control = format!("a=T,i=1,f=32,s={side},v={side},o=z,c=1,r=1,C=1,q=2");
    for col in 1..=48 {
        term.process(format!("\x1b[1;{col}H").as_bytes());
        term.process(&apc_g(&control, &zeros));
        term.process(b"\x1b[?1049h");
        term.process(&apc_g("a=d,d=I,i=1,q=2", &[]));
        term.process(b"\x1b[?1049l");
        assert!(
            image_at(&term, 0, col - 1).is_some(),
            "the parked screen keeps the image the store freed"
        );
        let live = live_image_bytes(&term);
        assert!(
            live <= MAX_LIVE_IMAGE_BYTES + raster,
            "after {col} rounds the main screen holds {live} bytes of images"
        );
    }
}

#[test]
fn kitty_frames_replaced_behind_a_parked_screen_stay_within_the_live_budget() {
    use crate::terminal::handler_osc_1337::MAX_LIVE_IMAGE_BYTES;
    let mut term = Terminal::new(24, 80);
    term.set_cell_pixel_size(10, 20);
    let side = 1024;
    let raster = side * side * 4;
    let zeros = zlib_zeros(raster);
    let transmit = format!("a=T,i=1,f=32,s={side},v={side},o=z,c=1,r=1,C=1,q=2");
    let edit = format!("a=f,i=1,r=1,f=32,s={side},v={side},o=z,q=2");
    for col in 1..=48 {
        term.process(format!("\x1b[1;{col}H").as_bytes());
        if col == 1 {
            term.process(&apc_g(&transmit, &zeros));
        } else {
            term.process(&apc_g("a=p,i=1,c=1,r=1,C=1,q=2", &[]));
        }
        term.process(b"\x1b[?1049h");
        term.process(&apc_g(&edit, &zeros));
        term.process(b"\x1b[?1049l");
        assert!(
            image_at(&term, 0, col - 1).is_some(),
            "the parked screen keeps the frame the store replaced"
        );
        let live = live_image_bytes(&term);
        assert!(
            live <= MAX_LIVE_IMAGE_BYTES + raster,
            "after {col} rounds the main screen holds {live} bytes of images"
        );
    }
}
