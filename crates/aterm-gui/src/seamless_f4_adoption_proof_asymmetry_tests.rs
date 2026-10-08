// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::restore::{
    RestoreManifest, RestoredSplitTree, RestoredTab, RestoredView, TerminalLeafRestore,
    WindowLayout,
};

/// EXACTLY what `layout_digest` does, but keyed on the WIRE the parent
/// wrote rather than on a struct. `layout_digest(m) == wire_digest(m.to_toml())`
/// by construction (see `seamless::layout_digest`), so this lets a test stand in
/// for "build A's parent-side digest" using only build A's bytes.
fn wire_digest(wire: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(LAYOUT_PROOF_DOMAIN);
    hasher.update(u64::try_from(wire.len()).unwrap().to_be_bytes());
    hasher.update(wire.as_bytes());
    hasher.finalize()
}

/// Exactly what the child USED TO do: re-derive the digest from the PARSED
/// layout. Retained here as the counterfactual the fence measures against.
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
                        identity: None,
                        agent: None,
                        held: false,
                    },
                ))),
                second: Box::new(RestoredSplitTree::leaf(RestoredView::Terminal(
                    TerminalLeafRestore {
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
                        identity: None,
                        agent: None,
                        held: false,
                    },
                ))),
            },
            focused_path: vec![crate::restore::RestoreBranch::Second],
            zoomed: false,
        }],
    }])
}

/// CONTROL. Same binary on both ends: the parent's live digest and the
/// child's reparsed digest agree. So F4 is NOT an unconditional break.
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

/// CROSS-VERSION BREAK #1 — the manifest SCHEMA constant.
/// `RestoreManifest::from_toml` ACCEPTS `LEGACY_SCHEMA` and then
/// unconditionally REWRITES `manifest.schema = SCHEMA`. A parent built when
/// `SCHEMA == 1` writes `schema = 1`; the child re-emits `schema = 2`. The
/// adoption proof therefore CANNOT match, and nothing in the protocol
/// notices — the parent reports `AdoptionMismatch`.
#[test]
fn cross_version_schema_bump_breaks_the_layout_digest() {
    let layout = realistic_layout();
    let modern = layout.to_toml().expect("serialize");
    assert!(modern.contains("schema = 2"), "today's wire: {modern}");
    // The bytes an older parent (SCHEMA == 1) would have written+hashed.
    let old_parent_wire = modern.replacen("schema = 2", "schema = 1", 1);
    let old_parent_digest = wire_digest(&old_parent_wire);
    let child_digest =
        child_digest_of_wire(&old_parent_wire).expect("child still ACCEPTS the legacy schema");
    assert_ne!(
        old_parent_digest, child_digest,
        "a schema bump must break the proof"
    );
    crate::logging::stderr_line!(
        "F4/schema: parent={} child={} (child re-emitted `schema = 2`)",
        hex12(&old_parent_digest),
        hex12(&child_digest)
    );
}

/// CROSS-VERSION BREAK #2 — an ADDED serde field.
/// `RestoredTab::zoomed` is `#[serde(default)]` with NO
/// `skip_serializing_if`, so it is ALWAYS emitted. A parent built before
/// `zoomed` existed writes a wire without it; the child defaults it and
/// re-emits `zoomed = false`. Same semantic layout, different digest.
/// EVERY future additive field of this shape re-breaks the handoff.
#[test]
fn cross_version_added_field_breaks_the_layout_digest() {
    let modern = realistic_layout().to_toml().expect("serialize");
    assert!(modern.contains("zoomed = false"), "wire: {modern}");
    let old_parent_wire = modern.replacen("zoomed = false\n", "", 1);
    let old_parent_digest = wire_digest(&old_parent_wire);
    let child_digest = child_digest_of_wire(&old_parent_wire)
        .expect("child parses the older wire (serde default)");
    assert_ne!(
        old_parent_digest, child_digest,
        "an additive always-emitted field must break the proof"
    );
    crate::logging::stderr_line!(
        "F4/field: parent={} child={} (child re-emitted `zoomed = false`)",
        hex12(&old_parent_digest),
        hex12(&child_digest)
    );
}

/// The general lemma both breaks are instances of: the child's digest
/// equals the parent's IFF the child's parse+reserialize is a byte fixed
/// point on the parent's wire. Nothing in the protocol establishes that
/// across a version boundary, and the parent's own pre-flight round-trip
/// check (`app_input.rs` ~579) runs the PARENT's codec against itself.
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

// ---- screen side -------------------------------------------------------

fn live_checkpoint() -> aterm_core::terminal::TerminalCheckpoint {
    let mut t = aterm_core::terminal::Terminal::new(24, 80);
    t.process(b"\x1b[1;38;5;202muser@mac\x1b[0m ~/aterm % cargo test");
    for i in 0..8 {
        t.process(format!("\r\ntest line {i}").as_bytes());
    }
    t.checkpoint_visible().expect("parser is Ground")
}

/// THE DEGRADE ARM'S PREMISE, pinned at the seam the producer uses.
///
/// The producer's rescue is "re-carry at 0 and the shape refusal goes away".
/// That is only true if a visible-only carry is canonical BY CONSTRUCTION for a
/// terminal whose inactive grid holds a scrollback-bearing saved primary —
/// exactly the state that stranded the update. Pin both halves: the carried
/// capture is refused, and the visible-only capture of the SAME terminal is not.
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

    // DECSET 1049 — the primary, scrollback and all, is parked in `alt_grid`,
    // which is the slot the wire allows exactly `rows` records.
    t.process(b"\x1b[?1049h");
    t.process(b"a full-screen app draws here");

    // The blob the OLD producer put in the inactive slot: the SAVED PRIMARY
    // projected at the carry bound, which is `rows + min(scrollback, 256)`
    // records. Taken from the same terminal before it switched, so this is the
    // real shape rather than a synthetic one — and building it here keeps the
    // test meaningful now that the producer no longer emits it.
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

/// The refusal NAMES ITSELF. Three separate multi-day investigations into a
/// stuck update were spent recovering information this assertion now pins:
/// which check refused, which session, and which of its two grids.
///
/// The blob shape here is exactly what a saved primary with scrollback used
/// to produce — more records than the wire allows the INACTIVE grid, whose
/// history is unrepresentable because the single `history_lines` beside it
/// describes the MAIN blob.
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

/// The healthy path still yields a digest — the `Result` rewrite changed no
/// predicate, so a well-formed capture is accepted exactly as before.
#[test]
fn a_well_formed_capture_still_commits() {
    screen_digest(&[(0, live_checkpoint())]).expect("a live checkpoint commits");
}

/// CONTROL for the screen side. The child never passed `screen_digest`
/// through verbatim — it RE-DERIVED it from the reconstructed checkpoints,
/// so the screen half carried the SAME round-trip exposure as the layout
/// half. It now hashes the carried bytes instead. Same build ⇒ equal either
/// way, which is what this control pins.
#[test]
fn same_build_parent_and_child_screen_digests_agree() {
    let cp = live_checkpoint();
    let parent = screen_digest(&[(0, cp.clone())]).expect("parent screen digest");

    // Exactly the child's path: meta JSON off the wire → CheckpointMeta →
    // into_checkpoint → screen_digest_refs.
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

/// The colour and shell-integration carry crosses the REAL wire: the meta
/// JSON the parent writes, parsed by the strict child parser, reassembled,
/// and adopted. A session with neither writes no `color` or `shell` key;
/// it does write `absolute_row_counter` — a live grid's counter is never
/// 0 (it is at least its rows), so every session carries it, and an older
/// child ignores the key (no `deny_unknown_fields`). So is `output_state`
/// written by every session that has printed anything: its REP character
/// (round eight of the update audit, finding 502). The untouched session's
/// key set is pinned exactly below. A session with both carries them
/// intact, and the screen digest stays a fixed point.
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

/// The parser carry (the round-five plan's item 12) crosses the REAL wire:
/// a session parked inside `ESC [ 38;5;19` is carried at the Full rung with
/// its partial CSI in the meta's additive `parser` key; the strict child
/// parser reads it, the screen digest is a fixed point over it AND covers
/// it (a different partial state is a different commitment, so the
/// adoption proof binds the carry like every other meta field); and the
/// adopted engine finishes the sequence with the queued `6m`. A producer
/// from before the key — the same meta without it — still parses and
/// adopts at Ground, which is exactly what it did before.
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

/// The engine's in-flight output state (round eight of the update audit,
/// finding 502) crosses the REAL wire: each of the REP character, an open
/// OSC 8 link, an SGR 58 underline colour, a VT52 address in progress, a
/// ZWJ waiting for its emoji, the XTPUSHSGR stack (whole and selective)
/// and an XTSAVE'd mode, carried at the Full rung in the meta's additive `output_state` key,
/// parsed by the strict child parser and adopted, finishes the queued tail
/// exactly as one engine fed the whole stream does. The screen digest is a
/// fixed point over it and covers it. A producer from before the key — the
/// same meta without it — still parses and adopts with none of it, which is
/// what every adoption did before.
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

        // A producer from before the key: parses, and adopts with none of it.
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

/// CROSS-VERSION BREAK #3 — the screen half, `CheckpointMeta` drift.
/// `CheckpointMeta` has NO `deny_unknown_fields`. A parent built with an
/// 18th meta field writes it; the older child silently DROPS it and hashes
/// 17 keys. The parent hashed 18. `AdoptionMismatch`, with no diagnostic.
#[test]
fn cross_version_extra_meta_field_breaks_the_screen_digest() {
    let cp = live_checkpoint();
    let meta_json =
        aterm_json::to_string(&CheckpointMeta::from_checkpoint(&cp)).expect("meta json");
    // What a NEWER parent's wire looks like: one additive key the running
    // (older) child does not know.
    let newer_parent_meta = meta_json.replacen('{', "{\"scene_overlay\":true,", 1);
    let parent_digest = {
        let mut hasher = Sha256::new();
        hasher.update(newer_parent_meta.as_bytes());
        let d: [u8; 32] = hasher.finalize();
        d
    };
    let carry = ScreenCarry {
        schema: ScreenCarry::SCHEMA,
        meta: newer_parent_meta.clone(),
        grid_file: "unused".to_string(),
        alt_grid_file: None,
        repaint: false,
    };
    let parsed = parse_checkpoint_meta(&carry).expect("child parses (no deny_unknown)");
    let child_meta = aterm_json::to_string(&parsed).expect("child reserializes");
    assert_ne!(
        newer_parent_meta, child_meta,
        "the child dropped the unknown key"
    );
    let child_digest = {
        let mut hasher = Sha256::new();
        hasher.update(child_meta.as_bytes());
        let d: [u8; 32] = hasher.finalize();
        d
    };
    assert_ne!(parent_digest, child_digest);
    crate::logging::stderr_line!(
        "F4/meta: parent meta {} bytes, child meta {} bytes — screen_digest inputs differ",
        newer_parent_meta.len(),
        child_meta.len()
    );
}

/// CROSS-VERSION BREAK #4 — the HARDER screen failure. `parse_checkpoint_meta`
/// demands EVERY key in [`CHECKPOINT_META_REQUIRED_KEYS`]. If a newer child
/// adds an 18th meta field to that list, an older parent's 17-key wire does
/// not parse. That used to refuse the adoption outright — the child adopted
/// NOTHING, never wrote a proof, and the parked parent read EOF
/// (`ChildDied`, the 0.58-era symptom). Since the 2026-09-22/23 audit (plan
/// P0-2) it costs that session's screen and nothing else: the session is
/// adopted onto a blank engine at its pane's geometry and repainted. Still a
/// break worth pinning — every tab repainting on every hop is not seamless —
/// which is why the list is frozen (next test).
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

/// THE REQUIRED KEY LIST IS FROZEN, and this pins it VERBATIM. It may only
/// ever SHRINK — delete a line here and in [`CHECKPOINT_META_REQUIRED_KEYS`]
/// together, never add one. The producer of every handoff is the OLDER
/// build, so a key added to the list is one every producer in the field
/// fails to send, and each of its sessions would arrive without a parseable
/// meta: since the 2026-09-22/23 audit (plan P0-2) that repaints every tab
/// on every hop instead of refusing the update, but it is still a break. A
/// new meta field is additive — `#[serde(default)]` on `CheckpointMeta` —
/// and never required.
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

// ---- build / commit identity -------------------------------------------

/// F4(b). The child proves with ITS OWN `build_info::build_number()` /
/// `GIT_COMMIT`; the parent expects the apply ticket's
/// `target_build`/`target_commit`. Any disagreement is a silent
/// `AdoptionMismatch`.
#[test]
fn child_build_and_commit_must_equal_the_parents_expectation_exactly() {
    let ids = [(0u64, 7i32, 4242i32)];
    let nonce = "0123456789abcdef0123456789abcdef";
    let (ld, sd) = ([1u8; 32], [2u8; 32]);
    let base = adoption_proof(nonce, 1_784_869_524, "7bf23bbe1234", &ld, &sd, &ids).unwrap();

    // (i) the swap did not take: the child is still the OLD binary.
    let stale_build = adoption_proof(nonce, 1_784_825_157, "7bf23bbe1234", &ld, &sd, &ids).unwrap();
    assert_ne!(base, stale_build, "build number mismatch breaks the proof");

    // (ii) a `-dirty` working tree on either side.
    let dirty = adoption_proof(nonce, 1_784_869_524, "7bf23bbe1234-dirty", &ld, &sd, &ids).unwrap();
    assert_ne!(base, dirty, "`-dirty` breaks the proof");

    // (iii) SAFE: 40-hex vs 12-hex vs mixed case all normalize together.
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

    // (iv) an un-stamped build cannot prove at all in a RELEASE binary:
    // `unknown` is admitted ONLY under debug_assertions — a property of the
    // binary. No environment variable can widen this; the
    // `|| ATERM_DEBUG_SEAMLESS_REEXEC` that used to sit in this predicate
    // let an exported value switch the identity gate off in a shipped build.
    let unknown = adoption_proof(nonce, 1_784_869_524, "unknown", &ld, &sd, &ids);
    assert_eq!(
        unknown.is_ok(),
        cfg!(debug_assertions),
        "`unknown` commit is release-fatal (no proof is ever emitted)"
    );
    // …and the refusal NAMES the commit (warm successor P6, finding 2):
    // it was a bare `None`, which the outgoing process reported as the
    // session set exceeding the proof format. `unknown-dirty` is refused
    // by every build, this debug test binary included; the start asks the
    // same gate before it launches or parks anything.
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

/// THE TRIPWIRE for the frozen cross-version wire, and the only test in this
/// file that does not compare [`adoption_proof`] against itself. Every other
/// one stays green while a changed domain string, a reordered field, a
/// dropped length prefix or a different [`normalize_commit`] quietly makes
/// the outgoing build and the incoming build compute different digests — and
/// that disagreement is `AdoptionMismatch`, which `finish_update_handoff`
/// latches manual-only for every parent already in the field. A self-
/// consistent suite cannot see it, because both sides of every comparison
/// move together.
///
/// So this compares against a constant derived once from the wire as
/// specified, and the input is chosen to hold down the parts that are easiest
/// to change by accident: an unsorted identity set (the sort is hashed
/// membership, not presentation) and a 40-hex uppercase commit (the
/// fold-and-truncate to 12 is hashed too).
///
/// If this fails, the question is never "what is the digest now". It is
/// whether the edit was meant to be a wire break at all — and if it was,
/// nothing in the protocol tells the parents in the field why their update
/// lane stopped working.
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

fn hex12(d: &[u8; 32]) -> String {
    d[..6].iter().map(|b| format!("{b:02x}")).collect()
}
