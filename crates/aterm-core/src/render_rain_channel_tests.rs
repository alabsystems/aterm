// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{
    CharFg, FireHaloCell, FireMode, FirePatch, GlowQuad, HaloMode, PrependBlessing, RainHalo,
    RenderInput, SceneAtlas,
};
use std::sync::Arc;

fn wallpaper(version: u64, fill: u8) -> Arc<SceneAtlas> {
    Arc::new(SceneAtlas {
        width: 2,
        height: 2,
        rgba: vec![fill; 2 * 2 * 4],
        version,
    })
}

fn rain_halo(row: u16) -> RainHalo {
    RainHalo {
        row,
        x: 4,
        y: row * 16,
        w: 8,
        h: 16,
        color: 0x0020_6030,
        cx: 8,
        cy: row * 16 + 8,
        rx: 8,
        ry: 8,
        mode: HaloMode::Add,
    }
}

fn under_quad(row: u16) -> GlowQuad {
    GlowQuad {
        row,
        x: 2,
        y: row * 16 + 3,
        w: 12,
        h: 10,
        color: 0x0060_2008,
        alpha: 0,
        color2: 0x0060_2008,
        alpha2: 0,
    }
}

fn char_fg(row: u16, col: u16) -> CharFg {
    CharFg {
        row,
        col,
        fg: 0x0018_0C04,
    }
}

fn fire_halo(row: u16, col: u16) -> FireHaloCell {
    FireHaloCell {
        row,
        col,
        strength: 160,
    }
}

fn fire_patch(row: u16) -> FirePatch {
    FirePatch {
        row,
        x: 4,
        y: row * 16,
        w: 24,
        h: 16,
        base_y: (row + 1) * 16,
        peak_h: 40,
        phase: 2048,
        temp: 128,
        strength: 200,
        lean: -24,
        cov_cap: 90,
        cell_h: 16,
        mode: FireMode::Add,
    }
}

fn populated() -> RenderInput {
    let mut input = RenderInput::empty();
    input.wallpaper = Some(wallpaper(7, 0xFF));
    input.rain_add = vec![rain_halo(1)];
    input.glow_halo = vec![rain_halo(2)];
    input.glow_under = vec![under_quad(1), under_quad(2)];
    input.fire_patch = vec![fire_patch(1), fire_patch(2)];
    input.char_fg = vec![char_fg(1, 3), char_fg(1, 4)];
    input.fire_halo = vec![fire_halo(1, 3), fire_halo(1, 4)];
    input
}

#[test]
fn the_composed_fill_blessing_is_revoked_by_every_other_producer() {
    let mut input = RenderInput::empty();
    assert!(
        !input.composed_fill_intact(),
        "a never-blessed snapshot must not claim to be a composite"
    );

    input.snapshot_seq = 41;
    input.bless_composed_fill();
    assert!(input.composed_fill_intact(), "the blessing did not take");

    let mut host = input.clone();
    host.snapshot_seq = host.snapshot_seq.wrapping_add(1);
    assert!(
        !host.composed_fill_intact(),
        "a host mutator's `snapshot_seq` bump must revoke the blessing — \
         this is the clause every unenumerated composite producer relies on"
    );

    let mut rebuilt = input.clone();
    rebuilt.invalidate_row_revisions();
    assert!(
        !rebuilt.composed_fill_intact(),
        "a row rebuild must revoke the blessing"
    );

    for blessed in [
        PrependBlessing::Disowned,
        PrependBlessing::Shift,
        PrependBlessing::Lane,
    ] {
        let mut spliced = input.clone();
        spliced.note_host_row_prepend(1, blessed);
        assert!(
            !spliced.composed_fill_intact(),
            "a {blessed:?} row prepend must revoke the blessing — composed row `r` is \
             no longer the row a ledger indexed"
        );
    }

    let mut unspliced = RenderInput::empty();
    unspliced.rows = 1;
    unspliced.cells = vec![Vec::new()];
    unspliced.clusters = vec![Vec::new()];
    unspliced.combining = vec![Vec::new()];
    unspliced.row_rev = vec![0];
    unspliced.row_rev_lane = 9;
    unspliced.snapshot_seq = 7;
    unspliced.engine_fill_seq = 7;
    unspliced.shifted_fill_seq = 7;
    assert_eq!(unspliced.host_prepend_blessing(), PrependBlessing::Lane);
    unspliced.snapshot_seq = 8;
    unspliced.note_host_row_prepend(1, PrependBlessing::Lane);
    unspliced.cells.insert(0, Vec::new());
    unspliced.clusters.insert(0, Vec::new());
    unspliced.combining.insert(0, Vec::new());
    unspliced.rows = 2;
    unspliced.bless_composed_fill();
    let mut pool = Vec::new();
    assert!(unspliced.undo_host_row_prepend(1, &mut pool, 1));
    assert!(
        !unspliced.composed_fill_intact(),
        "an un-splice restores an engine fill and must not leave a composed \
         blessing standing over it"
    );
}

#[test]
fn partial_eq_sees_atlas_identity() {
    let base = populated();

    let mut version_bumped = base.clone();
    version_bumped.wallpaper = Some(wallpaper(8, 0xFF));
    assert_ne!(base, version_bumped, "a fresh atlas publish must be seen");

    let mut same_version_rebake = base.clone();
    same_version_rebake.wallpaper = Some(wallpaper(7, 0x00));
    assert_ne!(
        base, same_version_rebake,
        "a same-version DIFFERENT-Arc publish (rebuilt engine) compares UNEQUAL"
    );

    let mut same_arc = base.clone();
    same_arc.wallpaper.clone_from(&base.wallpaper);
    assert_eq!(
        base, same_arc,
        "re-presenting the same published Arc stays EQUAL (a resting wallpaper is free)"
    );
}

#[test]
fn damage_cache_keeps_effect_revisions_and_drops_large_payloads() {
    let mut source = populated();
    source.rows = 4;
    source.refresh_cursor_effect_damage();

    let mut cached = RenderInput::empty();
    cached.cursor_glow_add = vec![under_quad(3); 64];
    cached.glow_halo = vec![rain_halo(3); 64];
    cached.glow_under = vec![under_quad(3); 64];
    cached.fire_patch = vec![fire_patch(3); 64];
    cached.clone_damage_cache_from(&source);

    assert!(cached.cursor_glow_add.is_empty());
    assert!(cached.glow_halo.is_empty());
    assert!(cached.glow_under.is_empty());
    assert!(cached.fire_patch.is_empty());
    assert!(cached.cursor_glow_add_damage.is_valid());
    assert!(cached.glow_halo_damage.is_valid());
    assert!(cached.glow_under_damage.is_valid());
    assert!(cached.fire_patch_damage.is_valid());

    let mut dirty = vec![false; source.rows];
    cached.glow_under_damage.mark_rows(&mut dirty);
    assert_eq!(dirty, [false, true, true, false]);
    assert_eq!(
        source
            .glow_under_damage
            .same_content(&cached.glow_under_damage),
        Some(true),
        "the cache carries the exact producer revision"
    );
}

#[test]
fn cursor_effect_damage_words_cover_every_visible_field() {
    macro_rules! assert_fields_change {
        ($name:literal, $base:ident, $($field:literal => $changed:expr),+ $(,)?) => {{
            let base_words = $base.damage_words();
            $(
                assert_ne!(
                    ($changed).damage_words(),
                    base_words,
                    concat!($name, "::", $field)
                );
            )+
        }};
    }

    let glow = under_quad(1);
    assert_fields_change!(
        "GlowQuad",
        glow,
        "row" => GlowQuad { row: 9, ..glow },
        "x" => GlowQuad { x: 9, ..glow },
        "y" => GlowQuad { y: 9, ..glow },
        "w" => GlowQuad { w: 9, ..glow },
        "h" => GlowQuad { h: 9, ..glow },
        "color" => GlowQuad { color: 0x65_4321, ..glow },
        "alpha" => GlowQuad { alpha: 9, ..glow },
        "color2" => GlowQuad { color2: 0x65_4321, ..glow },
        "alpha2" => GlowQuad { alpha2: 9, ..glow },
    );

    let rain = rain_halo(1);
    assert_fields_change!(
        "RainHalo",
        rain,
        "row" => RainHalo { row: 10, ..rain },
        "x" => RainHalo { x: 10, ..rain },
        "y" => RainHalo { y: 10, ..rain },
        "w" => RainHalo { w: 10, ..rain },
        "h" => RainHalo { h: 10, ..rain },
        "color" => RainHalo { color: 0x65_4321, ..rain },
        "cx" => RainHalo { cx: 10, ..rain },
        "cy" => RainHalo { cy: 10, ..rain },
        "rx" => RainHalo { rx: 10, ..rain },
        "ry" => RainHalo { ry: 10, ..rain },
        "mode" => RainHalo { mode: HaloMode::Over, ..rain },
    );

    let fire = fire_patch(1);
    assert_fields_change!(
        "FirePatch",
        fire,
        "row" => FirePatch { row: 14, ..fire },
        "x" => FirePatch { x: 14, ..fire },
        "y" => FirePatch { y: 14, ..fire },
        "w" => FirePatch { w: 14, ..fire },
        "h" => FirePatch { h: 14, ..fire },
        "base_y" => FirePatch { base_y: 14, ..fire },
        "peak_h" => FirePatch { peak_h: 14, ..fire },
        "phase" => FirePatch { phase: 14, ..fire },
        "temp" => FirePatch { temp: 14, ..fire },
        "strength" => FirePatch { strength: 14, ..fire },
        "lean" => FirePatch { lean: 14, ..fire },
        "cov_cap" => FirePatch { cov_cap: 14, ..fire },
        "cell_h" => FirePatch { cell_h: 14, ..fire },
        "mode" => FirePatch { mode: FireMode::Over, ..fire },
    );
}

#[test]
fn damage_record_covers_8k_glow_under_without_retaining_its_payload() {
    const QUADS: usize = 8_191;
    const ROWS: usize = 128;

    let mut source = RenderInput::empty();
    source.rows = ROWS;
    source.glow_under = (0..QUADS)
        .map(|index| GlowQuad {
            row: (index % ROWS) as u16,
            x: index as u16,
            y: (index % ROWS * 16) as u16,
            w: 12,
            h: 10,
            color: 0x0060_2008,
            alpha: 0,
            color2: 0x0060_2008,
            alpha2: 0,
        })
        .collect();
    source.refresh_cursor_effect_damage();

    let mut cache = RenderInput::empty();
    cache.clone_damage_cache_from(&source);
    assert!(cache.glow_under.is_empty(), "the cache retained 8k quads");
    assert_eq!(
        cache.glow_under_damage.rows.len(),
        ROWS.div_ceil(u64::BITS as usize),
        "one bit per grid row, not one entry per quad"
    );
    assert!(
        cache.glow_under_damage.rows.len() * std::mem::size_of::<u64>()
            < source.glow_under.len() * std::mem::size_of::<GlowQuad>(),
        "the compact cache must be smaller than the payload it replaces"
    );

    let mut changed = source.clone();
    changed.glow_under[QUADS - 1].color ^= 1;
    changed.refresh_cursor_effect_damage();
    assert_eq!(
        source
            .glow_under_damage
            .same_content(&changed.glow_under_damage),
        Some(false),
        "the compact damage revision must detect a payload change"
    );
}

/// A glow stream's damage record tells apart two ramp streams that draw
/// different rows: a ramp's run (`join`, `joined`) and where its kept
/// ends start (`ink`) change what it draws as its box and its ends do.
#[test]
fn the_glow_damage_record_sees_a_ramps_run_and_its_ends() {
    use crate::render_ramps::{GlowRamp, GlowRamps, glow_end};
    let stream = |join: u16, ink: u32| {
        let mut ramps = GlowRamps {
            items: vec![GlowRamp {
                at: 0,
                row: 1,
                x: 4,
                y: 20,
                w: 6,
                h: 4,
                join,
                joined: 1,
                ink,
            }],
            ends: (1..=8).map(|c| glow_end(c * 0x10, 0)).collect(),
            lit: 0,
        };
        ramps.lit = ramps.count_lit();
        let mut input = RenderInput::empty();
        input.rows = 4;
        input.glow_under_ramps = ramps;
        input.refresh_cursor_effect_damage();
        input
    };
    let drawn = |input: &RenderInput| {
        let mut quads = Vec::new();
        input.glow_under_ramps.expand_into(&[], &mut quads);
        quads
    };
    let base = stream(1, 0);
    assert_eq!(
        base.glow_under_damage
            .same_content(&stream(1, 0).glow_under_damage),
        Some(true),
        "the same stream is the same content"
    );
    for (what, other) in [("its run", stream(2, 0)), ("its ends", stream(1, 2))] {
        assert_ne!(drawn(&base), drawn(&other), "{what}: the rows drawn differ");
        assert_eq!(
            base.glow_under_damage
                .same_content(&other.glow_under_damage),
            Some(false),
            "{what}: the damage record sees the change"
        );
    }
}
