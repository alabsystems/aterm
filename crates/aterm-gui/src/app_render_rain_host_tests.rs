// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Phosphor host-side wiring tests.

use super::{prepend_strip_rows, rain_refresh_needed, update_rain_hidden_band};
use crate::matrix_rain::{EffectGeom, MatrixRain, RainConfig, RainSignal, RainTickInput};
use aterm_core::grid::damage::{Damage, DamageTracker};
use aterm_effects::pane::translate_rain_into_pane;

fn q(row: u16, x: u16, y: u16, w: u16, h: u16) -> crate::matrix_rain::RainCell {
    crate::matrix_rain::RainCell {
        row,
        x,
        y,
        w,
        h,
        glyph: 7,
        tint: 0x00FF_FFFF,
        alpha: 255,
        flip_x: false,
    }
}

fn halo(row: u16, x: u16, y: u16, w: u16, h: u16, cx: u16, cy: u16) -> aterm_render::RainHalo {
    aterm_render::RainHalo {
        row,
        x,
        y,
        w,
        h,
        color: 0x0020_FF40,
        cx,
        cy,
        rx: 12,
        ry: 8,
        mode: aterm_render::HaloMode::Add,
    }
}

#[test]
fn translate_rain_into_pane_confines_emission_to_the_pane_box() {
    let (cw, ch) = (8u32, 16u32);
    let (row_off, col_off, rows, cols) = (5u16, 42u16, 10u16, 38u16);
    let mut quads = vec![
        q(0, 0, 0, 8, 16),
        q(4, 80, 64, 8, 16),
        q(9, (u32::from(cols - 1) * cw) as u16, (9 * ch) as u16, 8, 16),
    ];
    let mut add = vec![
        halo(0, ((u32::from(cols) - 1) * cw) as u16, 0, 24, 16, 300, 8),
        halo(1, (u32::from(cols) * cw + 40) as u16, 16, 24, 16, 340, 24),
    ];
    translate_rain_into_pane(&mut quads, &mut add, row_off, col_off, rows, cols, cw, ch);
    let x0 = u32::from(col_off) * cw;
    let y0 = u32::from(row_off) * ch;
    let x1 = (u32::from(col_off) + u32::from(cols)) * cw;
    let y1 = (u32::from(row_off) + u32::from(rows)) * ch;
    assert_eq!(quads.len(), 3, "in-bounds quads all survive");
    for quad in &quads {
        assert!(u32::from(quad.x) >= x0 && u32::from(quad.x) + u32::from(quad.w) <= x1);
        assert!(u32::from(quad.y) >= y0 && u32::from(quad.y) + u32::from(quad.h) <= y1);
        assert!(
            (row_off..row_off + rows).contains(&quad.row),
            "row tag shifted"
        );
    }
    assert_eq!(
        quads[0].x, x0 as u16,
        "origin quad lands at the pane origin"
    );
    assert_eq!(quads[0].y, y0 as u16);
    assert_eq!(add.len(), 1, "spilling halo clips, not drops");
    let h0 = &add[0];
    assert!(
        u32::from(h0.x) + u32::from(h0.w) <= x1,
        "halo clipped at the divider"
    );
    assert_eq!(
        u32::from(h0.cx),
        300 + x0,
        "falloff centre shifts with the pane"
    );
    assert_eq!(u32::from(h0.cy), 8 + y0);
}

#[test]
fn translate_rain_into_pane_drops_u16_overflow() {
    let (cw, ch) = (8u32, 16u32);
    let mut quads = vec![q(0, 65_000, 0, 40, 16)];
    let mut add = Vec::new();
    translate_rain_into_pane(&mut quads, &mut add, 0, 100, 5, 8200, cw, ch);
    assert!(quads.is_empty(), "overflowing quad drops whole");
}

fn shell_execute_edge_model() -> aterm_spec::derive::Model {
    use aterm_spec::ty_model;
    ty_model! {
        ShellExecuteEdge {
            const Buggy = 0;
            var observed = 0;
            var session = 0;
            var executing = 0;
            var pulse = 0;
            var valid_edge = 0;
            action ObserveIdle0 {
                valid_edge = 0;
                pulse = 0;
                observed = 1;
                session = 0;
                executing = 0;
            }
            action ObserveExec0 {
                valid_edge = if observed == 1 && session == 0 && executing == 0 { 1 } else { 0 };
                pulse = if Buggy == 1 { 1 } else { if observed == 1 && session == 0 && executing == 0 { 1 } else { 0 } };
                observed = 1;
                session = 0;
                executing = 1;
            }
            action ObserveIdle1 {
                valid_edge = 0;
                pulse = 0;
                observed = 1;
                session = 1;
                executing = 0;
            }
            action ObserveExec1 {
                valid_edge = if observed == 1 && session == 1 && executing == 0 { 1 } else { 0 };
                pulse = if Buggy == 1 { 1 } else { if observed == 1 && session == 1 && executing == 0 { 1 } else { 0 } };
                observed = 1;
                session = 1;
                executing = 1;
            }
            action Reset {
                observed = 0;
                session = 0;
                executing = 0;
                pulse = 0;
                valid_edge = 0;
            }
            invariant BooleanBounds: observed <= 1 && session <= 1 && executing <= 1 && pulse <= 1 && valid_edge <= 1;
            invariant PulseExactlyMatchesEdge: pulse == valid_edge;
        }
    }
}

#[test]
fn shell_execute_edge_model_proves_and_catches_level_refresh() {
    let model = shell_execute_edge_model();
    aterm_spec::verify::prove_and_catch_scalar(&model, model.name);
}

#[test]
fn real_shell_execute_edge_conforms_and_long_level_drains() {
    let model = shell_execute_edge_model();
    let mut state = model.init_state();
    let mut last = aterm_effects::matrix_rain::RainLatches::default();
    let observations = [
        (7, true, "ObserveExec0", false),
        (7, true, "ObserveExec0", false),
        (7, false, "ObserveIdle0", false),
        (7, true, "ObserveExec0", true),
        (7, true, "ObserveExec0", false),
        (9, true, "ObserveExec1", false),
        (9, false, "ObserveIdle1", false),
        (9, true, "ObserveExec1", true),
    ];
    for (session, executing, action, expected) in observations {
        let pulse = last.note_shell_executing(session, executing);
        assert_eq!(pulse, expected);
        assert!(model.fire(action, &mut state));
        assert_eq!(i64::from(pulse), state["pulse"]);
        assert_eq!(i64::from(last.shell_executing.is_some()), state["observed"]);
        assert_eq!(
            i64::from(last.shell_executing.expect("observed").1),
            state["executing"]
        );
    }

    let mut rain = MatrixRain::new(RainConfig {
        enabled: true,
        output_material: false,
        idle_secs: 2,
        ..RainConfig::default()
    });
    let mut last = aterm_effects::matrix_rain::RainLatches {
        shell_executing: Some((7, false)),
        ..Default::default()
    };
    assert!(last.note_shell_executing(7, true));
    rain.note_signal(RainSignal::Execute as u32, 4);
    let geom = EffectGeom {
        cell_w: 8,
        cell_h: 16,
        rows: 24,
        cols: 80,
    };
    let (mut quads, mut add) = (Vec::new(), Vec::new());
    for _ in 0..240 {
        assert!(
            !last.note_shell_executing(7, true),
            "a held OSC execution level emits no repeat pulse"
        );
        rain.advance_ms(83);
        rain.emit(geom, &RainTickInput::default(), &mut quads, &mut add);
    }
    assert!(
        !rain.is_active(),
        "one Execute edge expires and drains despite a held execution level"
    );
}

#[test]
fn literal_reload_refreshes_without_new_grid_damage() {
    let classic = RainConfig {
        enabled: true,
        output_material: false,
        ..RainConfig::default()
    };
    let mut engine = MatrixRain::new(classic);
    engine.rescan_from_cells(&[], &[], &[], 0, 0, classic.default_bg, 42);
    assert!(!engine.needs_rescan(42));
    assert!(!engine.needs_material_sample());
    assert!(!rain_refresh_needed(
        true,
        false,
        false,
        0,
        Some(&engine),
        42
    ));

    engine.set_config(RainConfig {
        output_material: true,
        ..classic
    });
    assert!(engine.needs_material_sample());
    assert!(rain_refresh_needed(
        true,
        false,
        false,
        0,
        Some(&engine),
        42
    ));
    assert!(!rain_refresh_needed(
        true,
        false,
        false,
        1,
        Some(&engine),
        42
    ));
    assert!(!rain_refresh_needed(
        true,
        true,
        false,
        0,
        Some(&engine),
        42
    ));
    assert!(!rain_refresh_needed(
        true,
        false,
        true,
        0,
        Some(&engine),
        42
    ));
    assert!(!rain_refresh_needed(
        false,
        false,
        false,
        0,
        Some(&engine),
        42
    ));
}

#[test]
fn hidden_band_tracks_recent_damage() {
    let mut band: Vec<u16> = Vec::new();
    let mut dmg = Damage::Partial(DamageTracker::new(30));
    dmg.mark_rows(27, 30); // half-open: rows 27, 28, 29
    update_rain_hidden_band(&mut band, &dmg, 30);
    assert_eq!(band, vec![29, 28, 27], "ascending scan ⇒ bottom rows first");

    let mut dmg = Damage::Partial(DamageTracker::new(30));
    dmg.mark_rows(5, 6);
    update_rain_hidden_band(&mut band, &dmg, 30);
    assert_eq!(band, vec![5, 29, 28, 27]);

    let mut dmg = Damage::Partial(DamageTracker::new(30));
    dmg.mark_rows(28, 29);
    update_rain_hidden_band(&mut band, &dmg, 30);
    assert_eq!(band, vec![28, 5, 29, 27]);

    let mut dmg = Damage::Partial(DamageTracker::new(30));
    dmg.mark_rows(10, 13); // rows 10, 11, 12
    update_rain_hidden_band(&mut band, &dmg, 30);
    assert_eq!(band.len(), crate::matrix_rain::HIDDEN_CURSOR_BAND_ROWS);
    assert_eq!(band, vec![12, 11, 10, 28, 5], "oldest rows evicted");
}

#[test]
fn hidden_band_ignores_full_damage() {
    let mut band: Vec<u16> = vec![7, 3];
    let mut dmg = Damage::Partial(DamageTracker::new(10));
    dmg.mark_full();
    update_rain_hidden_band(&mut band, &dmg, 10);
    assert_eq!(band, vec![7, 3], "full damage leaves the band unchanged");
}

#[test]
fn strip_splice_shifts_every_grid_relative_channel() {
    struct Want {
        strip: usize,
        /// The viewport-row-2 rain cell and halo, and the halo's centre.
        rain_row: u16,
        rain_y: u16,
        halo_cy: u16,
        /// The viewport-row-2 fire-halo and `char_fg` cells.
        grid_row: u16,
        /// The viewport-row-1 companion's y.
        sprite_y: u16,
        /// Terminal row zero, where the selection sits, after the splice.
        selected_row: usize,
    }
    let blank = crate::chrome_band::blank_cell(aterm_render::Theme::default());
    for want in [
        Want {
            strip: 1,
            rain_row: 3,
            rain_y: 48,
            halo_cy: 56,
            grid_row: 3,
            sprite_y: 32,
            selected_row: 1,
        },
        Want {
            strip: 2,
            rain_row: 4,
            rain_y: 64,
            halo_cy: 72,
            grid_row: 4,
            sprite_y: 48,
            selected_row: 2,
        },
    ] {
        let strip = want.strip;
        let mut input = aterm_render::RenderInput::empty();
        input.cells = vec![vec![blank; 8]; 4];
        input.clusters = vec![vec![]; 4];
        input.combining = vec![vec![]; 4];
        input.images = vec![vec![]; 4];
        input.line_sizes = vec![aterm_core::grid::LineSize::SingleWidth; 4];
        input.rows = 4;
        input.cols = 8;
        input.selection.start_selection(
            0,
            0,
            aterm_core::selection::SelectionSide::Left,
            aterm_core::selection::SelectionType::Simple,
        );
        input
            .selection
            .update_selection(0, 1, aterm_core::selection::SelectionSide::Right);
        input.selection_clip = Some(aterm_render::SelectionClip::new(0, 1, 0, 2));
        input.selections = vec![aterm_core::render::PaneSelection {
            selection: input.selection.clone(),
            clip: aterm_render::SelectionClip::new(0, 1, 0, 2),
            bg: aterm_core::render::COLOR_UNSET,
            fg: aterm_core::render::COLOR_UNSET,
            inactive: true,
        }];
        // A rain row: one Linear Under layer whose cell sits at viewport row 2.
        let tile = aterm_render::fx::bitmap(
            aterm_core::render::FxTexels::Rgba8Straight,
            8,
            16,
            vec![255; 8 * 16 * 4],
        )
        .expect("a tile");
        input.fx_layers.push(aterm_core::render::FxLayer::new(
            1,
            std::sync::Arc::new(aterm_core::render::FxContent::new(
                vec![aterm_core::render::FxItem::Sprite(
                    aterm_core::render::FxSprite {
                        x: 8,
                        y: 32,
                        dw: 8,
                        dh: 16,
                        flip_x: false,
                        art: std::sync::Arc::clone(&tile),
                        ink: 0,
                        clip: aterm_core::render::PxRect::EVERYWHERE,
                    },
                )],
                vec![aterm_core::render::FxInk::over(0x0028_D75F, 96)],
            )),
            aterm_core::render::FxSlot::Under,
            aterm_core::render::FxSpace::Grid,
            aterm_core::render::FxBlend::Linear,
        ));
        input.rain_add.push(aterm_render::RainHalo {
            row: 2,
            x: 8,
            y: 32,
            w: 8,
            h: 16,
            color: 0x0010_3010,
            cx: 12,
            cy: 40,
            rx: 6,
            ry: 8,
            ..Default::default()
        });
        input.fire_halo.push(aterm_render::FireHaloCell {
            row: 2,
            col: 5,
            strength: 200,
        });
        input.char_fg.push(aterm_render::CharFg {
            row: 2,
            col: 5,
            fg: 0x0010_0804,
        });
        // A companion at viewport row 1.
        aterm_render::sprites::SpriteLayers::new(aterm_render::sprites::WORD_SPRITE_SALT).publish(
            &[aterm_render::sprites::FreeSprite::whole(0, 16, tile)],
            &mut input.fx_layers,
        );

        prepend_strip_rows(
            &mut input,
            &vec![vec![blank; 8]; strip],
            16,
            0,
            &mut Vec::new(),
        );

        let at = format!("{strip}-row strip");
        let rain = &input.fx_layers[0];
        let [aterm_core::render::FxItem::Sprite(cell)] = rain.content.items() else {
            panic!("{at}: the rain row keeps its one cell");
        };
        assert_eq!(
            rain.offset.1 + cell.y,
            i32::from(want.rain_y),
            "{at}: the rain row's pixel y shifted by strip*cell_h"
        );
        assert_eq!(input.rain_add[0].row, want.rain_row, "{at}");
        assert_eq!(input.rain_add[0].y, want.rain_y, "{at}");
        assert_eq!(
            input.rain_add[0].cy, want.halo_cy,
            "{at}: the halo falloff CENTRE rides the same vertical shift as its quad"
        );
        assert_eq!(
            input.rain_add[0].cx, 12,
            "{at}: a vertical splice leaves cx alone"
        );
        assert_eq!(
            input.fire_halo[0].row, want.grid_row,
            "{at}: fire_halo row shifted"
        );
        assert_eq!(input.fire_halo[0].col, 5, "{at}: the column never moves");
        assert_eq!(
            input.fire_halo[0].strength, 200,
            "{at}: the strength rides untouched"
        );
        assert_eq!(
            input.char_fg[0].row, want.grid_row,
            "{at}: char_fg shifts identically (the shared GRID-stream rule)"
        );
        let (top, bottom) = (want.selected_row, want.selected_row + 1);
        assert!(
            !input.selection.contains(0, 0),
            "{at}: terminal row-zero selection never paints the strip"
        );
        assert!(
            input.selection.contains(want.selected_row as i32, 0),
            "{at}: selection follows terminal row zero"
        );
        assert_eq!(
            input.selection_clip,
            Some(aterm_render::SelectionClip::new(top, bottom, 0, 2)),
            "{at}: the renderer-visible pane clip follows the same strip translation"
        );
        assert!(
            !input.selections[0].selection.contains(0, 0)
                && input.selections[0]
                    .selection
                    .contains(want.selected_row as i32, 0),
            "{at}: a pane entry's anchors follow terminal row zero too"
        );
        assert_eq!(
            input.selections[0].clip,
            aterm_render::SelectionClip::new(top, bottom, 0, 2),
            "{at}: …and so does its box"
        );
        assert!(
            !input.selection_contains_cell(0, 0, false, false)
                && input.selection_contains_cell(want.selected_row, 0, false, false),
            "{at}: the renderer predicate agrees with both halves after the splice"
        );
        assert_eq!(
            aterm_render::sprites::published(&input.fx_layers)[0].y,
            i32::from(want.sprite_y),
            "{at}: the companion's destination follows strip*cell_h"
        );
    }
}
