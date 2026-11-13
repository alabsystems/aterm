// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn forced_solo_segments(cols: u16, metadata: &[TabStripMetadata]) -> Vec<TabSegment> {
    let mut segments = layout_segments_with_metadata(cols, 1, metadata, 0, false);
    if let Some(band) = segments.first_mut() {
        band.solo = true;
        band.close_col = None;
    }
    segments
}

#[test]
fn single_tab_is_a_full_width_title_band_only_where_it_is_the_window_title() {
    let segs = layout_segments(80, 1, 0, false);
    assert_eq!(segs.len(), 2, "one tab + the new-tab affordance");
    assert_eq!(segs[0].kind, TabHit::Select(0));
    assert_eq!(segs[0].start_col, 0, "the lone tab always LEADS");
    match SOLO_CHIP_MAX_COLS {
        None => {
            assert_eq!(segs[0].end_col, 80 - NEW_TAB_W);
            assert!(segs[0].solo, "a lone tab is the window title");
            assert_eq!(
                segs[0].close_col, None,
                "a title band has no close affordance; the window's own already exists"
            );
            assert_eq!(segs[1].start_col, 80 - NEW_TAB_W);
        }
        Some(cap) => {
            assert_eq!(
                segs[0].end_col, cap,
                "the lone chip is capped, not full-width"
            );
            assert!(
                cap < 80 - NEW_TAB_W,
                "and the cap must actually leave band showing at this width"
            );
            assert!(
                !segs[0].solo,
                "the OS caption carries the title here, so a lone tab is a CHIP"
            );
            assert!(
                segs[0].close_col.is_some(),
                "and a chip keeps the close affordance a title band declines"
            );
            assert_eq!(segs[1].start_col, cap);
        }
    }
    assert_eq!(segs[1].kind, TabHit::NewTab);
    assert!(!segs[1].solo);
    assert_eq!(segs[1].end_col, segs[1].start_col + NEW_TAB_W);

    let segs = layout_segments(20, 1, 0, false);
    assert_eq!(segs[0].end_col, 20 - NEW_TAB_W);
    assert_eq!(segs[1].start_col, 20 - NEW_TAB_W);
}

#[test]
fn the_rename_field_paints_the_text_with_a_reverse_video_caret() {
    let theme = Theme::default();
    let metadata = [
        TabStripMetadata::from_presentation(&crate::tab_model::TabPresentation::terminal("a")),
        TabStripMetadata::from_presentation(&crate::tab_model::TabPresentation::terminal("b")),
    ];
    let segments = layout_segments_with_metadata(60, 2, &metadata, 0, false);
    let titles = ["one".to_string(), "two".to_string()];
    let mut row = vec![blank_cell(theme); 60];
    let (_, painted_caret) = paint_strip_with_metadata_and_rename_caret(
        &mut row,
        &segments,
        &titles,
        &metadata,
        StripPaint {
            hovered: None,
            new_tab_hovered: false,
            version_hovered: false,
            caption_hovered: None,
            subtitle: None,
            rename: Some(StripRenameField {
                tab: 0,
                text: "bu日ld",
                cursor: 2,
            }),
        },
        0,
        theme.into(),
        StripFloor::Ground,
    );
    let seg = segments[0];
    let painted: String = row[seg.start_col as usize..seg.end_col as usize]
        .iter()
        .filter(|c| !c.wide)
        .map(|c| c.ch)
        .collect();
    assert!(
        painted.contains("bu日ld"),
        "the field's text, not the title"
    );
    assert!(
        !painted.contains("one"),
        "the label is replaced while editing, not drawn beside the field"
    );
    let colors = crate::chrome_band::band_colors(theme);
    let lead_col = row
        .iter()
        .position(|cell| cell.ch == '日')
        .expect("the wide char under the caret is painted as itself");
    let (caret, tail) = (row[lead_col], row[lead_col + 1]);
    assert_eq!(caret.fg, colors.field_bg, "reverse video: bg becomes ink");
    assert_eq!(caret.bg, colors.caret);
    assert!(tail.wide, "the continuation is flagged for the renderer");
    assert_eq!(tail.bg, colors.caret, "and the block covers both cells");
    assert_eq!(
        row[lead_col + 2].bg,
        colors.field_bg,
        "the next cell is field"
    );
    assert_eq!(
        caret.underline,
        UnderlineStyle::Single,
        "the edited chip is still the selected one"
    );
    let other: String = row[segments[1].start_col as usize..segments[1].end_col as usize]
        .iter()
        .map(|c| c.ch)
        .collect();
    assert!(other.contains("two"));
    assert_eq!(painted_caret, Some(lead_col));
    assert_eq!(
        rename_ime_cursor_cell(2, painted_caret),
        Some((1, lead_col))
    );

    for solo in [false, true] {
        let mut segments = forced_solo_segments(4, &metadata[..1]);
        segments[0].solo = solo;
        let mut narrow = vec![blank_cell(theme); 4];
        let (_, painted_caret) = paint_strip_with_metadata_and_rename_caret(
            &mut narrow,
            &segments,
            &titles[..1],
            &metadata[..1],
            StripPaint {
                hovered: None,
                new_tab_hovered: false,
                version_hovered: false,
                caption_hovered: None,
                subtitle: None,
                rename: Some(StripRenameField {
                    tab: 0,
                    text: "日",
                    cursor: 0,
                }),
            },
            0,
            theme.into(),
            StripFloor::Ground,
        );
        assert_eq!(painted_caret, None, "no caret fits in the narrow well");
        assert!(!narrow.iter().any(|cell| cell.wide));
        assert_eq!(rename_ime_cursor_cell(2, painted_caret), Some((1, 0)));
    }
    assert_eq!(rename_ime_cursor_cell(0, painted_caret), None);
}

#[test]
fn a_busy_narrow_chip_spends_its_whole_interior_on_the_field() {
    let theme = Theme::default();
    let mut presentation = crate::tab_model::TabPresentation::terminal("a");
    presentation.icon = Some(TabIconKind::Settings);
    presentation.indicators.busy = true;
    presentation.indicators.attention = true;
    let metadata = [
        TabStripMetadata::from_presentation(&presentation),
        TabStripMetadata::from_presentation(&presentation),
    ];
    let segments = layout_segments_with_metadata(28, 2, &metadata, 0, false);
    let titles = ["one".to_string(), "two".to_string()];
    let mut row = vec![blank_cell(theme); 28];
    let images = paint_strip_with_metadata(
        &mut row,
        &segments,
        &titles,
        &metadata,
        StripPaint {
            hovered: None,
            new_tab_hovered: false,
            version_hovered: false,
            caption_hovered: None,
            subtitle: None,
            rename: Some(StripRenameField {
                tab: 0,
                text: "abcdef",
                cursor: 6,
            }),
        },
        0,
        theme,
        None,
    );
    let seg = segments[0];
    let painted: String = row[seg.start_col as usize..seg.end_col as usize]
        .iter()
        .map(|c| c.ch)
        .collect();
    assert!(
        painted.contains("def"),
        "the field scrolled to keep the caret in view: {painted:?}"
    );
    assert!(
        !painted.contains('…'),
        "a field scrolls; it never ellipsises what you are typing: {painted:?}"
    );
    assert!(
        images
            .iter()
            .all(|(col, _)| *col < seg.start_col as usize || *col >= seg.end_col as usize),
        "the edited chip's icon and status marks yield their cells to the field"
    );
}

#[test]
fn editing_the_solo_band_drops_the_centred_group_for_a_left_aligned_field() {
    let theme = Theme::default();
    let metadata = [TabStripMetadata::from_presentation(
        &crate::tab_model::TabPresentation::terminal("aterm"),
    )];
    let segments = forced_solo_segments(40, &metadata);
    let band = segments[0];
    assert!(band.solo);
    let mut row = vec![blank_cell(theme); 40];
    paint_strip_with_metadata(
        &mut row,
        &segments,
        &["aterm".to_string()],
        &metadata,
        StripPaint {
            hovered: None,
            new_tab_hovered: false,
            version_hovered: false,
            caption_hovered: None,
            subtitle: Some("~/aterm"),
            rename: Some(StripRenameField {
                tab: 0,
                text: "agent",
                cursor: 5,
            }),
        },
        0,
        theme,
        None,
    );
    let painted: String = row[..band.end_col as usize].iter().map(|c| c.ch).collect();
    assert!(painted.contains("agent"), "the field is drawn");
    assert!(
        !painted.contains("~/aterm"),
        "the description would recompose under the caret: {painted:?}"
    );
    let first = painted.find(|c: char| c != ' ').expect("some ink");
    assert_eq!(
        first, SOLO_EDGE_COLS,
        "left-aligned at the band's inset, not centred"
    );
}

/// THE RENAME FIELD SHOWS WHERE IT IS (audit r20 MC1): in the solo band its well is the
/// terminal's ground, which on macOS is the band's own (1:1, a field with no edge at all) and
/// elsewhere a step off it (Default 1.4:1, GitHub Light 1.2:1), so the well's cells carry the
/// edge every faint text field draws, 3:1 on the well and on the band, and the band's cells
/// beside it none.
#[test]
fn the_solo_bands_rename_field_draws_its_edge() {
    let light = Theme::from(&aterm_types::scheme::builtin("GitHub Light").expect("built-in"));
    for theme in [Theme::default(), light] {
        let metadata = [TabStripMetadata::from_presentation(
            &crate::tab_model::TabPresentation::terminal("aterm"),
        )];
        let segments = forced_solo_segments(40, &metadata);
        let band = segments[0];
        let mut row = vec![blank_cell(theme); 40];
        paint_strip_with_metadata(
            &mut row,
            &segments,
            &["aterm".to_string()],
            &metadata,
            StripPaint {
                hovered: None,
                new_tab_hovered: false,
                version_hovered: false,
                caption_hovered: None,
                subtitle: None,
                rename: Some(StripRenameField {
                    tab: 0,
                    text: "agent",
                    cursor: 5,
                }),
            },
            0,
            theme,
            None,
        );
        let colors = crate::chrome_band::band_colors(theme);
        let field = SOLO_EDGE_COLS..usize::from(band.end_col) - SOLO_EDGE_COLS;
        let ground = row[SOLO_EDGE_COLS - 1].bg;
        assert!(
            crate::chrome_band::contrast(colors.field_bg, ground) < 3.0,
            "{theme:?}: the fixture is only interesting because the well is faint"
        );
        for (col, cell) in row.iter().enumerate() {
            if field.contains(&col) {
                let edge = cell.underline_color.expect("the well's edge");
                assert_eq!(cell.underline, UnderlineStyle::Single, "{theme:?} {col}");
                for on in [colors.field_bg, ground] {
                    assert!(
                        crate::chrome_band::contrast(edge, on) >= 3.0,
                        "{theme:?} {col}: the edge shows on the well and the band"
                    );
                }
            } else if col < usize::from(band.end_col) {
                assert_eq!(cell.underline, UnderlineStyle::None, "{theme:?} {col}");
            }
        }
    }
}

#[test]
fn an_empty_rename_field_placeholds_with_the_resolved_label() {
    let theme = Theme::default();
    let metadata = [
        TabStripMetadata::from_presentation(&crate::tab_model::TabPresentation::terminal("a")),
        TabStripMetadata::from_presentation(&crate::tab_model::TabPresentation::terminal("b")),
    ];
    let segments = layout_segments_with_metadata(60, 2, &metadata, 0, false);
    let titles = ["vim src/main.rs".to_string(), "two".to_string()];
    let mut row = vec![blank_cell(theme); 60];
    paint_strip_with_metadata(
        &mut row,
        &segments,
        &titles,
        &metadata,
        StripPaint {
            hovered: None,
            new_tab_hovered: false,
            version_hovered: false,
            caption_hovered: None,
            subtitle: None,
            rename: Some(StripRenameField {
                tab: 0,
                text: "",
                cursor: 0,
            }),
        },
        0,
        theme,
        None,
    );
    let seg = segments[0];
    let painted: String = row[seg.start_col as usize..seg.end_col as usize]
        .iter()
        .map(|c| c.ch)
        .collect();
    assert!(
        painted.contains("vim src"),
        "the fallback label is the placeholder: {painted:?}"
    );
    let colors = crate::chrome_band::band_colors(theme);
    let hint = row
        .iter()
        .find(|cell| cell.ch == 'v')
        .expect("the placeholder is drawn");
    assert_eq!(hint.fg, colors.label, "dimmed — it is not content");
}

#[test]
fn the_close_mark_is_hover_revealed_and_resident_on_the_selected_card() {
    let theme = Theme::default();
    let metadata = [
        TabStripMetadata::from_presentation(&crate::tab_model::TabPresentation::terminal("a")),
        TabStripMetadata::from_presentation(&crate::tab_model::TabPresentation::terminal("b")),
    ];
    let titles = ["a".to_string(), "b".to_string()];
    let segments = layout_segments_with_metadata(60, 2, &metadata, 0, false);
    let close_cols: Vec<u16> = segments.iter().filter_map(|seg| seg.close_col).collect();
    assert_eq!(close_cols.len(), 2, "both chips reserve a close column");

    let marks = |hovered: Option<usize>| {
        let mut row = vec![blank_cell(theme); 60];
        paint_strip_with_metadata(
            &mut row,
            &segments,
            &titles,
            &metadata,
            StripPaint {
                hovered,
                new_tab_hovered: false,
                version_hovered: false,
                caption_hovered: None,
                subtitle: None,
                rename: None,
            },
            0,
            theme,
            None,
        );
        close_cols
            .iter()
            .map(|col| row[*col as usize].ch)
            .collect::<Vec<_>>()
    };
    if STRIP_CHIP_CARDS {
        assert_eq!(
            marks(None),
            ['✕', ' '],
            "the selected card's ✕ is resident; quiet chips stay bare"
        );
        assert_eq!(marks(Some(0)), ['✕', ' ']);
        assert_eq!(
            marks(Some(1)),
            ['✕', '✕'],
            "hover reveals a quiet chip's ✕ beside the resident one"
        );
    } else {
        assert_eq!(
            marks(None),
            [' ', ' '],
            "no pointer, no ✕ — not even on the selected tab"
        );
        assert_eq!(
            marks(Some(0)),
            ['✕', ' '],
            "only the hovered tab reveals it"
        );
        assert_eq!(marks(Some(1)), [' ', '✕']);
    }
    assert_eq!(
        layout_segments_with_metadata(60, 2, &metadata, 1, false),
        segments,
        "selection does not move the close columns either"
    );
}

#[test]
fn solo_band_compresses_the_description_before_the_title() {
    let (title, subtitle) = solo_band_text("aterm", Some("~/very/long/path/here"), 40);
    assert_eq!(title, "aterm", "a title that fits is never cut");
    assert_eq!(
        subtitle.as_deref(),
        Some("~/very/long/path/here"),
        "and a description that fits is not cut either"
    );

    let (title, subtitle) = solo_band_text("aterm", Some("~/very/long/path/here"), 24);
    assert_eq!(title, "aterm");
    assert_eq!(subtitle.as_deref(), Some("~/very/long…"));

    let (title, subtitle) = solo_band_text("aterm", Some("~/aterm"), 14);
    assert_eq!(title, "aterm");
    assert_eq!(
        subtitle, None,
        "no room for a description, so none is drawn"
    );

    let (title, subtitle) = solo_band_text("a-very-long-session-title", Some("~/aterm"), 14);
    assert_eq!(title, "a-very-lo…", "the title takes every cell there is");
    assert_eq!(subtitle, None);
    assert_eq!(
        title.chars().count(),
        14 - 2 * SOLO_EDGE_COLS,
        "and never overruns the band"
    );
}

#[test]
fn multiple_tabs_split_the_whole_band_evenly() {
    for count in 2..=6usize {
        let segs = layout_segments(200, count, 0, false);
        let tabs: Vec<_> = segs
            .iter()
            .filter(|s| matches!(s.kind, TabHit::Select(_)))
            .collect();
        assert_eq!(tabs.len(), count);
        let band = 200 - NEW_TAB_W;
        let share = band / u16::try_from(count).unwrap();
        let per = CHIP_MAX_COLS.map_or(share, |cap| share.min(cap));
        for (index, seg) in tabs.iter().enumerate() {
            assert!(!seg.solo, "only a LONE tab is a title band");
            assert_eq!(seg.start_col, per * u16::try_from(index).unwrap());
            assert_eq!(seg.end_col - seg.start_col, per, "equal shares");
        }
        if per == share {
            assert!(band - tabs.last().unwrap().end_col < u16::try_from(count).unwrap());
        }
    }
}

#[test]
fn a_lone_chip_is_as_wide_as_each_of_a_pair() {
    if !STRIP_WINDOWS_PAINT {
        return;
    }
    for cols in 20..=300u16 {
        let width = |count: usize| {
            let segs = layout_segments(cols, count, 0, false);
            let seg = segs
                .iter()
                .find(|s| s.kind == TabHit::Select(0))
                .expect("the first chip");
            seg.end_col - seg.start_col
        };
        assert!(
            width(1) >= width(2),
            "{cols} cols: {} < {}",
            width(1),
            width(2)
        );
    }
}

#[test]
fn chip_widths_are_each_platforms_own() {
    let width = |count: usize| {
        let segs = layout_segments(200, count, 0, false);
        let seg = segs
            .iter()
            .find(|s| s.kind == TabHit::Select(0))
            .expect("the first chip");
        seg.end_col - seg.start_col
    };
    let band = 200 - NEW_TAB_W;
    let expected = if cfg!(windows) {
        (27, 27)
    } else if cfg!(target_os = "macos") {
        (band, band / 2)
    } else {
        (28, band / 2)
    };
    assert_eq!((width(1), width(2)), expected);
}

#[test]
fn the_windows_tab_cap_is_counted_in_dips() {
    for (cell_w, scale, cap, grow) in [
        (9usize, 1.0f32, 27u16, 48u16),
        (13, 1.5, 28, 50),
        (17, 2.0, 28, 51),
    ] {
        let sizing = TabSizing::at(cell_w, scale, &[]);
        assert_eq!((sizing.cap, sizing.grow), (cap, grow), "{scale}x");
        for (cols, dip) in [(sizing.cap, TAB_MAX_DIP), (sizing.grow, TAB_GROW_DIP)] {
            let off = (f32::from(cols) * cell_w as f32 - dip * scale).abs();
            assert!(
                off <= cell_w as f32 * 0.5,
                "{scale}x: {cols} cols is {off} px off"
            );
        }
        assert!(
            f32::from(MAX_CHIP_COLS) * (cell_w as f32) < TAB_MAX_DIP * scale - 6.0 || scale <= 1.0,
            "CONTROL: {MAX_CHIP_COLS} columns fall short at {scale}x"
        );
    }
}

#[test]
fn a_windows_tab_grows_toward_its_title_while_the_strip_has_room() {
    if !STRIP_WINDOWS_PAINT {
        return;
    }
    let widths = |cols: u16, wants: &[u16]| {
        let metadata = vec![PLAIN_TAB; wants.len()];
        let sizing = TabSizing::at(9, 1.0, wants);
        let segs = layout_segments_with_version_sized(
            cols,
            wants.len(),
            &metadata,
            0,
            false,
            VersionSlotCols::NONE,
            sizing,
        );
        let mut x = 0;
        let mut out = Vec::new();
        for seg in &segs {
            assert_eq!(seg.start_col, x, "segments are packed: {segs:?}");
            x = seg.end_col;
            if let TabHit::Select(_) = seg.kind {
                out.push(seg.end_col - seg.start_col);
            }
        }
        assert!(
            segs.last().is_some_and(|s| s.kind == TabHit::NewTab),
            "the + follows the last tab"
        );
        out
    };
    assert_eq!(widths(111, &[40]), vec![40], "a lone long title");
    assert_eq!(widths(111, &[12]), vec![27], "a short title keeps the tab");
    assert_eq!(widths(111, &[90]), vec![48], "the growth cap");
    assert_eq!(widths(111, &[40, 0]), vec![40, 27], "two tabs");
    assert_eq!(
        widths(95, &[48, 48, 0]),
        vec![33, 32, 27],
        "the room past 3 x 27 is shared by the tabs that want it"
    );
    assert_eq!(
        widths(100, &[48, 48, 48, 48, 48]),
        vec![19; 5],
        "a crowded strip keeps equal shares"
    );
    let metadata = vec![PLAIN_TAB; 2];
    assert_eq!(
        layout_segments_with_version_sized(
            111,
            2,
            &metadata,
            0,
            false,
            VersionSlotCols::NONE,
            TabSizing::FIXED
        ),
        layout_segments_with_version(111, 2, &metadata, 0, false, VersionSlotCols::NONE)
    );
}

#[test]
fn a_sized_band_keeps_a_foot_of_ground_under_its_seam() {
    if !STRIP_WINDOWS_PAINT {
        assert_eq!(strip_foot_px(38, 32, 1.0), 0, "no foot off Windows");
        return;
    }
    for (canvas, target, scale, want) in [
        (38, 32, 1.0, 6),
        (57, 48, 1.5, 9),
        (76, 64, 2.0, 12),
        (42, 32, 1.0, 6),
        (25, 32, 1.0, 0),
        (25, 0, 1.0, 0),
    ] {
        assert_eq!(
            strip_foot_px(canvas, target, scale),
            want,
            "canvas {canvas}, target {target}, {scale}x"
        );
    }
    let theme = Theme::default();
    let colors = strip_colors(theme);
    let pack = |c: [u8; 3]| (u32::from(c[0]) << 16) | (u32::from(c[1]) << 8) | u32::from(c[2]);
    let footed = strip_seam_rail(0, 100, 1, 6, theme, StripFloor::Ground).expect("a rail");
    assert_eq!(footed.rail_h, 6);
    assert!(footed.rail.iter().all(|&px| px == pack(colors.foot)));
    let page = [0x1e, 0x1e, 0x20];
    let paged = strip_seam_rail(0, 100, 1, 6, theme, StripFloor::Surface(page)).expect("a rail");
    assert!(paged.rail.iter().all(|&px| px == pack(page)));
    let bare = strip_seam_rail(0, 100, 1, 0, theme, StripFloor::Ground).expect("a rail");
    assert_eq!(bare.rail_h, 1);
    assert!(
        bare.rail
            .iter()
            .all(|&px| px == pack(colors.seam.expect("a seam")))
    );
    let mut app = crate::App::headless_for_test();
    app.tab_strip_rows = 1;
    assert_eq!(
        app.synthetic_strip_head_px(1.0, 23),
        32 + 6 - crate::logical_to_device_px(app.config.window_padding_top_or_default(), 1.0) - 23,
        "the head holds the 32 px band less its row and pad, plus the foot"
    );
}

#[test]
fn update_icon_prepends_and_shifts_tabs() {
    let none = layout_segments(80, 2, 0, false);
    assert!(
        !none.iter().any(|s| s.kind == TabHit::Update),
        "no icon when not staged"
    );

    let segs = layout_segments(80, 2, 0, true);
    assert_eq!(
        segs[0].kind,
        TabHit::Update,
        "the leading segment is the update icon"
    );
    assert_eq!(segs[0].start_col, 0);
    assert_eq!(segs[0].end_col, UPDATE_W);
    let first_tab = segs
        .iter()
        .find(|s| matches!(s.kind, TabHit::Select(_)))
        .unwrap();
    assert_eq!(
        first_tab.start_col, UPDATE_W,
        "tabs shifted right by the icon width"
    );
    assert_eq!(hit_test(&segs, 1), Some(TabHit::Update));
    assert_eq!(hit_test(&segs, UPDATE_W), Some(TabHit::Select(0)));
}

#[test]
fn canonical_metadata_preserves_segment_and_hit_geometry() {
    let base = layout_segments(80, 2, 0, false);
    let terminal = crate::tab_model::TabPresentation::terminal("long-live-session-title");
    let metadata = [
        TabStripMetadata::from_presentation(&terminal),
        TabStripMetadata {
            icon: Some(TabIconKind::Settings),
            dirty: true,
            busy: true,
            attention: ChipLevel::Wait,
            conn: Some(TabConnRole::Both),
            closable: false,
            team: TabTeam::NONE,
            load: 0,
        },
    ];
    let canonical = layout_segments_with_metadata(80, metadata.len(), &metadata, 0, false);
    assert_eq!(canonical.len(), base.len());
    for (actual, prior) in canonical.iter().zip(&base) {
        assert_eq!(
            (actual.start_col, actual.end_col, actual.kind),
            (prior.start_col, prior.end_col, prior.kind),
            "icons/status never alter select/reorder geometry"
        );
    }
    assert_eq!(canonical[0].close_col, base[0].close_col);
    assert_eq!(canonical[1].close_col, None, "canonical closable=false");
    assert_eq!(
        hit_test(&canonical, canonical[0].close_col.unwrap()),
        Some(TabHit::Close(0))
    );
    assert_eq!(
        hit_test(&canonical, base[1].close_col.unwrap()),
        Some(TabHit::Select(1)),
        "the painted status cell selects its tab"
    );
}

/// Every built-in theme, then palettes with nothing to borrow.
fn app_tile_palettes() -> Vec<(String, ChromePalette)> {
    let mut out: Vec<(String, ChromePalette)> = aterm_types::scheme::builtin_names()
        .iter()
        .map(|name| {
            let scheme = aterm_types::scheme::builtin(name).expect("listed scheme exists");
            (
                (*name).to_string(),
                ChromePalette::from(Theme::from(&scheme)),
            )
        })
        .collect();
    let base = out[0].1;
    for (name, bg, fg, cursor) in [
        ("mono dark", 0x16_1616, 0xc8_c8c8, 0xa0_a0a0),
        ("mono light", 0xf4_f4f4, 0x33_3333, 0x66_6666),
        ("low cursor", 0x20_242c, 0xb8_c0cc, 0x26_2c3a),
        ("mid grey", 0x77_7777, 0x88_8888, 0x80_8080),
    ] {
        let mut palette = base;
        palette.theme.bg = bg;
        palette.theme.fg = fg;
        palette.theme.cursor = cursor;
        palette.ansi = None;
        out.push((name.to_string(), palette));
    }
    for (name, tab) in [
        ("active mid", [0x80, 0x70, 0x60]),
        ("active red", [0xc0, 0x20, 0x20]),
    ] {
        let mut palette = base;
        palette.authored.active_tab = Some(tab);
        out.push((name.to_string(), palette));
    }
    out
}

#[test]
fn app_tiles_hold_their_floors_in_every_theme_and_state() {
    use crate::chrome_band::contrast;
    for (name, palette) in app_tile_palettes() {
        let c = strip_colors_for(palette);
        for (state, tile, ground) in [
            ("active", c.app_active, c.active_bg),
            ("solo", c.app_solo, c.band_bg),
        ] {
            assert!(
                contrast(tile.fill, ground) >= APP_TILE_FLOOR,
                "{name} {state} tile"
            );
            assert!(
                contrast(tile.glyph, tile.fill) >= APP_TILE_FLOOR,
                "{name} {state} glyph"
            );
        }
        for (state, tile, ground) in [
            ("quiet", c.app_quiet, c.chip_bg),
            ("hover", c.app_hover, c.hover_bg),
        ] {
            assert!(
                contrast(tile.fill, ground) >= APP_TILE_QUIET_FLOOR,
                "{name} {state} tile"
            );
            assert!(
                contrast(tile.glyph, tile.fill) >= APP_TILE_FLOOR,
                "{name} {state} glyph"
            );
        }
        assert!(
            contrast(c.app_quiet.glyph, c.chip_bg) >= APP_TILE_FLOOR,
            "{name}: the narrow chip's spine takes the quiet glyph's ink"
        );
        assert_ne!(c.app_active.fill, c.app_quiet.fill, "{name}: states differ");
        for (active, hovered) in [(true, false), (false, true), (false, false)] {
            assert!(
                contrast(c.app_spine(active, hovered), c.band_bg) >= APP_TILE_FLOOR,
                "{name}: a spine stands on the band"
            );
        }
    }
}

#[test]
fn the_macos_inks_come_from_the_theme_not_the_system_accent() {
    for (name, palette) in app_tile_palettes() {
        let tokens = palette.tokens();
        let inks = native_app_tiles(palette, tokens.bg);
        assert_eq!(
            inks.quiet,
            app_tile_tonal(tokens.fg, tokens.theme_tint, tokens.bg),
            "{name}"
        );
        assert_eq!(
            inks.solo,
            app_tile_solid(tokens.fg, tokens.theme_tint, tokens.bg),
            "{name}"
        );
    }
}

#[test]
fn an_app_tabs_title_is_bold_whether_or_not_it_is_selected() {
    let theme = Theme::default();
    let app = TabStripMetadata {
        icon: Some(TabIconKind::SystemState),
        ..PLAIN_TAB
    };
    let metadata = [PLAIN_TAB, PLAIN_TAB, app];
    let titles = ["zero", "one", "two"].map(str::to_string);
    for active in 0..metadata.len() {
        let segments = layout_segments_with_metadata(90, 3, &metadata, active, false);
        let mut row = vec![blank_cell(theme); 90];
        paint_strip_with_metadata(
            &mut row,
            &segments,
            &titles,
            &metadata,
            StripPaint {
                hovered: None,
                new_tab_hovered: false,
                version_hovered: false,
                caption_hovered: None,
                subtitle: None,
                rename: None,
            },
            active,
            theme,
            None,
        );
        for (tab, title) in titles.iter().enumerate() {
            let segment = segments
                .iter()
                .find(|segment| matches!(segment.kind, TabHit::Select(i) if i == tab))
                .expect("every tab has a chip");
            let span = usize::from(segment.start_col)..usize::from(segment.end_col);
            let letters: Vec<&RenderCell> = row[span]
                .iter()
                .filter(|cell| title.contains(cell.ch))
                .collect();
            assert_eq!(letters.len(), title.len(), "{title} with {active} selected");
            let want = tab == active || metadata[tab].is_app_tab();
            assert!(
                letters.iter().all(|cell| cell.bold == want),
                "{title} with {active} selected"
            );
        }
    }
}

#[test]
fn a_tab_is_an_app_by_its_kinds_surface_not_by_wearing_an_icon() {
    let with = |icon| TabStripMetadata { icon, ..PLAIN_TAB };
    assert!(!with(None).is_app_tab());
    assert!(
        !with(Some(TabIconKind::Codex)).is_app_tab(),
        "a program in a terminal"
    );
    for kind in TabIconKind::ALL
        .into_iter()
        .filter(|k| *k != TabIconKind::Codex)
    {
        assert!(with(Some(kind)).is_app_tab(), "{kind:?}");
    }
}

#[test]
fn every_icon_kind_rasters_its_own_glyph() {
    let rasters: Vec<_> = TabIconKind::ALL
        .iter()
        .map(|kind| rasterize_icon(tab_icon_primitives(*kind), [255, 255, 255]))
        .collect();
    for (i, a) in rasters.iter().enumerate() {
        for b in &rasters[i + 1..] {
            assert_ne!(a, b, "two icon kinds draw alike");
        }
    }
}

#[test]
fn a_tile_paints_its_fill_and_glyph_and_leaves_its_corners_clear() {
    let tile = AppTile {
        fill: [10, 20, 30],
        glyph: [200, 210, 220],
    };
    let mut images = Vec::new();
    append_icon_images(
        &mut images,
        0,
        TabIconKind::SystemState,
        tile.glyph,
        Some(tile),
    );
    let bytes = &images[0].1.image.bytes;
    let side = usize::from(ICON_RASTER_SIZE);
    let at = |x: usize, y: usize| &bytes[(y * side + x) * 4..(y * side + x) * 4 + 4];
    assert_eq!(at(0, 0)[3], 0, "the rounded corner shows the chip");
    assert_eq!(at(side / 8, side / 2), &[10, 20, 30, 255][..], "the fill");
    assert!(
        bytes
            .as_chunks::<4>()
            .0
            .iter()
            .any(|p| p[..3] == tile.glyph),
        "the glyph stands on the fill"
    );
}

#[test]
fn memoized_strip_glyphs_match_a_fresh_raster_for_every_key() {
    for color in [[213u8, 219, 255], [255, 180, 64]] {
        for kind in TabIconKind::ALL {
            let mut images = Vec::new();
            append_icon_images(&mut images, 0, kind, color, None);
            let expected = rasterize_icon(tab_icon_primitives(kind), color);
            assert_eq!(images.len(), usize::from(ICON_COLS));
            for (_, placed) in &images {
                assert_eq!(placed.image.bytes, expected, "{kind:?} in {color:?}");
                assert_eq!(placed.image.cols, ICON_COLS, "{kind:?} keeps its footprint");
            }
        }
        for bits in 0..8u8 {
            let metadata = TabStripMetadata {
                icon: None,
                dirty: bits & 0b001 != 0,
                busy: bits & 0b010 != 0,
                attention: crate::app_presence::chip_of_attention(bits & 0b100 != 0),
                conn: None,
                team: TabTeam::NONE,
                closable: true,
                load: 0,
            };
            let mut images = Vec::new();
            append_status_image(&mut images, 0, metadata, color);
            let expected = rasterize_icon(&status_primitives(metadata), color);
            let placed = &images[0].1;
            assert_eq!(placed.image.bytes, expected, "{metadata:?} in {color:?}");
            assert_eq!(placed.image.cols, 1, "status marks stay one cell wide");
        }
    }
}

#[test]
fn terminal_tab_removes_icon_raster_and_recovers_title_cells() {
    let theme = Theme::default();
    let presentations = [
        crate::tab_model::TabPresentation::terminal("terminal-session-with-a-long-name"),
        crate::tab_model::TabPresentation {
            title: "Settings".to_string(),
            icon: Some(TabIconKind::Settings),
            indicators: crate::tab_model::TabIndicators::default(),
            conn: None,
            closable: true,
            tooltip: None,
        },
    ];
    let metadata = presentations
        .iter()
        .map(TabStripMetadata::from_presentation)
        .collect::<Vec<_>>();
    assert_eq!(metadata[0].icon, None, "terminal identity is title-only");
    assert_eq!(metadata[1].icon, Some(TabIconKind::Settings));

    let segments = layout_segments_with_metadata(80, metadata.len(), &metadata, 0, false);
    let terminal_layout = tab_content_layout(&segments[0], metadata[0]);
    let settings_layout = tab_content_layout(&segments[1], metadata[1]);
    let lead = 1 + u16::from(STRIP_CHIP_CARDS);
    assert_eq!(terminal_layout.icon_start, None);
    assert_eq!(
        settings_layout.icon_start,
        Some(segments[1].start_col + lead)
    );
    assert_eq!(terminal_layout.title_start, segments[0].start_col + lead);
    assert_eq!(
        settings_layout.title_start,
        segments[1].start_col + lead + ICON_COLS + ICON_GAP
    );
    assert_eq!(
        settings_layout.title_start - (segments[1].start_col + lead),
        ICON_COLS + ICON_GAP,
        "terminal title recovers the entire icon-and-gap reservation"
    );

    let mut row = vec![blank_cell(theme); 80];
    let images = paint_strip_with_metadata(
        &mut row,
        &segments,
        &presentations
            .iter()
            .map(|presentation| presentation.title.clone())
            .collect::<Vec<_>>(),
        &metadata,
        StripPaint {
            hovered: Some(0),
            new_tab_hovered: false,
            version_hovered: false,
            caption_hovered: None,
            subtitle: None,
            rename: None,
        },
        0,
        theme,
        None,
    );
    assert!(
        images
            .iter()
            .all(|(col, _)| *col >= usize::from(segments[1].start_col)),
        "terminal segment has no icon image placement"
    );
    assert_eq!(
        row[terminal_layout.title_start as usize].ch, 't',
        "the recovered leading cell carries session-title text"
    );
}

#[test]
fn solo_subtitle_prefers_authored_prose_and_never_echoes_the_title() {
    for (title, chrome, expected) in [
        (
            "aterm",
            Some("aterm\ndescription: the release cutter\nactivity: Building\ncwd: ~/aterm"),
            Some("the release cutter"),
        ),
        (
            "aterm",
            Some("aterm\ncwd: ~/aterm\nactivity: Building"),
            Some("~/aterm"),
        ),
        ("aterm", Some("aterm\nactivity: Building"), Some("Building")),
        ("aterm", Some("aterm\nstate: exited"), None),
        (
            "aterm · Ready",
            Some("aterm · Ready\nactivity: Ready"),
            None,
        ),
        (
            "aterm · Ready",
            Some("aterm · Ready\nactivity: Ready\ncwd: ~/aterm"),
            Some("~/aterm"),
        ),
        ("aterm", None, None),
        ("aterm", Some("aterm"), None),
        ("aterm", Some("aterm\ncwd: "), None),
        ("aterm", Some("aterm\ncwd: -"), None),
        ("aterm", Some("aterm\n\nstate changed · just now"), None),
    ] {
        assert_eq!(
            solo_subtitle(title, chrome).as_deref(),
            expected,
            "{title:?} over {chrome:?}"
        );
    }
}

#[test]
fn native_point_layout_keeps_close_and_tab_targets_while_reserving_app_icon_only() {
    let plain = native_tab_content_layout(220.0, 14.0, false, false);
    let app = native_tab_content_layout(220.0, 14.0, true, true);
    assert_eq!(app.close, plain.close, "close hit target never moves");
    assert!(plain.close_available && app.close_available);
    assert!(plain.icon.is_none() && app.icon.is_some() && app.status.is_some());
    for layout in [
        plain,
        app,
        native_tab_content_layout(220.0, 14.0, true, false),
    ] {
        assert_eq!(
            layout.label[0],
            220.0 - (layout.label[0] + layout.label[2]),
            "the title's insets are symmetric, so a centred title reads centred"
        );
    }
    assert_eq!(
        plain.label[2] - native_tab_content_layout(220.0, 14.0, true, false).label[2],
        2.0 * (TAB_ICON_NATIVE_SIZE + 4.0),
        "a terminal gets the native icon slot and gap back for its label"
    );

    for width in [48.0, 63.0] {
        let compact = native_tab_content_layout(width, 14.0, true, true);
        assert_eq!(compact.close, app.close);
        assert!(
            !compact.close_available,
            "{width}: title reclaims close space"
        );
        assert_eq!(compact.icon, None, "{width}: title wins before identity");
        assert_eq!(compact.status, None, "{width}: no room for the states");
        assert!(
            compact.label[2] >= 0.0,
            "{width}: label clips, never underflows"
        );
    }

    let lowered = native_tab_content_layout(220.0, 16.0, true, true);
    assert_eq!(lowered.close[1], app.close[1] + 2.0);
    assert_eq!(lowered.icon.unwrap()[1], app.icon.unwrap()[1] + 2.0);
    assert_eq!(lowered.status.unwrap()[1], app.status.unwrap()[1] + 2.0);
    assert_eq!(lowered.label[1], app.label[1] + 2.0);
    assert_eq!(
        [lowered.close[0], lowered.label[0], lowered.label[2]],
        [app.close[0], app.label[0], app.label[2]],
        "a vertical re-centre never disturbs horizontal geometry"
    );
}

#[test]
fn a_chip_load_meter_shows_a_tenth_of_a_core_in_whole_points() {
    assert_eq!(chip_load_step(0.05, 10.0), 0, "quiet earns no meter");
    assert_eq!(chip_load_step(0.1, 512.0), 1, "a tenth of a core shows");
    assert_eq!(chip_load_step(5.0, 10.0), 128);
    assert_eq!(chip_load_step(40.0, 10.0), 255, "the machine is the end");
    assert_eq!(chip_load_step(1.0, 0.0), 0, "no capacity, no meter");
    assert_eq!(chip_load_step(f32::NAN, 10.0), 0);
    assert_eq!(chip_load_len_pt(120.0, 0), None);
    assert_eq!(chip_load_len_pt(120.0, 1), Some(1.0));
    assert_eq!(chip_load_len_pt(120.0, 128), Some(50.0));
    assert_eq!(chip_load_len_pt(120.0, 255), Some(100.0));
    assert_eq!(
        chip_load_len_pt(22.0, 255),
        None,
        "no room between the insets"
    );
    let loaded = TabStripMetadata {
        load: 200,
        ..PLAIN_TAB
    };
    assert!(!loaded.has_status());
    assert_eq!(loaded.status_count(), 0);
}

#[test]
fn automatic_attention_never_draws_a_tab_mark() {
    for attention in [
        ChipLevel::Note,
        ChipLevel::Done,
        ChipLevel::Wait,
        ChipLevel::Stop(crate::presence::StopCause::Wall),
    ] {
        let metadata = TabStripMetadata {
            attention,
            ..PLAIN_TAB
        };
        assert!(!metadata.has_status());
        assert_eq!(metadata.status_count(), 0);
        assert!(status_primitives(metadata).is_empty());
    }
    let held = TabStripMetadata {
        attention: ChipLevel::Yours,
        ..PLAIN_TAB
    };
    assert_eq!(held.status_count(), 1);
    assert!(!status_primitives(held).is_empty());
}

#[test]
fn narrow_tab_drops_close_but_selectable() {
    let segs = layout_segments(9, 3, 0, false);
    let tabs: Vec<_> = segs
        .iter()
        .filter(|s| matches!(s.kind, TabHit::Select(_)))
        .collect();
    assert!(!tabs.is_empty());
    let mut saw_narrow = false;
    for t in &tabs {
        let width = t.end_col - t.start_col;
        assert_eq!(
            t.close_col.is_some(),
            width >= MIN_SEG_WITH_CLOSE,
            "a close x exists exactly where the segment can carry one: {t:?}"
        );
        saw_narrow |= width < MIN_SEG_WITH_CLOSE;
        assert_eq!(hit_test(&segs, t.start_col), Some(t.kind));
    }
    assert!(
        saw_narrow,
        "the case is still reachable — a segment too narrow for a close x: {tabs:?}"
    );
}

#[test]
fn truncate_title_cuts_display_cells_from_either_end() {
    let head = truncate_title as fn(&str, usize) -> String;
    let tail = truncate_title_tail as fn(&str, usize) -> String;
    for (cut, title, max, expected) in [
        (head, "bash", 10, "bash"),
        (head, "abcde", 5, "abcde"),
        (head, "a-very-long-title", 5, "a-ve…"),
        (head, "anything", 1, "…"),
        (head, "anything", 0, ""),
        (head, "日本語", 6, "日本語"),
        (head, "日本語", 5, "日本…"),
        (head, "日本語", 4, "日…"),
        (head, "日本語", 2, "…"),
        (head, "a🚀bc", 5, "a🚀bc"),
        (head, "a🚀bcd", 4, "a🚀…"),
        (tail, "abcdef", 6, "abcdef"),
        (tail, "abcdef", 4, "…def"),
        (tail, "abcdef", 1, "…"),
        (tail, "abcdef", 0, ""),
        (tail, "user@m17-tower: ~/aterm", 10, "…~/aterm"),
        (tail, "Ready in aterm", 13, "…in aterm"),
        (tail, "Typing a command", 14, "…a command"),
        (tail, "verylongtokenwithoutspaces/x", 12, "…outspaces/x"),
        (tail, "日本語", 5, "…本語"),
        (tail, "日本語", 4, "…語"),
        (tail, "日本語", 2, "…"),
    ] {
        assert_eq!(cut(title, max), expected, "{title:?} in {max} cells");
    }
}

#[test]
fn a_cut_command_line_keeps_its_file() {
    let vim = "vim -n -R -i NONE -c 'syntax on' +1925 crates/aterm-gui/src/platform_win.rs";
    for (title, max, expected) in [
        (
            vim,
            60,
            "vim -n -R -i NONE -c … crates/aterm-gui/src/platform_win.rs",
        ),
        (vim, 30, "vim -n … src/platform_win.rs"),
        (vim, 21, "vim … platform_win.rs"),
        (vim, 20, "…src/platform_win.rs"),
        (vim, 16, "…platform_win.rs"),
        ("less -R +1200 CHANGELOG.md", 15, "…CHANGELOG.md"),
        ("less -R +1200 CHANGELOG.md", 20, "less … CHANGELOG.md"),
        (
            "less -R +1200 CHANGELOG.md",
            26,
            "less -R +1200 CHANGELOG.md",
        ),
        (
            r"code --wait C:\Users\m6-an\aterm-winperfect\NOTES.md",
            32,
            r"code … aterm-winperfect\NOTES.md",
        ),
        (
            r"code --wait C:\Users\m6-an\aterm-winperfect\NOTES.md",
            30,
            "code --wait … NOTES.md",
        ),
        ("tail -f /var/log/syslog/", 14, "tail … syslog/"),
    ] {
        let cut = chip_cut(title, max);
        assert_eq!(cut, expected, "{title:?} in {max} cells");
        assert!(strip_display_cells(&cut) <= max, "{cut:?} in {max} cells");
    }
    for (title, max) in [
        (vim, 15),
        (
            "Claude Code: reviewing platform_win.rs and the Default Light contrast",
            40,
        ),
        ("aterm nightly build 0.106.1241", 20),
        ("wait for it, e.g.", 10),
        ("crates/aterm-gui/src/platform_win.rs", 20),
        (r"C:\Program Files\Git\usr\share\doc\README.md", 30),
    ] {
        assert_eq!(command_cut(title, max), None, "{title:?} in {max} cells");
        assert_eq!(chip_cut(title, max), truncate_title(title, max));
    }
    assert!(
        !truncate_title(vim, 60).contains("platform_win.rs"),
        "CONTROL: the head cut loses the file"
    );
    for tabs in [1usize, 2] {
        if SOLO_TITLE_BAND && tabs == 1 {
            continue;
        }
        let metadata = vec![PLAIN_TAB; tabs];
        let segments = layout_segments_with_metadata(120, tabs, &metadata, 0, false);
        let mut titles = vec![vim.to_string()];
        titles.resize(tabs, "logs".to_string());
        let labels = distinct_chip_labels(&segments, &titles, Some(&metadata), 0, None);
        let label = labels[0].as_deref().expect("a seated chip");
        assert!(
            label.ends_with("platform_win.rs") && label.starts_with("vim"),
            "{tabs} tab(s): {label:?}"
        );
    }
}

#[test]
fn a_title_the_shell_cut_keeps_its_end_cut() {
    let sent = "vim -n -R -i NONE -c 'syntax on' +1925 crates/aterm-gui/src/plat…";
    for max in [12, 20, 30, 40, 60] {
        assert_eq!(command_cut(sent, max), None, "{max} cells");
        let cut = chip_cut(sent, max);
        assert_eq!(cut, truncate_title(sent, max), "{max} cells");
        assert!(
            cut.starts_with("vim -n") && cut.ends_with('…') && !cut.contains(" … "),
            "{cut:?} in {max} cells"
        );
    }
    assert_eq!(chip_cut(sent, 80), sent);
    let unmarked = sent.trim_end_matches('…');
    assert!(
        chip_cut(unmarked, 40).ends_with(" … crates/aterm-gui/src/plat"),
        "CONTROL: an unmarked cut reads as its file"
    );
}

#[test]
fn a_cut_path_keeps_its_directory_and_never_a_fragment_of_a_name() {
    for (subject, room, expected) in [
        ("C:\\Windows\\Temp", 12, Some("…Temp")),
        ("C:\\Windows\\Temp", 13, Some("…Windows\\Temp")),
        ("C:\\Windows\\System32\\drivers", 12, Some("…drivers")),
        (
            "C:\\Windows\\System32\\drivers",
            17,
            Some("…System32\\drivers"),
        ),
        ("C:\\Program Files\\Git", 11, Some("…Files\\Git")),
        (
            "C:\\Program Files\\Common Files\\microsoft shared",
            12,
            Some("…shared"),
        ),
        (
            "C:\\Program Files\\Common Files\\microsoft shared",
            17,
            Some("…microsoft shared"),
        ),
        (
            "~\\aterm\\crates\\aterm-cli\\src",
            14,
            Some("…aterm-cli\\src"),
        ),
        ("~\\aterm\\crates\\aterm-cli\\src", 13, Some("…cli\\src")),
        ("~\\aterm\\crates\\aterm-cli\\src", 4, Some("…src")),
        ("/home/person/work/api", 9, Some("…work/api")),
        ("C:\\Windows\\System32", 8, Some("System32")),
        ("~\\aterm\\crates\\aterm-cli\\src", 3, Some("src")),
        ("C:\\Windows\\System32", 7, None),
        ("~/a/verylongname", 8, None),
    ] {
        assert_eq!(
            path_tail(subject, room, &[]).as_deref(),
            expected,
            "{subject:?} in {room} cells"
        );
    }

    let gui = "~\\aterm\\crates\\aterm-gui\\src";
    let cli = "~\\aterm\\crates\\aterm-cli\\src";
    let sys = "C:\\Windows\\System32";
    assert_eq!(common_suffix_bytes(gui, cli), "i\\src".len(), "fixture");
    for (room, expected) in [
        (15, Some("…aterm-gui\\src")),
        (14, Some("…aterm-gui\\src")),
        (13, Some("…gui\\src")),
        (8, Some("…gui\\src")),
        (7, Some("…gui")),
        (4, Some("…gui")),
        (3, None),
    ] {
        assert_eq!(
            path_tail(gui, room, &[cli]).as_deref(),
            expected,
            "{room} cells"
        );
        if let Some(label) = path_tail(gui, room, &[cli]) {
            assert_ne!(
                Some(label),
                path_tail(cli, room, &[gui]),
                "{room} cells: the pair stays tellable apart"
            );
        }
    }
    assert_eq!(
        path_tail(gui, 7, &[cli, sys, "~\\aterm"]).as_deref(),
        Some("…gui")
    );
    assert_eq!(
        path_tail("~\\aterm\\crates", 7, &[gui]).as_deref(),
        Some("…crates")
    );
    assert!(ends_with_token("~\\x\\aterm-gui", "gui"));
    assert!(!ends_with_token("~\\x\\argui", "gui"));

    for (tab, subject, room, kin, expected) in [
        (6, sys, 7, &[][..], "System…"),
        (6, sys, 4, &[], "Sys…"),
        (6, sys, 7, &[gui, cli, "C:\\Windows\\Temp"], "System…"),
        (6, sys, 3, &[], "7"),
        (6, sys, 7, &["C:\\Windows\\SystemApps"], "7"),
        (4, gui, 3, &[cli], "5"),
        (11, sys, 1, &[], "…"),
        (11, sys, 0, &[], ""),
    ] {
        assert_eq!(
            path_fallback(tab, subject, room, kin),
            expected,
            "{subject:?} in {room} cells beside {kin:?}"
        );
    }

    let subjects = ["~\\aterm", gui, cli, sys, gui, "cargo test -p crates"];
    assert_eq!(path_chip_label(1, &subjects, 7), "…gui");
    assert_eq!(path_chip_label(2, &subjects, 7), "…cli");
    assert_eq!(path_chip_label(4, &subjects, 7), "…gui");
    assert_eq!(path_chip_label(3, &subjects, 7), "System…");
    assert_eq!(path_chip_label(1, &["~\\aterm", gui, sys], 7), "…src");
    assert_eq!(
        path_chip_label(0, &["~\\aterm\\crates", "cargo test -p crates"], 7),
        "…crates"
    );
    assert_eq!(path_ordinal_label(4, &subjects, 7), "5");
    assert_eq!(path_ordinal_label(4, &subjects, 8), "5 · …gui");
    assert_eq!(path_ordinal_label(4, &subjects, 12), "5 · …gui\\src");
    assert_eq!(path_ordinal_label(11, &subjects, 1), "…");

    assert_eq!(snap_shed("build-10", 1), 0, "`0` is inside a name");
    assert_eq!(snap_shed("x · Ready", " · Ready".len()), " · Ready".len());
    assert_eq!(
        snap_shed("same", 4),
        4,
        "a whole-title suffix is cut_core's"
    );
    for (subject, path) in [
        ("~", true),
        ("~\\aterm", true),
        ("~/aterm", true),
        ("/tmp", true),
        ("C:\\Windows", true),
        ("c:/windows", true),
        ("\\\\build-01\\share", true),
        ("~aterm", false),
        ("user@m17-tower: ~/aterm", false),
        ("cargo build", false),
        ("C:", false),
        ("", false),
    ] {
        assert_eq!(is_path_subject(subject), path, "{subject:?}");
    }
}

fn honest_path_cut(tab: usize, subject: &str, label: &str) -> bool {
    let position = (tab + 1).to_string();
    if label == position || label == "…" {
        return true;
    }
    let label = label
        .strip_prefix(format!("{position} · ").as_str())
        .unwrap_or(label);
    let name = subject.rsplit(['/', '\\']).next().unwrap_or_default();
    if label == subject || label == name {
        return true;
    }
    if let Some(text) = label.strip_prefix('…') {
        return !text.is_empty()
            && subject.match_indices(text).any(|(at, _)| {
                subject[..at].ends_with(is_token_boundary)
                    && subject[at + text.len()..]
                        .chars()
                        .next()
                        .is_none_or(|c| matches!(c, '/' | '\\'))
            });
    }
    label.strip_suffix('…').is_some_and(|head| {
        name.starts_with(head) && strip_display_cells(head) >= strip_display_cells(name).min(3)
    })
}

fn chip_avail(segments: &[TabSegment], tab: usize) -> usize {
    let seg = segments
        .iter()
        .find(|seg| seg.kind == TabHit::Select(tab))
        .expect("a seated chip has a segment");
    let layout = tab_content_layout(seg, PLAIN_TAB);
    usize::from(layout.title_end.saturating_sub(layout.title_start))
}

#[test]
fn a_windowed_strip_names_each_path_the_same_on_every_page() {
    let gui = "~\\aterm\\crates\\aterm-gui\\src";
    let cli = "~\\aterm\\crates\\aterm-cli\\src";
    let sys = "C:\\Windows\\System32";
    let titles: Vec<String> = [
        "~\\aterm",
        "~\\aterm",
        "~\\aterm",
        "~\\aterm",
        gui,
        cli,
        sys,
        "C:\\Windows\\Temp",
        "~\\aterm",
        "C:\\Program Files\\Git",
        "C:\\Windows\\System32\\drivers",
        "~\\aterm",
        "~\\aterm",
        gui,
        "~\\aterm",
    ]
    .map(String::from)
    .to_vec();
    let mut moved = titles.clone();
    moved[7] = "C:\\Users".to_string();
    let mut by_width: std::collections::HashMap<(usize, usize), String> =
        std::collections::HashMap::new();
    let mut apart = 0usize;
    for cols in 30..=240u16 {
        for active in 0..titles.len() {
            let segments = layout_segments(cols, titles.len(), active, false);
            let labels = seated(&segments, &titles, active);
            let on_page = |tab: usize| labels.iter().any(|(seat, _)| *seat == tab);
            if on_page(4) != on_page(5) {
                apart += 1;
            }
            for (tab, label) in &labels {
                let subject = titles[*tab].as_str();
                if !is_path_subject(subject) || subject == "~\\aterm" {
                    continue;
                }
                let avail = chip_avail(&segments, *tab);
                let at = format!("{cols} cols, active {active}, tab {tab} ({avail} cells)");
                assert!(
                    honest_path_cut(*tab, subject, label),
                    "{at}: {label:?} is not an honest cut of {subject:?}"
                );
                let position = format!("{} · ", tab + 1);
                let ordinal = *label == (tab + 1).to_string() || label.starts_with(&position);
                if let Some(name) = match tab {
                    4 | 13 => Some("gui"),
                    5 => Some("cli"),
                    _ => None,
                } && avail >= 4
                    && !ordinal
                {
                    assert!(
                        label.contains(name),
                        "{at}: {label:?} lost the name that tells it apart"
                    );
                }
                if !ordinal {
                    let first = by_width
                        .entry((*tab, avail))
                        .or_insert_with(|| label.clone());
                    assert_eq!(
                        label, first,
                        "{at}: tab {tab} reads differently on another page"
                    );
                }
            }
            let sys_of = |titles: &[String]| {
                seated(&segments, titles, active)
                    .into_iter()
                    .find(|(tab, _)| *tab == 6)
                    .map(|(_, label)| label)
            };
            assert_eq!(
                sys_of(&titles),
                sys_of(&moved),
                "{cols} cols, active {active}: System32's label turned on the Temp tab"
            );
        }
    }
    assert!(
        apart > 100,
        "vacuous: gui and cli shared every page ({apart})"
    );
}

fn on_band<'a>(chip_card: &'a str, padless: &'a str) -> &'a str {
    if STRIP_CHIP_CARDS { chip_card } else { padless }
}

fn seated(segments: &[TabSegment], titles: &[String], active: usize) -> Vec<(usize, String)> {
    let labels = distinct_chip_labels(segments, titles, None, active, None);
    segments
        .iter()
        .filter_map(|seg| match seg.kind {
            TabHit::Select(i) => Some((i, labels[i].clone().expect("a seated chip is labelled"))),
            _ => None,
        })
        .collect()
}

fn seated_labels(segments: &[TabSegment], titles: &[String], active: usize) -> Vec<String> {
    seated(segments, titles, active)
        .into_iter()
        .map(|(_, label)| label)
        .collect()
}

#[test]
fn deep_siblings_past_the_chrome_cap_stay_tellable_apart() {
    use crate::chrome_label::{ChromeSurface, capped_label, present};
    use aterm_grapheme::GraphemeClusters as _;
    type Case = (
        String,
        String,
        &'static str,
        &'static str,
        &'static [u16],
        [bool; 2],
    );

    let head_cut: fn(&str) -> String =
        |t| format!("{}…", t.graphemes().take(96).collect::<String>());
    let middle_cut: fn(&str) -> String = |t| capped_label(t).into_owned();
    let deep = format!("user@m17-tower: ~/{}", "ab/".repeat(40));
    let cases: [Case; 4] = [
        (
            format!("{deep}deep-one"),
            format!("{deep}deep-two"),
            "deep-one",
            "deep-two",
            &[80],
            [true, false],
        ),
        (
            "~/src/github.com/alabsystems/aterm/.claude/worktrees/lane-bec3123b-89c-10/crates/aterm-gui/src/native_config/fixtures/model".to_string(),
            "~/src/github.com/alabsystems/aterm/.claude/worktrees/lane-bec3123b-89c-25/crates/aterm-gui/src/native_config/fixtures/model".to_string(),
            "10",
            "25",
            &[80],
            [false, true],
        ),
        (
            "user@m17-tower: ~/aterm/.claude/worktrees/lane-bec3123b-89c-10/crates/aterm-gui/src/native_config/description.rs".to_string(),
            "user@m17-tower: ~/aterm/.claude/worktrees/lane-bec3123b-89c-11/crates/aterm-gui/src/native_config/description.rs".to_string(),
            "89c-10",
            "89c-11",
            &[60, 120, 240],
            [false, true],
        ),
        (
            "~/work/clients/acme-corporation/platform/monorepo/services/payments/ledger/src/main/java/com/example/alpha/internal/handlers/impl/validation/rules/default/v2".to_string(),
            "~/work/clients/acme-corporation/platform/monorepo/services/payments/ledger/src/main/java/com/example/bravo/internal/handlers/impl/validation/rules/default/v2".to_string(),
            "alpha",
            "bravo",
            &[80],
            [true, true],
        ),
    ];
    for (one, two, own, other, widths, collapse) in cases {
        let raw = [one, two];
        assert!(raw.iter().all(|t| t.graphemes().count() > 96), "{raw:?}");
        let negatives: Vec<Vec<String>> = [head_cut, middle_cut]
            .into_iter()
            .zip(collapse)
            .filter_map(|(cut, collapses)| {
                let cut: Vec<String> = raw.iter().map(|t| cut(t)).collect();
                assert_eq!(
                    cut[0] == cut[1],
                    collapses,
                    "{raw:?}: which fixed cuts collapse the pair"
                );
                collapses.then_some(cut)
            })
            .collect();
        let titles: Vec<String> = raw
            .iter()
            .map(|t| present(t, ChromeSurface::TabStrip))
            .collect();
        assert_eq!(titles, raw, "the strip receives the whole titles");
        for &cols in widths {
            let segments = layout_segments(cols, 2, 0, false);
            let labels = seated_labels(&segments, &titles, 0);
            assert_eq!(labels.len(), 2, "{cols} cols: {labels:?}");
            assert!(
                labels[0].contains(own) && !labels[0].contains(other),
                "{cols} cols: {labels:?}"
            );
            assert!(
                labels[1].contains(other) && !labels[1].contains(own),
                "{cols} cols: {labels:?}"
            );
            for cut in &negatives {
                let labels = seated_labels(&segments, cut, 0);
                assert!(
                    labels
                        .iter()
                        .all(|label| !label.contains(own) && !label.contains(other)),
                    "{cols} cols: {labels:?}"
                );
            }
        }
    }
}

#[test]
fn colliding_truncated_titles_keep_their_distinguishing_tails() {
    let titles = [
        "user@m17-tower: ~/aterm".to_string(),
        "user@m17-tower: $HOME/trust".to_string(),
        "user@m17-tower: ~".to_string(),
        "user@m17-tower: ~/aterm/crates".to_string(),
    ];
    let segments = layout_segments(80, titles.len(), 0, false);
    let labels = distinct_chip_labels(&segments, &titles, None, 0, None);
    let resolved: Vec<String> = labels.into_iter().map(Option::unwrap).collect();
    for (i, a) in resolved.iter().enumerate() {
        for (j, b) in resolved.iter().enumerate() {
            if i != j {
                assert_ne!(a, b, "tabs {i} and {j} must be tellable apart at a glance");
            }
        }
    }
    assert_eq!(resolved[0], "…~/aterm");
    assert_eq!(resolved[1], "…$HOME/trust");
    assert_eq!(resolved[2], "…~");
    assert_eq!(
        resolved[3],
        on_band("…/aterm/crates", "…~/aterm/crates"),
        "chip-card: a remainder one cell too wide falls back to the plain \
         tail cut; padless: the exact fit keeps the word-boundary cut, \
         `~` included"
    );

    let suffixed = [
        "user@m17-tower: ~/aterm · Typing a command".to_string(),
        "user@m17-tower: $HOME/trust · Typing a command".to_string(),
        "user@m17-tower: ~ · Typing a command".to_string(),
        "user@m17-tower: ~/aterm/crates · Typing a command".to_string(),
    ];
    let segments = layout_segments(80, suffixed.len(), 0, false);
    let labels = distinct_chip_labels(&segments, &suffixed, None, 0, None);
    let resolved: Vec<String> = labels.into_iter().map(Option::unwrap).collect();
    assert!(
        resolved[0].ends_with("aterm"),
        "the shared suffix is shed, the cwd tail survives: {:?}",
        resolved[0]
    );
    assert!(resolved[1].ends_with("trust"), "{:?}", resolved[1]);
    assert!(resolved[2].ends_with('~'), "{:?}", resolved[2]);
    assert!(resolved[3].ends_with("crates"), "{:?}", resolved[3]);

    let segments = layout_segments(130, suffixed.len(), 0, false);
    let labels = distinct_chip_labels(&segments, &suffixed, None, 0, None);
    let resolved: Vec<String> = labels.into_iter().map(Option::unwrap).collect();
    assert_eq!(
        resolved,
        ["…~/aterm", "…$HOME/trust", "…~", "…~/aterm/crates"],
        "every member of the shared-prompt family speaks the tail dialect"
    );

    let mixed = [
        "user@m17-tower: ~/aterm · Ready".to_string(),
        "user@m17-tower: $HOME/trust · Ready".to_string(),
        "user@m17-tower: ~/ay · Ready".to_string(),
        "user@m17-tower: /tmp · Typing a command".to_string(),
    ];
    let segments = layout_segments(80, mixed.len(), 3, false);
    let resolved: Vec<String> = distinct_chip_labels(&segments, &mixed, None, 3, None)
        .into_iter()
        .enumerate()
        .map(|(i, l)| l.unwrap_or_else(|| mixed[i].clone()))
        .collect();
    for (i, label) in resolved.iter().enumerate().take(3) {
        let painted = label.trim_start_matches('…').trim_start();
        assert!(
            !painted.is_empty() && painted != "· Ready" && painted != "Ready",
            "tab {i} paints only the shared furniture: {resolved:?}"
        );
    }
    for (i, a) in resolved.iter().enumerate() {
        for (j, b) in resolved.iter().enumerate() {
            if i != j {
                assert_ne!(a, b, "tabs {i} and {j} must differ: {resolved:?}");
            }
        }
    }

    if !STRIP_CHIP_CARDS {
        return;
    }
    let theme = Theme::default();
    let segments = layout_segments(80, titles.len(), 0, false);
    let mut row = vec![blank_cell(theme); 80];
    paint_strip(&mut row, &segments, &titles, None, 0, theme);
    let tab_text = |seg: &TabSegment| -> String {
        (seg.start_col..seg.end_col)
            .map(|c| row[c as usize].ch)
            .collect::<String>()
            .trim()
            .to_string()
    };
    let painted: Vec<String> = segments
        .iter()
        .filter(|seg| matches!(seg.kind, TabHit::Select(_)))
        .map(tab_text)
        .collect();
    assert!(
        painted[0].contains("aterm") && painted[1].contains("trust"),
        "the strip shows the tails that differ: {painted:?}"
    );
}

#[test]
fn every_strip_width_labels_a_twin_honestly() {
    let mut marks = 0usize;
    let sweep: [(usize, Vec<u16>); 5] = [
        (10, (1..=240).collect()),
        (11, (1..=240).collect()),
        (17, (1..=240).collect()),
        (20, (1..=240).collect()),
        (120, (240..=1200).step_by(53).collect()),
    ];
    for (tabs, widths) in sweep {
        let titles: Vec<String> = (0..tabs)
            .map(|_| "user@m17-tower: ~/aterm".to_string())
            .collect();
        for cols in widths {
            for active in [0usize, tabs / 2] {
                let segments = layout_segments(cols, tabs, active, false);
                let labels = distinct_chip_labels(&segments, &titles, None, active, None);
                for seg in &segments {
                    let TabHit::Select(i) = seg.kind else {
                        continue;
                    };
                    if i == active {
                        continue;
                    }
                    let layout = tab_content_layout(seg, PLAIN_TAB);
                    let avail = usize::from(layout.title_end.saturating_sub(layout.title_start));
                    if strip_display_cells(&titles[i]) <= avail {
                        continue;
                    }
                    let label = labels[i].as_deref().unwrap_or("");
                    let digits = (i + 1).to_string();
                    let where_ = format!("{tabs} tabs, {cols} cols, tab {i}, avail {avail}");
                    if label.is_empty() {
                        assert_eq!(avail, 0, "{where_}: a window with room says something");
                        continue;
                    }
                    if label == "…" {
                        marks += 1;
                        assert!(
                            strip_display_cells(&digits) > avail,
                            "{where_}: the mark is only for a window that \
                             seats no number — this one seats {digits}"
                        );
                        continue;
                    }
                    assert!(
                        label == digits || label.starts_with(&format!("{digits} · ")),
                        "{where_}: {label:?} is neither this tab's position \
                         nor an honest mark"
                    );
                    assert!(
                        strip_display_cells(label) <= avail,
                        "{where_}: {label:?} overflows its window"
                    );
                }
            }
        }
    }
    assert!(
        marks > 0,
        "the mark's window is reachable — if it ever stops being, this \
         test is the place that says so"
    );
}

/// The ordinal's tail follows the survivor rule ([`furniture_survivor_recut`]).
#[cfg(not(target_os = "macos"))]
#[test]
fn an_ordinal_tail_is_never_the_shared_furniture() {
    for (head, last_state) in [("build: ", "Typing a command"), ("work/", "Ready")] {
        let titles = [
            format!("{head}aterm · Ready"),
            format!("{head}aterm · Ready"),
            format!("{head}trust · Ready"),
            format!("{head}other · {last_state}"),
        ];
        let segments = layout_segments(80, titles.len(), 0, false);
        let resolved: Vec<String> = distinct_chip_labels(&segments, &titles, None, 0, None)
            .into_iter()
            .map(Option::unwrap)
            .collect();
        let twin = &resolved[1];
        assert!(
            twin.starts_with("2 · "),
            "{head:?}: the non-active twin is labelled by its position: {twin:?}"
        );
        assert!(
            !twin.contains("Ready"),
            "{head:?}: the tail is this tab's own text, not the state every chip \
             on the strip shares: {twin:?}"
        );
        assert!(
            twin.contains("aterm"),
            "{head:?}: and what it spends the tail on is the name that tells it \
             apart: {twin:?}"
        );
        assert!(
            resolved[0].contains("aterm"),
            "{head:?}: the active twin keeps that name too, never the furniture \
             alone: {:?}",
            resolved[0]
        );
        let seg = segments
            .iter()
            .find(|seg| matches!(seg.kind, TabHit::Select(1)))
            .expect("the twin has a chip");
        let layout = tab_content_layout(seg, PLAIN_TAB);
        let avail = usize::from(layout.title_end.saturating_sub(layout.title_start));
        let naive = truncate_title_tail(&titles[1], avail.saturating_sub(4));
        assert!(
            naive.contains("Ready") && !naive.contains("aterm"),
            "{head:?} fixture: the naive tail is pure furniture: {naive:?}"
        );
    }
}

/// A tail one chip outside a cluster happens to share is not furniture.
#[cfg(not(target_os = "macos"))]
#[test]
fn a_tail_shared_outside_the_cluster_still_names_its_members() {
    let strips: [([&str; 4], [Option<&str>; 4]); 4] = [
        (
            [
                "building: aterm",
                "building: trust",
                "checking: aterm",
                "checking: trust",
            ],
            [Some("aterm"), Some("trust"), Some("aterm"), Some("trust")],
        ),
        (
            [
                "user@gb10: ~/aterm",
                "user@gb10: $HOME/trust",
                "root@gb10: ~/aterm",
                "zsh",
            ],
            [Some("aterm"), Some("trust"), None, None],
        ),
        (
            [
                "building: aterm",
                "building: trust",
                "nvim ~/src/aterm",
                "zsh",
            ],
            [Some("aterm"), Some("trust"), None, None],
        ),
        (
            [
                "building: aterm",
                "building: aterm",
                "nvim ~/src/aterm",
                "zsh",
            ],
            [Some("aterm"), Some("aterm"), None, None],
        ),
    ];
    for (raw, named) in strips {
        let titles = raw.map(str::to_string);
        let segments = layout_segments(80, titles.len(), 0, false);
        let resolved: Vec<String> = distinct_chip_labels(&segments, &titles, None, 0, None)
            .into_iter()
            .map(Option::unwrap)
            .collect();
        for (tab, name) in named.iter().enumerate() {
            let Some(name) = name else { continue };
            assert!(
                resolved[tab].contains(name),
                "{raw:?}: tab {tab} keeps {name:?}, the name that tells it apart: \
                 {resolved:?}"
            );
            assert!(
                titles[tab] != resolved[tab],
                "{raw:?} fixture: tab {tab} is cut: {resolved:?}"
            );
            for (other, other_name) in named.iter().enumerate() {
                if other_name.is_some_and(|n| n != *name) {
                    assert_ne!(
                        resolved[tab], resolved[other],
                        "{raw:?}: tabs {tab} and {other} are told apart"
                    );
                }
            }
        }
    }
}

#[test]
fn a_shed_ending_is_whole_words() {
    let strips: [(&[&str], u16); 2] = [
        (
            &[
                "building: aterm",
                "building: aterm",
                "checking: aterm",
                "linting: aterm",
                "zsh",
            ],
            80,
        ),
        (
            &[
                "checking: other · Ready",
                "checking: other · Ready",
                "checking: other · Ready",
                "linting: other · Ready",
                "linting: other · Ready",
                "make",
                "zsh",
            ],
            150,
        ),
    ];
    for (raw, cols) in strips {
        let titles: Vec<String> = raw.iter().map(|t| t.to_string()).collect();
        let segments = layout_segments(cols, titles.len(), 0, false);
        let resolved = distinct_chip_labels(&segments, &titles, None, 0, None);
        for (tab, label) in resolved.iter().enumerate() {
            let Some(label) = label else { continue };
            if titles.iter().filter(|t| **t == titles[tab]).count() < 2 {
                continue;
            }
            let core = label
                .trim_start_matches(|c: char| c.is_ascii_digit())
                .trim_start_matches(" · ")
                .trim_start_matches('…');
            if core.is_empty() || label.ends_with('…') {
                continue;
            }
            let Some(at) = titles[tab].find(core) else {
                continue;
            };
            let next = titles[tab][at + core.len()..].chars().next();
            assert!(
                !(core.ends_with(char::is_alphanumeric) && next.is_some_and(char::is_alphanumeric)),
                "{raw:?} at {cols} columns: tab {tab} paints {label:?}, a word of {:?} \
                 cut off with no mark: {resolved:?}",
                titles[tab]
            );
        }
    }
}

fn is_twin_context(subject: &str, painted: &str) -> bool {
    if painted == subject {
        return true;
    }
    if subject
        .strip_suffix(painted)
        .is_some_and(|before| !painted.is_empty() && before.ends_with(['/', '\\']))
    {
        return true;
    }
    let leaf = subject.rsplit(['/', '\\']).next().unwrap_or_default();
    if painted.strip_suffix('…').is_some_and(|head| {
        leaf.starts_with(head) && strip_display_cells(head) >= strip_display_cells(leaf).min(3)
    }) {
        return true;
    }
    if let Some(tail) = painted.strip_prefix('…') {
        return !tail.is_empty()
            && subject.strip_suffix(tail).is_some_and(|before| {
                tail.starts_with(['/', '\\']) || before.ends_with(['/', '\\'])
            });
    }
    painted
        .strip_suffix('…')
        .is_some_and(|head| !head.is_empty() && subject.starts_with(head))
}

#[test]
fn twins_keep_whole_components_or_the_head_at_every_width() {
    let subject = "~\\aterm";
    let title = subject;
    let furniture = shared_furniture(title, &[title, title], 1);
    let expected: [(usize, &str, &str); 12] = [
        (0, "", ""),
        (1, "2", "…"),
        (2, "2", "~…"),
        (3, "2", "~\\…"),
        (4, "2", "~\\a…"),
        (5, "2", "~\\at…"),
        (6, "2 · ~…", "…aterm"),
        (7, "2 · ~\\…", "~\\aterm"),
        (8, "2 · ~\\a…", "~\\aterm"),
        (9, "2 · ~\\at…", "~\\aterm"),
        (10, "2 · …aterm", "~\\aterm"),
        (11, "2 · ~\\aterm", "~\\aterm"),
    ];
    for (avail, ordinal, active) in expected {
        assert_eq!(
            ordinal_chip_label(1, title, subject, "", avail, furniture, twin_tail),
            ordinal,
            "window {avail}: the non-active twin"
        );
        assert_eq!(
            twin_tail(subject, "", avail),
            active,
            "window {avail}: the active twin"
        );
    }
    assert_eq!(family_tail(subject, "", 5), "…term");
    assert_eq!(family_tail(subject, "", 3), "…rm");
    assert_eq!(twin_tail("~\\work\\api", "", 10), "~\\work\\api");
    assert_eq!(twin_tail("~\\work\\api", "", 9), "…work\\api");
    assert_eq!(twin_tail("~\\work\\api", "", 8), "…api");
    assert_eq!(twin_tail("~\\work\\api", "", 5), "…\\api");
    assert_eq!(twin_tail("~/work/api", "", 4), "…api");
    assert_eq!(twin_tail("~/work/api", "", 3), "~/…");
    assert_eq!(twin_tail("Settings.toml", "", 6), "….toml");
    assert_eq!(twin_tail("same shell", "shell", 4), "…ell");
    let core = "user@m17-tower: ~/aterm";
    assert_eq!(twin_tail(core, "~/aterm", 10), "…~/aterm");
    assert_eq!(twin_tail(core, "~/aterm", 8), "…~/aterm");
    assert_eq!(twin_tail(core, "~/aterm", 7), "…/aterm");
    assert_eq!(twin_tail(core, "~/aterm", 6), "…aterm");
    assert_eq!(twin_tail(core, "~/aterm", 5), "ayat…");

    let mut cut = 0usize;
    let titles: Vec<String> = (0..3).map(|_| subject.to_string()).collect();
    for cols in 12..=120u16 {
        for active in 0..3 {
            let segments = layout_segments(cols, 3, active, false);
            let labels = distinct_chip_labels(&segments, &titles, None, active, None);
            for seg in &segments {
                let TabHit::Select(i) = seg.kind else {
                    continue;
                };
                let Some(label) = labels[i].as_deref() else {
                    continue;
                };
                if label == subject {
                    // Distinct branch.
                    continue;
                }
                cut += 1;
                let digits = (i + 1).to_string();
                let painted = label.strip_prefix(&format!("{digits} · ")).unwrap_or(label);
                assert!(
                    painted.is_empty()
                        || painted == "…"
                        || painted == digits
                        || is_twin_context(subject, painted),
                    "{cols} cols, active {active}, tab {i}: {label:?} is not context \
                     for {subject:?}, a number, or the mark"
                );
            }
        }
    }
    assert!(cut > 50, "vacuous: only {cut} chips were cut");
}

#[test]
fn roomy_twins_name_their_directory_whole() {
    let dir = r"C:\Users\me\AppData\Local\Temp\wp-shells\work\sub dir".to_string();
    let titles = vec![dir.clone(), dir];
    let segments = layout_segments(100, 2, 0, false);
    let seats = seated(&segments, &titles, 0);
    assert!(seats[0].1.ends_with(r"work\sub dir"), "{seats:?}");
    assert!(
        seats[1].1.starts_with("2 · …") && seats[1].1.ends_with(r"work\sub dir"),
        "the quiet twin names the directory: {seats:?}"
    );

    let prompt = "me@box MINGW64 /tmp/wp-shells/work/ünï 日本".to_string();
    let titles = vec![prompt.clone(), prompt];
    let segments = layout_segments(48, 2, 1, false);
    let seats = seated(&segments, &titles, 1);
    let active = &seats[1].1;
    assert!(
        active.ends_with("/ünï 日本") && active.len() > "…/ünï 日本".len(),
        "the active twin keeps whole components: {seats:?}"
    );
}

#[test]
fn twin_context_stops_at_a_mixed_family_and_a_head_flips_no_loner() {
    let strip = |dir: &str| -> Vec<String> {
        vec![
            dir.to_string(),
            dir.to_string(),
            "cargo build --release".to_string(),
            "README.md".to_string(),
            "Setup.sh".to_string(),
        ]
    };
    let titles = strip("~/wave/workspace");
    let segments = layout_segments(48, titles.len(), 0, false);
    let seats = seated(&segments, &titles, 0);
    assert_eq!(
        seats.iter().map(|(tab, _)| *tab).collect::<Vec<_>>(),
        [0, 1, 2],
        "the fixture's geometry: twin, twin, loner on one page"
    );
    assert!(
        seats[1].1.starts_with("2 · ~/") && seats[1].1.ends_with('…'),
        "the quiet twin keeps its head: {seats:?}"
    );
    assert!(
        seats[2].1.starts_with("cargo"),
        "the loner keeps the head dialect the family speaks: {seats:?}"
    );
    let titles = strip("~/wave/services/api");
    let segments = layout_segments(48, titles.len(), 0, false);
    let seats = seated(&segments, &titles, 0);
    assert!(
        seats[1].1.starts_with("2 · …") && seats[1].1.ends_with("api"),
        "the quiet twin names the directory by its whole component: {seats:?}"
    );
    assert!(
        seats[2].1.starts_with('…') && seats[2].1.ends_with("release"),
        "the loner joins the tail dialect: {seats:?}"
    );

    let titles = [
        "user@m17-tower: ~/src/aterm-gui".to_string(),
        "user@m17-tower: ~/src/aterm-gui".to_string(),
        "user@m17-tower: ~/src/aterm-cli".to_string(),
    ];
    let mut mixed = 0usize;
    for cols in 40..=160u16 {
        let segments = layout_segments(cols, titles.len(), 0, false);
        let seats = seated(&segments, &titles, 0);
        if seats.iter().map(|(tab, _)| *tab).collect::<Vec<_>>() != [0, 1, 2]
            || !seats[2].1.contains('…')
            || !seats[1].1.starts_with("2 · ")
        {
            continue;
        }
        mixed += 1;
        let twin = &seats[1].1["2 · ".len()..];
        assert!(
            twin.starts_with('…') && twin != seats[2].1,
            "{cols} cols: a twin beside a sibling that differs keeps the \
             ending that tells them apart: {seats:?}"
        );
    }
    assert!(mixed > 0, "vacuous: no width seated the mixed family cut");
}

#[test]
fn an_out_of_range_selection_resolves_like_the_clamped_one() {
    let titles: Vec<String> = (0..4)
        .map(|_| "user@m17-tower: ~/aterm".to_string())
        .collect();
    for cols in [40u16, 80, 130] {
        let segments = layout_segments(cols, titles.len(), 99, false);
        let clamped = layout_segments(cols, titles.len(), titles.len() - 1, false);
        assert_eq!(segments, clamped, "fixture: the LAYOUT already clamps");
        let out_of_range = distinct_chip_labels(&segments, &titles, None, 99, None);
        let in_range = distinct_chip_labels(&segments, &titles, None, titles.len() - 1, None);
        assert_eq!(
            out_of_range, in_range,
            "{cols} cols: the labels follow the same clamp the layout did"
        );
        assert!(
            out_of_range[titles.len() - 1]
                .as_deref()
                .is_some_and(|label| !label.starts_with('4')),
            "the chip the layout widened keeps its TITLE: {out_of_range:?}"
        );
    }
}

fn one_dialect_titles() -> [&'static str; 10] {
    [
        "Settings.toml",
        "Settings.toml",
        "Settings.toml",
        "README.md",
        "Settings.toml",
        "cargo build",
        "Settings.toml",
        "Setup.sh",
        "Settings.toml",
        "Settings.toml.bak",
    ]
}

#[test]
fn a_pressure_strip_speaks_one_truncation_dialect() {
    let titles: Vec<String> = one_dialect_titles().iter().map(|t| t.to_string()).collect();
    let segments = layout_segments(70, titles.len(), 9, false);
    let resolved = seated_labels(&segments, &titles, 9);
    assert_eq!(
        resolved,
        [
            "…build",
            on_band("7 · …oml", "7 · …toml"),
            "Setup.sh",
            on_band("9 · …oml", "9 · …toml"),
            "….bak"
        ],
        "one strip, one dialect — and the twins are addressable"
    );

    let segments: Vec<TabSegment> = std::iter::once(TabSegment {
        start_col: 0,
        end_col: 18,
        close_col: Some(16),
        kind: TabHit::Select(0),
        solo: false,
    })
    .chain((1..10u16).map(|i| {
        let start = 18 + (i - 1) * 6;
        TabSegment {
            start_col: start,
            end_col: start + 6,
            close_col: Some(start + 4),
            kind: TabHit::Select(usize::from(i)),
            solo: false,
        }
    }))
    .collect();
    let resolved = seated_labels(&segments, &titles, 0);
    assert_eq!(
        resolved,
        [
            "Settings.toml",
            "2",
            "3",
            "R…",
            "5",
            "c…",
            "7",
            "…h",
            "9",
            "…k"
        ],
        "distinctness outranks dialect when the tail cut cannot distinguish"
    );
    for (i, a) in resolved.iter().enumerate() {
        for (j, b) in resolved.iter().enumerate() {
            if i != j {
                assert_ne!(a, b, "tabs {i} and {j} collide at two visible cells");
            }
        }
    }

    let roomy = [
        "user@m17-tower: ~/aterm".to_string(),
        "user@m17-tower: $HOME/trust".to_string(),
        "user@m17-tower: ~".to_string(),
        "user@m17-tower: ~/aterm/crates".to_string(),
        "zzz-not-a-prompt-here".to_string(),
    ];
    let segments = layout_segments(80, roomy.len(), 0, false);
    let resolved: Vec<String> = distinct_chip_labels(&segments, &roomy, None, 0, None)
        .into_iter()
        .map(Option::unwrap)
        .collect();
    assert!(
        resolved[0].starts_with('…') && resolved[1].starts_with('…'),
        "the family still flips together: {resolved:?}"
    );
    assert_eq!(
        resolved[4],
        on_band("zzz-not-a…", "zzz-not-a-…"),
        "a roomy strip's distinct-headed loner keeps the shipped head cut"
    );
}

#[test]
fn paint_active_inactive_and_title() {
    let theme = Theme::default();
    let cols = 40usize;
    let mut row = vec![strip_cell(' ', &strip_colors(theme), StripRole::Inactive); cols];
    let segs = layout_segments(cols as u16, 2, 0, false);
    let titles = vec!["zsh".to_string(), "vim".to_string()];
    paint_strip(&mut row, &segs, &titles, None, 0, theme);
    let fg_rgb = [
        ((theme.fg >> 16) & 0xff) as u8,
        ((theme.fg >> 8) & 0xff) as u8,
        (theme.fg & 0xff) as u8,
    ];
    let bg_rgb = [
        ((theme.bg >> 16) & 0xff) as u8,
        ((theme.bg >> 8) & 0xff) as u8,
        (theme.bg & 0xff) as u8,
    ];
    let t0 = &segs[0];
    let card0 = t0.start_col as usize + usize::from(STRIP_CHIP_CARDS);
    if SELECTED_TAB_IS_GROUND {
        assert_eq!(row[card0].bg, bg_rgb, "the selected tab is the ground");
        assert_ne!(
            row[card0].bg,
            strip_colors(theme).band_bg,
            "apart from the band"
        );
    } else {
        assert_ne!(
            row[card0].bg, bg_rgb,
            "active tab bg is raised above the body"
        );
    }
    assert_eq!(
        row[card0].fg, fg_rgb,
        "active tab fg = full-strength theme fg"
    );
    if STRIP_CHIP_CARDS {
        assert_eq!(
            row[card0].underline,
            UnderlineStyle::None,
            "a chip-card selection is a filled card, not an underline"
        );
    } else {
        assert_eq!(
            row[card0].underline,
            UnderlineStyle::Single,
            "active tab carries the underline accent"
        );
        assert_eq!(
            row[card0].underline_color,
            Some([
                ((theme.cursor >> 16) & 0xff) as u8,
                ((theme.cursor >> 8) & 0xff) as u8,
                (theme.cursor & 0xff) as u8,
            ]),
            "selected underline follows the theme accent"
        );
    }
    assert!(row[card0].bold, "active identity is explicit");
    let t1 = &segs[1];
    let card1 = t1.start_col as usize + usize::from(STRIP_CHIP_CARDS);
    let colors = strip_colors(theme);
    if STRIP_CHIP_CARDS {
        assert_eq!(
            row[t1.start_col as usize].bg, colors.band_bg,
            "the gutter column between cards is bare band"
        );
        assert_eq!(
            row[card1].bg, colors.chip_bg,
            "inactive tab bg = its own quiet chip card"
        );
    } else {
        assert_eq!(
            row[card1].bg, colors.band_bg,
            "inactive tab bg = the band (recedes)"
        );
    }
    assert_ne!(row[card1].bg, row[card0].bg, "inactive differs from active");
    assert!(!row[card1].bold, "inactive tab text is not bold");
}

#[test]
fn wide_title_chars_paint_as_lead_plus_continuation() {
    let theme = Theme::default();
    for title in ["🚀 build", "日本語シェル", "✅ done"] {
        let titles = [title.to_string(), "plain".to_string()];
        let segs = layout_segments(80, 2, 0, false);
        let mut row = vec![blank_cell(theme); 80];
        paint_strip(&mut row, &segs, &titles, None, 0, theme);
        assert!(
            row.iter().all(|cell| cell.ch != '·'),
            "{title:?}: the middle-dot placeholder is gone"
        );
        let t0 = segs[0];
        let layout = tab_content_layout(
            &t0,
            TabStripMetadata::from_presentation(&crate::tab_model::TabPresentation::terminal(
                title,
            )),
        );
        let mut col = layout.title_start;
        for ch in title.chars() {
            let w = strip_char_cells(ch);
            if col + w > layout.title_end {
                break;
            }
            assert_eq!(
                row[col as usize].ch, ch,
                "{title:?}: the char itself is painted at its display column"
            );
            assert!(!row[col as usize].wide, "{title:?}: the lead is a lead");
            if w == 2 {
                let tail = row[col as usize + 1];
                assert!(tail.wide, "{title:?}: width-2 char carries a continuation");
                assert_eq!(tail.ch, ' ', "{title:?}: the continuation has no glyph");
            }
            col += w;
        }
        assert!(col <= layout.title_end);

        let metadata = [TabStripMetadata::from_presentation(
            &crate::tab_model::TabPresentation::terminal(title),
        )];
        let segments = forced_solo_segments(40, &metadata);
        let mut row = vec![blank_cell(theme); 40];
        paint_strip_with_metadata(
            &mut row,
            &segments,
            &[title.to_string()],
            &metadata,
            StripPaint::default(),
            0,
            theme,
            None,
        );
        let first = row
            .iter()
            .position(|cell| cell.ch != ' ')
            .expect("the solo title is drawn");
        let lead = title.chars().next().unwrap();
        assert_eq!(row[first].ch, lead, "{title:?} on the solo band");
        assert!(
            row[first + 1].wide,
            "{title:?}: solo lead + continuation, not a narrow cell"
        );
    }
}

#[test]
fn a_wide_char_never_straddles_the_title_boundary() {
    let theme = Theme::default();
    let segs = layout_segments(24, 2, 0, false);
    let titles = ["日本語シェルの長い名前".to_string(), "第二".to_string()];
    let mut row = vec![blank_cell(theme); 24];
    paint_strip(&mut row, &segs, &titles, None, 0, theme);
    for seg in segs.iter().filter(|s| matches!(s.kind, TabHit::Select(_))) {
        let boundary = match seg.close_col {
            Some(cx) => cx.saturating_sub(1),
            None => seg.end_col.saturating_sub(1),
        };
        assert!(
            !row[boundary as usize].wide || boundary == 0,
            "a continuation leaked onto the pad at {boundary}"
        );
    }
    for col in 1..row.len() {
        if row[col].wide {
            assert_ne!(
                row[col - 1].ch,
                ' ',
                "continuation at {col} has no lead glyph"
            );
        }
    }
}

fn realistic_strips() -> [(&'static str, Vec<String>); 3] {
    let same: Vec<String> = (0..8)
        .map(|_| "claude --resume release-notes".to_string())
        .collect();
    let distinct: Vec<String> = [
        "vim src/main.rs",
        "cargo test",
        "~/aterm",
        "ssh build-01",
        "psql aterm_dev",
        "docker compose",
        "git log",
        "claude",
    ]
    .iter()
    .map(|title| (*title).to_string())
    .collect();
    let mixed: Vec<String> = [
        "claude --resume release-notes",
        "claude --resume release-notes",
        "vim TODO",
        "~/src/aterm/crates/aterm-gui",
        "~/src/aterm/crates/aterm-gui",
        "cargo test",
        "git log",
        "claude --resume release-notes",
    ]
    .iter()
    .map(|title| (*title).to_string())
    .collect();
    [("same", same), ("distinct", distinct), ("mixed", mixed)]
}

fn painted_text(label: &str) -> &str {
    label
        .trim_start_matches(|c: char| c.is_ascii_digit())
        .trim_start_matches(' ')
        .trim_start_matches('\u{00b7}')
        .trim_start_matches(' ')
        .trim_matches('…')
}

fn says_its_own(label: &str, title: &str) -> bool {
    let painted = painted_text(label);
    if let Some((head, tail)) = painted.rsplit_once(MIDDLE_MARK) {
        return title.starts_with(head) && title.ends_with(tail);
    }
    !painted.is_empty() && title.contains(painted)
}

#[test]
fn the_windowed_strip_degrades_sanely_at_every_width_and_tab_count() {
    for tabs in 1..=20usize {
        for cols in (40..=300u16).step_by(1) {
            for active in [0, tabs / 2, tabs - 1] {
                for update in [false, true] {
                    let segs = layout_segments(cols, tabs, active, update);
                    let chips: Vec<&TabSegment> = segs
                        .iter()
                        .filter(|seg| matches!(seg.kind, TabHit::Select(_)))
                        .collect();
                    let where_ =
                        format!("{tabs} tabs, {cols} cols, active {active}, update {update}");
                    for pair in segs.windows(2) {
                        assert!(
                            pair[0].end_col <= pair[1].start_col,
                            "{where_}: segments overlap: {segs:?}"
                        );
                    }
                    for seg in &segs {
                        assert!(seg.start_col < seg.end_col, "{where_}: empty segment");
                        assert!(seg.end_col <= cols, "{where_}: segment past the strip");
                    }
                    if !chips.is_empty() {
                        assert!(
                            chips.iter().any(|seg| seg.kind == TabHit::Select(active)),
                            "{where_}: the selected tab is not on the strip: {chips:?}"
                        );
                    }
                    let seated: Vec<usize> = chips
                        .iter()
                        .filter_map(|seg| match seg.kind {
                            TabHit::Select(i) => Some(i),
                            _ => None,
                        })
                        .collect();
                    for pair in seated.windows(2) {
                        assert_eq!(
                            pair[1],
                            pair[0] + 1,
                            "{where_}: the window has a hole: {seated:?}"
                        );
                    }
                    if seated.len() < tabs && seated.len() > STRIP_MIN_SEATS {
                        for seg in &chips {
                            assert!(
                                seg.end_col - seg.start_col >= STRIP_SEATS_FLOOR,
                                "{where_}: a windowed strip seated a chip below \
                                 the floor: {chips:?}"
                            );
                        }
                    }
                    let marks = strip_overflow(&segs, tabs);
                    for (col, _) in marks.columns() {
                        assert!(col < cols, "{where_}: mark off the strip");
                        assert_eq!(
                            hit_test(&segs, col),
                            None,
                            "{where_}: the mark at {col} sits on a hit target"
                        );
                    }
                    if let Some(&first) = seated.first() {
                        assert_eq!(
                            marks.left.is_some(),
                            first > 0 && chips[0].start_col > 0,
                            "{where_}: left mark disagrees with the window"
                        );
                    }
                    if let Some(&last) = seated.last() {
                        let end = chips[chips.len() - 1].end_col;
                        let free = !segs
                            .iter()
                            .any(|seg| end >= seg.start_col && end < seg.end_col);
                        assert_eq!(
                            marks.right.is_some(),
                            last + 1 < tabs && free,
                            "{where_}: right mark disagrees with the window"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn the_selected_chip_names_its_tab_at_every_width() {
    for (set, titles) in realistic_strips() {
        for cols in (40..=300u16).step_by(1) {
            for active in [0, titles.len() / 2, titles.len() - 1] {
                let segs = layout_segments(cols, titles.len(), active, false);
                let labels = distinct_chip_labels(&segs, &titles, None, active, None);
                let Some(label) = labels[active].as_deref() else {
                    assert!(
                        !segs.iter().any(|s| matches!(s.kind, TabHit::Select(_))),
                        "{set} at {cols} cols: the selection lost its chip"
                    );
                    continue;
                };
                let where_ = format!("{set} at {cols} cols, active {active}");
                assert!(!label.is_empty(), "{where_}: empty label");
                assert_ne!(label, "…", "{where_}: the selection names nothing");
                assert!(
                    says_its_own(label, &titles[active]),
                    "{where_}: the selection keeps nothing of its own title: {label:?}"
                );
            }
        }
    }
}

#[test]
fn the_overflow_mark_paints_on_bare_band_and_only_there() {
    let titles: Vec<String> = (0..10)
        .map(|i| format!("job-{i} · Ready in aterm"))
        .collect();
    let theme = Theme::default();
    for cols in [60u16, 80, 100] {
        for active in [0, 4, 9] {
            let segs = layout_segments(cols, titles.len(), active, false);
            let marks = strip_overflow(&segs, titles.len());
            let mut row = vec![blank_cell(theme); usize::from(cols)];
            paint_strip(&mut row, &segs, &titles, None, active, theme);
            let where_ = format!("{cols} cols, active {active}");
            assert!(
                marks.left.is_some() || marks.right.is_some(),
                "{where_}: ten tabs cannot fit, so an edge must be marked"
            );
            for (col, glyph) in marks.columns() {
                assert_eq!(
                    row[usize::from(col)].ch,
                    glyph,
                    "{where_}: no {glyph:?} painted at {col}"
                );
                assert_eq!(hit_test(&segs, col), None, "{where_}: {col} is a target");
            }
            for (col, cell) in row.iter().enumerate() {
                if cell.ch == STRIP_OVERFLOW_LEFT || cell.ch == STRIP_OVERFLOW_RIGHT {
                    assert!(
                        marks.columns().any(|(mark, _)| usize::from(mark) == col),
                        "{where_}: a stray edge mark at column {col}"
                    );
                }
            }
        }
    }
}

#[test]
fn extreme_tab_counts_stay_inside_the_band() {
    assert!(layout_segments(0, 3, 0, false).is_empty());
    for (cols, count, active) in [(130u16, 150usize, 0usize), (130, 150, 149), (12, 40, 39)] {
        let segs = layout_segments(cols, count, active, false);
        for seg in &segs {
            assert!(seg.end_col <= cols, "{cols} cols / {count} tabs");
            assert!(seg.start_col < seg.end_col);
        }
        for pair in segs.windows(2) {
            assert!(pair[0].end_col <= pair[1].start_col);
        }
    }
}

/// `active_tab_color` recolours only the selected tab, never the `+`.
#[cfg(not(target_os = "macos"))]
#[test]
fn the_new_tab_button_ignores_the_active_tab_override() {
    let theme = Theme::default();
    let colors = strip_colors(theme);
    let plus = strip_cell('+', &colors, StripRole::NewTab);
    let picked = [200, 30, 90];
    let overridden = strip_colors_with_active(theme, Some(picked));
    assert_eq!(overridden.active_bg, picked, "the chosen tab colour lands");
    assert_eq!(
        strip_cell('+', &overridden, StripRole::NewTab).bg,
        plus.bg,
        "and stops at the tab: the `+` keeps the theme's own surface"
    );
}

#[test]
fn strip_char_sanitizes() {
    assert_eq!(strip_char('a'), 'a');
    assert_eq!(strip_char('\t'), ' ');
    assert_eq!(strip_char('世'), '世');
    assert_eq!(strip_char_cells('世'), 2);
    assert_eq!(strip_char('\u{1F680}'), '\u{1F680}');
    assert_eq!(strip_char_cells('\u{1F680}'), 2);
    assert_eq!(strip_char('\u{2705}'), '\u{2705}');
    assert_eq!(strip_char_cells('\u{2705}'), 2);
    assert_eq!(strip_char('\u{F0154}'), '\u{F0154}');
    assert_eq!(strip_char_cells('\u{F0154}'), 1);
    assert_eq!(strip_char_cells('\u{FE0F}'), 1);
    assert_eq!(
        strip_display_cells("日本語"),
        6,
        "display width, not char count"
    );
}

#[test]
fn strip_inks_clear_the_legibility_floor() {
    use aterm_types::Rgb;
    let rgb = |c: [u8; 3]| Rgb::new(c[0], c[1], c[2]);
    let rgb_of = crate::settings::u32_rgb;
    assert_eq!(
        crate::chrome_band::forced_chrome(),
        None,
        "the theme-derived strip is only defined with no OS-forced palette"
    );
    for name in aterm_types::scheme::builtin_names() {
        let s = aterm_types::scheme::builtin(name).expect("builtin exists");
        let terminal = Theme::from(&s);
        for (side, theme) in [
            ("terminal", terminal),
            (
                "forced light",
                crate::native_appearance::forced_chrome_theme(terminal, false),
            ),
            (
                "forced dark",
                crate::native_appearance::forced_chrome_theme(terminal, true),
            ),
        ] {
            let at = format!("{name} ({side})");
            let raw = side == "terminal";
            if !raw {
                assert_eq!(
                    theme_is_dark(theme.bg),
                    side == "forced dark",
                    "{at}: the forced side classifies as the side it forces"
                );
            }
            let c = strip_colors(theme);
            let floor = |what: &str, ink: [u8; 3], surface: [u8; 3]| {
                let ratio = rgb(ink).contrast(rgb(surface));
                assert!(ratio >= 3.0, "{at}: {what} contrast {ratio:.2} < 3.0:1");
            };
            floor("active-tab text", c.active_fg, c.active_bg);
            floor("inactive-tab label", c.inactive_fg, c.band_bg);
            if STRIP_IS_CHROME_BAND {
                floor("hovered-tab label", c.hover_fg, c.hover_bg);
            }
            if STRIP_CHIP_CARDS {
                floor("quiet-chip label", c.chip_fg, c.chip_bg);
                floor("'+' affordance", c.chip_button_fg, c.chip_bg);
            }
            assert_ne!(c.active_bg, c.band_bg, "{at}: active card is the body");
            let inactive = rgb(c.inactive_fg).contrast(rgb(c.band_bg));
            let full = rgb(c.fg).contrast(rgb(c.band_bg));
            assert!(
                inactive <= full + 1e-9,
                "{at}: the dim ({inactive:.2}:1) was floored past what it dims \
                 ({full:.2}:1)"
            );
            if !raw {
                continue;
            }
            floor("update rule on its card", c.update_rule, c.update_bg);
            let hostile = rgb_of(theme.bg);
            let authored = strip_colors(ChromePalette {
                authored: aterm_messages::tokens::AuthoredChrome {
                    accent: Some([0xff, 0x00, 0xaa]),
                    border: Some(hostile),
                    ..aterm_messages::tokens::AuthoredChrome::NONE
                },
                ..ChromePalette::from(theme)
            });
            let at_bg = strip_colors(ChromePalette {
                authored: aterm_messages::tokens::AuthoredChrome {
                    accent: Some(hostile),
                    ..aterm_messages::tokens::AuthoredChrome::NONE
                },
                ..ChromePalette::from(theme)
            });
            assert_ne!(authored.accent, c.accent, "{at}: CONTROL");
            for (what, colors) in [("derived", &c), ("authored", &authored), ("at bg", &at_bg)] {
                for (mark, on) in [
                    (colors.accent, colors.active_bg),
                    (colors.accent, colors.band_bg),
                    (colors.accent, colors.hover_bg),
                    (colors.update_rule, colors.update_bg),
                ] {
                    let ratio = rgb(mark).contrast(rgb(on));
                    assert!(ratio >= 3.0, "{at}: {what} accent {ratio:.2} < 3.0:1");
                }
                if let Some(seam) = colors.seam {
                    let ratio = rgb(seam).contrast(rgb(colors.band_bg));
                    assert!(
                        ratio >= aterm_messages::tokens::SEAM_FLOOR,
                        "{at}: {what} seam {ratio:.2}"
                    );
                }
            }
            if !STRIP_CHIP_CARDS {
                let (fg, bg) = if STRIP_IS_CHROME_BAND {
                    (c.raise_fg, c.raise_bg)
                } else {
                    (c.fg, c.band_bg)
                };
                floor("'+' affordance", fg, bg);
            }
            if STRIP_IS_CHROME_BAND {
                floor("hovered '+'", c.button_hover_fg, c.hover_bg);
                assert_ne!(c.hover_bg, c.chip_bg, "{at}: hover is the quiet chip");
                assert_ne!(c.hover_bg, c.active_bg, "{at}: hover is the selection");
            }
            let fg = [
                ((theme.fg >> 16) & 0xff) as u8,
                ((theme.fg >> 8) & 0xff) as u8,
                (theme.fg & 0xff) as u8,
            ];
            assert_eq!(c.fg, fg, "{at}: the BAND is too heavy for its own ink");
        }
    }
}

#[test]
fn latte_strip_accent_is_floored_against_the_card() {
    let s = aterm_types::scheme::builtin("Catppuccin Latte").expect("builtin exists");
    let tp = s.to_theme_parts();
    let theme = Theme {
        fg: tp.fg,
        bg: tp.bg,
        cursor: tp.cursor,
        selection: tp.selection,
    };
    let c = strip_colors(theme);
    let raw = [
        ((tp.cursor >> 16) & 0xff) as u8,
        ((tp.cursor >> 8) & 0xff) as u8,
        (tp.cursor & 0xff) as u8,
    ];
    let contrast = crate::chrome_band::contrast;
    let surfaces = [c.active_bg, c.band_bg, c.hover_bg];
    if !SELECTED_TAB_IS_GROUND {
        assert!(
            contrast(raw, c.raise_bg) < 3.0,
            "negative control: Latte's raw cursor already clears 3:1 on the card"
        );
    }
    assert!(surfaces.iter().all(|on| contrast(c.accent, *on) >= 3.0));
    assert!(contrast(c.update_rule, c.update_bg) >= 3.0);
}

#[test]
fn strip_contrast_holds_under_forced_high_contrast_palettes() {
    use aterm_types::Rgb;
    let rgb = |c: [u8; 3]| Rgb::new(c[0], c[1], c[2]);
    let hostile = Theme {
        fg: 0x0080_8080,
        bg: 0x0080_8080,
        cursor: 0x0080_8080,
        selection: 0x0080_8080,
    };
    for (name, palette) in crate::chrome_band::hc_fixtures::STOCK {
        crate::chrome_band::hc_fixtures::with_forced(palette, || {
            let c = strip_colors(hostile);
            let version = |attention: bool, hovered: bool| {
                let (card, ink) = version_label_material(&c, attention, hovered);
                (ink, card.unwrap_or(c.band_bg))
            };
            let (rest_ink, rest_on) = version(false, false);
            let (hover_ink, hover_on) = version(false, true);
            let (cta_ink, cta_on) = version(true, false);
            for (role, ink, on) in [
                ("active-tab label", c.active_fg, c.active_bg),
                ("'+' affordance", c.raise_fg, c.raise_bg),
                ("inactive label", c.inactive_fg, c.band_bg),
                ("hovered label", c.hover_fg, c.hover_bg),
                ("'↻' update CTA", c.update_fg, c.update_bg),
                ("version label", rest_ink, rest_on),
                ("hovered version label", hover_ink, hover_on),
                ("version label under attention", cta_ink, cta_on),
                ("'↻' update rule", c.update_rule, c.update_bg),
            ] {
                let ratio = rgb(ink).contrast(rgb(on));
                assert!(
                    ratio >= 3.0,
                    "{name}: {role} contrast {ratio:.2} < 3.0:1 under High Contrast"
                );
            }
            assert_ne!(
                c.active_bg, c.band_bg,
                "{name}: the selected chip must stay a distinct surface"
            );
            assert_eq!(c.band_bg, palette.btn_face, "{name}: band is BTNFACE");
            assert_eq!(c.active_bg, palette.highlight, "{name}: chip is HIGHLIGHT");
            assert_eq!(
                c.seam,
                Some(palette.window_text),
                "{name}: the seam is a WINDOWTEXT border, not a blend"
            );
            assert_ne!(
                c.raise_bg, palette.highlight,
                "{name}: the '+' must not borrow the selection surface"
            );
            assert_ne!(
                c.hover_bg, palette.highlight,
                "{name}: hover must not impersonate the selection"
            );
            assert_ne!(
                c.update_bg, palette.highlight,
                "{name}: the update CTA must not borrow the selection surface"
            );
            assert_ne!(
                c.update_bg, c.active_bg,
                "{name}: the update CTA must not be a copy of the selected tab"
            );
            let overridden = strip_colors_with_active(hostile, Some([0xFF, 0x00, 0xFF]));
            assert_eq!(
                overridden.active_bg, palette.highlight,
                "{name}: active_tab_color must not override an HC palette"
            );
        });
    }
}

#[test]
fn the_automatic_tab_color_is_the_card_the_strip_paints() {
    let selection = [0x0A, 0x64, 0xD8];
    for name in aterm_types::scheme::builtin_names() {
        let theme = Theme::from(&aterm_types::scheme::builtin(name).expect("builtin"));
        let header = ChromePalette::from(theme);
        for floor in [StripFloor::Ground, StripFloor::Surface([0xEA, 0xEB, 0xEB])] {
            let painted = strip_colors_for_on(header, floor).active_bg;
            assert_eq!(
                automatic_active_tab_color(header, floor, None),
                painted,
                "{name} {floor:?}"
            );
        }
        if !SELECTED_TAB_IS_GROUND {
            assert_ne!(
                automatic_active_tab_color(header, StripFloor::Ground, None),
                strip_colors(theme).band_bg,
                "{name}: the card, not the band under it"
            );
        }
        assert_eq!(
            automatic_active_tab_color(header, StripFloor::Ground, Some(selection)),
            crate::chrome_band::mix3(
                strip_colors(theme).band_bg,
                selection,
                NATIVE_PILL_AUTO_ALPHA
            ),
            "{name}: the native pill over the band"
        );
        let authored = ChromePalette {
            authored: aterm_messages::tokens::AuthoredChrome {
                active_tab: Some([0x33, 0x66, 0xCC]),
                ..aterm_messages::tokens::AuthoredChrome::NONE
            },
            ..header
        };
        assert_eq!(
            automatic_active_tab_color(authored, StripFloor::Ground, Some(selection)),
            [0x33, 0x66, 0xCC],
            "{name}: the theme's own selected-tab colour"
        );
    }
    for (name, palette) in crate::chrome_band::hc_fixtures::STOCK {
        crate::chrome_band::hc_fixtures::with_forced(palette, || {
            let header = ChromePalette::from(Theme::default());
            assert_eq!(
                automatic_active_tab_color(header, StripFloor::Ground, Some(selection)),
                palette.highlight,
                "{name}"
            );
            assert_eq!(
                automatic_active_tab_color(header, StripFloor::Ground, None),
                strip_colors_for(header).active_bg,
                "{name}"
            );
        });
    }
}

#[test]
fn strip_colors_raise_direction_follows_appearance() {
    let luma =
        |c: [u8; 3]| 0.299 * f32::from(c[0]) + 0.587 * f32::from(c[1]) + 0.114 * f32::from(c[2]);
    let parts = |name: &str| {
        let s = aterm_types::scheme::builtin(name).expect("builtin exists");
        strip_colors(Theme::from(&s))
    };
    let dark = parts("Dracula");
    let light = parts("Solarized Light");
    if SELECTED_TAB_IS_GROUND {
        assert_eq!(dark.active_bg, dark.foot, "the selected tab is the ground");
        assert!(
            luma(dark.active_bg) < luma(dark.band_bg),
            "dark: below the band"
        );
        assert!(
            luma(light.active_bg) > luma(light.band_bg),
            "light: above the band"
        );
        assert!(
            luma(light.raise_bg) > luma(light.band_bg),
            "light update CTA rises toward white"
        );
        assert!(
            luma(dark.hover_bg) > luma(dark.band_bg),
            "dark hover rises toward fg"
        );
        for name in aterm_types::scheme::builtin_names() {
            let c = parts(name);
            if !bg_is_light(c.foot) {
                continue;
            }
            assert!(
                luma(c.hover_bg) < luma(c.band_bg) && luma(c.active_bg) > luma(c.band_bg),
                "{name}: hover {:?} must step below the band {:?}, the selected tab {:?} above",
                c.hover_bg,
                c.band_bg,
                c.active_bg
            );
            assert!(
                crate::chrome_band::contrast(c.hover_bg, c.band_bg) >= 1.08,
                "{name}: the hover wash {:?} hardly leaves the band {:?}",
                c.hover_bg,
                c.band_bg
            );
        }
    } else {
        assert!(
            luma(dark.active_bg) > luma(dark.band_bg),
            "dark theme active card should be brighter than the body"
        );
        assert!(
            luma(light.active_bg) < luma(light.band_bg),
            "light theme active card should be darker than the body"
        );
    }
}

#[test]
fn every_selected_tab_of_a_light_band_wears_one_outline() {
    use crate::chrome_band::contrast;
    if !SELECTED_TAB_IS_GROUND {
        return;
    }
    let theme_of = |name: &str| Theme::from(&aterm_types::scheme::builtin(name).expect("builtin"));
    let mut light = 0;
    for name in aterm_types::scheme::builtin_names() {
        let theme = theme_of(name);
        let page = crate::settings::Roles::from_theme(theme).surface;
        for floor in [StripFloor::Ground, StripFloor::Surface(page)] {
            let c = strip_colors_for_on(theme, floor);
            let ratio = contrast(c.active_bg, c.band_bg);
            if bg_is_light(c.band_bg) {
                assert!(c.seam.is_some(), "{name}: a band has a seam");
                assert_eq!(c.active_rim, c.seam, "{name} {floor:?} {ratio:.2}:1");
            } else {
                assert_eq!(
                    c.active_rim.is_some(),
                    ratio < TAB_RIM_BELOW,
                    "{name} {floor:?} {ratio:.2}:1: a dark band keeps its rule"
                );
            }
        }
        light += usize::from(bg_is_light(strip_colors(theme).band_bg));
    }
    assert!(light >= 2, "fixture: light built-ins ({light})");
    let grid = strip_colors_for_on(theme_of("Default Light"), StripFloor::Ground);
    assert!(
        contrast(grid.active_bg, grid.band_bg) >= TAB_RIM_BELOW,
        "CONTROL: the contrast rule alone leaves Default Light's terminal tab bare"
    );
    assert!(
        strip_colors(Theme::default()).active_rim.is_none(),
        "Default's terminal tab stays bare"
    );
}

const STRIP_GRID_COLS: [u16; 9] = [40, 60, 80, 100, 120, 140, 200, 240, 300];
const STRIP_GRID_TABS: [usize; 4] = [1, 2, 8, 20];

fn busy_tab() -> TabStripMetadata {
    TabStripMetadata {
        busy: true,
        ..PLAIN_TAB
    }
}

fn strip_cases(tabs: usize) -> Vec<(&'static str, Vec<String>, Vec<TabStripMetadata>)> {
    let same: Vec<String> = (0..tabs).map(|_| "claude".to_string()).collect();
    let distinct: Vec<String> = (0..tabs)
        .map(|i| format!("job-{i} · Ready in aterm"))
        .collect();
    let idle = vec![PLAIN_TAB; tabs];
    let busy = vec![busy_tab(); tabs];
    vec![
        ("same/idle", same.clone(), idle.clone()),
        ("same/busy", same, busy.clone()),
        ("distinct/idle", distinct.clone(), idle),
        ("distinct/busy", distinct, busy),
    ]
}

#[test]
fn selecting_a_seated_chip_re_lays_out_the_identical_strip() {
    let mut compared = 0usize;
    for &tabs in &STRIP_GRID_TABS {
        for (case, titles, metadata) in strip_cases(tabs) {
            for &cols in &STRIP_GRID_COLS {
                for active in 0..tabs {
                    let before =
                        layout_segments_with_metadata(cols, titles.len(), &metadata, active, false);
                    for seg in &before {
                        let TabHit::Select(seated) = seg.kind else {
                            continue;
                        };
                        compared += 1;
                        assert_eq!(
                            layout_segments_with_metadata(
                                cols,
                                titles.len(),
                                &metadata,
                                seated,
                                false
                            ),
                            before,
                            "{case} at {cols} cols, {tabs} tabs: selecting seated tab \
                             {seated} (from {active}) re-anchored the strip"
                        );
                    }
                }
            }
        }
    }
    assert!(compared > 2_000, "vacuous: {compared} selections compared");
}

#[test]
fn the_strip_window_is_an_even_partition_of_the_tab_list() {
    for tab_count in 1..=24usize {
        for seats in 1..=tab_count {
            let windows: Vec<(usize, usize)> = (0..tab_count)
                .map(|active| strip_window(active, seats, tab_count))
                .collect();
            let mut sizes: Vec<usize> = Vec::new();
            let mut covered = vec![0usize; tab_count];
            for (active, &(first, len)) in windows.iter().enumerate() {
                let where_ = format!("{tab_count} tabs, {seats} seats, active {active}");
                assert!(len > 0 && len <= seats, "{where_}: {len} chips seated");
                assert!(
                    first <= active && active < first + len,
                    "{where_}: the page {first}..{} does not seat it",
                    first + len
                );
                assert!(
                    first + len <= tab_count,
                    "{where_}: the page runs past the end"
                );
                for seated in first..first + len {
                    assert_eq!(
                        strip_window(seated, seats, tab_count),
                        (first, len),
                        "{where_}: seated tab {seated} would turn the page"
                    );
                }
                if windows[..active].iter().all(|&(f, _)| f != first) {
                    sizes.push(len);
                    for seat in covered.iter_mut().skip(first).take(len) {
                        *seat += 1;
                    }
                }
            }
            assert!(
                covered.iter().all(|&n| n == 1),
                "{tab_count} tabs, {seats} seats: the pages are not a partition ({covered:?})"
            );
            assert_eq!(
                sizes.len(),
                tab_count.div_ceil(seats),
                "{tab_count} tabs, {seats} seats: not the fewest pages ({sizes:?})"
            );
            let (lo, hi) = (
                *sizes.iter().min().expect("a page"),
                *sizes.iter().max().expect("a page"),
            );
            assert!(
                hi - lo <= 1,
                "{tab_count} tabs, {seats} seats: uneven pages {sizes:?}"
            );
            assert!(
                sizes.windows(2).all(|pair| pair[0] >= pair[1]),
                "{tab_count} tabs, {seats} seats: the longer pages come first ({sizes:?})"
            );
        }
    }
}

#[test]
fn the_legibility_floor_is_a_title_width_and_every_seat_is_minimal() {
    let floor = preferred_min_title_cols();
    assert_eq!(
        floor,
        chip_title_cols(PREFERRED_MIN_TAB_COLS, PLAIN_TAB),
        "the floor IS what a plain chip keeps at the constant"
    );
    assert_eq!(
        chip_seat_cols(PLAIN_TAB),
        PREFERRED_MIN_TAB_COLS,
        "an idle strip's floor is the shipped one, to the column"
    );

    let mut wider = 0usize;
    for icon in [None, Some(TabIconKind::Settings)] {
        for busy in [false, true] {
            for closable in [false, true] {
                let chrome = TabStripMetadata {
                    icon,
                    busy,
                    closable,
                    ..PLAIN_TAB
                };
                let seat = chip_seat_cols(chrome);
                assert!(
                    chip_title_cols(seat, chrome) >= floor,
                    "{chrome:?}: a {seat}-column seat keeps only {} title cells",
                    chip_title_cols(seat, chrome)
                );
                assert!(
                    seat == PREFERRED_MIN_TAB_COLS || chip_title_cols(seat - 1, chrome) < floor,
                    "{chrome:?}: {seat} columns is not the NARROWEST seat that \
                     keeps the floor"
                );
                assert!(
                    seat < SEAT_COLS_CEILING && chip_title_cols(SEAT_COLS_CEILING, chrome) >= floor,
                    "{chrome:?}: the seat scan's bound is binding — \
                     [`SEAT_COLS_CEILING`] has to be past the widest chrome, \
                     or a seat is whatever the bound happened to be"
                );
                wider += usize::from(seat > PREFERRED_MIN_TAB_COLS);
            }
        }
    }
    assert!(
        wider > 0,
        "vacuous: no chrome costs a chip anything, so the gate cannot be wrong"
    );
}

#[test]
fn no_width_seats_a_chip_that_can_only_say_a_number() {
    let mut seats = 0usize;
    for &tabs in &STRIP_GRID_TABS {
        for (case, titles, metadata) in strip_cases(tabs) {
            for &cols in &STRIP_GRID_COLS {
                for active in [0usize, tabs / 2, tabs - 1] {
                    let segments =
                        layout_segments_with_metadata(cols, titles.len(), &metadata, active, false);
                    let labels =
                        distinct_chip_labels(&segments, &titles, Some(&metadata), active, None);
                    for seg in &segments {
                        let TabHit::Select(i) = seg.kind else {
                            continue;
                        };
                        if seg.solo {
                            assert!(
                                labels[i].is_none(),
                                "{case} at {cols} cols: the label pass claimed the solo \
                                 title band ({:?})",
                                labels[i]
                            );
                            continue;
                        }
                        let label = labels[i].as_deref().expect("a seated chip is labelled");
                        seats += 1;
                        assert!(
                            !label.chars().all(|c| c.is_ascii_digit()),
                            "{case} at {cols} cols, {tabs} tabs, active {active}: chip {i} \
                             is the bare ordinal {label:?} — the photographed defect"
                        );
                        assert!(
                            !label.is_empty() && label != "…",
                            "{case} at {cols} cols: chip {i} says nothing ({label:?})"
                        );
                    }
                }
            }
        }
    }
    assert!(seats > 500, "vacuous: only {seats} chips were labelled");
}

const SLOT: VersionSlotCols = VersionSlotCols {
    full: 22,
    short: 13,
    attention: false,
};

fn version_of(segments: &[TabSegment]) -> Option<(crate::menu::VersionRung, u16, u16)> {
    segments.iter().find_map(|seg| match seg.kind {
        TabHit::Version { rung, .. } => Some((rung, seg.start_col, seg.end_col)),
        _ => None,
    })
}

fn without_version(segments: &[TabSegment]) -> Vec<TabSegment> {
    segments
        .iter()
        .copied()
        .filter(|seg| !matches!(seg.kind, TabHit::Version { .. }))
        .collect()
}

fn slot_width(rung: crate::menu::VersionRung, slot: VersionSlotCols) -> u16 {
    match rung {
        crate::menu::VersionRung::Full => slot.full,
        crate::menu::VersionRung::Short => slot.short,
    }
}

fn version_sweep(mut each: impl FnMut(usize, &str, &[TabStripMetadata], bool, usize, u16)) {
    for tabs in 0..=14usize {
        for (name, chip) in [("plain", PLAIN_TAB), ("busy", busy_tab())] {
            let metadata = vec![chip; tabs];
            for show_update in [false, true] {
                for active in [0, tabs.saturating_sub(1)] {
                    for cols in 0..=220u16 {
                        each(tabs, name, &metadata, show_update, active, cols);
                    }
                }
            }
        }
    }
}

#[test]
fn the_layout_left_of_the_slot_is_todays_layout_to_the_column() {
    let mut laid = 0usize;
    version_sweep(|tabs, name, metadata, show_update, active, cols| {
        let today = layout_segments_with_metadata(cols, tabs, metadata, active, show_update);
        let none = layout_segments_with_version(
            cols,
            tabs,
            metadata,
            active,
            show_update,
            VersionSlotCols::NONE,
        );
        assert_eq!(none, today, "{name}: NONE is today's strip");
        if name == "plain" {
            assert_eq!(
                none,
                layout_segments(cols, tabs, active, show_update),
                "plain chips at {cols} cols, {tabs} tabs"
            );
        }
        let segs = layout_segments_with_version(cols, tabs, metadata, active, show_update, SLOT);
        match version_of(&segs) {
            Some((rung, start, end)) => {
                laid += 1;
                let w = slot_width(rung, SLOT);
                assert_eq!((start, end), (cols - w, cols), "flush right at {cols}");
                assert_eq!(
                    without_version(&segs),
                    layout_segments_with_metadata(cols - w, tabs, metadata, active, show_update),
                    "{name}, {cols} cols, {tabs} tabs, update {show_update}, active {active}"
                );
                assert!(
                    matches!(segs.last().map(|s| s.kind), Some(TabHit::Version { .. })),
                    "the label is the strip's last segment"
                );
            }
            None => assert_eq!(segs, today, "{name}: no label is today's strip"),
        }
    });
    assert!(laid > 2_000, "vacuous: only {laid} strips laid a label out");
}

#[test]
fn the_slot_yields_before_any_chip_drops_below_its_seat() {
    let mut checked = 0usize;
    version_sweep(|tabs, name, metadata, show_update, active, cols| {
        let segs = layout_segments_with_version(cols, tabs, metadata, active, show_update, SLOT);
        if tabs < 2 || version_of(&segs).is_none() {
            return;
        }
        checked += 1;
        let seat = strip_seat_cols(tabs, metadata);
        let chips: Vec<&TabSegment> = segs
            .iter()
            .filter(|s| matches!(s.kind, TabHit::Select(_)))
            .collect();
        assert_eq!(chips.len(), tabs, "{name} at {cols}: every tab seated");
        for chip in chips {
            assert!(
                chip.end_col - chip.start_col >= seat,
                "{name} at {cols} cols, {tabs} tabs: a chip of {} < seat {seat}",
                chip.end_col - chip.start_col
            );
        }
        assert_eq!(
            strip_overflow(&segs, tabs),
            StripOverflow::NONE,
            "{name} at {cols}: no marked edge"
        );
    });
    assert!(checked > 1_000, "vacuous: only {checked} strips checked");
}

/// A lone chip never shrinks for the label.
#[cfg(not(target_os = "macos"))]
#[test]
fn a_lone_chip_never_shrinks_for_the_slot() {
    let chip = |segs: &[TabSegment]| {
        segs.iter()
            .find(|s| s.kind == TabHit::Select(0))
            .map(|s| (s.start_col, s.end_col, s.close_col))
    };
    let mut beside = 0usize;
    for show_update in [false, true] {
        for cols in 0..=220u16 {
            let plain = layout_segments_with_metadata(cols, 1, &[PLAIN_TAB], 0, show_update);
            let segs = layout_segments_with_version(cols, 1, &[PLAIN_TAB], 0, show_update, SLOT);
            assert_eq!(
                chip(&segs),
                chip(&plain),
                "{cols} cols, update {show_update}"
            );
            if version_of(&segs).is_some() {
                beside += 1;
                let (start, end, _) = chip(&segs).expect("the lone chip");
                assert_eq!(end - start, LONE_CHIP_COLS, "{cols} cols");
            }
        }
    }
    assert!(beside > 100, "vacuous: {beside}");
}

#[test]
fn the_slot_steps_down_a_rung_then_hides() {
    use crate::menu::VersionRung::{Full, Short};
    for (cols, tabs, want) in [
        (132, 8, Some(Full)),
        (132, 9, Some(Short)),
        (132, 10, None),
        (77, 4, Some(Full)),
        (77, 5, Some(Short)),
        (77, 6, None),
    ] {
        let metadata = vec![PLAIN_TAB; tabs];
        let segs = layout_segments_with_version(cols, tabs, &metadata, 0, false, SLOT);
        assert_eq!(
            version_of(&segs).map(|v| v.0),
            want,
            "{tabs} tabs at {cols} columns"
        );
    }
}

#[test]
fn the_cell_lane_paints_the_version_label_right_aligned() {
    use crate::menu::{VersionRung, strip_version_label};
    let theme = Theme::default();
    let colors = strip_colors(theme);
    let metadata = [PLAIN_TAB; 2];
    let titles = vec!["one".to_string(), "two".to_string()];
    let paint = |segs: &[TabSegment], version_hovered: bool| {
        let mut row = vec![blank_cell(theme); 132];
        paint_strip_with_metadata(
            &mut row,
            segs,
            &titles,
            &metadata,
            StripPaint {
                version_hovered,
                caption_hovered: None,
                ..StripPaint::default()
            },
            0,
            theme,
            None,
        );
        row
    };
    let cells = |label: &str| u16::try_from(strip_display_cells(label)).expect("short");
    for (attention, hovered, role) in [
        (false, false, StripRole::Inactive),
        (false, true, StripRole::Hover),
        (true, false, StripRole::Update),
    ] {
        let full = strip_version_label(VersionRung::Full, attention).expect("full");
        let slot = VersionSlotCols {
            full: cells(&full) + 4,
            short: 0,
            attention,
        };
        let segs = layout_segments_with_version(132, 2, &metadata, 0, false, slot);
        let (_, start, end) = version_of(&segs).expect("the label fits at 132 columns");
        let row = paint(&segs, hovered);
        let want = strip_cell(' ', &colors, role);
        for col in start..end {
            assert_eq!(
                row[usize::from(col)].bg,
                want.bg,
                "attention {attention}, hovered {hovered}: column {col}"
            );
        }
        if hovered {
            assert_eq!(want.bg, colors.hover_bg, "the band's hover material");
        }
        let text_end = usize::from(end) - 1;
        assert_eq!(row[text_end].ch, ' ', "one trailing pad cell");
        let text_start = text_end - usize::from(cells(&full));
        let painted: String = row[text_start..text_end].iter().map(|c| c.ch).collect();
        assert_eq!(painted, full, "right-aligned against the pad");
        assert_eq!(row[text_start].fg, want.fg, "the role's own ink");
        assert_eq!(row[text_start - 1].ch, ' ', "nothing left of the text");
    }
    let short = strip_version_label(VersionRung::Short, false).expect("a dev short rung");
    let label_segment = |width: u16| TabSegment {
        start_col: 132 - width,
        end_col: 132,
        close_col: None,
        kind: TabHit::Version {
            rung: VersionRung::Full,
            attention: false,
        },
        solo: false,
    };
    let row = paint(&[label_segment(cells(&short) + 1)], false);
    let painted: String = row[131 - usize::from(cells(&short))..131]
        .iter()
        .map(|c| c.ch)
        .collect();
    assert_eq!(
        painted, short,
        "the short rung when the full one cannot fit"
    );
    let row = paint(&[label_segment(cells(&short))], false);
    assert!(
        row[usize::from(132 - cells(&short))..]
            .iter()
            .all(|c| c.ch == ' '),
        "no room for either rung: no text, never a cut label"
    );
}

fn caption_slot() -> CaptionSlot {
    CaptionSlot::measure(
        Some(crate::client_frame::ClientFrame {
            minimize: true,
            maximize: true,
            window_menu: true,
            maximized: false,
            edges: true,
            header: true,
            layout: crate::client_frame::ButtonLayout::FALLBACK,
        }),
        9,
        1.0,
    )
}

#[test]
fn window_buttons_are_flush_right_and_the_tabs_yield_to_them() {
    let caption = caption_slot();
    assert_eq!(caption.cols(), 12);
    for tabs in [0usize, 1, 2, 5, 40] {
        let metadata = vec![PLAIN_TAB; tabs];
        let none = VersionSlotCols::NONE;
        let segs = layout_segments_with_caption(80, tabs, &metadata, 0, false, none, caption);
        let today = layout_segments_with_version(68, tabs, &metadata, 0, false, none);
        assert_eq!(segs[..segs.len() - 3], today[..], "{tabs} tabs");
        let buttons: Vec<_> = segs[segs.len() - 3..]
            .iter()
            .map(|s| (s.kind, s.start_col, s.end_col, s.close_col, s.solo))
            .collect();
        assert_eq!(
            buttons,
            [
                (
                    TabHit::Caption(CaptionButton::Minimize),
                    68,
                    72,
                    None,
                    false
                ),
                (
                    TabHit::Caption(CaptionButton::Maximize),
                    72,
                    76,
                    None,
                    false
                ),
                (TabHit::Caption(CaptionButton::Close), 76, 80, None, false),
            ]
        );
        for col in 76..80 {
            assert_eq!(
                hit_test(&segs, col),
                Some(TabHit::Caption(CaptionButton::Close)),
                "{tabs} tabs, column {col}"
            );
        }
        assert_eq!(
            layout_segments_with_caption(80, tabs, &metadata, 0, false, none, CaptionSlot::NONE),
            layout_segments_with_version(80, tabs, &metadata, 0, false, none),
            "{tabs} tabs: no buttons is today's strip"
        );
    }
}

#[test]
fn a_left_end_layout_puts_buttons_flush_left_and_the_strip_moves_past_them() {
    let layout =
        crate::client_frame::ButtonLayout::parse("close,minimize:maximize").expect("a layout");
    let caption = CaptionSlot::measure(
        Some(crate::client_frame::ClientFrame {
            minimize: true,
            maximize: true,
            window_menu: true,
            maximized: false,
            edges: true,
            header: true,
            layout,
        }),
        9,
        1.0,
    );
    let none = VersionSlotCols::NONE;
    for tabs in [0usize, 1, 2, 5, 40] {
        let metadata = vec![PLAIN_TAB; tabs];
        let segs = layout_segments_with_caption(80, tabs, &metadata, 0, false, none, caption);
        let today = layout_segments_with_version(68, tabs, &metadata, 0, false, none);
        let (strip, buttons) = segs.split_at(segs.len() - 3);
        let moved: Vec<_> = today
            .iter()
            .map(|s| TabSegment {
                start_col: s.start_col + 8,
                end_col: s.end_col + 8,
                close_col: s.close_col.map(|c| c + 8),
                ..*s
            })
            .collect();
        assert_eq!(strip, &moved[..], "{tabs} tabs");
        let buttons: Vec<_> = buttons
            .iter()
            .map(|s| (s.kind, s.start_col, s.end_col))
            .collect();
        assert_eq!(
            buttons,
            [
                (TabHit::Caption(CaptionButton::Close), 0, 4),
                (TabHit::Caption(CaptionButton::Minimize), 4, 8),
                (TabHit::Caption(CaptionButton::Maximize), 76, 80),
            ],
            "{tabs} tabs"
        );
        for col in 0..4 {
            assert_eq!(
                hit_test(&segs, col),
                Some(TabHit::Caption(CaptionButton::Close))
            );
        }
    }
    let segs = layout_segments_with_caption(80, 1, &[PLAIN_TAB], 0, false, none, caption_slot());
    assert!(hit_test(&segs, 0) != Some(TabHit::Caption(CaptionButton::Close)));
    let theme = Theme::default();
    let segs = layout_segments_with_caption(80, 1, &[PLAIN_TAB], 0, false, none, caption);
    let mut row = vec![blank_cell(theme); 80];
    paint_strip_with_metadata(
        &mut row,
        &segs,
        &["one".to_string()],
        &[PLAIN_TAB],
        StripPaint::default(),
        0,
        theme,
        None,
    );
    assert_eq!(row[2].ch, '✕');
    assert_eq!(row[6].ch, '_');
    assert_eq!(row[78].ch, '□');
}

#[test]
fn a_narrow_header_sheds_minimize_then_maximize() {
    let caption = caption_slot();
    let kinds = |cols: u16| -> Vec<TabHit> {
        layout_segments_with_caption(
            cols,
            1,
            &[PLAIN_TAB],
            0,
            false,
            VersionSlotCols::NONE,
            caption,
        )
        .into_iter()
        .filter_map(|s| matches!(s.kind, TabHit::Caption(_)).then_some(s.kind))
        .collect()
    };
    assert_eq!(kinds(80).len(), 3);
    assert_eq!(
        kinds(CAPTION_STRIP_FLOOR + 8),
        [
            TabHit::Caption(CaptionButton::Maximize),
            TabHit::Caption(CaptionButton::Close)
        ]
    );
    assert_eq!(
        kinds(CAPTION_STRIP_FLOOR + 4),
        [TabHit::Caption(CaptionButton::Close)]
    );
}

#[test]
fn the_cell_lane_paints_each_window_button_with_its_mark() {
    let theme = Theme::default();
    let colors = strip_colors(theme);
    let metadata = [PLAIN_TAB];
    let titles = vec!["one".to_string()];
    let segs = layout_segments_with_caption(
        80,
        1,
        &metadata,
        0,
        false,
        VersionSlotCols::NONE,
        caption_slot(),
    );
    if STRIP_IS_CHROME_BAND {
        assert_ne!(
            strip_cell(' ', &colors, StripRole::Hover).bg,
            strip_cell(' ', &colors, StripRole::Inactive).bg,
            "the hover material differs from bare band"
        );
    }
    for hovered in [None, Some(CaptionButton::Close)] {
        let mut row = vec![blank_cell(theme); 80];
        paint_strip_with_metadata(
            &mut row,
            &segs,
            &titles,
            &metadata,
            StripPaint {
                caption_hovered: hovered,
                ..StripPaint::default()
            },
            0,
            theme,
            None,
        );
        for (button, mark, start) in [
            (CaptionButton::Minimize, '_', 68usize),
            (CaptionButton::Maximize, '□', 72),
            (CaptionButton::Close, '✕', 76),
        ] {
            assert_eq!(row[start + 2].ch, mark, "{button:?}");
            let role = if hovered == Some(button) {
                StripRole::Hover
            } else {
                StripRole::Inactive
            };
            let want = strip_cell(' ', &colors, role).bg;
            for (i, cell) in row[start..start + 4].iter().enumerate() {
                assert_eq!(cell.bg, want, "{button:?} hovered {hovered:?}, cell {i}");
            }
        }
    }
}
