// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Phosphor rain field: the pure `(seed, tick)` math (matrix-rain design §4).
//!
//! Everything here is a total function of `(seed32, config-derived params,
//! tick)` on u32 lattice math through the one hash [`rain_hash32`], so the
//! field ports bit-exactly to WGSL (which has no u64). No `Instant`, no
//! allocation, no state beyond the caller-owned [`DensityRing`].

/// GPU-path quad budget (contract §4): the hard cap both backends size for.
pub const MAX_RAIN_QUADS: usize = 2048;
/// CPU texel budget: the CPU stamp cost scales with cell area, so the quad cap
/// on that path derives from texels (`2048 · (10·20)`), floored at 256 quads.
pub const MAX_RAIN_TEXELS: usize = 409_600;
/// Bright-head additive halo budget (`rain_add` GlowQuads per frame).
pub const MAX_RAIN_ADD: usize = 64;
/// Head-brightness trail depth: levels are `0..=15` (16 ramp tints).
pub const LEVELS: u32 = 15;
/// Glyph ROM size — `glyph_from_epoch` reduces the hash modulo this.
pub const GLYPH_COUNT: u32 = 64;
/// Density change-point ring capacity: cycle starts are at most `C·p` ticks
/// old and the quantized staircase steps a handful of times per weather front.
pub const DENSITY_RING: usize = 8;

/// murmur3 finalizer — the one field hash (WGSL-trivial, bit-exact in u32).
#[must_use]
#[inline]
pub const fn rain_hash32(mut x: u32) -> u32 {
    x ^= x >> 16;
    x = x.wrapping_mul(0x85EB_CA6B);
    x ^= x >> 13;
    x = x.wrapping_mul(0xC2B2_AE35);
    x ^= x >> 16;
    x
}

/// Half-open LSB-first bit slice `(x >> a) & ((1 << (b-a)) - 1)` — the doc
/// notation `x.bits(a..b)`.
#[must_use]
#[inline]
pub const fn bits(x: u32, a: u32, b: u32) -> u32 {
    (x >> a) & ((1 << (b - a)) - 1)
}

/// 32-bit cell packing `(row << 16) | col` (grids < 2^16 on both axes).
#[must_use]
#[inline]
pub const fn pack(row: u32, col: u32) -> u32 {
    (row << 16) | col
}

/// Config + geometry derived field parameters, rebuilt only on
/// `set_config` / geometry change — every per-tick field call threads this.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FieldParams {
    /// Host-mixed 32-bit seed (the u64 config seed diffused once, host-side).
    pub seed32: u32,
    /// Viewport rows (the live value — the flash floor must see real geometry).
    pub rows: u32,
    /// Base tick period in ms (`1000 / fps`).
    pub tick_ms: u32,
    /// Speed knob `1..=10` (5 = neutral); shifts the per-column step period.
    pub speed: u32,
    /// Trail knob `1..=10` (5 = neutral); scales the per-column trail length.
    pub trail: u32,
    /// Mutation quantum in ticks: `max(1, round(mutation_ms / tick_ms))`.
    pub mq: u32,
    /// Dither quantum: smallest multiple of `mq` at or above `ticks(350ms)`
    /// (the `TWINKLE_GRID_MS` floor) — every dither boundary lands on a
    /// mutation tick, so non-mutation ticks touch only stepped rows.
    pub dq: u32,
    /// Stammer quantum: `ticks(~500ms)` — bright heads hold one extra tick,
    /// in unison, once per 8 stammer windows.
    pub sq: u32,
}

/// Round `ms` to whole ticks, floored at 1.
#[must_use]
#[inline]
pub const fn ticks_of(ms: u32, tick_ms: u32) -> u32 {
    let t = if tick_ms > 1 { tick_ms } else { 1 };
    let n = (ms + t / 2) / t;
    if n > 1 { n } else { 1 }
}

impl FieldParams {
    /// Derive the tick quanta from a tick period + mutation window. `dq` is
    /// the smallest multiple of `mq` at or above `ticks(350ms)`.
    #[must_use]
    #[inline]
    pub const fn quanta(tick_ms: u32, mutation_ms: u32) -> (u32, u32, u32) {
        let mq = ticks_of(mutation_ms, tick_ms);
        let floor = ticks_of(350, tick_ms);
        let dq = mq * floor.div_ceil(mq);
        let sq = ticks_of(500, tick_ms);
        (mq, dq, sq)
    }
}

/// Per-column derived parameters (pure function of `(seed32, col, params)`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ColParams {
    /// Column hash root `rain_hash32(seed32 ^ ((c<<1)|1))`.
    pub h: u32,
    /// Step period in ticks/row (`>= 2` — a head advances at most 1 row/tick
    /// by construction, the structural speed clamp).
    pub p: u32,
    /// Trail length in rows (`3..=rows`).
    pub l: u32,
    /// Off-screen gap in rows, extended so the flash floor holds (see below).
    pub g: u32,
    /// Cycle length `rows + l + g`.
    pub c: u32,
    /// Phase offset in ticks, `< c·p`.
    pub phi: u32,
}

/// Column parameters with the geometry-aware flash floor: `g` is extended at
/// runtime so `c·p·tick_ms >= 1000` holds for the live `rows` (a config-space
/// clamp cannot see an 8-row split pane), keeping per-cell re-ignition under
/// 1 Hz (WCAG 2.3.1).
#[must_use]
#[inline]
pub const fn col_params(fp: &FieldParams, col: u32) -> ColParams {
    // Floor at 3: the trail clamps below are `.clamp(3, rows)`, which panics
    // when `min > max`, so a 1–2-row viewport derives its cycle from a 3-row
    // virtual field. Drawing is bounded by the real rows elsewhere, so the
    // only effect is a slightly longer off-screen cycle.
    let rows = if fp.rows > 3 { fp.rows } else { 3 };
    let h = rain_hash32(fp.seed32 ^ ((col << 1) | 1));
    // Step period 2..=5 from the hash, then the speed-knob shift (5 =
    // neutral; faster knobs shorten the period), clamped >= 2.
    let p0 = (2 + bits(h, 0, 2)) as i64;
    let speed = if fp.speed < 1 {
        1
    } else if fp.speed > 10 {
        10
    } else {
        fp.speed
    } as i64;
    let p_raw = p0.saturating_add(5 - speed);
    let p = if p_raw < 2 {
        2
    } else if p_raw > 16 {
        16
    } else {
        p_raw as u32
    };
    // Trail: 3/8..6/8 of the viewport from the hash, then the trail knob
    // (5 = neutral), clamped 3..=rows.
    let l0_raw = rows * (3 + bits(h, 2, 4)) / 8;
    let l0 = if l0_raw < 3 {
        3
    } else if l0_raw > rows {
        rows
    } else {
        l0_raw
    };
    let trail = if fp.trail < 1 {
        1
    } else if fp.trail > 10 {
        10
    } else {
        fp.trail
    };
    let l_raw = l0 * trail / 5;
    let l = if l_raw < 3 {
        3
    } else if l_raw > rows {
        rows
    } else {
        l_raw
    };
    // Gap: styled `l/2 + bits·l/8`, extended to the flash floor
    // `ceil(1000 / (p·tick_ms)) - rows - l` so `c·p·tick_ms >= 1000`.
    let g_style = l / 2 + bits(h, 4, 7) * l / 8;
    let tick_ms = if fp.tick_ms > 1 { fp.tick_ms } else { 1 };
    let floor_c = 1000u32.div_ceil(p * tick_ms);
    let min_g = floor_c.saturating_sub(rows + l);
    let g = if g_style > min_g { g_style } else { min_g };
    let c = rows + l + g;
    let phi = rain_hash32(h) % (c * p);
    ColParams { h, p, l, g, c, phi }
}

/// Stammer (design §2): bright-head columns hold one extra tick, in unison,
/// once per 8 stammer windows — `tick' = tick - ((tick/sq) % 8 == 7)`.
#[must_use]
#[inline]
pub const fn stammer_tick(tick: u64, sq: u32) -> u64 {
    let sq = if sq > 1 { sq } else { 1 } as u64;
    tick - ((tick / sq) % 8 == 7) as u64
}

/// A column's resolved cycle state at `tick`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CycleView {
    /// The column's effective tick (stammer-adjusted for bright heads).
    pub eff_tick: u64,
    /// Head position within the cycle, `0..c` (on-screen while `< rows`).
    pub head_row: u32,
    /// Cycle index.
    pub k: u64,
    /// Per-cycle re-roll hash `rain_hash32(h ^ k)`.
    pub hk: u32,
    /// Bright head this cycle (`(hk & 7) < 2` — ~1 stream in 4).
    pub bright: bool,
}

/// Resolve a column's cycle at `tick`. Brightness is probed from the
/// unadjusted tick first (the stammer adjustment depends on it); at a cycle
/// boundary the two probes can disagree for one tick, which is deterministic
/// and visually free (the stammer is a single held tick).
#[must_use]
#[inline]
pub const fn cycle_at(cp: &ColParams, fp: &FieldParams, tick: u64) -> CycleView {
    let (p, c) = (cp.p as u64, cp.c as u64);
    let phi = cp.phi as u64;
    let s0 = (tick + phi) / p;
    let k0 = s0 / c;
    let ha = rain_hash32(cp.h ^ (k0 as u32));
    let bright0 = (ha & 7) < 2;
    let eff_tick = if bright0 {
        stammer_tick(tick, fp.sq)
    } else {
        tick
    };
    let s = (eff_tick + phi) / p;
    let k = s / c;
    // rain_hash32 is a pure murmur3 finalizer, so when the stammer does not
    // cross a cycle boundary (k == k0) the k0 hash is bit-identical — reuse it.
    let hk = if k == k0 {
        ha
    } else {
        rain_hash32(cp.h ^ (k as u32))
    };
    CycleView {
        eff_tick,
        head_row: (s % c) as u32,
        k,
        hk,
        bright: (hk & 7) < 2,
    }
}

/// The tick at which cycle `k` started (head at row 0) — the density-ring
/// lookup key for cycle admission.
#[must_use]
#[inline]
pub const fn cycle_start_tick(cp: &ColParams, k: u64) -> u64 {
    (k * cp.c as u64 * cp.p as u64).saturating_sub(cp.phi as u64)
}

/// Cycle admission: the column rains this cycle iff its re-roll byte clears
/// the density staircase at the cycle start — admission flips only at cycle
/// boundaries, so intensity changes read as weather fronts, never popping.
#[must_use]
#[inline]
pub const fn cycle_active(hk: u32, density_byte: u8) -> bool {
    ((hk >> 8) & 0xFF) < density_byte as u32
}

/// Step-stable, time-based trail level (design §4): decay keys to elapsed
/// ticks since the head passed (`tick_ign = (k·c + r)·p - phi`), so per
/// stepping column only head/tail/`ceil(15/p)` bucket rows change per tick.
/// Returns the level `0..=15`, or `None` when unlit.
#[must_use]
#[inline]
pub const fn trail_level(cp: &ColParams, eff_tick: u64, k: u64, row: u32) -> Option<u32> {
    let ign = ((k * cp.c as u64 + row as u64) * cp.p as u64) as i64 - cp.phi as i64;
    let elapsed = eff_tick as i64 - ign;
    let lp = (cp.l * cp.p) as i64;
    if elapsed < 0 || elapsed > lp {
        return None;
    }
    let lvl = LEVELS - (elapsed * LEVELS as i64 / lp) as u32;
    Some(if lvl < LEVELS { lvl } else { LEVELS })
}

/// Flicker dither from a precomputed epoch: a per-row subtractive twinkle
/// `0..=3`. The epoch is `tick / dq` folded once per frame by the caller; this
/// is the single hash site so the byte layout cannot drift.
#[must_use]
#[inline]
pub const fn dither(h: u32, row: u32, epoch: u32) -> u32 {
    rain_hash32(h ^ row ^ epoch) & 3
}

/// The frame-invariant dither epoch `tick / dq` (`dq` a multiple of the
/// mutation quantum at/above the 350 ms floor). Hoisted once per frame.
#[must_use]
#[inline]
pub const fn dither_epoch(tick: u64, dq: u32) -> u32 {
    let dq = if dq > 1 { dq } else { 1 } as u64;
    (tick / dq) as u32
}

/// Glyph choice + mirror bit from a precomputed epoch:
/// `rain_hash32(seed ^ pack(r,c) ^ glyph_epoch)` — the single hash site, so the
/// byte layout cannot drift. The mirror rides bit 8 of the same hash.
#[must_use]
#[inline]
pub const fn glyph_from_epoch(seed32: u32, row: u32, col: u32, glyph_epoch: u32) -> (u32, bool) {
    let g = rain_hash32(seed32 ^ pack(row, col) ^ glyph_epoch);
    (g % GLYPH_COUNT, (g >> 8) & 1 == 1)
}

/// The frame-invariant glyph epoch `tick / mq`. `mq` is global, so swaps are
/// synchronized screen-wide and iso-luminant. Hoisted once per frame.
#[must_use]
#[inline]
pub const fn glyph_epoch(tick: u64, mq: u32) -> u32 {
    let mq = if mq > 1 { mq } else { 1 } as u64;
    (tick / mq) as u32
}

/// The effective per-frame quad cap: the GPU budget intersected with the
/// texel-derived CPU cap (`max(256, MAX_RAIN_TEXELS / (cw·ch))`) — retina
/// cells thin uniformly instead of blowing the CPU stamp bar.
#[must_use]
#[inline]
pub const fn quad_cap(cell_w: u32, cell_h: u32) -> usize {
    let cw = if cell_w > 1 { cell_w } else { 1 } as usize;
    let ch = if cell_h > 1 { cell_h } else { 1 } as usize;
    let cpu_cap = MAX_RAIN_TEXELS / (cw * ch);
    let floored = if cpu_cap > 256 { cpu_cap } else { 256 };
    if MAX_RAIN_QUADS < floored {
        MAX_RAIN_QUADS
    } else {
        floored
    }
}

/// Bounded ring of density-staircase change points `(tick, density_byte)`:
/// cycle admission looks up the density at the cycle-start tick, so the
/// weather history the field replays from is explicit engine state.
#[derive(Clone, Copy, Debug, Default)]
pub struct DensityRing {
    entries: [(u64, u8); DENSITY_RING],
    head: usize,
    len: usize,
}

impl DensityRing {
    /// Record a change point. Push only when the quantized byte changes —
    /// the caller (the weather EMA) owns that dedupe.
    #[inline]
    pub const fn push(&mut self, tick: u64, density: u8) {
        self.entries[self.head] = (tick, density);
        self.head = (self.head + 1) % DENSITY_RING;
        let next_len = self.len + 1;
        self.len = if next_len < DENSITY_RING {
            next_len
        } else {
            DENSITY_RING
        };
    }

    /// The density in force at `tick`: the newest change point at or before
    /// it. Ticks older than the whole ring resolve to the oldest retained
    /// entry (bounded history, best effort); an empty ring is density 0.
    #[must_use]
    #[inline]
    pub const fn at(&self, tick: u64) -> u8 {
        let mut oldest = 0u8;
        let mut idx = self.head;
        let mut i = 0;
        while i < self.len {
            // Newest-first walk: head-1 is the most recent entry.
            idx = if idx == 0 { DENSITY_RING - 1 } else { idx - 1 };
            let (t, d) = self.entries[idx];
            if t <= tick {
                return d;
            }
            oldest = d;
            i += 1;
        }
        oldest
    }

    /// Drop all change points (config rebuild / reset).
    #[inline]
    pub const fn clear(&mut self) {
        self.len = 0;
        self.head = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(rows: u32, tick_ms: u32, speed: u32, trail: u32) -> FieldParams {
        let (mq, dq, sq) = FieldParams::quanta(tick_ms, 133);
        FieldParams {
            seed32: rain_hash32(0xC0FF_EE00),
            rows,
            tick_ms,
            speed,
            trail,
            mq,
            dq,
            sq,
        }
    }

    /// Flash floor (design §4): for every clamped config and short-pane
    /// geometry, `c·p·tick_ms >= 1000`, so re-ignition stays under 1 Hz. Rows
    /// 3..=8 are the small grids.
    #[test]
    fn flash_floor_holds_at_all_clamped_configs() {
        for rows in [3u32, 4, 5, 6, 7, 8, 50, 120] {
            for fps in [12u32, 30, 60] {
                for speed in [1u32, 5, 10] {
                    for trail in [1u32, 5, 10] {
                        let fp = params(rows, 1000 / fps, speed, trail);
                        for col in 0..200 {
                            let cp = col_params(&fp, col);
                            assert!(cp.p >= 2, "a head steps at most every other tick");
                            assert!(
                                cp.c * cp.p * fp.tick_ms >= 1000,
                                "flash floor broken: rows={rows} fps={fps} speed={speed} \
                                 trail={trail} col={col} c={} p={} tick_ms={}",
                                cp.c,
                                cp.p,
                                fp.tick_ms
                            );
                        }
                    }
                }
            }
        }
    }

    /// A 1- or 2-row field (tiny split pane or window) never panics: the
    /// virtual 3-row cycle keeps the trail at its 3-row floor, `p >= 2`, and
    /// the flash floor.
    #[test]
    fn tiny_viewports_never_panic_and_keep_invariants() {
        for rows in [1u32, 2, 3] {
            for speed in [1u32, 5, 10] {
                for trail in [1u32, 5, 10] {
                    let fp = params(rows, 83, speed, trail);
                    for col in 0..100 {
                        let cp = col_params(&fp, col);
                        // rows <= 3 ⇒ the virtual field is exactly 3 rows,
                        // so the trail clamp pins l to its 3-row floor.
                        assert_eq!(cp.l, 3);
                        assert!(cp.p >= 2);
                        assert!(
                            cp.c * cp.p * fp.tick_ms >= 1000,
                            "flash floor broken at rows={rows}"
                        );
                    }
                }
            }
        }
    }

    /// The dither quantum is a multiple of the mutation quantum at or above
    /// the 350 ms `TWINKLE_GRID_MS` floor.
    #[test]
    fn dither_quantum_rides_the_mutation_grid() {
        for tick_ms in [16u32, 33, 83] {
            for mutation_ms in [80u32, 133, 500, 2000] {
                let (mq, dq, _) = FieldParams::quanta(tick_ms, mutation_ms);
                assert_eq!(dq % mq, 0, "dq must be a multiple of mq");
                assert!(
                    dq * tick_ms + tick_ms / 2 >= 350,
                    "dq below the 350ms floor"
                );
            }
        }
    }

    #[test]
    fn density_ring_looks_up_cycle_start() {
        let mut ring = DensityRing::default();
        assert_eq!(ring.at(100), 0, "empty ring is density 0");
        ring.push(10, 42);
        ring.push(50, 126);
        ring.push(90, 63);
        assert_eq!(ring.at(9), 42, "older than all → oldest retained");
        assert_eq!(ring.at(10), 42);
        assert_eq!(ring.at(60), 126);
        assert_eq!(ring.at(1000), 63);
        // Overflow retains the newest DENSITY_RING entries.
        for i in 0..12u64 {
            ring.push(100 + i, i as u8);
        }
        assert_eq!(ring.at(u64::MAX), 11);
        assert_eq!(ring.at(0), 4, "evicted history resolves to the oldest kept");
    }
}
