// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The per-sequence response cap on OSC palette queries. The parser stops at
//! 16 OSC params (`MAX_OSC_PARAMS`), so these tests call the handler
//! directly via `split_for_process` to feed more.
use super::LEGACY_PALETTE_PER_SEQUENCE_MAX;
use crate::terminal::Terminal;
use crate::terminal::response_capability::ResponseCapability;

fn count_osc_frames(bytes: &[u8]) -> usize {
    let mut count = 0;
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == 0x1b && bytes[i + 1] == b']' {
            count += 1;
            i += 2;
        } else {
            i += 1;
        }
    }
    count
}

fn build_osc_4_query_params(n: usize) -> Vec<Vec<u8>> {
    let mut params: Vec<Vec<u8>> = Vec::with_capacity(1 + n * 2);
    params.push(b"4".to_vec());
    for idx in 0..n {
        params.push(idx.to_string().into_bytes());
        params.push(b"?".to_vec());
    }
    params
}

#[test]
fn osc_4_query_256_pairs_capped_at_per_sequence_limit() {
    let mut term = Terminal::new(24, 80);
    let params_owned = build_osc_4_query_params(256);

    let cap = ResponseCapability::mint_for_dispatch();
    let (_parser, mut handler) = term.split_for_process();
    let params_refs: Vec<&[u8]> = params_owned.iter().map(Vec::as_slice).collect();
    handler.handle_osc_4(&cap, &params_refs);
    drop(handler);

    let response = term
        .take_response()
        .expect("256-pair OSC 4 query must emit at least one response");
    let frames = count_osc_frames(&response);
    assert_eq!(
        frames, LEGACY_PALETTE_PER_SEQUENCE_MAX,
        "256-pair OSC 4 query must emit exactly {} response frames, got {}. \
         Without the cap this would be 256.",
        LEGACY_PALETTE_PER_SEQUENCE_MAX, frames,
    );
}

#[test]
fn osc_4_query_8_pairs_all_emit_below_cap() {
    let mut term = Terminal::new(24, 80);
    let params_owned = build_osc_4_query_params(8);

    let cap = ResponseCapability::mint_for_dispatch();
    let (_parser, mut handler) = term.split_for_process();
    let params_refs: Vec<&[u8]> = params_owned.iter().map(Vec::as_slice).collect();
    handler.handle_osc_4(&cap, &params_refs);
    drop(handler);

    let response = term
        .take_response()
        .expect("8-pair OSC 4 query must emit responses");
    let frames = count_osc_frames(&response);
    assert_eq!(
        frames, 8,
        "8-pair OSC 4 query (below cap of {}) must emit all 8 response \
         frames, got {}",
        LEGACY_PALETTE_PER_SEQUENCE_MAX, frames,
    );
}

#[test]
fn osc_4_set_operations_unaffected_by_query_cap() {
    let mut term = Terminal::new(24, 80);
    term.modes_mut().allow_palette_reconfigure = true;

    let mut params_owned: Vec<Vec<u8>> = Vec::new();
    params_owned.push(b"4".to_vec());
    for idx in 0..=LEGACY_PALETTE_PER_SEQUENCE_MAX {
        params_owned.push(idx.to_string().into_bytes());
        params_owned.push(b"?".to_vec());
    }
    params_owned.push(b"200".to_vec());
    params_owned.push(b"rgb:ff/00/00".to_vec());

    let cap = ResponseCapability::mint_for_dispatch();
    let (_parser, mut handler) = term.split_for_process();
    let params_refs: Vec<&[u8]> = params_owned.iter().map(Vec::as_slice).collect();
    handler.handle_osc_4(&cap, &params_refs);
    drop(handler);

    let response = term.take_response().unwrap_or_default();
    let frames = count_osc_frames(&response);
    assert_eq!(
        frames, LEGACY_PALETTE_PER_SEQUENCE_MAX,
        "query responses must be capped at {} even when mixed with set \
         operations",
        LEGACY_PALETTE_PER_SEQUENCE_MAX,
    );
    assert_eq!(
        term.palette_color(200),
        aterm_types::Rgb::new(0xff, 0x00, 0x00),
        "SET operation after the query cap must still apply",
    );
}

#[test]
fn osc_21_query_pairs_capped_at_per_sequence_limit() {
    let mut term = Terminal::new(24, 80);

    let mut params_owned: Vec<Vec<u8>> = Vec::new();
    params_owned.push(b"21".to_vec());
    for idx in 0u8..32 {
        params_owned.push(format!("{idx}=?").into_bytes());
    }

    let cap = ResponseCapability::mint_for_dispatch();
    let (_parser, mut handler) = term.split_for_process();
    let params_refs: Vec<&[u8]> = params_owned.iter().map(Vec::as_slice).collect();
    handler.handle_osc_21(&cap, &params_refs);
    drop(handler);

    let response = term
        .take_response()
        .expect("OSC 21 query must emit a response");
    let reply_str = std::str::from_utf8(&response).expect("OSC 21 reply is ASCII");
    let body = reply_str
        .strip_prefix("\x1b]21;")
        .and_then(|s| s.strip_suffix("\x1b\\").or_else(|| s.strip_suffix('\x07')))
        .expect("OSC 21 reply must carry header and terminator");
    let pairs: Vec<&str> = body.split(';').collect();
    assert_eq!(
        pairs.len(),
        LEGACY_PALETTE_PER_SEQUENCE_MAX,
        "OSC 21 response must contain exactly {} pairs (cap), got {}",
        LEGACY_PALETTE_PER_SEQUENCE_MAX,
        pairs.len(),
    );
    for (idx, pair) in pairs.iter().enumerate() {
        assert!(
            pair.starts_with(&format!("{idx}=rgb:")),
            "unexpected OSC 21 pair[{idx}]: {pair:?}"
        );
    }
}

#[test]
fn osc_recolor_with_no_glyph_marks_damage_and_advances_epoch() {
    let mut term = Terminal::new(24, 80);
    term.process(b"X");
    term.take_damage();
    let epoch0 = term.damage_epoch();
    assert!(
        !term.has_damage(),
        "damage consumed by the simulated present"
    );

    term.process(b"\x1b]11;rgb:0000/0000/8080\x1b\\");
    assert!(
        term.has_damage(),
        "an OSC default-bg recolor must mark grid damage even with no glyph written",
    );
    assert_eq!(
        term.damage_epoch(),
        epoch0 + 1,
        "the recolor must advance damage_epoch so the frontend repaints",
    );
}

#[test]
fn osc_19_selection_foreground_set_query_and_reset_are_stateful() {
    let mut term = Terminal::new(4, 8);
    term.take_damage();
    let before = term.damage_epoch();
    let foreground = aterm_types::Rgb::new(0x11, 0x22, 0x33);

    term.process(b"\x1b]19;rgb:11/22/33\x1b\\");
    assert_eq!(term.selection_foreground(), Some(foreground));
    assert!(
        term.has_damage(),
        "OSC 19 SET must repaint a live selection"
    );
    assert!(
        term.damage_epoch() > before,
        "OSC 19 SET advances the frontend redraw key"
    );

    term.process(b"\x1b]19;?\x07");
    let expected = format!(
        "\x1b]19;{}\x07",
        aterm_types::ColorPalette::format_color_spec(foreground),
    )
    .into_bytes();
    assert_eq!(term.take_response(), Some(expected));

    term.take_damage();
    term.process(b"\x1b]119\x1b\\");
    assert_eq!(term.selection_foreground(), None);
    assert!(
        term.has_damage(),
        "OSC 119 RESET must repaint a live selection"
    );
}

#[test]
fn osc_21_selection_foreground_set_and_query_share_osc_19_state() {
    let mut term = Terminal::new(4, 8);
    let foreground = aterm_types::Rgb::new(0x12, 0x34, 0x56);

    term.process(b"\x1b]21;selection_foreground=rgb:12/34/56\x07");
    assert_eq!(term.selection_foreground(), Some(foreground));

    term.process(b"\x1b]21;selection_foreground=?\x07");
    let expected = format!(
        "\x1b]21;selection_foreground={}\x07",
        aterm_types::ColorPalette::format_color_spec(foreground),
    )
    .into_bytes();
    assert_eq!(term.take_response(), Some(expected));
}

#[test]
fn osc_21_queries_report_undefined_and_unknown_fields() {
    let mut term = Terminal::new(4, 8);

    term.process(
        b"\x1b]21;selection_background=?;selection_foreground=?;\
          cursor=?;not_a_color=?\x1b\\",
    );
    assert_eq!(
        term.take_response(),
        Some(
            b"\x1b]21;selection_background=;selection_foreground=;\
              cursor=;not_a_color=?\x1b\\"
                .to_vec()
        ),
    );
}

#[test]
fn osc_21_unknown_key_is_echoed_only_when_well_formed() {
    let mut term = Terminal::new(4, 8);
    let long = "k".repeat(super::MAX_OSC21_ECHO_KEY_BYTES + 1);
    let seq = format!(
        "\x1b]21;rm\x7f-rf=?;echo pwned=?;a/b=?;{long}=?;transparent_background_color1=?\x1b\\"
    );
    term.process(seq.as_bytes());
    assert_eq!(
        term.take_response(),
        Some(b"\x1b]21;transparent_background_color1=?\x1b\\".to_vec()),
    );

    term.process(b"\x1b]21;bad key=?;x\x7f=?;+1=?\x1b\\");
    assert_eq!(term.take_response(), None, "nothing echoable, no reply");
}

#[test]
fn osc_21_empty_values_are_dynamic_and_bare_keys_reset() {
    let mut term = Terminal::new(4, 8);
    let configured_cursor = aterm_types::Rgb::new(0x10, 0x20, 0x30);
    let configured_bg = aterm_types::Rgb::new(0x40, 0x50, 0x60);
    let configured_fg = aterm_types::Rgb::new(0x70, 0x80, 0x90);
    let mut config = crate::config::TerminalConfig::default();
    config.cursor_color = Some(configured_cursor);
    config.selection_background = Some(configured_bg);
    config.selection_foreground = Some(configured_fg);
    term.apply_config(&config);

    term.process(
        b"\x1b]21;cursor=rgb:aa/bb/cc;\
          selection_background=rgb:bb/cc/dd;\
          selection_foreground=rgb:cc/dd/ee\x1b\\",
    );
    term.process(
        b"\x1b]21;cursor=;selection_background=;\
          selection_foreground=\x1b\\",
    );
    assert_eq!(term.cursor_color(), None);
    assert_eq!(term.selection_background(), None);
    assert_eq!(term.selection_foreground(), None);
    assert_eq!(
        term.cell_frame(4, 8).selection_fg,
        crate::render::COLOR_DYNAMIC,
        "the frame must preserve an explicit automatic foreground instead of delegating to a stale renderer setting",
    );

    term.process(
        b"\x1b]21;cursor;selection_background;\
          selection_foreground\x1b\\",
    );
    assert_eq!(term.cursor_color(), Some(configured_cursor));
    assert_eq!(term.selection_background(), Some(configured_bg));
    assert_eq!(term.selection_foreground(), Some(configured_fg));
}

#[test]
fn osc_21_bare_numeric_key_resets_without_set_permission() {
    let mut term = Terminal::new(4, 8);
    let default = term.palette_color(42);

    let mut palette = aterm_types::ColorPalette::new();
    palette.set(42, aterm_types::Rgb::new(0xaa, 0xbb, 0xcc));
    let mut config = crate::config::TerminalConfig::default();
    config.custom_palette = Some(palette);
    term.apply_config(&config);
    assert_eq!(
        term.palette_color(42),
        aterm_types::Rgb::new(0xaa, 0xbb, 0xcc)
    );

    term.modes_mut().allow_palette_reconfigure = true;
    term.process(b"\x1b]21;42=rgb:01/02/03\x1b\\");
    assert_eq!(
        term.palette_color(42),
        aterm_types::Rgb::new(0x01, 0x02, 0x03)
    );
    term.modes_mut().allow_palette_reconfigure = false;
    term.process(b"\x1b]21;42\x1b\\");
    assert_eq!(
        term.palette_color(42),
        aterm_types::Rgb::new(0xaa, 0xbb, 0xcc)
    );

    config.custom_palette = None;
    term.apply_config(&config);
    term.modes_mut().allow_palette_reconfigure = true;
    term.process(b"\x1b]21;42=rgb:04/05/06\x1b\\");
    assert_eq!(
        term.palette_color(42),
        aterm_types::Rgb::new(0x04, 0x05, 0x06)
    );
    term.modes_mut().allow_palette_reconfigure = false;
    term.process(b"\x1b]21;42\x1b\\");
    assert_eq!(term.palette_color(42), default);
}

#[test]
fn color_stack_pop_restores_a_dynamic_cursor_as_dynamic() {
    let mut term = Terminal::new(4, 8);
    term.process(
        b"\x1b]30001\x1b\\\
          \x1b]12;rgb:aa/bb/cc\x1b\\",
    );
    assert!(term.cursor_color().is_some());
    term.process(b"\x1b]30101\x1b\\");
    assert_eq!(term.cursor_color(), None);
}

#[test]
fn color_stack_round_trip_restores_both_selection_colors() {
    let mut term = Terminal::new(4, 8);
    let saved_bg = aterm_types::Rgb::new(0x10, 0x20, 0x30);
    let saved_fg = aterm_types::Rgb::new(0x40, 0x50, 0x60);

    term.process(
        b"\x1b]17;rgb:10/20/30\x1b\\\
          \x1b]19;rgb:40/50/60\x1b\\\
          \x1b]30001\x1b\\\
          \x1b]17;rgb:a0/b0/c0\x1b\\\
          \x1b]19;rgb:d0/e0/f0\x1b\\\
          \x1b]30101\x1b\\",
    );

    assert_eq!(term.selection_background(), Some(saved_bg));
    assert_eq!(term.selection_foreground(), Some(saved_fg));
}

#[test]
fn selection_color_resets_and_ris_restore_host_baselines() {
    let mut term = Terminal::new(4, 8);
    let configured_bg = aterm_types::Rgb::new(0x21, 0x32, 0x43);
    let configured_fg = aterm_types::Rgb::new(0x54, 0x65, 0x76);
    let mut config = crate::config::TerminalConfig::default();
    config.selection_background = Some(configured_bg);
    config.selection_foreground = Some(configured_fg);
    term.apply_config(&config);

    term.process(
        b"\x1b]17;rgb:aa/bb/cc\x1b\\\
          \x1b]19;rgb:dd/ee/ff\x1b\\\
          \x1b]117\x1b\\\
          \x1b]119\x1b\\",
    );
    assert_eq!(term.selection_background(), Some(configured_bg));
    assert_eq!(term.selection_foreground(), Some(configured_fg));

    term.process(
        b"\x1b]17;rgb:aa/bb/cc\x1b\\\
          \x1b]19;rgb:dd/ee/ff\x1b\\\
          \x1bc",
    );
    assert_eq!(
        term.selection_background(),
        Some(configured_bg),
        "RIS restores the configured selection background"
    );
    assert_eq!(
        term.selection_foreground(),
        Some(configured_fg),
        "RIS restores configured selected-text ink"
    );
}
