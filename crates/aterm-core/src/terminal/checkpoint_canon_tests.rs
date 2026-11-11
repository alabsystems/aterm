// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;
use crate::terminal::Terminal;

/// A screen that sets most of what a checkpoint carries: styles, history, margins, tab stops, charsets (a 96-set too), both keyboards, progress, cwd, colours and their stack, titles, shell marks, an SGR push, a link, saved modes, both screens and a saved cursor.
fn rich(rows: u16, cols: u16) -> Terminal {
    let mut t = Terminal::new(rows, cols);
    t.modes.allow_palette_reconfigure = true;
    for bytes in [
        &b"\x1b[1;4;38;5;202;48;2;10;20;30mstyled\x1b[0m\r\n"[..],
        b"\x1b[2;5r\x1b[?69h\x1b[2;30s\x1b[?69l\x1b[r",
        b"\x1b[3g\x1b[1;5H\x1bH\x1b[4;1H",
        b"\x1b(0\x1b-A\x1b~",
        b"\x1b[>5u\x1b[>4;2m\x1b]9;4;1;42\x07\x1b]7;file://host/tmp/work\x07",
        b"\x1b]4;1;rgb:12/34/56\x07\x1b]10;rgb:aa/bb/cc\x07\x1b]12;rgb:01/02/03\x07",
        b"\x1b]30001\x07\x1b]4;2;rgb:65/43/21\x07",
        b"\x1b]0;a title\x07\x1b[22;0t\x1b]2;second\x07",
        b"\x1b]133;A\x07$ \x1b]133;B\x07true\r\n\x1b]133;C\x07\x1b]133;D;0\x07",
        b"\x1b]133;A\x07$ \x1b]133;B\x07sleep 9\r\n\x1b]133;C\x07",
        b"\x1b[3;4m\x1b[#{\x1b[0m\x1b]8;id=k;https://example.com\x07link",
        b"\x1b[?1000h\x1b[?1006h\x1b[?2004h\x1b[?1000s\x1b[?2004s",
        b"\x1b7\x1b[5;9H\x1b[?1049hALT\r\n\x1b[2;2H\x1b7",
    ] {
        t.process(bytes);
    }
    for i in 0..30 {
        t.process(format!("line{i}\r\n").as_bytes());
    }
    t
}

fn states() -> Vec<TerminalCheckpoint> {
    let mut out = Vec::new();
    let plain = Terminal::new(4, 10);
    out.push(plain.checkpoint());
    let mut t = rich(8, 40);
    out.push(t.checkpoint());
    out.push(t.checkpoint_visible().expect("at ground"));
    t.process(b"\x1b[?1049l\x1b[1;3");
    out.push(t.checkpoint_continuing().expect("a CSI carries"));
    t.process(b"m\x1b]2;unterminated");
    out.push(t.checkpoint_continuing().expect("an OSC carries"));
    out
}

#[test]
fn a_checkpoint_decodes_to_itself_and_reencodes_to_its_bytes() {
    for (i, c) in states().into_iter().enumerate() {
        let bytes = c.canonical();
        let back =
            TerminalCheckpoint::from_canonical(&bytes).unwrap_or_else(|e| panic!("state {i}: {e}"));
        assert_eq!(back, c, "state {i}");
        assert_eq!(back.canonical(), bytes, "state {i}");
    }
}

#[test]
fn the_rich_state_reaches_the_fields_it_is_meant_to() {
    let c = rich(8, 40).checkpoint();
    assert!(c.alt_grid.is_some() && c.modes.alternate_screen);
    assert!(c.saved_cursor_main.is_some() && c.saved_cursor_alt.is_some());
    assert!(
        c.charset.designated_96(1).is_some(),
        "ESC - A designates G1"
    );
    assert!(!c.color.palette.is_empty() && !c.color.stack_overrides.is_empty());
    assert!(c.title.as_ref().is_some_and(|t| !t.stack.is_empty()));
    assert!(!c.output_state.sgr_stack.is_empty());
    assert!(!c.output_state.saved_modes.is_empty());
    assert!(c.taskbar_progress.is_some() && c.current_working_directory.is_some());
    let mut partial = rich(8, 40);
    partial.process(b"\x1b[1;3");
    assert!(
        partial
            .checkpoint_continuing()
            .is_some_and(|c| c.parser.is_some()),
        "a carried CSI"
    );
}

#[test]
fn a_poisoned_field_changes_the_bytes() {
    let c = rich(8, 40).checkpoint();
    let base = c.canonical();
    let mut poisons: Vec<(&str, TerminalCheckpoint)> = Vec::new();
    let mut p = |name, f: fn(&mut TerminalCheckpoint)| {
        let mut x = c.clone();
        f(&mut x);
        poisons.push((name, x));
    };
    p("modes", |c| {
        c.modes.cursor_visible = !c.modes.cursor_visible
    });
    p("modes.last", |c| {
        c.modes.ambiguous_width_double = !c.modes.ambiguous_width_double;
    });
    p("cursor", |c| c.cursor.cursor_col ^= 1);
    p("saved cursor resized", |c| {
        let saved = c.saved_cursor_main.as_mut().expect("a DECSC slot");
        saved.resized = SavedCursorResized::new(!saved.resized.get());
    });
    p("tab stops", |c| {
        c.cursor.tab_stops[0] = !c.cursor.tab_stops[0]
    });
    p("glyph before the cursor", |c| {
        c.cursor.follows_glyph = !c.cursor.follows_glyph
    });
    p("grid", |c| {
        let last = c.grid.len() - 1;
        c.grid[last] ^= 1;
    });
    p("charset 96", |c| {
        c.charset.designate(1, CharacterSet::Ascii)
    });
    p("kitty", |c| c.kitty_keyboard.alt_sp ^= 1);
    p("xterm keyboard", |c| {
        c.xterm_keyboard.disable_modify_other_keys()
    });
    p("cwd", |c| c.current_working_directory = None);
    p("nonce", |c| {
        c.shell_integration_nonce = Some(ShellIntegrationNonce([7; 32]))
    });
    p("color", |c| {
        c.color.foreground = Some(Rgb { r: 1, g: 2, b: 4 })
    });
    p("shell", |c| c.shell.completed_seq += 1);
    p("title", |c| {
        c.title.as_mut().expect("title").window.push('x')
    });
    p("output", |c| {
        c.output_state.zwj_pending = !c.output_state.zwj_pending
    });
    p("holder", |c| {
        c.holder.seed_untrusted = !c.holder.seed_untrusted
    });
    p("rev", |c| {
        c.shell_integration_rev = Some("0123456789abcdef".into())
    });
    for (name, x) in poisons {
        assert_ne!(x, c, "{name}: precondition, the poison changes the value");
        assert_ne!(x.canonical(), base, "{name}");
    }
}

#[test]
fn bytes_that_are_not_canonical_are_refused_by_field() {
    let c = rich(8, 40).checkpoint();
    let bytes = c.canonical();
    let refused = |b: &[u8]| TerminalCheckpoint::from_canonical(b).expect_err("refused");
    assert_eq!(refused(&bytes[..bytes.len() - 1]).field, "holder");
    let mut trailing = bytes.clone();
    trailing.push(0);
    assert_eq!(refused(&trailing).why, "bytes follow the value");
    let mut magic = bytes.clone();
    magic[0] ^= 1;
    assert_eq!(refused(&magic).field, "magic");
    let mut version = bytes.clone();
    version[CHECKPOINT_MAGIC.len()] ^= 1;
    assert_eq!(refused(&version).why, "a version this build does not read");
    let mut geometry = bytes.clone();
    let at = CHECKPOINT_MAGIC.len() + 4;
    geometry[at..at + 2].copy_from_slice(&0u16.to_le_bytes());
    assert_eq!(refused(&geometry).field, "rows");
    let mut history = c.clone();
    history.history_lines += 1;
    assert_eq!(refused(&history.canonical()).field, "history_lines");
    let mut alt = c.clone();
    alt.alt_cursor = None;
    assert_eq!(refused(&alt.canonical()).field, "alt_cursor");
    let mut grid = c.clone();
    grid.grid.truncate(grid.grid.len() / 2);
    assert_eq!(refused(&grid.canonical()).field, "grid");
}

/// A STREAM VERSION 2 WROTE IS REFUSED AT ITS VERSION, NOT READ ASKEW: version 2 wrote the
/// retired `program_state_began` (an optional u64) after the program's report, and this
/// version writes nothing there. A version-2 stream with a report standing, laid out here as
/// version 2 wrote it, is refused at the magic's version word. The same bytes under this
/// build's version word are read past the magic into the holder, which the version keeps
/// from happening.
#[test]
fn a_version_2_stream_is_refused_at_its_version() {
    let mut t = Terminal::new(4, 20);
    t.process(b"\x1b]7501;state=blocked:app=codex:kind=permission\x1b\\");
    let c = t.checkpoint();
    assert!(
        c.output_state.program_state.is_some(),
        "precondition: version 2 wrote the field while a report stood"
    );
    let bytes = c.canonical();
    let mut holder = W(Vec::new());
    enc_holder(&mut holder, &c.holder);
    let (head, tail) = bytes.split_at(bytes.len() - holder.0.len());
    let laid_out_as_v2 = |version: u32| {
        let mut w = W(head.to_vec());
        w.0[CHECKPOINT_MAGIC.len()..CHECKPOINT_MAGIC.len() + 4]
            .copy_from_slice(&version.to_le_bytes());
        w.opt_u64(Some(t.program_state().began));
        w.raw(tail);
        w.0
    };
    assert_eq!(
        TerminalCheckpoint::from_canonical(&laid_out_as_v2(2)),
        Err(fault("magic", "a version this build does not read"))
    );
    let askew = TerminalCheckpoint::from_canonical(&laid_out_as_v2(CHECKPOINT_CANON_VERSION));
    assert!(
        askew.as_ref().is_err_and(|f| f.field != "magic"),
        "under this build's version word the decode reads on past the magic: {:?}",
        askew.map(drop)
    );
}

#[test]
fn a_config_decodes_to_itself_and_a_poisoned_field_changes_its_bytes() {
    let mut config = TerminalConfig::default();
    assert_eq!(
        TerminalConfig::from_canonical(&config.canonical()),
        Ok(config.clone())
    );
    let base = config.canonical();
    let mut palette = ColorPalette::new();
    palette.set(3, Rgb { r: 9, g: 8, b: 7 });
    config.custom_palette = Some(palette);
    config.color_scheme = Some(Appearance::Light);
    config.scrollback_limit = Some(77);
    config.faint_opacity = 0.25;
    config.cursor_style = CursorStyle::SteadyBar;
    let bytes = config.canonical();
    assert_ne!(bytes, base);
    assert_eq!(TerminalConfig::from_canonical(&bytes), Ok(config.clone()));
    let mut poisoned = config.clone();
    poisoned.bidi.reorder_nsm = !poisoned.bidi.reorder_nsm;
    assert_ne!(poisoned.canonical(), bytes, "the last field counts");
    let mut cut = bytes.clone();
    cut.pop();
    assert!(TerminalConfig::from_canonical(&cut).is_err());
}

fn reply(t: &mut Terminal, query: &[u8]) -> Vec<u8> {
    let _ = t.take_response();
    t.process(query);
    t.take_response().unwrap_or_default()
}

/// A replay restored from a checkpoint takes the session's colours, cursor and mode seeds from its config and keeps every mode the program set.
#[test]
fn a_restored_engine_under_the_sessions_config_answers_as_the_live_one() {
    let mut config = TerminalConfig::default();
    config.default_background = Rgb {
        r: 0xfa,
        g: 0xf0,
        b: 0xe6,
    };
    config.default_foreground = Rgb {
        r: 0x10,
        g: 0x20,
        b: 0x30,
    };
    config.cursor_style = CursorStyle::SteadyBar;
    config.color_scheme = Some(Appearance::Light);
    config.bracketed_paste = false;
    let live = || {
        let mut t = Terminal::new(6, 30);
        let _ = t.apply_config(&config);
        t.process(b"\x1b[?2004h\x1b[1 q\x1b]4;5;rgb:01/02/03\x07");
        let _ = t.take_response();
        t
    };
    let cp = live().checkpoint();
    for keep in [false, true] {
        let mut replay = Terminal::replay_from_checkpoint(&cp);
        if keep {
            replay.apply_config_keeping_modes(&config);
            let _ = replay.take_response();
        }
        let mut probe = live();
        assert_eq!(
            replay.checkpoint().canonical(),
            cp.canonical(),
            "keep: {keep}"
        );
        assert_eq!(replay.modes, probe.modes, "keep: {keep}");
        let queries = [
            &b"\x1b]11;?\x07"[..],
            b"\x1b]10;?\x07",
            b"\x1b]4;5;?\x07",
            b"\x1b[?996n",
        ];
        let answers = queries.map(|q| reply(&mut replay, q) == reply(&mut probe, q));
        assert_eq!(
            answers,
            [keep, keep, true, keep],
            "keep: {keep} (a program's OSC 4 crosses in the checkpoint either way)"
        );
        for t in [&mut replay, &mut probe] {
            t.process(b"\x1b[0 q\x1b[!p");
        }
        assert_eq!(
            replay.modes == probe.modes,
            keep,
            "DECSCUSR 0 and DECSTR, keep: {keep}"
        );
    }
}

#[test]
fn pane_step_carry_roundtrips_and_rejects_noncanonical_shell_rekey_priors() {
    let mut t = Terminal::new(4, 20);
    t.authorize_shell_integration_on_first_mark([0x11; 32]);
    t.authorize_shell_integration_rekey([0x22; 32]);
    t.set_foreground_job(true);
    let origin = aterm_time::Instant::now();
    let carry = PaneStepCarry::capture(&t, origin, 2048);
    assert!(carry.foreground_job);
    assert_eq!(
        carry.awaiting_shell_nonce,
        Some(ShellIntegrationNonce([0x22; 32]))
    );
    assert_eq!(
        carry.typed_rekey_prior,
        Some((Some(ShellIntegrationNonce([0x11; 32])), true))
    );
    let bytes = carry.canonical();
    assert_eq!(PaneStepCarry::from_canonical(&bytes), Ok(carry.clone()));

    let mut flipped_fg = carry.clone();
    flipped_fg.foreground_job = false;
    assert_ne!(flipped_fg.canonical(), bytes);

    let mut orphan_prior = carry.clone();
    orphan_prior.awaiting_shell_nonce = None;
    assert_eq!(
        PaneStepCarry::from_canonical(&orphan_prior.canonical())
            .expect_err("typed_rekey_prior without awaiting_shell_nonce")
            .field,
        "pane_carry.typed_rekey_prior"
    );

    let mut impossible_prior = carry.clone();
    impossible_prior.typed_rekey_prior = Some((None, true));
    assert_eq!(
        PaneStepCarry::from_canonical(&impossible_prior.canonical())
            .expect_err("awaiting prior without nonce")
            .field,
        "pane_carry.typed_rekey_prior"
    );

    let mut both_jobs = carry;
    both_jobs.probe_job_active = true;
    assert_eq!(
        PaneStepCarry::from_canonical(&both_jobs.canonical())
            .expect_err("probe_job_active while foreground_job is true")
            .field,
        "pane_carry.probe_job_active"
    );
}

#[test]
fn pane_step_carry_dcs_query_roundtrips_and_rejects_noncanonical_carries() {
    let origin = aterm_time::Instant::now();
    for (head, tail) in [
        (&b"\x1bP$q"[..], &b"m\x1b\\"[..]),
        (&b"\x1bP$qm\x1b"[..], &b"\\"[..]),
        (&b"\x1bP+q54"[..], &b"4E\x1b\\"[..]),
        (&b"\x1bPtmux;\x1b\x1bP$qm\x1b"[..], &b"\x1b\\\x1b\\"[..]),
    ] {
        let mut live = Terminal::new(4, 20);
        live.process(head);
        let cp = live
            .checkpoint_pane_step(2048)
            .expect("pane step carries query");
        assert!(!cp.parser_ground && cp.parser.is_none());
        let carry = PaneStepCarry::capture(&live, origin, 2048);
        assert!(carry.dcs_query.is_some(), "{head:?}");
        let bytes = carry.canonical();
        let decoded = PaneStepCarry::from_canonical(&bytes).expect("canonical dcs_query");
        assert_eq!(decoded, carry);

        let mut replay = Terminal::replay_from_checkpoint(&cp);
        replay.apply_pane_step_carry(&decoded, origin);
        live.process(tail);
        replay.process(tail);
        assert_eq!(replay.take_response(), live.take_response(), "{head:?}");
        assert_eq!(replay.checkpoint(), live.checkpoint(), "{head:?}");
    }

    let mut base_term = Terminal::new(4, 20);
    base_term.process(b"\x1bP$qm");
    let valid = PaneStepCarry::capture(&base_term, origin, 2048);

    let mut bad_seq = valid.clone();
    bad_seq.dcs_query.as_mut().unwrap().sequence_bytes = 5;
    assert_eq!(
        PaneStepCarry::from_canonical(&bad_seq.canonical())
            .expect_err("data.len() != sequence_bytes.min(cap)")
            .field,
        "pane_carry.dcs_query.sequence_bytes"
    );

    let mut over_budget = valid.clone();
    over_budget.dcs_query.as_mut().unwrap().data = vec![b'm'; 256];
    over_budget.dcs_query.as_mut().unwrap().sequence_bytes =
        u32::try_from(super::super::MAX_DCS_GLOBAL_BUDGET + 1).unwrap();
    assert_eq!(
        PaneStepCarry::from_canonical(&over_budget.canonical())
            .expect_err("sequence_bytes > MAX_DCS_GLOBAL_BUDGET")
            .field,
        "pane_carry.dcs_query.sequence_bytes"
    );

    let mut bad_parser = valid;
    bad_parser.dcs_query.as_mut().unwrap().parser = HookedDcsCarry {
        st_pending: true,
        string_utf8_tail: vec![0xE6],
        tmux: TmuxPassthrough::default(),
    };
    assert_eq!(
        PaneStepCarry::from_canonical(&bad_parser.canonical())
            .expect_err("st_pending with string_utf8_tail")
            .field,
        "pane_carry.dcs_query.parser"
    );
}

#[test]
fn pane_step_carry_kitty_store_numbers_and_pending_roundtrip_and_reject_noncanonical() {
    use crate::terminal::{KittyLayoutCarry, KittyVariantCarry, KittyVariantSourceCarry};
    use aterm_types::{ImageFormat, ImageScaling};

    let origin = aterm_time::Instant::now();
    let cfg = crate::config::TerminalConfig::default();
    let mut live = Terminal::new(6, 30);
    live.set_cell_pixel_size(10, 20);
    // Image 1 (assigned via number 42): 2 RGBA frames (3x1 = 12 bytes each), shown_frame = 1
    // (c=2), playing, two U=1 virtual placements (one cropped variant, one stretched), and a
    // placeholder cell.
    live.process(b"\x1b_Ga=t,f=32,s=3,v=1,I=42;////AAAA////AAAA\x1b\\");
    live.process(b"\x1b_Ga=f,f=32,s=3,v=1,i=1,z=80;AAAA////AAAA////\x1b\\");
    live.process(b"\x1b_Ga=a,i=1,c=2,s=3\x1b\\");
    live.process(b"\x1b_Ga=p,U=1,i=1,p=2,x=1,y=0,w=2,h=1,c=2,r=1\x1b\\");
    live.process(b"\x1b_Ga=p,U=1,i=1,p=3,c=3,r=1\x1b\\");
    live.process("\x1b[2;1H\x1b[38;5;1m\u{10EEEE}\u{0305}\u{0305}\x1b[39m".as_bytes());
    // Image 9: 1x1 RGB (expanded to 1x1 RGBA = 4 bytes) plus an in-flight chunked transfer (`m=1`).
    live.process(b"\x1b_Ga=t,f=24,s=1,v=1,i=9;/wAA\x1b\\");
    live.process(b"\x1b_Ga=t,f=24,s=1,v=1,i=12,m=1;AP\x1b\\");
    let _ = live.take_response();

    let cp = live.checkpoint_pane_step(2048).expect("at ground");
    let carry = PaneStepCarry::capture(&live, origin, 2048);
    assert_eq!(carry.kitty_store.len(), 2);
    assert_eq!(carry.kitty_numbers, vec![(42, 1)]);
    assert!(carry.kitty_pending.is_some());

    let replay_carry = live.capture_replay_carry();
    assert!(replay_carry.retained_image_bytes() >= 12 + 12 + 4 + 2);
    let from_rc = PaneStepCarry::from_replay_carry(&replay_carry, &cp, &cfg, None, origin);
    assert_eq!(from_rc, carry);

    let bytes = carry.canonical();
    let decoded = PaneStepCarry::from_canonical(&bytes).expect("canonical kitty carry");
    assert_eq!(decoded, carry);
    assert_eq!(decoded.canonical(), bytes);

    // Also exercise the Orphan variant arm roundtrip.
    let mut with_orphan = carry.clone();
    with_orphan.kitty_store[1].variants.push(KittyVariantCarry {
        source: KittyVariantSourceCarry::Orphan {
            bytes: vec![7, 8, 9, 255],
            format: ImageFormat::RawRgba8 {
                width: 1,
                height: 1,
            },
        },
        layout: KittyLayoutCarry {
            cols: 2,
            rows: 1,
            z_index: 0,
            cell_offset_px: (0, 0),
            scaling: ImageScaling::Stretch,
            source_rect: None,
        },
    });
    let orphan_bytes = with_orphan.canonical();
    let orphan_decoded =
        PaneStepCarry::from_canonical(&orphan_bytes).expect("orphan variant decodes");
    assert_eq!(orphan_decoded, with_orphan);
    assert_eq!(orphan_decoded.canonical(), orphan_bytes);

    let mut pane_replay = Terminal::replay_from_checkpoint(&cp);
    pane_replay.set_cell_pixel_size(10, 20);
    pane_replay.apply_pane_step_carry(&decoded, origin);

    let mut carry_replay = Terminal::replay_from_checkpoint(&cp);
    carry_replay.set_cell_pixel_size(10, 20);
    carry_replay.apply_replay_carry(&replay_carry);

    let tail = b"\x1b_Gm=0;8A\x1b\\\
                 \x1b[3;1H\x1b_Ga=p,i=12,c=1,r=1\x1b\\\
                 \x1b[4;1H\x1b_Ga=p,I=42,c=2,r=1\x1b\\\
                 \x1b_Ga=a,i=1,c=1\x1b\\\
                 \x1b_Ga=d,d=I,i=1\x1b\\";
    live.process(tail);
    pane_replay.process(tail);
    carry_replay.process(tail);

    let live_resp = live.take_response();
    assert_eq!(pane_replay.take_response(), live_resp);
    assert_eq!(carry_replay.take_response(), live_resp);
    assert_eq!(pane_replay.checkpoint(), live.checkpoint());
    assert_eq!(carry_replay.checkpoint(), live.checkpoint());
    assert_eq!(
        pane_replay.transient.kitty_total_bytes,
        live.transient.kitty_total_bytes
    );
    assert_eq!(
        carry_replay.transient.kitty_total_bytes,
        live.transient.kitty_total_bytes
    );
    assert_eq!(
        pane_replay.transient.live_image_charge,
        live.transient.live_image_charge
    );
    assert_eq!(
        carry_replay.transient.live_image_charge,
        live.transient.live_image_charge
    );
    assert_eq!(
        pane_replay.transient.history_image_room,
        live.transient.history_image_room
    );
    assert_eq!(
        carry_replay.transient.history_image_room,
        live.transient.history_image_room
    );

    // Negative controls on canonical validation.
    let mut zero_id = carry.clone();
    zero_id.kitty_store[0].id = 0;
    assert_eq!(
        PaneStepCarry::from_canonical(&zero_id.canonical())
            .expect_err("zero kitty id")
            .field,
        "pane_carry.kitty_store.id"
    );

    let mut unsorted = carry.clone();
    unsorted.kitty_store.swap(0, 1);
    assert_eq!(
        PaneStepCarry::from_canonical(&unsorted.canonical())
            .expect_err("unsorted kitty ids")
            .field,
        "pane_carry.kitty_store.id"
    );

    let mut bad_shown = carry.clone();
    bad_shown.kitty_store[0].shown_frame = 2;
    assert_eq!(
        PaneStepCarry::from_canonical(&bad_shown.canonical())
            .expect_err("shown_frame >= frames.len()")
            .field,
        "pane_carry.kitty_store.shown_frame"
    );

    let mut bad_cols = carry.clone();
    bad_cols.kitty_store[0].frames[0].layout.cols = 0;
    assert_eq!(
        PaneStepCarry::from_canonical(&bad_cols.canonical())
            .expect_err("zero frame layout cols")
            .field,
        "pane_carry.kitty_layout.cols"
    );

    let mut bad_rgba = carry.clone();
    bad_rgba.kitty_store[0].frames[0].bytes = vec![0u8; 11];
    assert_eq!(
        PaneStepCarry::from_canonical(&bad_rgba.canonical())
            .expect_err("RGBA byte length mismatch")
            .field,
        "pane_carry.kitty_raster.bytes"
    );

    let mut dup_variant = carry.clone();
    let v0 = dup_variant.kitty_store[0].variants[0].clone();
    dup_variant.kitty_store[0].variants.push(v0);
    assert_eq!(
        PaneStepCarry::from_canonical(&dup_variant.canonical())
            .expect_err("duplicate variant layout")
            .field,
        "pane_carry.kitty_store.variants"
    );

    let mut orphan_num = carry.clone();
    orphan_num.kitty_numbers = vec![(42, 999)];
    assert_eq!(
        PaneStepCarry::from_canonical(&orphan_num.canonical())
            .expect_err("kitty_numbers id missing from store")
            .field,
        "pane_carry.kitty_numbers"
    );

    let mut settled_pending = carry;
    settled_pending.kitty_pending.as_mut().unwrap().more = false;
    assert_eq!(
        PaneStepCarry::from_canonical(&settled_pending.canonical())
            .expect_err("kitty_pending with more == false")
            .field,
        "pane_carry.kitty_pending"
    );
}

#[test]
fn pane_step_carry_placed_images_floating_relations_and_history_roundtrip_and_reject_noncanonical()
{
    let origin = aterm_time::Instant::now();
    let cfg = crate::config::TerminalConfig::default();
    let mut live = Terminal::new(6, 30);
    live.set_cell_pixel_size(10, 20);

    // 1. Place an iTerm2 OSC 1337 2-row loose image at the top and scroll its top row into
    // primary history (`history_rev`), keeping its second row on screen (`visible`) sharing
    // the same LooseImageCarry slot.
    live.process(
        b"\x1b]1337;File=inline=1;width=3;height=2;preserveAspectRatio=0:QUJDREVGRw==\x1b\\",
    );
    live.process(b"\x1b[6;1H\r\n");

    // 2. Unnamed (`i=0`) Kitty `a=T` loose placement on the primary screen.
    live.process(b"\x1b[1;10H\x1b_Ga=T,f=24,s=1,v=1,c=2,r=1;/wAA\x1b\\");

    // 3. Stored 2-frame Kitty image `i=1` with:
    //    - cell parent `p=10` + cell-relative child `p=11` (`kitty_relations`),
    //    - virtual parent `p=2` + placeholder + floating child `p=21` (`kitty_floating`).
    live.process(b"\x1b_Ga=t,f=32,s=2,v=1,i=1;////AAAA//8=\x1b\\");
    live.process(b"\x1b_Ga=f,f=32,s=2,v=1,i=1,z=40;AAAA//////8=\x1b\\");
    live.process(b"\x1b[3;2H\x1b_Ga=p,i=1,p=10,c=2,r=1\x1b\\");
    live.process(b"\x1b_Ga=p,i=1,p=11,P=1,Q=10,H=3,V=0,c=1,r=1\x1b\\");
    live.process(b"\x1b_Ga=p,U=1,i=1,p=2,c=2,r=1\x1b\\");
    live.process("\x1b[4;2H\x1b[38;5;1;58;5;2m\u{10EEEE}\u{0305}\u{0305}\x1b[39;59m".as_bytes());
    live.process(b"\x1b_Ga=p,i=1,p=21,P=1,Q=2,H=1,V=0,c=1,r=1\x1b\\");

    // 4. Enter alternate screen and place another loose OSC 1337 image so both `active_images`
    // (alt) and `inactive_images` (primary visible + history_rev) carry placements.
    live.process(b"\x1b[?1049h\x1b[2;2H\x1b]1337;File=inline=1;width=2;height=1:WFla\x1b\\");
    let _ = live.take_response();

    let cp = live.checkpoint_pane_step(2048).expect("at ground");
    let carry = PaneStepCarry::capture(&live, origin, 2048);
    assert_eq!(
        carry.loose_images.len(),
        3,
        "alt OSC1337 + primary OSC1337 + primary i=0 a=T"
    );
    assert_eq!(carry.kitty_floating.len(), 1);
    assert_eq!(carry.kitty_relations.len(), 1);
    assert!(!carry.active_images.visible.is_empty());
    let inactive = carry.inactive_images.as_ref().expect("alt up -> inactive");
    assert!(!inactive.visible.is_empty());
    assert!(!inactive.history_rev.is_empty());

    let replay_carry = live.capture_replay_carry();
    assert!(replay_carry.retained_image_bytes() > 0);
    let from_rc = PaneStepCarry::from_replay_carry(&replay_carry, &cp, &cfg, None, origin);
    assert_eq!(from_rc, carry);

    let bytes = carry.canonical();
    let decoded = PaneStepCarry::from_canonical(&bytes).expect("canonical placed carry");
    assert_eq!(decoded, carry);
    assert_eq!(decoded.canonical(), bytes);

    let mut pane_replay = Terminal::replay_from_checkpoint(&cp);
    pane_replay.set_cell_pixel_size(10, 20);
    pane_replay.apply_pane_step_carry(&decoded, origin);

    let mut carry_replay = Terminal::replay_from_checkpoint(&cp);
    carry_replay.set_cell_pixel_size(10, 20);
    carry_replay.apply_replay_carry(&replay_carry);

    // Leave alt screen, move parent p=10 (cascades to relative child p=11), switch frame 2 on
    // i=1, and delete parent p=10 (`d=P` cascades to p=11).
    let base_clock = super::super::ClockReading::now();
    let c1 = super::super::ClockReading {
        monotonic: origin + core::time::Duration::from_millis(50),
        ..base_clock
    };
    let tail = b"\x1b[?1049l\
                 \x1b[3;6H\x1b_Ga=p,i=1,p=10,c=2,r=1\x1b\\\
                 \x1b_Ga=a,i=1,c=2\x1b\\";
    live.process_at(tail, c1);
    pane_replay.process_at(tail, c1);
    carry_replay.process_at(tail, c1);

    let live_resp = live.take_response();
    assert_eq!(pane_replay.take_response(), live_resp);
    assert_eq!(carry_replay.take_response(), live_resp);
    assert_eq!(pane_replay.checkpoint(), live.checkpoint());
    assert_eq!(carry_replay.checkpoint(), live.checkpoint());
    assert_eq!(
        PaneStepCarry::capture(&pane_replay, origin, 2048),
        PaneStepCarry::capture(&live, origin, 2048),
    );
    assert_eq!(
        PaneStepCarry::capture(&carry_replay, origin, 2048),
        PaneStepCarry::capture(&live, origin, 2048),
    );

    // Cascading delete of parent p=10 removes both p=10 and relative child p=11 identically.
    let c2 = super::super::ClockReading {
        monotonic: origin + core::time::Duration::from_millis(100),
        ..base_clock
    };
    let del = b"\x1b_Ga=d,d=P,i=1,p=10\x1b\\";
    live.process_at(del, c2);
    pane_replay.process_at(del, c2);
    carry_replay.process_at(del, c2);
    assert_eq!(
        PaneStepCarry::capture(&pane_replay, origin, 2048),
        PaneStepCarry::capture(&live, origin, 2048),
    );
    assert_eq!(
        PaneStepCarry::capture(&carry_replay, origin, 2048),
        PaneStepCarry::capture(&live, origin, 2048),
    );

    // Negative controls on v5 canonical validation.
    let mut unref_loose = carry.clone();
    let extra_loose = unref_loose.loose_images[0].clone();
    unref_loose.loose_images.push(extra_loose);
    assert_eq!(
        PaneStepCarry::from_canonical(&unref_loose.canonical())
            .expect_err("unreferenced loose image")
            .field,
        "pane_carry.loose_images"
    );

    let mut swapped_loose = carry.clone();
    swapped_loose.active_images.visible[0].1[0].image = PlacedImageSourceCarry::Loose(1);
    assert_eq!(
        PaneStepCarry::from_canonical(&swapped_loose.canonical())
            .expect_err("Loose(1) referenced before Loose(0)")
            .field,
        "pane_carry.placed_grid.span.image"
    );

    let mut bad_store_ref = carry.clone();
    bad_store_ref.active_images.visible[0].1[0].image = PlacedImageSourceCarry::KittyFrame {
        image_id: 999,
        frame: 0,
    };
    assert_eq!(
        PaneStepCarry::from_canonical(&bad_store_ref.canonical())
            .expect_err("unstored KittyFrame image_id")
            .field,
        "pane_carry.placed_grid.span.image"
    );

    let mut dup_float = carry.clone();
    let fl0 = dup_float.kitty_floating[0];
    dup_float.kitty_floating.push(fl0);
    assert_eq!(
        PaneStepCarry::from_canonical(&dup_float.canonical())
            .expect_err("duplicate floating serial")
            .field,
        "pane_carry.kitty_floating.tag"
    );

    let mut dup_rel = carry.clone();
    let rel0 = dup_rel.kitty_relations[0];
    dup_rel.kitty_relations.push(rel0);
    assert_eq!(
        PaneStepCarry::from_canonical(&dup_rel.canonical())
            .expect_err("duplicate relation serial")
            .field,
        "pane_carry.kitty_relations.child"
    );

    let mut bad_span = carry.clone();
    bad_span.active_images.visible[0].1[0].image_row = 99;
    assert_eq!(
        PaneStepCarry::from_canonical(&bad_span.canonical())
            .expect_err("span image_row >= layout.rows")
            .field,
        "pane_carry.placed_grid.span"
    );

    let mut bad_inactive = carry;
    bad_inactive.inactive_images = None;
    assert_eq!(
        PaneStepCarry::from_canonical(&bad_inactive.canonical())
            .expect_err("inactive_images None while inactive_handback Some")
            .field,
        "pane_carry.inactive_images"
    );
}
