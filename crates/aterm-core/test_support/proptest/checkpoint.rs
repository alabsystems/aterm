// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! White-box checkpoint extended-state roundtrip.
//!
//! The public-API checkpoint tests (`checkpoint_restore_grid_identical`,
//! `checkpoint_restore_scrollback_identical`, `checkpoint_multiple_cycles`)
//! migrated to `aterm-integration-tests/tests/proptest_checkpoint_basic.rs`
//! as part of Gate 3 (#6803). This file retains only the extended-state
//! roundtrip that depends on crate-private checkpoint accessors.

use crate::grid::Grid;

// The extended-state roundtrip is a deterministic serialization property
// with ~40 parameters. 64 cases covers the space adequately without the
// disk I/O cost of 256 tempdir save/restore cycles.
proptest::proptest! {
    #![proptest_config(proptest::prelude::ProptestConfig::with_cases(64))]

    /// Extended state (5 categories) survives save_with_terminal/restore_to_terminal (#4462).
    ///
    /// Generates arbitrary values for CurrentStyle, CharacterSetState,
    /// SavedCursorState, TitleState, and ColorState, then verifies all
    /// roundtrip through the checkpoint file.
    #[test]
    fn checkpoint_extended_state_arbitrary_roundtrip(
        fg in proptest::num::u32::ANY,
        bg in proptest::num::u32::ANY,
        flags_bits in 0u16..2048u16,
        protected in proptest::bool::ANY,
        g0 in 0u8..14u8,
        g1 in 0u8..14u8,
        g2 in 0u8..14u8,
        g3 in 0u8..14u8,
        gl in 0u8..4u8,
        single_shift in 0u8..3u8,
        cursor_main_row in 0u16..100u16,
        cursor_main_col in 0u16..200u16,
        cursor_alt_present in proptest::bool::ANY,
        cursor_alt_row in 0u16..100u16,
        cursor_alt_col in 0u16..200u16,
        origin_mode in proptest::bool::ANY,
        auto_wrap in proptest::bool::ANY,
        sc_fg in proptest::num::u32::ANY,
        sc_bg in proptest::num::u32::ANY,
        sc_flags in 0u16..2048u16,
        sc_protected in proptest::bool::ANY,
        sc_g0 in 0u8..14u8,
        sc_gl in 0u8..4u8,
        window_title in "[a-zA-Z0-9_ -]{0,100}",
        icon_name in "[a-zA-Z0-9_ -]{0,50}",
        fg_r in proptest::num::u8::ANY,
        fg_g in proptest::num::u8::ANY,
        fg_b in proptest::num::u8::ANY,
        bg_r in proptest::num::u8::ANY,
        bg_g in proptest::num::u8::ANY,
        bg_b in proptest::num::u8::ANY,
        cursor_color_present in proptest::bool::ANY,
        cc_r in proptest::num::u8::ANY,
        cc_g in proptest::num::u8::ANY,
        cc_b in proptest::num::u8::ANY,
        selection_background_present in proptest::bool::ANY,
        sb_r in proptest::num::u8::ANY,
        sb_g in proptest::num::u8::ANY,
        sb_b in proptest::num::u8::ANY,
    ) {
        use crate::checkpoint::{CheckpointManager, CheckpointManagerExt, CheckpointTerminal};
        use crate::grid::{CellFlags, Cursor, PackedColor};
        use crate::terminal::{
            CharacterSet, CharacterSetState, CurrentStyle, GlMapping,
            SavedCursorState, SingleShift, Terminal,
        };
        use aterm_types::Rgb;
        use proptest::prelude::*;
        use aterm_tempfile::tempdir;

        let dir = tempdir().expect("temp dir");
        let mut manager = CheckpointManager::new(dir.path());
        let grid = Grid::new(24, 80);
        let mut terminal = Terminal::from_grid(grid);

        // 1. CurrentStyle
        let style = CurrentStyle::new(
            PackedColor(fg),
            PackedColor(bg),
            CellFlags::from_bits(flags_bits),
            protected,
        );
        terminal.restore_checkpoint_style(style);

        // 2. CharacterSetState
        fn charset_from_u8(v: u8) -> CharacterSet {
            CharacterSet::from_u8(v).unwrap_or(CharacterSet::Ascii)
        }
        let gl_mapping = match gl {
            0 => GlMapping::G0,
            1 => GlMapping::G1,
            2 => GlMapping::G2,
            _ => GlMapping::G3,
        };
        let ss = match single_shift {
            0 => SingleShift::None,
            1 => SingleShift::Ss2,
            _ => SingleShift::Ss3,
        };
        let charset = CharacterSetState::from_94(
            charset_from_u8(g0),
            charset_from_u8(g1),
            charset_from_u8(g2),
            charset_from_u8(g3),
            gl_mapping,
            ss,
        );
        terminal.restore_checkpoint_charset(charset);

        // 3. SavedCursorState
        let sc_style = CurrentStyle::new(
            PackedColor(sc_fg),
            PackedColor(sc_bg),
            CellFlags::from_bits(sc_flags),
            sc_protected,
        );
        let sc_charset = CharacterSetState::from_94(
            charset_from_u8(sc_g0),
            charset_from_u8(g1),
            CharacterSet::default(),
            CharacterSet::default(),
            match sc_gl {
                0 => GlMapping::G0,
                1 => GlMapping::G1,
                2 => GlMapping::G2,
                _ => GlMapping::G3,
            },
            SingleShift::default(),
        );
        terminal.restore_checkpoint_cursor_main(Some(SavedCursorState {
            cursor: Cursor { row: cursor_main_row, col: cursor_main_col },
            style: sc_style,
            origin_mode,
            auto_wrap,
            charset: sc_charset,
            pending_wrap: false,
            underline_color: None,
        }));

        if cursor_alt_present {
            let alt_style = CurrentStyle::new(
                PackedColor(!sc_fg),
                PackedColor(!sc_bg),
                CellFlags::from_bits(sc_flags ^ 0x07FF),
                !sc_protected,
            );
            terminal.restore_checkpoint_cursor_alt(Some(SavedCursorState {
                cursor: Cursor { row: cursor_alt_row, col: cursor_alt_col },
                style: alt_style,
                origin_mode: false,
                auto_wrap: true,
                charset: CharacterSetState::default(),
                pending_wrap: false,
                underline_color: None,
            }));
        } else {
            terminal.restore_checkpoint_cursor_alt(None);
        }

        // 4. TitleState
        terminal.restore_checkpoint_title(&window_title, &icon_name);

        // 5. ColorState
        let cursor_color = if cursor_color_present {
            Some(Rgb { r: cc_r, g: cc_g, b: cc_b })
        } else {
            None
        };
        let selection_background = if selection_background_present {
            Some(Rgb { r: sb_r, g: sb_g, b: sb_b })
        } else {
            None
        };
        terminal.restore_checkpoint_colors(
            Rgb { r: fg_r, g: fg_g, b: fg_b },
            Rgb { r: bg_r, g: bg_g, b: bg_b },
            cursor_color,
            selection_background,
        );

        // Save
        manager
            .save_with_terminal(terminal.grid(), None, Some(&terminal))
            .expect("save failed");

        // Restore
        let restored = manager.restore_to_terminal().expect("restore failed");

        // Verify CurrentStyle
        let rs = restored.checkpoint_style();
        prop_assert_eq!(rs.fg.0, fg, "style fg");
        prop_assert_eq!(rs.bg.0, bg, "style bg");
        prop_assert_eq!(rs.flags.bits(), flags_bits, "style flags");
        prop_assert_eq!(rs.protected, protected, "style protected");

        // Verify CharacterSetState
        let rc = restored.checkpoint_charset();
        prop_assert_eq!(rc.g0, charset.g0, "charset g0");
        prop_assert_eq!(rc.g1, charset.g1, "charset g1");
        prop_assert_eq!(rc.g2, charset.g2, "charset g2");
        prop_assert_eq!(rc.g3, charset.g3, "charset g3");
        prop_assert_eq!(rc.gl, charset.gl, "charset gl");
        prop_assert_eq!(rc.single_shift, charset.single_shift, "charset ss");

        // Verify SavedCursorState main
        let cm = restored.checkpoint_cursor_main().expect("cursor_main present");
        prop_assert_eq!(cm.cursor.row, cursor_main_row, "cursor_main row");
        prop_assert_eq!(cm.cursor.col, cursor_main_col, "cursor_main col");
        prop_assert_eq!(cm.origin_mode, origin_mode, "cursor_main origin");
        prop_assert_eq!(cm.auto_wrap, auto_wrap, "cursor_main autowrap");
        prop_assert_eq!(cm.style.fg.0, sc_fg, "cursor_main style fg");
        prop_assert_eq!(cm.style.bg.0, sc_bg, "cursor_main style bg");
        prop_assert_eq!(cm.style.flags.bits(), sc_flags, "cursor_main style flags");
        prop_assert_eq!(cm.style.protected, sc_protected, "cursor_main style protected");
        prop_assert_eq!(cm.charset.g0, charset_from_u8(sc_g0), "cursor_main charset g0");
        prop_assert_eq!(cm.charset.g1, charset_from_u8(g1), "cursor_main charset g1");

        // Verify SavedCursorState alt
        if cursor_alt_present {
            let ca = restored.checkpoint_cursor_alt().expect("cursor_alt present");
            prop_assert_eq!(ca.cursor.row, cursor_alt_row, "cursor_alt row");
            prop_assert_eq!(ca.cursor.col, cursor_alt_col, "cursor_alt col");
            prop_assert_eq!(ca.style.fg.0, !sc_fg, "cursor_alt style fg");
            prop_assert_eq!(ca.style.bg.0, !sc_bg, "cursor_alt style bg");
            prop_assert_eq!(ca.style.flags.bits(), sc_flags ^ 0x07FF, "cursor_alt style flags");
            prop_assert_eq!(ca.style.protected, !sc_protected, "cursor_alt style protected");
        } else {
            prop_assert!(restored.checkpoint_cursor_alt().is_none(), "cursor_alt absent");
        }

        // Verify TitleState
        prop_assert_eq!(restored.checkpoint_window_title(), &window_title, "title");
        prop_assert_eq!(restored.checkpoint_icon_name(), &icon_name, "icon");

        // Verify ColorState
        prop_assert_eq!(restored.checkpoint_default_fg(), Rgb { r: fg_r, g: fg_g, b: fg_b }, "fg color");
        prop_assert_eq!(restored.checkpoint_default_bg(), Rgb { r: bg_r, g: bg_g, b: bg_b }, "bg color");
        prop_assert_eq!(restored.checkpoint_cursor_color(), cursor_color, "cursor color");
        prop_assert_eq!(
            restored.selection_background(),
            selection_background,
            "selection background"
        );
    }
}
