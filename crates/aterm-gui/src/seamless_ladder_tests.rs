// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE PRODUCER IS TOTAL (the 2026-09-22/23 update audit, plan P0-1 and
//! P0-4d): whatever a session's engine holds, [`carry_for_wire`] hands the wire
//! a carry this build's own predicates admit, at the highest rung they allow —
//! and the consumer in the same build decodes it exactly.

use aterm_core::terminal::{HostBindings, Terminal, TerminalCheckpoint};

use super::*;

/// A deterministic xorshift, so a failing step names a reproducible state.
fn xorshift(seed: u64) -> impl FnMut() -> u64 {
    let mut state = seed;
    move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    }
}

/// One step of the HOSTILE WALK: every byte class the 2026-09-22/23 audit found
/// a producer refusing on, or that the old 3000-step walk never emitted — OSC 7
/// and OSC 633 directories carrying random `%xx` (NUL included) and 4 KiB paths,
/// OSC 8 links up to 64 spans of up to 8 KiB, SGR 38/48/58 colours, wide,
/// combining and ZWJ clusters, kitty keyboard push/pop, SCS charsets, OSC / DCS /
/// APC strings left unterminated (the parser then idles mid-string until a later
/// step's bytes, or nothing, end it), geometry up to 400x120, and the message
/// band's one-row toggle between entering 1049 and the capture — plus every
/// sequence the old walk drove (DECSC/DECRC on both screens, 1047/1049, DECSTBM,
/// DECLRMM+DECSLRM, origin mode, TBC/HTS, RIS).
fn hostile_walk_step(t: &mut Terminal, next: &mut impl FnMut() -> u64) {
    let roll = next();
    let a = u16::try_from((roll >> 8) % 120).expect("< 120") + 1;
    let b = u16::try_from((roll >> 24) % 400).expect("< 400") + 1;
    let pick = (roll >> 40) as usize;
    match roll % 30 {
        0 => t.process(b"$ command\r\n"),
        1 => t.process(format!("\x1b[{a};{b}H").as_bytes()),
        2 => t.process(b"\x1b7"),
        3 => t.process(b"\x1b8"),
        4 => t.process(b"\x1b[?1049h"),
        5 => t.process(b"\x1b[?1049l"),
        6 => t.process(b"\x1b[?1047h"),
        7 => t.process(b"\x1b[?1047l"),
        8 => t.process(format!("\x1b[{};{}r", a.min(b), a.max(b)).as_bytes()),
        9 => t.process(b"\x1b[r"),
        10 => t.process(format!("\x1b[?69h\x1b[{};{}s", a.min(b), a.max(b)).as_bytes()),
        11 => t.process(b"\x1b[?69l\x1b[?6l"),
        12 => t.process(b"\x1b[?6h"),
        13 => t.process(b"\x1b[3g\x1bH\t"),
        14 => t.process(b"\x1bc"),
        15 => t.process(&[b'x'; 200]),
        16 => t.resize(a, b),
        // OSC 7 / OSC 633 P;Cwd with random %xx, NUL included, up to 4 KiB.
        17 | 18 => {
            let len = 1 + pick % 4096;
            let mut path = String::with_capacity(len + 16);
            while path.len() < len {
                let byte = next();
                if byte.is_multiple_of(5) {
                    path.push_str(&format!("%{:02x}", (byte >> 8) % 256));
                } else {
                    path.push(char::from(b'a' + u8::try_from((byte >> 8) % 26).expect("< 26")));
                }
                if byte.is_multiple_of(11) {
                    path.push('/');
                }
            }
            if roll % 30 == 17 {
                t.process(format!("\x1b]7;file://host/{path}\x07").as_bytes());
            } else {
                t.process(format!("\x1b]633;P;Cwd=/{path}\x07").as_bytes());
            }
        }
        // OSC 8: up to 64 one-cell spans, each URL up to 8 KiB.
        19 => {
            let spans = 1 + pick % 64;
            let url_len = 1 + (pick >> 8) % 8192;
            let mut bytes = Vec::new();
            for span in 0..spans {
                let url = format!("https://example.test/{span}/{}", "u".repeat(url_len));
                bytes.extend_from_slice(format!("\x1b]8;;{url}\x1b\\L\x1b]8;;\x1b\\ ").as_bytes());
            }
            t.process(&bytes);
        }
        // SGR 38/48/58 in both the 256 and the direct colour forms, underline styles.
        20 => t.process(
            format!(
                "\x1b[38;5;{}m\x1b[48;2;{};{};{}m\x1b[58;5;{}m\x1b[4:3mcolour\x1b[1;3;9mbold\x1b[0m",
                pick % 256,
                (pick >> 8) % 256,
                (pick >> 16) % 256,
                a,
                b % 256
            )
            .as_bytes(),
        ),
        // Wide, combining and ZWJ clusters, bold, running to the right margin.
        21 => {
            let cluster = [
                "\u{6f22}\u{5b57}",
                "e\u{301}",
                "\u{1F468}\u{200D}\u{1F4BB}",
                "\u{1F1FA}\u{1F1F8}",
                "a\u{300}\u{301}\u{302}",
            ][pick % 5];
            t.process(format!("\x1b[1m{}{cluster}\x1b[0m", "a".repeat(pick % 420)).as_bytes());
        }
        // Kitty keyboard push / pop / set.
        22 => t.process([&b"\x1b[>1u"[..], b"\x1b[<u", b"\x1b[=5;1u", b"\x1b[>31u"][pick % 4]),
        // SCS: DEC graphics into G0/G1, shifts, a line drawn in it.
        23 => t.process(
            [
                &b"\x1b(0lqqk\x1b(B"[..],
                b"\x1b)0\x0eqqq\x0f",
                b"\x1b*A\x1bn#\x1bo",
                b"\x1b(0",
            ][pick % 4],
        ),
        // Unterminated strings, then idle: the capture lands mid-string.
        24 => t.process(
            [
                &b"\x1b]0;a title with no end"[..],
                b"\x1bP$q",
                b"\x1b_Gf=24,s=1,v=1;AAAA",
                b"\x1b]8;;https://example.test/half",
                b"\x1b[38;5",
            ][pick % 5],
        ),
        // The message band's one-row toggle between entering 1049 and the capture.
        25 => {
            t.process(b"\x1b[?1049h");
            let (rows, cols) = (t.rows(), t.cols());
            t.resize(rows.saturating_sub(1).max(1), cols);
        }
        26 => {
            let (rows, cols) = (t.rows(), t.cols());
            t.resize(rows.saturating_add(1).min(120), cols);
        }
        // DECSC on the last row, then the band takes a row.
        27 => {
            let (rows, cols) = (t.rows(), t.cols());
            t.process(format!("\x1b[{rows};1H\x1b7").as_bytes());
            t.resize(rows.saturating_sub(1).max(1), cols);
        }
        // A terminator for whatever string is open.
        28 => t.process(b"\x1b\\\x07"),
        _ => t.process(b"prompt % "),
    }
}

/// What this build's CONSUMER makes of `checkpoint` as a producer publishes it:
/// the meta through the frozen required-key parse, the grids through
/// `admit_incoming_screen`. `Err` is the cause it degraded the screen with.
fn consumer_decodes(checkpoint: &TerminalCheckpoint) -> Result<TerminalCheckpoint, String> {
    let carry = ScreenCarry {
        schema: ScreenCarry::SCHEMA,
        meta: aterm_json::to_string(&CheckpointMeta::from_checkpoint(checkpoint))
            .map_err(|error| error.to_string())?,
        grid_file: String::new(),
        alt_grid_file: None,
        repaint: false,
    };
    let wire = ScreenWireBytes {
        grid: checkpoint.grid.clone(),
        alt_grid: checkpoint.alt_grid.clone(),
    };
    match admit_incoming_screen(parse_checkpoint_meta(&carry), wire, &mut 0) {
        IncomingScreen::Exact(decoded) => Ok(decoded),
        IncomingScreen::Degraded { cause, .. } => Err(cause),
    }
}

/// Everything a carry the producer emits must satisfy: this build's own full
/// predicate commits it, and this build's consumer adopts it EXACTLY — the
/// same checkpoint, not a degraded one.
fn assert_wire_admits(at: &str, checkpoint: &TerminalCheckpoint) {
    screen_digest(&[(0, checkpoint.clone())])
        .unwrap_or_else(|refusal| panic!("{at}: this build's own predicate refused it: {refusal}"));
    let decoded = consumer_decodes(checkpoint)
        .unwrap_or_else(|cause| panic!("{at}: this build's consumer degraded it: {cause}"));
    assert_eq!(
        &decoded, checkpoint,
        "{at}: the consumer adopts exactly what was carried"
    );
}

/// `n` one-cell OSC 8 links, each with a URL of exactly `url_len` bytes (at
/// most the engine's 8 KiB `MAX_HYPERLINK_URL_BYTES`, past which it drops the
/// link): past a small pane's line-record cap once `n * url_len` passes
/// `16 KiB + cols * 512`, so the line's record cannot take the wire's shape at
/// any history depth (the finding's reproduction: 8 links of 8 KiB in an
/// 80-column pane).
fn link_dense(n: usize, url_len: usize) -> Vec<u8> {
    let mut bytes = Vec::new();
    for link in 0..n {
        let mut url = format!("https://example.test/{link}/");
        let pad = url_len.saturating_sub(url.len());
        url.push_str(&"u".repeat(pad));
        bytes.extend_from_slice(format!("\x1b]8;;{url}\x1b\\L\x1b]8;;\x1b\\ ").as_bytes());
    }
    bytes
}

/// EVERY REACHABLE ENGINE STATE IS ADMITTED BY THE WIRE — now over the hostile
/// grammar ([`hostile_walk_step`]) and with NO state skipped (the 2026-09-22/23
/// update audit, plan P0-4d). The old walk drove only cursor and mode
/// sequences at up to 64x96 and `continue`d past every non-Ground state, which
/// is exactly where two of the refusals lived; through the pre-ladder pipeline
/// this walk is red on 105 mid-sequence states, 285 dimension refusals and 514
/// non-canonical grids (measured before the fix).
///
/// Every state is carried by [`carry_for_wire`] and must be committed by
/// `screen_digest` and adopted exactly by the consumer. Two fidelity
/// properties ride along: an honest engine never breaks a meta bound (no state
/// may need the Sanitized rung — one that did would be the 2026-09-22 class,
/// a producer/predicate disagreement), and a Full carry of a Ground engine is
/// byte-identical to the pre-ladder `checkpoint_carry`, so healthy handoffs
/// are unchanged. The only Repaint the grammar can reach is a link-dense line
/// past the record cap (plan P2-4 would strip its links instead).
#[test]
fn every_reachable_engine_state_is_admitted_by_the_wire() {
    let mut next = xorshift(0x9E37_79B9_7F4A_7C15);
    let mut t = Terminal::new(24, 80);
    let mut rungs = [0_u32; 4];
    let mut mid_sequence = 0_u32;
    for step in 0..3000_u32 {
        hostile_walk_step(&mut t, &mut next);
        let (checkpoint, rung, cause) = carry_for_wire(
            &t,
            0,
            MAX_HANDOFF_HISTORY_LINES,
            &mut 0,
            WireCaps::current(),
        );
        let at = format!(
            "step {step} at {}x{} ({rung}{})",
            t.rows(),
            t.cols(),
            cause
                .as_deref()
                .map(|cause| format!(": {cause}"))
                .unwrap_or_default()
        );
        assert_ne!(
            rung,
            CarryRung::Sanitized,
            "{at}: an honest engine broke a meta bound"
        );
        if rung == CarryRung::Repaint {
            assert!(
                cause
                    .as_deref()
                    .is_some_and(|cause| cause.contains("is not canonical")),
                "{at}: only a line past the record cap may cost the screen"
            );
        }
        if t.parser_is_ground() {
            if rung == CarryRung::Full {
                assert_eq!(
                    Some(&checkpoint),
                    t.checkpoint_carry(MAX_HANDOFF_HISTORY_LINES as usize)
                        .as_ref(),
                    "{at}: a healthy carry is byte-identical to the pre-ladder one"
                );
            }
        } else {
            mid_sequence += 1;
        }
        assert_wire_admits(&at, &checkpoint);
        rungs[rung as usize] += 1;
    }
    assert!(
        mid_sequence > 0 && rungs[CarryRung::Repaint as usize] > 0,
        "the walk must reach the states it was widened for: {mid_sequence} mid-sequence, \
         rungs {rungs:?}"
    );
    assert!(
        rungs[CarryRung::Full as usize] > 2000,
        "the ladder must not cost healthy states their screen: rungs {rungs:?}"
    );
}

/// THE PRODUCER IS TOTAL OVER THE DESKS THAT REFUSED THE UPDATE (the
/// 2026-09-22/23 update audit, plan P0-1). Each desk below refused every
/// in-session update through the pre-ladder pipeline — (i) a directory OSC 7
/// reported with `%00`, (ii) a styled full-width row with a combining mark or
/// a ZWJ emoji on the saved primary under 1049, (iii) a title left
/// unterminated (`printf '\e]0;x'`), (iv) a full-screen 5K and a maximized 6K
/// window, (v) a DECSC on the last row, then 1049, then the message band's
/// one-row shrink — and (vi) a link-dense line past the record cap still does.
/// Each is now carried at a named rung, committed by `screen_digest`, adopted
/// exactly by the consumer, and the whole pool settles.
#[test]
fn producer_is_total_over_hostile_and_large_desks() {
    use CarryRung::{Full, Repaint, Sanitized};

    let mut desks: Vec<(&str, Terminal, CarryRung)> = Vec::new();

    let mut nul_cwd = Terminal::new(24, 80);
    nul_cwd.process(b"\x1b]7;file:///tmp/ok\x07\x1b]7;file:///tmp/a%00b\x07prompt % ");
    assert_eq!(
        nul_cwd.current_working_directory(),
        Some("/tmp/ok"),
        "the engine no longer stores a NUL directory (plan P0-4c)"
    );
    desks.push(("(i) OSC 7 %00", nul_cwd, Full));

    // An engine already HOLDING a NUL directory — adopted from a build whose
    // engine still stored one — is carried with the directory dropped and the
    // screen exact.
    let mut source = Terminal::new(24, 80);
    source.process(b"prompt % ls\r\nfile\r\nprompt % ");
    let mut held = source.checkpoint_carry(0).expect("Ground");
    held.current_working_directory = Some("/tmp/a\0b".to_string());
    let held = Terminal::from_checkpoint(&held, HostBindings::none());
    // What the Sanitized rung must equal: that engine's own exact carry, with
    // only the directory dropped.
    let mut held_exact = held.checkpoint_carry(0).expect("Ground");
    assert_eq!(
        held_exact.current_working_directory.as_deref(),
        Some("/tmp/a\0b"),
        "PRECONDITION: the restored engine holds the NUL directory"
    );
    held_exact.current_working_directory = None;
    desks.push(("(i') a held NUL directory", held, Sanitized));

    for cluster in ["e\u{301}", "\u{1F468}\u{200D}\u{1F4BB}"] {
        let mut t = Terminal::new(55, 149);
        let fill = 149 - if cluster.starts_with('e') { 1 } else { 2 };
        t.process(format!("\x1b[1m{}{cluster}\x1b[0m\r\n", "a".repeat(fill)).as_bytes());
        t.process(b"\x1b[?1049h");
        desks.push(("(ii) a styled full row on the saved primary", t, Full));
    }

    let mut title = Terminal::new(24, 80);
    title.process(b"prompt % printf '\\e]0;x'\r\n\x1b]0;x");
    assert!(
        title.checkpoint_carry(0).is_none(),
        "control: the pre-ladder carry has nothing to send"
    );
    desks.push(("(iii) an unterminated title", title, Full));

    for (rows, cols) in [(99_u16, 338_u16), (116, 397)] {
        let mut t = Terminal::new(rows, cols);
        t.process(b"prompt % ");
        desks.push(("(iv) a full-screen 5K / maximized 6K window", t, Full));
    }

    let mut band = Terminal::new(56, 149);
    band.process(b"\x1b[56;1Hprompt % \x1b7\x1b[?1049h");
    band.resize(55, 149);
    desks.push((
        "(v) DECSC on the last row, 1049, the band's shrink",
        band,
        Full,
    ));

    let mut links = Terminal::new(24, 80);
    links.process(&link_dense(8, 8192));
    desks.push(("(vi) a link-dense line past the record cap", links, Repaint));

    let mut pool = Vec::new();
    let mut cells = 0_u64;
    for (index, (what, terminal, expected)) in desks.iter().enumerate() {
        let local_id = index as u64;
        let (checkpoint, rung, cause) = carry_for_wire(
            terminal,
            local_id,
            MAX_HANDOFF_HISTORY_LINES,
            &mut cells,
            WireCaps::current(),
        );
        let at = format!("{what} ({rung}: {cause:?})");
        assert_eq!(rung, *expected, "{at}");
        assert_eq!(
            cause.is_some(),
            rung != Full,
            "{at}: a rung below Full names its cause"
        );
        assert_wire_admits(&at, &checkpoint);
        match rung {
            Sanitized => {
                assert_eq!(
                    checkpoint, held_exact,
                    "{at}: the screen and every other scalar are exact"
                );
            }
            Repaint => {
                let blank = Terminal::new(24, 80).checkpoint_carry(0).expect("Ground");
                assert_eq!(checkpoint.grid, blank.grid, "{at}: the screen is blank");
            }
            _ => {}
        }
        pool.push(WireCarry {
            local_id,
            checkpoint,
            rung,
        });
    }
    // (v): the DECSC slot rides raw — the row the grid no longer has.
    let band = &pool[7].checkpoint;
    assert_eq!(
        band.saved_cursor_main.map(|saved| saved.cursor_row),
        Some(55)
    );
    let before: Vec<CarryRung> = pool.iter().map(|carry| carry.rung).collect();
    let digest = settle_wire_carries(&mut pool, WireCaps::current()).expect("the pool commits");
    assert_eq!(
        pool.iter().map(|carry| carry.rung).collect::<Vec<_>>(),
        before,
        "a pool whose carries each passed commits without lowering any"
    );
    let screens: Vec<(u64, TerminalCheckpoint)> = pool
        .into_iter()
        .map(|carry| (carry.local_id, carry.checkpoint))
        .collect();
    assert_eq!(screen_digest(&screens), Ok(digest));
}

/// A ROLLBACK hands to an older consumer whose per-grid cap is the frozen
/// 32 Ki cells, so a full-screen 5K window must reach it as a blank screen it
/// admits — never as a grid it refuses, which would cost the whole rollback
/// (plan P0-4a). The Repaint carry is 24x80 at most, so a legacy consumer
/// admits it too.
#[test]
fn a_downgrade_target_carries_an_over_cap_window_as_a_repaint() {
    for (rows, cols) in [(99_u16, 338_u16), (116, 397)] {
        let mut t = Terminal::new(rows, cols);
        t.process(b"\x1b[?1049hvim\x1b[5;7r");
        let mut cells = 0;
        let (checkpoint, rung, cause) = carry_for_wire(
            &t,
            3,
            MAX_HANDOFF_HISTORY_LINES,
            &mut cells,
            WireCaps::legacy(),
        );
        assert_eq!(rung, CarryRung::Repaint, "{rows}x{cols}: {cause:?}");
        assert!(
            cause
                .as_deref()
                .is_some_and(|cause| cause.contains("per-grid cap")),
            "{rows}x{cols}: the cause names the cap that bound: {cause:?}"
        );
        assert_eq!((checkpoint.rows, checkpoint.cols), (24, 80));
        assert!(
            checkpoint.alt_grid.is_some() && checkpoint.modes.alternate_screen,
            "the alternate screen survives, so the app's 1049 exit has a grid to return to"
        );
        assert!(
            WireCaps::legacy().admit(&mut 0, 24, 80, 0, true).is_ok(),
            "the legacy consumer admits it"
        );
        assert_wire_admits(&format!("{rows}x{cols} legacy"), &checkpoint);
    }
}

/// THE SANITIZED RUNG CLAMPS EXACTLY THE FIELDS THAT BROKE A BOUND, in the
/// consumer's order, and never touches the grids (plan P0-1a); a bound it has
/// no clamp for — a geometry the protocol does not have — sends the carry on
/// to the Repaint rung instead.
#[test]
fn sanitizing_clamps_exactly_the_fields_that_broke_a_bound() {
    let mut t = Terminal::new(6, 40);
    t.process(b"\x1b[3;5Hx\x1b7");
    let mut checkpoint = t.checkpoint_carry(0).expect("Ground");
    let grid = checkpoint.grid.clone();
    checkpoint.cursor.cursor_row = 9;
    checkpoint.cursor.scroll_bottom = 6;
    checkpoint.cursor.tab_stops.truncate(39);
    checkpoint
        .saved_cursor_main
        .as_mut()
        .expect("DECSC slot")
        .cursor_row = aterm_core::grid::MAX_GRID_ROWS;
    checkpoint.current_working_directory = Some("/tmp/a\0b".to_string());

    let clamped = sanitize_checkpoint_for_wire(&mut checkpoint).expect("every field has a clamp");
    assert_eq!(
        clamped
            .iter()
            .map(|violation| (violation.slot, violation.field))
            .collect::<Vec<_>>(),
        [
            ("cursor", "cursor_row"),
            ("cursor", "scroll_bottom"),
            ("cursor", "tab_stops.len()"),
            ("saved_cursor_main", "cursor_row"),
            ("", "current_working_directory NUL bytes"),
        ]
    );
    assert_eq!(checkpoint.cursor.cursor_row, 5);
    assert_eq!(
        (
            checkpoint.cursor.scroll_top,
            checkpoint.cursor.scroll_bottom
        ),
        (0, 5)
    );
    assert_eq!(checkpoint.cursor.tab_stops.len(), 40);
    assert_eq!(
        checkpoint.saved_cursor_main.map(|saved| saved.cursor_row),
        Some(aterm_core::grid::MAX_GRID_ROWS - 1)
    );
    assert_eq!(checkpoint.current_working_directory, None);
    assert_eq!(checkpoint.grid, grid, "the grids are never touched");
    assert_wire_admits("sanitized", &checkpoint);

    let mut zero = t.checkpoint_carry(0).expect("Ground");
    zero.rows = 0;
    let why = sanitize_checkpoint_for_wire(&mut zero).expect_err("no clamp for a geometry");
    assert!(why.contains("has no sanitizer"), "{why}");
}

/// THE REPAINT RUNG IS ADMITTED BY THIS BUILD'S OWN PREDICATES, FOR EVERY
/// GEOMETRY, BOTH CAPS AND ANY AGGREGATE (plan P0-1c): the one capture `Err`
/// that is about a screen — a Repaint carry refused by its own build — is
/// pinned unreachable here. That includes the engine's extreme shapes, the
/// alternate screen, a pool whose aggregate is already spent (the carry falls
/// to an uncharged 1x1 blank that the self-check then makes room for), and a
/// rollback's legacy cap.
#[test]
fn the_repaint_rung_is_admitted_for_every_geometry_cap_and_aggregate() {
    let full = MAX_HANDOFF_AGGREGATE_GRID_CELLS;
    for (rows, cols) in [
        (1_u16, 1_u16),
        (24, 80),
        (99, 338),
        (4096, 1),
        (1, 4096),
        (512, 4096),
    ] {
        for alt in [false, true] {
            let mut t = Terminal::new(rows, cols);
            t.process(b"\x1b7\x1b]0;half");
            if alt {
                t.process(b"\x07\x1b[?1049h");
            }
            for caps in [WireCaps::current(), WireCaps::legacy()] {
                for spent in [0, full / 2, full - 1, full] {
                    let mut cells = spent;
                    let at = format!("{rows}x{cols} alt={alt} {caps:?} spent={spent}");
                    let (checkpoint, rung, cause) =
                        repaint_carry_for_wire(&t, 9, &mut cells, caps, "the test".to_string());
                    assert_eq!(rung, CarryRung::Repaint, "{at}");
                    assert_eq!(cause.as_deref(), Some("the test"), "{at}");
                    assert_wire_admits(&at, &checkpoint);
                    assert!(cells <= full, "{at}: never charged past the aggregate");
                    assert!(
                        caps.admit(&mut 0, checkpoint.rows, checkpoint.cols, 0, true)
                            .is_ok(),
                        "{at}: the successor's cap admits it"
                    );
                }
            }
        }
    }
}

/// THE SELF-CHECK LOWERS WHAT `screen_digest` BLAMES instead of refusing the
/// update (plan P0-1d) — the session it names goes to the Repaint rung, the
/// others are untouched — and what it cannot lower comes back TYPED: a
/// refusal of its own Repaint carry names the session (the pinned-unreachable
/// bug), and too many sessions is a pool fact no rung can change.
#[test]
fn the_self_check_lowers_the_blamed_session_and_types_what_it_cannot() {
    let carried = |local_id: u64, text: &[u8]| {
        let mut t = Terminal::new(24, 80);
        t.process(text);
        let (checkpoint, rung, _) = carry_for_wire(&t, local_id, 0, &mut 0, WireCaps::current());
        WireCarry {
            local_id,
            checkpoint,
            rung,
        }
    };
    let mut pool = vec![
        carried(0, b"one % "),
        carried(1, b"two % "),
        carried(2, b"three % "),
    ];
    let untouched = pool[0].checkpoint.clone();
    pool[1]
        .checkpoint
        .grid
        .extend_from_slice(&[0xff, 0x00, 0x7f]);
    let digest = settle_wire_carries(&mut pool, WireCaps::current()).expect("the pool commits");
    assert_eq!(pool[1].rung, CarryRung::Repaint, "the blamed session");
    assert_eq!(pool[0].rung, CarryRung::Full);
    assert_eq!(pool[0].checkpoint, untouched, "the others are untouched");
    assert_eq!(pool[2].rung, CarryRung::Full);
    let screens: Vec<(u64, TerminalCheckpoint)> = pool
        .iter()
        .map(|carry| (carry.local_id, carry.checkpoint.clone()))
        .collect();
    assert_eq!(screen_digest(&screens), Ok(digest));

    // Its own Repaint carry refused: typed, naming the session, no loop.
    pool[1].checkpoint.grid.extend_from_slice(&[0xff]);
    let refusal = settle_wire_carries(&mut pool, WireCaps::current())
        .expect_err("a Repaint carry is lowered at most once");
    assert_eq!(refusal.local_id(), Some(1), "{refusal}");

    let fresh = CheckpointMeta::from_checkpoint(&Terminal::new(1, 1).checkpoint_carry(0).unwrap());
    let mut crowd: Vec<WireCarry> = (0..=MAX_HANDOFF_SESSIONS as u64)
        .map(|local_id| WireCarry {
            local_id,
            checkpoint: unadmitted_blank_carry(&fresh),
            rung: CarryRung::Full,
        })
        .collect();
    assert!(matches!(
        settle_wire_carries(&mut crowd, WireCaps::current()),
        Err(ScreenDigestRefusal::TooManySessions { .. })
    ));
}

/// A POOL OVER THE AGGREGATE CELL BUDGET LOWERS ITS LARGEST SESSION (plan
/// P0-1d): two screens at the per-grid ceiling fill the 4 Mi-cell aggregate
/// exactly, so a third session tips it over. The self-check carries the
/// largest blank at a size the pool can still afford, and the rest exactly.
#[test]
fn a_pool_over_the_aggregate_lowers_its_largest_session() {
    let (rows, cols) = (512_u16, 4096_u16);
    let big = Terminal::new(rows, cols)
        .checkpoint_carry(0)
        .expect("Ground");
    let small = {
        let mut t = Terminal::new(24, 80);
        t.process(b"prompt % ");
        t.checkpoint_carry(0).expect("Ground")
    };
    let mut pool = vec![
        WireCarry {
            local_id: 0,
            checkpoint: big.clone(),
            rung: CarryRung::Full,
        },
        WireCarry {
            local_id: 1,
            checkpoint: big,
            rung: CarryRung::Full,
        },
        WireCarry {
            local_id: 2,
            checkpoint: small.clone(),
            rung: CarryRung::Full,
        },
    ];
    let digest = settle_wire_carries(&mut pool, WireCaps::current()).expect("the pool commits");
    let lowered: Vec<u64> = pool
        .iter()
        .filter(|carry| carry.rung == CarryRung::Repaint)
        .map(|carry| carry.local_id)
        .collect();
    assert_eq!(lowered.len(), 1, "exactly one session pays: {lowered:?}");
    assert_ne!(lowered, [2], "and it is a large one");
    assert_eq!(pool[2].checkpoint, small, "the small session is exact");
    let screens: Vec<(u64, TerminalCheckpoint)> = pool
        .into_iter()
        .map(|carry| (carry.local_id, carry.checkpoint))
        .collect();
    assert_eq!(screen_digest(&screens), Ok(digest));
}

/// Lowering a carry's scrollback needs no engine lock: the visible records of
/// its own strict decode, re-encoded, are byte-identical to the engine's own
/// visible-only carry — so the self-check's first step for a pool over the
/// real-byte aggregate costs history, never fidelity (plan P0-1d).
#[test]
fn dropping_carried_history_equals_the_engines_visible_only_carry() {
    let mut t = Terminal::new(10, 40);
    for line in 0..60 {
        t.process(format!("\x1b[3{}mline {line}\x1b[0m\r\n", line % 8).as_bytes());
    }
    t.process(b"\x1b]8;;https://example.test\x1b\\link\x1b]8;;\x1b\\");
    let (checkpoint, rung, _) = carry_for_wire(
        &t,
        4,
        MAX_HANDOFF_HISTORY_LINES,
        &mut 0,
        WireCaps::current(),
    );
    assert_eq!(rung, CarryRung::Full);
    assert!(
        checkpoint.history_lines > 0,
        "PRECONDITION: history carried"
    );
    let mut pool = vec![WireCarry {
        local_id: 4,
        checkpoint,
        rung,
    }];
    lower_wire_carry(&mut pool, 0, false, WireCaps::current(), "the test");
    assert_eq!(pool[0].rung, CarryRung::VisibleOnly);
    assert_eq!(
        Some(&pool[0].checkpoint),
        t.checkpoint_carry(0).as_ref(),
        "the lowered carry is the engine's own visible-only carry"
    );
    lower_wire_carry(&mut pool, 0, false, WireCaps::current(), "the test");
    assert_eq!(
        pool[0].rung,
        CarryRung::Repaint,
        "with no history left, the next step is the screen"
    );
    assert_wire_admits("lowered twice", &pool[0].checkpoint);
}

/// THE PRODUCER'S REPAINT REACHES THE SUCCESSOR (plan P0-1e): a session the
/// capture carried blank is marked `ScreenCarry::repaint` in the manifest —
/// only that one — the successor adopts it flagged for a redraw and without a
/// control carry, adopts every other session exactly, and the adoption proof
/// still equals the parent's, because the flag rides no digest.
#[test]
#[cfg(unix)]
fn a_producer_repaint_reaches_the_successor_and_the_proof_still_matches() {
    use super::tests::{
        ENV_LOCK, RestoreVar, StageCarry, child_proof_from, pipe_pair, stage_ladder_handoff,
    };
    use std::sync::PoisonError;

    let _env = ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let _restore = [
        RestoreVar::new("XDG_RUNTIME_DIR"),
        RestoreVar::new("HOME"),
        RestoreVar::new(ENV_MANIFEST),
        RestoreVar::new(ENV_NONCE),
        RestoreVar::new(ENV_FDS),
        RestoreVar::new(ENV_LAYOUT),
        RestoreVar::new(ENV_TARGET),
        RestoreVar::new(ENV_READY_FD),
        RestoreVar::new(ENV_COMMIT_FD),
        RestoreVar::new(ENV_PARENT_PID),
        RestoreVar::new(ENV_PARENT_BIRTH),
    ];
    let mut healthy = Terminal::new(24, 80);
    healthy.process(b"\x1b[1;38;5;202muser@mac\x1b[0m ~ % ls\r\nCargo.toml\r\n% ");
    let mut mid_title = Terminal::new(24, 80);
    mid_title.process(b"% \x1b]0;x");
    let mut links = Terminal::new(24, 80);
    links.process(&link_dense(8, 8192));
    let mut cells = 0;
    let carried: Vec<(TerminalCheckpoint, CarryRung, Option<String>)> =
        [&healthy, &mid_title, &links]
            .into_iter()
            .enumerate()
            .map(|(local_id, terminal)| {
                carry_for_wire(
                    terminal,
                    local_id as u64,
                    MAX_HANDOFF_HISTORY_LINES,
                    &mut cells,
                    WireCaps::current(),
                )
            })
            .collect();
    assert_eq!(
        carried.iter().map(|(_, rung, _)| *rung).collect::<Vec<_>>(),
        [CarryRung::Full, CarryRung::Full, CarryRung::Repaint]
    );
    let repaint: Vec<u64> = carried
        .iter()
        .enumerate()
        .filter(|(_, (_, rung, _))| rung.needs_repaint())
        .map(|(local_id, _)| local_id as u64)
        .collect();
    let stage = StageCarry {
        screens: carried
            .into_iter()
            .map(|(checkpoint, _, _)| checkpoint)
            .collect(),
        controls: Vec::new(),
        next_turn_id: None,
    };
    let staged = stage_ladder_handoff("ladder", &stage, &repaint);

    let body = std::fs::read_to_string(&staged.manifest_path).expect("read manifest");
    let (_, toml) = body.split_once('\n').expect("nonce header");
    let published = SessionHandoff::from_toml(toml).expect("parse the published manifest");
    for record in &published.sessions {
        assert_eq!(
            record.screen.as_ref().map(|screen| screen.repaint),
            Some(record.local_id == 2),
            "session {}: only the blank carry is marked",
            record.local_id
        );
    }

    let (ready_read, ready_write) = pipe_pair("ready");
    let (commit_read, commit_write) = pipe_pair("commit");
    staged.publish_env(
        ready_write,
        commit_read,
        Some(encode_target_identity(
            crate::build_info::BUILD_NUMBER.parse::<u64>().unwrap_or(0),
            crate::build_info::GIT_COMMIT,
        )),
    );
    let incoming = take_incoming_as(ReceiverShape::Current);
    assert_eq!(incoming.adopted.len(), 3, "every session adopts");
    assert_eq!(incoming.screen_digest, Some(staged.screen_digest));
    for adopted in &incoming.adopted {
        let checkpoint = adopted.checkpoint.as_ref().expect("a screen to adopt onto");
        assert_eq!(
            adopted.repaint,
            adopted.local_id == 2,
            "session {}: only the blank carry pulses a redraw",
            adopted.local_id
        );
        assert_eq!(
            checkpoint.grid,
            stage.screens[usize::try_from(adopted.local_id).expect("small")].grid,
            "session {}: adopted exactly as carried",
            adopted.local_id
        );
        if adopted.local_id == 2 {
            assert!(
                adopted.control.is_none(),
                "a repainted session carries no control"
            );
        }
    }
    let ((proof, ready, adopted), _) =
        child_proof_from(incoming).expect("the child adopts and proves");
    assert_eq!(adopted.len(), 3);
    assert_eq!(
        proof, staged.expected,
        "THE HANDOFF COMPLETES: the child's proof equals the parent's expectation"
    );
    drop(ready);
    for fd in [ready_read, commit_read, commit_write] {
        aterm_pty::close_fd(fd);
    }
    staged.teardown();
}

/// THE PRODUCER IS TOTAL OVER ARBITRARY INPUT (the 2026-09-22/23 update audit,
/// plan P0-1's property): whatever bytes a program wrote, however the pane was
/// resized between them, and whichever successor it hands to, the carry
/// [`carry_for_wire`] returns is committed by this build's own `screen_digest`,
/// fits the successor's caps, and is adopted EXACTLY by this build's consumer.
/// The walk above is one long deterministic trajectory; this samples many short
/// ones, with raw bytes biased toward the introducers and terminators that
/// leave the parser mid-sequence, and geometry out to the engine's
/// `MAX_GRID_ROWS`/`MAX_GRID_COLS` on thin shapes (a full 4096x4096 engine is
/// a quarter-gigabyte allocation per case). A rollback's legacy per-grid cap
/// is what sends ordinary 120x400 panes down the Repaint rung here.
mod arbitrary {
    use super::*;
    use proptest::prelude::*;

    /// One step of a program's life: bytes it wrote, or a resize of its pane.
    #[derive(Debug, Clone)]
    enum Op {
        Bytes(Vec<u8>),
        Resize(u16, u16),
    }

    /// A pane geometry: ordinary panes mostly, and the engine's extreme thin
    /// shapes out to its protocol ceiling.
    fn geometry() -> impl Strategy<Value = (u16, u16)> {
        prop_oneof![
            4 => (1..=120_u16, 1..=400_u16),
            1 => (1..=aterm_core::grid::MAX_GRID_ROWS, 1..=8_u16),
            1 => (1..=8_u16, 1..=aterm_core::grid::MAX_GRID_COLS),
        ]
    }

    /// Bytes a program might write: any byte at all, plus the fragments that
    /// open and close control strings, switch screens, save cursors and draw
    /// multi-codepoint clusters, so short samples still reach those states.
    fn fragment() -> impl Strategy<Value = Vec<u8>> {
        prop_oneof![
            4 => any::<u8>().prop_map(|byte| vec![byte]),
            2 => "[ -~]{1,24}".prop_map(String::into_bytes),
            2 => "[0-9]{1,4}".prop_map(String::into_bytes),
            1 => prop::sample::select(vec![
                &b"\x1b"[..],
                b"\x1b[",
                b"\x1b]",
                b"\x1bP",
                b"\x1b_",
                b"\x1b\\",
                b"\x07",
                b";",
                b"\r\n",
                b"\x1b[?1049h",
                b"\x1b[?1049l",
                b"\x1b[?1047h",
                b"\x1b7",
                b"\x1b8",
                b"\x1b[?69h\x1b[2;5s",
                b"\x1b[3;9r",
                b"\x1b]7;file:///tmp/a%00b\x07",
                b"\x1b]633;P;Cwd=/tmp/%ff%00\x07",
                b"\x1b]8;;https://example.test/l\x1b\\",
                b"\x1b[1;38;5;202;48;2;1;2;3;58;5;9m",
                "e\u{301}".as_bytes(),
                "\u{1F468}\u{200D}\u{1F4BB}".as_bytes(),
                "\u{6f22}".as_bytes(),
                b"\x1b[>1u",
                b"\x1b[<u",
                b"\x1b(0",
                b"\x1bc",
            ])
            .prop_map(<[u8]>::to_vec),
        ]
    }

    fn op() -> impl Strategy<Value = Op> {
        prop_oneof![
            6 => prop::collection::vec(fragment(), 0..48).prop_map(|parts| Op::Bytes(parts.concat())),
            1 => geometry().prop_map(|(rows, cols)| Op::Resize(rows, cols)),
        ]
    }

    proptest! {
        #![proptest_config(ProptestConfig {
            cases: 128,
            ..ProptestConfig::default()
        })]
        #[test]
        fn carry_for_wire_is_admitted_for_arbitrary_input(
            (rows, cols) in geometry(),
            ops in prop::collection::vec(op(), 0..24),
            legacy in any::<bool>(),
            want_history in 0..=MAX_HANDOFF_HISTORY_LINES,
        ) {
            let mut t = Terminal::new(rows, cols);
            for op in &ops {
                match op {
                    Op::Bytes(bytes) => t.process(bytes),
                    Op::Resize(rows, cols) => t.resize(*rows, *cols),
                }
            }
            let caps = if legacy { WireCaps::legacy() } else { WireCaps::current() };
            let mut cells = 0;
            let (checkpoint, rung, cause) = carry_for_wire(&t, 0, want_history, &mut cells, caps);
            let at = format!("{}x{} ({rung}: {cause:?})", t.rows(), t.cols());
            prop_assert_eq!(cause.is_some(), rung != CarryRung::Full, "{}", at);
            prop_assert!(
                caps.admit(
                    &mut 0,
                    checkpoint.rows,
                    checkpoint.cols,
                    checkpoint.history_lines,
                    checkpoint.alt_grid.is_some(),
                )
                .is_ok(),
                "{}: the successor's caps admit the carry",
                at
            );
            prop_assert!(cells <= MAX_HANDOFF_AGGREGATE_GRID_CELLS, "{}", at);
            assert_wire_admits(&at, &checkpoint);
        }
    }
}

/// EVERY BLANK FALLBACK KEEPS THE SHELL'S MARK AUTHORITY (the review of the
/// merge of main's shell-integration nonce carry into the update audit's
/// degrade ladder). The adopted shell keeps signing its OSC 133/633 marks with
/// the nonce it was spawned with, and main's rule is that the requirement is
/// never cleared as a fallback. The carries of last resort — the producer's
/// Repaint rung and the consumer's degrade, each past a spent aggregate, where
/// only the 1x1 blank is left — used to carry a FRESH engine's modes: the
/// requirement off and no nonce, so the successor adopted that shell with
/// `integration=off` and any program's output there could forge command marks
/// and exit codes. With no readable meta at all, the adopted engine holds the
/// requirement itself and reports the loss.
///
/// RED before the fix: `require_shell_integration_nonce` was false on both
/// carries, and the adopted posture read `Off`.
#[test]
fn every_blank_fallback_keeps_the_shells_mark_authority() {
    use aterm_core::terminal::ShellIntegrationPosture;
    let mut shell = Terminal::new(24, 80);
    shell.authorize_shell_integration([0x5a; 32]);
    shell.set_require_shell_integration_nonce(true);
    let source = shell.checkpoint_carry(0).expect("Ground");
    assert!(source.shell_integration_nonce.is_some());
    let meta = CheckpointMeta::from_checkpoint(&source);

    let mut spent = MAX_HANDOFF_AGGREGATE_GRID_CELLS;
    let producer = repaint_carry(&source, &mut spent, WireCaps::current());
    let mut spent = MAX_HANDOFF_AGGREGATE_GRID_CELLS;
    let IncomingScreen::Degraded {
        checkpoint: Some(consumer),
        ..
    } = admit_incoming_screen(
        Some(meta),
        ScreenWireBytes {
            grid: source.grid.clone(),
            alt_grid: source.alt_grid.clone(),
        },
        &mut spent,
    )
    else {
        panic!("a screen past a spent aggregate degrades, and a readable meta keeps a checkpoint");
    };
    for (side, carried) in [("producer", producer), ("consumer", consumer)] {
        assert_eq!(
            (carried.rows, carried.cols),
            (1, 1),
            "{side}: the last resort"
        );
        assert!(
            carried.modes.require_shell_integration_nonce,
            "{side}: never cleared"
        );
        assert_eq!(
            carried.shell_integration_nonce, source.shell_integration_nonce,
            "{side}: the shell's own nonce crosses"
        );
        let mut adopted = Terminal::new(24, 80);
        adopted.restore_checkpoint(&carried);
        crate::spawn::authorize_adopted_shell_nonce(&mut adopted, &carried, 1);
        assert_eq!(
            adopted.shell_integration_posture(),
            ShellIntegrationPosture::On,
            "{side}"
        );
    }

    // No readable meta: the adopted engine holds the requirement itself.
    let mut blind = Terminal::new(24, 80);
    assert_eq!(
        blind.shell_integration_posture(),
        ShellIntegrationPosture::Off
    );
    crate::spawn::hold_adopted_shell_nonce_requirement(&mut blind, 2);
    assert_eq!(
        blind.shell_integration_posture(),
        ShellIntegrationPosture::Degraded,
        "fail-closed, and `status` says the loss"
    );
}
