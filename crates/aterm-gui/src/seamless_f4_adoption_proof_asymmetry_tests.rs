// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::restore::{
    RestoreManifest, RestoredSplitTree, RestoredTab, RestoredView, TerminalLeafRestore,
    WindowLayout,
};

/// `layout_digest` keyed on the wire rather than a struct (`seamless::layout_wire_digest`).
fn wire_digest(wire: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(LAYOUT_PROOF_DOMAIN);
    hasher.update(u64::try_from(wire.len()).unwrap().to_be_bytes());
    hasher.update(wire.as_bytes());
    hasher.finalize()
}

/// The digest of a re-serialization of the parsed layout.
fn child_digest_of_wire(wire: &str) -> Option<[u8; 32]> {
    layout_digest(&RestoreManifest::from_toml(wire)?)
}

fn realistic_layout() -> RestoreManifest {
    RestoreManifest::new(vec![WindowLayout {
        rows: 46,
        cols: 168,
        active_tab: 0,
        outer_x: Some(120),
        outer_y: Some(64),
        maximized: None,
        inner_px: None,
        presence_row: false,
        show: crate::restore::WindowShow::UNKNOWN,
        tabs: Vec::new(),
        native_tabs: Vec::new(),
        tab_order: Vec::new(),
        active_item: Some(0),
        restored_tabs: vec![RestoredTab {
            root: RestoredSplitTree::Split {
                axis: crate::restore::SplitKind::Vertical,
                ratio: 0.618_034,
                first: Box::new(RestoredSplitTree::leaf(RestoredView::Terminal(
                    TerminalLeafRestore {
                        sid: None,
                        lineage: None,
                        log_ep: None,
                        cwd: Some("/Users//example/aterm".to_string()),
                        title: "zsh".to_string(),
                        profile: None,
                        local_id: Some(0),
                        user_title: None,
                        description: None,
                        icon: None,
                        role: None,
                        attention: None,
                        questions: None,
                        autopilot: None,
                        land_at: None,
                        autopilot_by: None,
                        theme: None,
                        agent: None,
                        supervised: None,
                        lost: None,
                        held: false,
                        holds: Vec::new(),
                    },
                ))),
                second: Box::new(RestoredSplitTree::leaf(RestoredView::Terminal(
                    TerminalLeafRestore {
                        sid: None,
                        lineage: None,
                        log_ep: None,
                        cwd: Some("/Users//example".to_string()),
                        title: "claude".to_string(),
                        profile: None,
                        local_id: Some(1),
                        user_title: None,
                        description: None,
                        icon: None,
                        role: None,
                        attention: None,
                        questions: None,
                        autopilot: None,
                        land_at: None,
                        autopilot_by: None,
                        theme: None,
                        agent: None,
                        supervised: None,
                        lost: None,
                        held: false,
                        holds: Vec::new(),
                    },
                ))),
            },
            focused_path: vec![crate::restore::RestoreBranch::Second],
            zoomed: false,
        }],
    }])
}

/// Control: with the same build on both ends the parent's digest and the reparsed one agree.
#[test]
fn same_build_parent_and_child_layout_digests_agree() {
    let layout = realistic_layout();
    let wire = layout.to_toml().expect("parent serializes its live layout");
    let parent = layout_digest(&layout).expect("parent digest");
    assert_eq!(
        parent,
        wire_digest(&wire),
        "parent digest == digest of wire"
    );
    let child = child_digest_of_wire(&wire).expect("child parses the parent wire");
    assert_eq!(
        parent, child,
        "same-build round trip is a fixed point:\n{wire}"
    );
}

/// The lemma both breaks are instances of: a digest over the child's re-serialization equals the
/// parent's iff the child's parse and re-serialize is a byte fixed point on the parent's wire,
/// which nothing establishes across a version boundary.
#[test]
fn layout_digest_equality_is_exactly_wire_fixed_pointness() {
    let modern = realistic_layout().to_toml().expect("serialize");
    for (label, wire) in [
        ("identical build", modern.clone()),
        (
            "schema-1 parent",
            modern.replacen("schema = 2", "schema = 1", 1),
        ),
        (
            "pre-zoomed parent",
            modern.replacen("zoomed = false\n", "", 1),
        ),
    ] {
        let parsed = RestoreManifest::from_toml(&wire).expect("parses");
        let reserialized = parsed.to_toml().expect("reserialize");
        let fixed_point = reserialized == wire;
        let digests_match = child_digest_of_wire(&wire) == Some(wire_digest(&wire));
        assert_eq!(
            fixed_point, digests_match,
            "{label}: digest equality IS wire fixed-pointness"
        );
        crate::logging::stderr_line!(
            "F4/lemma: {label}: fixed_point={fixed_point} digests_match={digests_match}"
        );
    }
}

fn live_checkpoint() -> aterm_core::terminal::TerminalCheckpoint {
    let mut t = aterm_core::terminal::Terminal::new(24, 80);
    t.process(b"\x1b[1;38;5;202muser@mac\x1b[0m ~/aterm % cargo test");
    for i in 0..8 {
        t.process(format!("\r\ntest line {i}").as_bytes());
    }
    t.checkpoint_visible().expect("parser is Ground")
}

/// The degrade arm's premise: re-carrying at 0 cures a shape refusal, because a visible-only carry
/// is canonical by construction even when the inactive grid holds a scrollback-bearing saved
/// primary. Both halves: the carried capture is refused, the visible-only capture of the same
/// terminal is not.
#[test]
fn lowering_the_carry_to_zero_cures_a_shape_refusal() {
    let mut t = aterm_core::terminal::Terminal::new(49, 131);
    for i in 0..400 {
        t.process(format!("primary line {i}\r\n").as_bytes());
    }
    let parked_primary = t
        .checkpoint_carry(256)
        .expect("parser is Ground")
        .grid
        .clone();

    t.process(b"\x1b[?1049h");
    t.process(b"a full-screen app draws here");

    let mut carried = t.checkpoint_carry(256).expect("parser is Ground");
    carried.alt_grid = Some(parked_primary);

    assert!(
        matches!(
            checkpoint_shape_refusal(0, &carried),
            Some(ScreenDigestRefusal::GridNotCanonical {
                which: GridSlot::Inactive,
                ..
            })
        ),
        "the premise: a scrollback-bearing inactive blob is refused"
    );

    let visible_only = t.checkpoint_carry(0).expect("parser is Ground");
    assert!(
        checkpoint_shape_refusal(0, &visible_only).is_none(),
        "the cure: at carry 0 the inactive blob is exactly `rows` records"
    );
    screen_digest(&[(0, visible_only)]).expect("and the digest commits it");
}

/// The refusal names which check refused, which session, and which of its two grids. The blob
/// here has more records than the wire allows the inactive grid, whose history is unrepresentable
/// because the single `history_lines` beside it describes the main blob.
#[test]
fn an_over_long_inactive_grid_blob_names_itself() {
    let mut cp = live_checkpoint();
    let over_long = vec![aterm_core::scrollback::Line::new(); usize::from(cp.rows) + 1];
    cp.alt_grid = Some(aterm_core::scrollback::serialize_lines(&over_long));
    cp.alt_cursor = Some(cp.cursor.clone());

    let refusal =
        screen_digest(&[(0, cp)]).expect_err("an over-long inactive blob must be refused");

    assert!(
        matches!(
            refusal,
            ScreenDigestRefusal::GridNotCanonical {
                local_id: 0,
                which: GridSlot::Inactive,
                ..
            }
        ),
        "expected an inactive-grid canonicality refusal, got {refusal:?}"
    );
    let rendered = refusal.to_string();
    assert!(
        rendered.contains("inactive grid") && rendered.contains("session 0"),
        "the message must name the grid and the session: {rendered}"
    );
}

/// Control for the screen side: same build, so the parent's digest and one over the child's
/// reconstructed checkpoint agree.
#[test]
fn same_build_parent_and_child_screen_digests_agree() {
    let cp = live_checkpoint();
    let parent = screen_digest(&[(0, cp.clone())]).expect("parent screen digest");

    let carry = ScreenCarry {
        schema: ScreenCarry::SCHEMA,
        meta: aterm_json::to_string(&CheckpointMeta::from_checkpoint(&cp)).expect("meta"),
        grid_file: "unused".to_string(),
        alt_grid_file: None,
        repaint: false,
    };
    let meta = parse_checkpoint_meta(&carry).expect("child parses meta");
    let rebuilt = meta.into_checkpoint(cp.grid.clone(), None);
    let child = screen_digest_refs(vec![(0, &rebuilt)]).expect("child screen digest");
    assert_eq!(
        parent, child,
        "same-build screen round trip is a fixed point"
    );
}

/// The colour and shell-integration carry crosses the real wire: parsed by the strict child
/// parser, reassembled and adopted intact, with the screen digest a fixed point. An untouched
/// session's key set is pinned exactly: no `color` or `shell` key, `absolute_row_counter` always
/// (a live grid's counter is at least its rows), `output_state` once it has printed.
#[test]
fn colour_and_shell_state_cross_the_meta_wire() {
    let plain = live_checkpoint();
    let plain_json = aterm_json::to_string(&CheckpointMeta::from_checkpoint(&plain)).expect("meta");
    let plain_value: aterm_json::Value = aterm_json::from_str(&plain_json).expect("json");
    let mut keys: Vec<&str> = plain_value
        .as_object()
        .expect("an object")
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    let mut expected: Vec<&str> = CHECKPOINT_META_REQUIRED_KEYS.to_vec();
    expected.extend([
        "history_lines",
        "absolute_row_counter",
        "title",
        "output_state",
    ]);
    expected.sort_unstable();
    assert_eq!(
        keys, expected,
        "an untouched session writes the frozen keys, its history depth, its \
         row counter, its (empty) title — the presence that says titles were \
         carried — and its REP character, and no colour, shell, nonce or \
         alt-counter key: {plain_json}"
    );
    assert!(
        plain_json.contains("\"output_state\":{\"rep\":\"7\"}"),
        "the REP character is the last one printed, and nothing else is in \
         flight: {plain_json}"
    );
    assert!(
        plain_json.contains("\"title\":{}"),
        "an untitled engine's title is `{{}}`: {plain_json}"
    );

    let mut t = aterm_core::terminal::Terminal::new(24, 80);
    t.modes_mut().allow_palette_reconfigure = true;
    t.process(b"\x1b]4;3;rgb:11/22/33\x07\x1b]11;rgb:05/06/07\x07");
    t.process(b"\x1b]133;A\x07$ \x1b]133;B\x07make\r\n\x1b]133;C\x07");
    let cp = t.checkpoint_carry(64).expect("parser is Ground");
    let carry = ScreenCarry {
        schema: ScreenCarry::SCHEMA,
        meta: aterm_json::to_string(&CheckpointMeta::from_checkpoint(&cp)).expect("meta"),
        grid_file: "unused".to_string(),
        alt_grid_file: None,
        repaint: false,
    };
    let meta = parse_checkpoint_meta(&carry).expect("child parses meta");
    let rebuilt = meta.into_checkpoint(cp.grid.clone(), None);
    assert_eq!(rebuilt.color, cp.color, "the colour diff crosses intact");
    assert_eq!(rebuilt.shell, cp.shell, "the shell state crosses intact");
    assert_eq!(
        screen_digest(&[(0, cp.clone())]).expect("parent digest"),
        screen_digest_refs(vec![(0, &rebuilt)]).expect("child digest"),
        "the screen digest is still a fixed point"
    );

    let mut adopted = aterm_core::terminal::Terminal::new(24, 80);
    adopted.restore_checkpoint(&rebuilt);
    assert_eq!(
        adopted.palette_color(3),
        aterm_core::terminal::Rgb {
            r: 0x11,
            g: 0x22,
            b: 0x33
        }
    );
    adopted.process(b"\x1b]133;D;0\x07");
    assert_eq!(
        adopted.completed_command_seq(),
        1,
        "the running command completes"
    );
}

/// What a shell's prompt left pending about its cwd crosses the REAL wire
/// (docs/DESIGN-cwd-reports-2026-10-19.md): that the shell signs its own report, the cwd a
/// key-bound program's report replaced (restored once the successor's host
/// says the program left the foreground), and a report held at the prompt of
/// a shell that does not sign (applied at the successor's next `A`). Each is
/// in the meta JSON, the strict child parser reads it back, and the adopted
/// engine acts on it.
#[test]
fn the_prompts_pending_cwd_crosses_the_meta_wire() {
    use aterm_core::terminal::{ShellIntegrationNonce, Terminal};
    const KEY: [u8; 32] = [0x5D; 32];
    let hex = ShellIntegrationNonce(KEY).to_hex();
    let mark = |m: &str| format!("\x1b]133;{m};id={hex}\x07").into_bytes();
    let wire = |t: &Terminal| {
        let cp = t.checkpoint_carry(64).expect("parser is Ground");
        let json = aterm_json::to_string(&CheckpointMeta::from_checkpoint(&cp)).expect("meta");
        let carry = ScreenCarry {
            schema: ScreenCarry::SCHEMA,
            meta: json.clone(),
            grid_file: "unused".to_string(),
            alt_grid_file: None,
            repaint: false,
        };
        let meta = parse_checkpoint_meta(&carry).expect("child parses meta");
        let rebuilt = meta.into_checkpoint(cp.grid.clone(), None);
        assert_eq!(rebuilt.shell, cp.shell, "the shell state crosses intact");
        let mut adopted = Terminal::new(24, 80);
        adopted.restore_checkpoint(&rebuilt);
        adopted.authorize_shell_integration(KEY);
        (json, adopted)
    };
    let prompt = |t: &mut Terminal, report: &[u8]| {
        t.process(report);
        t.process(&mark("A"));
        t.process(b"$ ");
        t.process(&mark("B"));
    };
    let gated = || {
        let mut t = Terminal::new(24, 80);
        t.authorize_shell_integration(KEY);
        t.set_require_shell_integration_nonce(true);
        t
    };

    let signed_report = format!(
        "\x1b]633;P;AtermCwd=file://host/home/me;id={hex}\x07\x1b]7;file://host/home/me\x07"
    )
    .into_bytes();
    let mut t = gated();
    prompt(&mut t, &signed_report);
    t.process(b"true\r\n");
    t.process(&mark("C"));
    t.process(&mark("D;0"));
    prompt(&mut t, &signed_report);
    t.set_foreground_job(true);
    t.process(b"\x1b]7;file://remote/srv/app\x07");
    assert_eq!(t.current_working_directory(), Some("/srv/app"));
    let (json, mut adopted) = wire(&t);
    assert!(json.contains("\"signs_cwd\":true"), "{json}");
    assert!(
        json.contains("\"cwd_before_job\":{\"path\":\"/home/me\",\"host\":\"host\"}"),
        "{json}"
    );
    assert_eq!(adopted.current_working_directory(), Some("/srv/app"));
    adopted.set_foreground_job(false);
    assert_eq!(adopted.current_working_directory(), Some("/home/me"));
    adopted.process(b"\x1b]7;file://host/elsewhere\x07");
    assert_eq!(
        adopted.current_working_directory(),
        Some("/home/me"),
        "a stray at the adopted prompt of a signing shell is dropped"
    );

    let unsigned_report = b"\x1b]7;file://host/home/me\x07";
    let mut t = gated();
    prompt(&mut t, unsigned_report);
    t.process(b"true\r\n");
    t.process(&mark("C"));
    t.process(&mark("D;0"));
    prompt(&mut t, unsigned_report);
    t.process(b"\x1b]7;file://host/home/me/src\x07");
    assert_eq!(t.current_working_directory(), Some("/home/me"), "held");
    let (json, mut adopted) = wire(&t);
    assert!(
        json.contains("\"held_cwd\":{\"path\":\"/home/me/src\",\"host\":\"host\"}"),
        "{json}"
    );
    adopted.process(&mark("A"));
    assert_eq!(adopted.current_working_directory(), Some("/home/me/src"));
}

/// The parser carry crosses the real wire: a session parked inside `ESC [ 38;5;19` is carried at
/// the Full rung with its partial CSI in the meta's additive `parser` key; the strict child parser
/// reads it, the screen digest is a fixed point over it and covers it, and the adopted engine
/// finishes the sequence with the queued `6m`. A meta without the key still parses and adopts at
/// Ground.
#[test]
fn a_partial_parser_crosses_the_meta_wire_inside_the_digest() {
    let mut t = aterm_core::terminal::Terminal::new(24, 80);
    t.process(b"$ make\r\n\x1b[38;5;19");
    let (cp, rung, cause) = carry_for_wire(&t, 0, 0, &mut 0, WireCaps::current());
    assert_eq!(rung, CarryRung::Full, "{cause:?}");
    assert!(cp.parser.is_some() && !cp.parser_ground);
    let meta = aterm_json::to_string(&CheckpointMeta::from_checkpoint(&cp)).expect("meta");
    assert!(
        meta.contains("\"parser\":{\"state\":\"CsiParam\""),
        "{meta}"
    );
    let carry = |meta: String| ScreenCarry {
        schema: ScreenCarry::SCHEMA,
        meta,
        grid_file: "unused".to_string(),
        alt_grid_file: None,
        repaint: false,
    };
    let rebuilt = parse_checkpoint_meta(&carry(meta.clone()))
        .expect("child parses meta")
        .into_checkpoint(cp.grid.clone(), None);
    assert_eq!(
        rebuilt.parser, cp.parser,
        "the partial state crosses intact"
    );
    let parent = screen_digest(&[(0, cp.clone())]).expect("parent digest");
    assert_eq!(
        parent,
        screen_digest_refs(vec![(0, &rebuilt)]).expect("child digest"),
        "the screen digest is a fixed point over the carry"
    );
    let mut other = cp.clone();
    if let Some(parser) = other.parser.as_mut() {
        parser.current_param = 18;
    }
    assert_ne!(
        screen_digest(&[(0, other)]).expect("digest"),
        parent,
        "the parser carry is inside the screen digest"
    );

    let mut adopted = aterm_core::terminal::Terminal::new(24, 80);
    adopted.restore_checkpoint(&rebuilt);
    adopted.process(b"6mX");
    assert_eq!(
        adopted.row_text(1).as_deref().map(str::trim_end),
        Some("X"),
        "the adopted engine finishes the queued SGR instead of printing it"
    );

    let before_the_key = meta.replace(
        &meta[meta.find(",\"parser\":").expect("the key")..meta.len() - 1],
        "",
    );
    assert!(!before_the_key.contains("parser"), "{before_the_key}");
    let old = parse_checkpoint_meta(&carry(before_the_key))
        .expect("an older producer's meta parses")
        .into_checkpoint(cp.grid.clone(), None);
    assert!(old.parser_ground && old.parser.is_none());
    assert!(screen_digest_refs(vec![(0, &old)]).is_ok());
}

/// A DCS header crosses the meta wire both ways. A session parked inside `ESC P $` (a DECRQSS
/// whose `qm ESC \` is still queued) carries the header under the parser key's `exact`, beside
/// the `DcsIgnore` an older build writes for every header: this build reads `exact`, the screen
/// digest covers it, and the adopted engine answers the query. An older build drops `exact` and
/// restores its own `DcsIgnore`, which swallows the string so the payload never prints; an older
/// producer's meta adopts here the same way.
#[test]
fn a_dcs_header_crosses_the_meta_wire_both_ways() {
    let mut t = aterm_core::terminal::Terminal::new(24, 80);
    t.process(b"$ query\r\n\x1bP$");
    let (cp, rung, cause) = carry_for_wire(&t, 0, 0, &mut 0, WireCaps::current());
    assert_eq!(rung, CarryRung::Full, "{cause:?}");
    let meta = aterm_json::to_string(&CheckpointMeta::from_checkpoint(&cp)).expect("meta");
    let exact = r#","exact":{"state":"DcsIntermediate","intermediates":"24"}"#;
    assert!(
        meta.contains(&format!(r#""parser":{{"state":"DcsIgnore"{exact}}}"#)),
        "{meta}"
    );
    let carry = |meta: String| ScreenCarry {
        schema: ScreenCarry::SCHEMA,
        meta,
        grid_file: "unused".to_string(),
        alt_grid_file: None,
        repaint: false,
    };
    let adopt = |meta: &str| {
        let rebuilt = parse_checkpoint_meta(&carry(meta.to_string()))
            .expect("the child parses the meta")
            .into_checkpoint(cp.grid.clone(), None);
        let mut adopted = aterm_core::terminal::Terminal::new(24, 80);
        adopted.restore_checkpoint(&rebuilt);
        adopted.process(b"qm\x1b\\after");
        (rebuilt, adopted)
    };

    let (rebuilt, mut adopted) = adopt(&meta);
    assert_eq!(rebuilt.parser, cp.parser, "the header crosses intact");
    let parent = screen_digest(&[(0, cp.clone())]).expect("parent digest");
    assert_eq!(
        parent,
        screen_digest_refs(vec![(0, &rebuilt)]).expect("child digest"),
        "the screen digest is a fixed point over the carry"
    );
    let mut other = cp.clone();
    if let Some(parser) = other.parser.as_mut() {
        parser.intermediates = vec![b'+'];
    }
    assert_ne!(
        screen_digest(&[(0, other)]).expect("digest"),
        parent,
        "the header is inside the screen digest"
    );
    assert!(
        adopted.take_response().is_some(),
        "the adopted engine answers the query"
    );
    assert_eq!(
        adopted.row_text(1).as_deref().map(str::trim_end),
        Some("after")
    );

    let older = meta.replace(exact, "");
    assert!(
        older.contains(r#""parser":{"state":"DcsIgnore"}"#),
        "{older}"
    );
    let (rebuilt, mut adopted) = adopt(&older);
    assert_eq!(
        rebuilt.parser.as_ref().map(|parser| parser.state.name()),
        Some("DcsIgnore")
    );
    assert_eq!(adopted.take_response(), None, "the string is swallowed");
    assert_eq!(
        adopted.row_text(1).as_deref().map(str::trim_end),
        Some("after"),
        "and its payload never prints"
    );
}

/// The engine's in-flight output state crosses the real wire: the REP character, an open OSC 8
/// link, an SGR 58 underline colour, a VT52 address in progress, a ZWJ waiting for its emoji, the
/// XTPUSHSGR stack (whole and selective) and an XTSAVE'd mode, carried at the Full rung in the
/// meta's additive `output_state` key, each finish the queued tail as one engine fed the whole
/// stream does. The screen digest is a fixed point over it and covers it. A meta without the key
/// still parses and adopts with none of it.
#[test]
fn the_in_flight_output_state_crosses_the_meta_wire_inside_the_digest() {
    use aterm_core::terminal::Terminal;
    let carry = |meta: String| ScreenCarry {
        schema: ScreenCarry::SCHEMA,
        meta,
        grid_file: "unused".to_string(),
        alt_grid_file: None,
        repaint: false,
    };
    let screen = |t: &mut Terminal| {
        let links: Vec<_> = (0..6)
            .flat_map(|r| (0..20).map(move |c| (r, c)))
            .map(|(r, c)| {
                (
                    t.hyperlink_at(r, c).map(str::to_owned),
                    t.hyperlink_id_at(r, c).map(str::to_owned),
                )
            })
            .collect();
        (t.cell_frame(6, 20).cells, links, t.cursor())
    };
    for (state, head, tail) in [
        ("the REP character", &b"X"[..], &b"\x1b[5b"[..]),
        (
            "an open OSC 8 link",
            b"\x1b]8;id=p;http://a\x1b\\ab",
            b"cd\x1b]8;;\x1b\\",
        ),
        (
            "an SGR 58 underline colour",
            b"\x1b[58:2::255:0:0m\x1b[4m",
            b"err",
        ),
        ("a VT52 address", b"\x1b[?2l\x1bY\x22", b"%"),
        (
            "a ZWJ",
            "\u{1F468}\u{200D}".as_bytes(),
            "\u{1F4BB}x".as_bytes(),
        ),
        (
            "the XTPUSHSGR stack",
            b"\x1b[31m\x1b[#{\x1b[32mA",
            b"\x1b[#}B",
        ),
        (
            "a selective XTPUSHSGR of the underline",
            b"\x1b[58:2::0:0:255m\x1b[4m\x1b[4#{\x1b[58:2::255:0:0mA",
            b"\x1b[#}B",
        ),
        (
            "an XTSAVE'd mode",
            b"\x1b[?7s\x1b[?7l",
            b"\x1b[?7rABCDEFGHIJKLMNOPQRSTUVWXY",
        ),
    ] {
        let mut outgoing = Terminal::new(6, 20);
        outgoing.process(head);
        let (cp, rung, cause) = carry_for_wire(&outgoing, 0, 0, &mut 0, WireCaps::current());
        assert_eq!(rung, CarryRung::Full, "{state}: {cause:?}");
        let meta = aterm_json::to_string(&CheckpointMeta::from_checkpoint(&cp)).expect("meta");
        assert!(meta.contains("\"output_state\":{"), "{state}: {meta}");
        let rebuilt = parse_checkpoint_meta(&carry(meta.clone()))
            .expect("child parses meta")
            .into_checkpoint(cp.grid.clone(), cp.alt_grid.clone());
        assert_eq!(
            rebuilt.output_state, cp.output_state,
            "{state}: crosses intact"
        );
        let parent = screen_digest(&[(0, cp.clone())]).expect("parent digest");
        assert_eq!(
            parent,
            screen_digest_refs(vec![(0, &rebuilt)]).expect("child digest"),
            "{state}: the screen digest is a fixed point over it"
        );
        let mut other = cp.clone();
        other.output_state = aterm_core::terminal::OutputStateRepr::default();
        assert_ne!(
            screen_digest(&[(0, other)]).expect("digest"),
            parent,
            "{state}: the output state is inside the screen digest"
        );

        let mut adopted = Terminal::new(6, 20);
        adopted.restore_checkpoint(&rebuilt);
        adopted.process(tail);
        let mut reference = Terminal::new(6, 20);
        reference.process(head);
        reference.process(tail);
        assert_eq!(
            screen(&mut adopted),
            screen(&mut reference),
            "{state}: the adopted engine finishes the queued tail as one engine does"
        );

        let mut value: aterm_json::Value = aterm_json::from_str(&meta).expect("json");
        value
            .as_object_mut()
            .expect("an object")
            .remove("output_state")
            .expect("the key");
        let old = parse_checkpoint_meta(&carry(aterm_json::to_string(&value).expect("json")))
            .expect("an older producer's meta parses")
            .into_checkpoint(cp.grid.clone(), cp.alt_grid.clone());
        assert!(old.output_state.is_default(), "{state}");
        assert!(screen_digest_refs(vec![(0, &old)]).is_ok(), "{state}");
        let mut before = Terminal::new(6, 20);
        before.restore_checkpoint(&old);
        before.process(tail);
        assert_ne!(
            screen(&mut before),
            screen(&mut reference),
            "{state}: NEGATIVE CONTROL: without the key the tail lands as it did before"
        );
    }
}

/// An `output_state` this build cannot read (a wrong type, a REP character
/// that is not one character) costs that state and nothing else: the meta
/// still parses, the session is adopted with its screen, and it resumes with
/// none of the in-flight state, as from a producer that wrote no key.
#[test]
fn an_unreadable_output_state_costs_only_itself() {
    let cp = live_checkpoint();
    assert!(
        !cp.output_state.is_default(),
        "PRECONDITION: a REP character"
    );
    let meta = aterm_json::to_string(&CheckpointMeta::from_checkpoint(&cp)).expect("meta");
    for unreadable in ["7", "{\"rep\":\"XY\"}", "{\"hyperlink\":{\"id\":\"p\"}}"] {
        let mut value: aterm_json::Value = aterm_json::from_str(&meta).expect("json");
        value.as_object_mut().expect("an object").insert(
            "output_state".to_string(),
            aterm_json::from_str(unreadable).expect("json"),
        );
        let carry = ScreenCarry {
            schema: ScreenCarry::SCHEMA,
            meta: aterm_json::to_string(&value).expect("json"),
            grid_file: "unused".to_string(),
            alt_grid_file: None,
            repaint: false,
        };
        let parsed = parse_checkpoint_meta(&carry).expect("the meta still parses");
        assert!(parsed.output_state.is_default(), "{unreadable}");
        let rebuilt = parsed.into_checkpoint(cp.grid.clone(), None);
        assert_eq!(rebuilt.grid, cp.grid, "{unreadable}: the screen is kept");
        assert_eq!(rebuilt.cursor, cp.cursor, "{unreadable}");
    }
}

/// Cross-version break 4: `parse_checkpoint_meta` demands every key in
/// [`CHECKPOINT_META_REQUIRED_KEYS`], so a wire missing one does not parse. That costs the
/// session's screen and nothing else: it adopts onto a blank engine at its pane's geometry and
/// repaints. Still a break (every tab repainting on every hop is not seamless), which is why the
/// list is frozen (next test).
#[test]
fn cross_version_missing_required_meta_key_costs_the_screen_not_the_adoption() {
    let cp = live_checkpoint();
    let mut value: aterm_json::Value =
        aterm_json::to_value(CheckpointMeta::from_checkpoint(&cp)).expect("meta value");
    value
        .as_object_mut()
        .expect("object")
        .remove("secure_keyboard_entry")
        .expect("key was present");
    let carry = ScreenCarry {
        schema: ScreenCarry::SCHEMA,
        meta: aterm_json::to_string(&value).expect("json"),
        grid_file: "unused".to_string(),
        alt_grid_file: None,
        repaint: false,
    };
    let meta = parse_checkpoint_meta(&carry);
    assert!(
        meta.is_none(),
        "a single missing REQUIRED key fails the parse"
    );
    let mut used_cells = 0;
    let screen = admit_incoming_screen(
        meta,
        ScreenWireBytes {
            grid: cp.grid.clone(),
            alt_grid: None,
        },
        &mut used_cells,
    );
    let IncomingScreen::Degraded {
        checkpoint, wire, ..
    } = screen
    else {
        panic!("an unparseable meta degrades the session instead of adopting it exactly");
    };
    assert!(
        checkpoint.is_none(),
        "with no meta to sanitize, the session adopts onto a blank engine at its pane"
    );
    assert_eq!(
        wire.grid, cp.grid,
        "and the bytes read are kept for the proof"
    );
    assert_eq!(used_cells, 0, "a screen that is not decoded is not charged");
}

/// The required key list is frozen, pinned verbatim: it may only shrink (delete a line here and in
/// [`CHECKPOINT_META_REQUIRED_KEYS`] together). The producer of every handoff is the older build,
/// so a key added to the list is one no producer in the field sends, and every tab would arrive
/// without a parseable meta and repaint on every hop. A new meta field is additive
/// (`#[serde(default)]` on `CheckpointMeta`), never required.
#[test]
fn the_required_meta_keys_are_frozen() {
    assert_eq!(
        CHECKPOINT_META_REQUIRED_KEYS,
        &[
            "rows",
            "cols",
            "cursor",
            "alt_cursor",
            "saved_cursor_main",
            "saved_cursor_alt",
            "modes",
            "style_fg_bits",
            "style_bg_bits",
            "style_flag_bits",
            "style_protected",
            "charset",
            "kitty_keyboard",
            "xterm_keyboard",
            "taskbar_progress",
            "secure_keyboard_entry",
            "current_working_directory",
        ],
        "the REQUIRED meta keys may only shrink — read this test's doc before editing either list"
    );
}

/// The child proves with its own `build_info::build_number()` and `build_info::git_commit()`; the
/// parent expects the apply ticket's `target_build`/`target_commit`. Any disagreement breaks the
/// proof.
#[test]
fn child_build_and_commit_must_equal_the_parents_expectation_exactly() {
    let ids = [(0u64, 7i32, 4242i32)];
    let nonce = "0123456789abcdef0123456789abcdef";
    let (ld, sd) = ([1u8; 32], [2u8; 32]);
    let base = adoption_proof(nonce, 1_784_869_524, "7bf23bbe1234", &ld, &sd, &ids).unwrap();

    let stale_build = adoption_proof(nonce, 1_784_825_157, "7bf23bbe1234", &ld, &sd, &ids).unwrap();
    assert_ne!(base, stale_build, "build number mismatch breaks the proof");

    let dirty = adoption_proof(nonce, 1_784_869_524, "7bf23bbe1234-dirty", &ld, &sd, &ids).unwrap();
    assert_ne!(base, dirty, "`-dirty` breaks the proof");

    let long = adoption_proof(
        nonce,
        1_784_869_524,
        "7BF23BBE1234567890ABCDEF1234567890ABCDEF",
        &ld,
        &sd,
        &ids,
    )
    .unwrap();
    assert_eq!(base, long, "commit length/case normalization is symmetric");

    let unknown = adoption_proof(nonce, 1_784_869_524, "unknown", &ld, &sd, &ids);
    assert_eq!(
        unknown.is_ok(),
        cfg!(debug_assertions),
        "`unknown` commit is release-fatal (no proof is ever emitted)"
    );
    let unstamped = adoption_proof(nonce, 1_784_869_524, "unknown-dirty", &ld, &sd, &ids);
    assert_eq!(
        unstamped,
        Err(ProofRefusal::UnstampedCommit("unknown-dirty".to_string())),
        "the refusal is the commit's, never the session set's"
    );
    assert_eq!(
        handoff_commit_stamp("unknown-dirty").err(),
        unstamped.err(),
        "the pre-launch gate refuses what the proof refuses"
    );
}

/// The tripwire for the frozen cross-version wire, and the one test here that does not compare
/// [`adoption_proof`] against itself: a changed domain string, a reordered field, a dropped length
/// prefix or a different [`normalize_commit`] keeps every self-consistent test green while the two
/// builds compute different digests (an `AdoptionMismatch`; see [`adoption_proof`]).
#[test]
fn the_v1_adoption_digest_is_frozen_to_this_exact_vector() {
    const FROZEN: &str = "9f32e4a273bbb14fb95b80b90788b41fce5cbfbc9fff94051934eb9b7d78ccbd";

    let ids = [(2u64, 9i32, 4242i32), (0, 7, 101), (1, 8, 777)];
    let proof = adoption_proof(
        "0123456789abcdef0123456789abcdef",
        1_784_869_524,
        "7BF23BBE1234567890ABCDEF1234567890ABCDEF",
        &[1u8; 32],
        &[2u8; 32],
        &ids,
    )
    .expect("a well-formed identity set proves");
    assert_eq!(proof.count, 3, "the count rides beside the digest");
    let digest: String = proof.digest.iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(
        digest, FROZEN,
        "the LIVE adoption wire changed — read this test's doc before touching the constant"
    );
}
