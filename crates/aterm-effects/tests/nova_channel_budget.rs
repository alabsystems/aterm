// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// THE SHARED `nova_add` CHANNEL HAS TWO PRODUCERS AND ONE BUDGET.
//
// `nova::MAX_NOVA_QUADS` (1536) is spent by exactly one of them. The
// decoration pass funds every classic nova and every supernova through
// `MAX_NOVA_QUADS.saturating_sub(nova.len())` (word_decorations.rs, in
// `emit_nova_axis` and `emit_super_axis`), so the DECORATION share is
// hard-capped at 1536 by construction and the §3.2 burst mutex is what keeps
// that cap from ever BINDING — i.e. from truncating a live effect mid-emission.
//
// PRISM WAKE (`output_streak`) is the second producer. The composed host
// appends its per-pane quads into the same channel AFTER the decoration pass
// has finished spending (app_render.rs `compose_output_streak`, whose
// `nova_scratch.extend_from_slice` runs after `compose_word_decorations`), so
// those quads are neither funded by that budget nor visible to it. The channel
// TOTAL is therefore `decoration share + Σ panes streak share`, and it can
// exceed 1536.
//
// This file measures all three facts and pins what the overrun actually does:
//
//   1. the streak's own share is TAIL-bounded, not column-bounded — one ribbon
//      row of ≤ cols cells plus ≤ MAX_COMETS heads of ≤ TAIL_MAX + 2 cells;
//   2. four panes of build output on a realistic grid push the combined
//      channel past 1536 while the decoration share sits at its own
//      genome-reachable worst case (3 × 392);
//   3. the consequence is COST, NOT CORRUPTION: the consumer
//      (`Renderer::draw_nova` → `draw_flat_add`) walks the whole slice, so
//      every over-budget quad is drawn — nothing truncates, nothing panics —
//      and a reused host scratch does not reallocate per frame.

use aterm_core::terminal::Terminal;
use aterm_effects::cursor_glow::Geom;
use aterm_effects::nova::MAX_NOVA_QUADS;
use aterm_effects::output_streak::{MAX_COMETS, OutputStreak, StreakConfig, TAIL_MAX};
use aterm_render::{GlowQuad, Renderer, Theme, premul_rgb};
use aterm_time::Instant;
use std::time::Duration;

/// The decoration share's genome-reachable worst case: `MAX_ACTIVE_NOVAS` (3)
/// live classic novas at the ay-CHC `Q ≤ 392` certificate — the exact product
/// `nova.rs` derives its "never binds" headroom from (3 × 392 = 1176 < 1536).
const DECO_WORST_CASE: usize = 3 * 392;

fn geom(cw: usize, ch: usize, cols: usize, rows: usize) -> Geom {
    Geom {
        cw,
        ch,
        rows,
        cols,
        origin_x: 0,
        origin_y: 0,
        win_w: (cols * cw) as u16,
        win_h: (rows * ch) as u16,
        head: 0,
    }
}

/// The most demanding SHIPPABLE config: the host clamps `tail` into
/// `TAIL_MIN..=TAIL_MAX` and `max_streaks` into `1..=MAX_COMETS`, so this is
/// the corner of the config space, not an impossible one.
fn cfg() -> StreakConfig {
    StreakConfig {
        enabled: true,
        intensity: 1.0,
        tail: TAIL_MAX,
        max_streaks: MAX_COMETS as u8,
        idle_secs: 10.0,
        dark_theme: true,
        theme_bg: 0x001A_1B26,
        theme_fg: 0x00C0_CAF5,
        theme_cursor: 0x0050_FA7B,
        sound: false,
    }
}

/// Drive one engine through `frames` of full-width build output (one fresh row
/// per frame at 120 Hz — the `cargo build` firehose), appending into `out`, and
/// return the largest per-frame quad count it produced.
fn drive(seed: u64, g: Geom, frames: u64, out: &mut Vec<GlowQuad>) -> usize {
    let mut e = OutputStreak::new(seed);
    let c = cfg();
    let t0 = Instant::now();
    let mut peak = 0usize;
    for i in 0..frames {
        let now = t0 + Duration::from_millis(i * 8);
        let row = (i % g.rows as u64) as u16;
        e.note_output(i + 1, &[(row, 0, (g.cols - 1) as u16)], now, false);
        out.clear();
        e.tick(now, g, &c, out);
        peak = peak.max(out.len());
    }
    peak
}

/// THE STREAK'S SHARE IS TAIL-BOUNDED. A comet lights `tail + 2` cells around
/// its head, NOT the whole row: `OutputStreak::emit` sweeps
/// `tail_start.floor() - 1 ..= head.round()`, and `push_grid_quad` emits exactly
/// one row-band quad per cell. So the per-engine worst case is one ribbon
/// (≤ cols) plus `MAX_COMETS` heads (≤ TAIL_MAX + 2 each) — `cols + 64` at the
/// config corner, not `cols × (MAX_COMETS + 1)`.
#[test]
fn the_streak_share_is_tail_bounded_not_column_bounded() {
    for cols in [80usize, 120, 200, 400] {
        let rows = 50usize;
        let g = geom(8, 17, cols, rows);
        let mut out = Vec::new();
        let peak = drive(7, g, 960, &mut out);
        let structural = cols + MAX_COMETS * (usize::from(TAIL_MAX) + 2);
        assert!(
            peak <= structural,
            "cols={cols}: peak {peak} must sit under the ribbon+heads bound {structural}"
        );
        // NON-VACUITY: the flood really did open its full-width ribbon, so the
        // bound above is being tested against a saturated engine.
        assert!(
            peak >= cols,
            "cols={cols}: peak {peak} — the flood ribbon never opened, the bound is vacuous"
        );
        // And the column-scaled reading of the same worst case overstates it by
        // roughly MAX_COMETS: a comet is a head, not a painted bar.
        assert!(
            peak * 2 < cols * (MAX_COMETS + 1),
            "cols={cols}: peak {peak} vs the column-scaled reading {}",
            cols * (MAX_COMETS + 1)
        );
    }
}

/// FOUR PANES OVERRUN THE BACKSTOP. Nothing sums the per-pane engines: each
/// appends its own frame into the one window channel (`compose_output_streak`).
/// With the decoration share at its own genome-reachable worst case, a four-way
/// split of build output puts the channel past 1536.
#[test]
fn four_panes_of_build_output_push_the_channel_past_the_backstop() {
    let (rows, cols) = (50usize, 200usize);
    let g = geom(8, 17, cols, rows);

    // The decoration pass spends first and is hard-capped by its own clamp.
    let mut channel: Vec<GlowQuad> = vec![GlowQuad::default(); DECO_WORST_CASE];
    let deco = channel.len();
    assert!(deco <= MAX_NOVA_QUADS, "premise: the deco share is funded");

    // …then every pane's streak appends, unfunded and uncounted.
    let mut pane = Vec::new();
    let mut streak = 0usize;
    for p in 0..4u64 {
        let peak = drive(0x5052_4953_4D5F_5057 ^ p, g, 960, &mut pane);
        streak += peak;
        channel.extend_from_slice(&pane);
    }

    assert!(
        deco + streak > MAX_NOVA_QUADS,
        "deco {deco} + 4 panes of streak {streak} = {} — expected to exceed the {MAX_NOVA_QUADS} \
         backstop",
        deco + streak
    );
    assert!(
        channel.len() > MAX_NOVA_QUADS,
        "the assembled channel is {} quads",
        channel.len()
    );
}

/// WHAT THE OVERRUN DOES: nothing. The consumer is `Renderer::draw_nova` →
/// `draw_flat_add`, a plain walk of the slice with a per-quad row gate and a
/// per-quad clip (`Renderer::draw_flat_add`, aterm-render/src/lib.rs). There is
/// no fixed buffer, no `MAX_NOVA_QUADS` clamp and no assert on the draw side, so
/// an over-budget channel is DRAWN IN FULL: the quads past 1536 reach the glass.
///
/// Pinned as a DIFFERENCE, which is the only way to tell "drawn" from "silently
/// dropped" apart: the same frame rendered with the channel truncated at the
/// backstop must not match the frame rendered with all of it.
#[test]
fn the_over_budget_channel_is_drawn_in_full_not_truncated() {
    let Some(mut rend) = Renderer::from_system(18.0, Theme::default()) else {
        eprintln!("SKIP: no system monospace font");
        return;
    };
    let (cw, ch) = rend.cell_size();
    let (rows, cols) = (24usize, 120usize);
    let g = geom(cw, ch, cols, rows);

    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"\x1b[?25lcompiling aterm-effects v0.69.0");

    // The decoration share, stacked on ONE cell so it cannot saturate the cells
    // the streak lights: only the streak's own quads decide the difference.
    let mut channel: Vec<GlowQuad> = (0..MAX_NOVA_QUADS)
        .map(|_| GlowQuad {
            row: 0,
            x: 0,
            y: 0,
            w: cw as u16,
            h: ch as u16,
            color: premul_rgb(0x0040_4040, 8),
            alpha: 0,
        })
        .collect();
    assert_eq!(
        channel.len(),
        MAX_NOVA_QUADS,
        "the funded share is at its cap"
    );

    let mut pane = Vec::new();
    for p in 0..4u64 {
        drive(0xA11C_E5 ^ p, g, 600, &mut pane);
        channel.extend_from_slice(&pane);
    }
    assert!(
        channel.len() > MAX_NOVA_QUADS,
        "premise: {} quads is over the backstop",
        channel.len()
    );

    let mut full = term.cell_frame(rows, cols);
    full.nova_add.clone_from(&channel);
    let full_px = rend.render_input(&full).pixels.clone();

    let mut clipped = term.cell_frame(rows, cols);
    clipped.nova_add = channel[..MAX_NOVA_QUADS].to_vec();
    let clipped_px = rend.render_input(&clipped).pixels.clone();

    assert_ne!(
        full_px, clipped_px,
        "the quads past MAX_NOVA_QUADS painted nothing — the draw side truncates after all"
    );

    // …and the over-budget draw is deterministic, not reading past anything:
    // the same channel renders byte-identically twice.
    let mut again = term.cell_frame(rows, cols);
    again.nova_add.clone_from(&channel);
    assert_eq!(
        full_px,
        rend.render_input(&again).pixels,
        "over-budget draw is deterministic"
    );
}

/// THE OTHER THING AN OVERRUN COULD HAVE COST: a per-frame allocation in the
/// hot path. It does not. The host's channel is a RESIDENT scratch that is
/// cleared, never freed (`ws.nova_scratch.clear()` then `extend_from_slice`,
/// and `input_scratch.nova_add.clone_from`, which reuses capacity), so the
/// growth past 1536 is paid ONCE and the steady state allocates nothing.
#[test]
fn the_resident_scratch_grows_once_not_every_frame() {
    let (rows, cols) = (50usize, 200usize);
    let g = geom(8, 17, cols, rows);
    let c = cfg();

    let mut scratch: Vec<GlowQuad> = Vec::new();
    let mut mirror: Vec<GlowQuad> = Vec::new();
    let mut engines: Vec<OutputStreak> = (0..4).map(OutputStreak::new).collect();
    let mut pane = Vec::new();
    let t0 = Instant::now();
    let mut settled = (0usize, 0usize);

    for i in 0..600u64 {
        let now = t0 + Duration::from_millis(i * 8);
        scratch.clear();
        // The decoration share arrives first, at its funded cap.
        scratch.extend((0..MAX_NOVA_QUADS).map(|_| GlowQuad::default()));
        for (p, e) in engines.iter_mut().enumerate() {
            let row = ((i + p as u64) % g.rows as u64) as u16;
            e.note_output(
                i * 4 + p as u64 + 1,
                &[(row, 0, (cols - 1) as u16)],
                now,
                false,
            );
            pane.clear();
            e.tick(now, g, &c, &mut pane);
            scratch.extend_from_slice(&pane);
        }
        mirror.clone_from(&scratch);
        // Let both vecs reach their high-water mark, then hold them to it.
        if i == 300 {
            settled = (scratch.capacity(), mirror.capacity());
        }
        if i > 300 {
            assert_eq!(
                (scratch.capacity(), mirror.capacity()),
                settled,
                "frame {i}: the resident channel reallocated mid-flight"
            );
        }
    }
    assert!(
        settled.0 > MAX_NOVA_QUADS,
        "premise: the scratch really did carry an over-budget channel ({} quads)",
        settled.0
    );
}
