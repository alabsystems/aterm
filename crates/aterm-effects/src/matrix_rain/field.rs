// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Pure `(seed32, FieldParams, tick)` u32 lattice math for the phosphor rain field.

/// Hard per-frame quad cap on the GPU path.
pub const MAX_RAIN_QUADS: usize = 2048;
/// CPU texel budget (`2048 * 10 * 20`), floored at 256 quads in [`quad_cap`].
pub const MAX_RAIN_TEXELS: usize = 409_600;
/// Bright-head additive halo budget (`rain_add` `GlowQuad`s per frame).
pub const MAX_RAIN_ADD: usize = 64;
/// Head-brightness trail depth (`0..=15`, 16 ramp tints).
pub const LEVELS: u32 = 15;
/// Glyph ROM size modulo which [`glyph_from_epoch`] reduces its hash.
pub const GLYPH_COUNT: u32 = 64;
/// Capacity of [`DensityRing`] for density change points.
pub const DENSITY_RING: usize = 8;

/// Murmur3 32-bit finalizer used as the single field hash.
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

/// Half-open LSB-first bit slice `(x >> a) & ((1 << (b - a)) - 1)`.
#[must_use]
#[inline]
pub const fn bits(x: u32, a: u32, b: u32) -> u32 {
    (x >> a) & ((1 << (b - a)) - 1)
}

/// Pack `(row, col)` into a 32-bit key `(row << 16) | col` for grids `< 2^16`.
#[must_use]
#[inline]
pub const fn pack(row: u32, col: u32) -> u32 {
    (row << 16) | col
}

/// Config- and geometry-derived field parameters rebuilt on config or viewport change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FieldParams {
    /// Host-mixed 32-bit seed.
    pub seed32: u32,
    /// Live viewport row count used by the flash floor.
    pub rows: u32,
    /// Base tick period in milliseconds (`1000 / fps`).
    pub tick_ms: u32,
    /// Speed knob `1..=10` (`5` = neutral) shifting the column step period.
    pub speed: u32,
    /// Trail knob `1..=10` (`5` = neutral) scaling the column trail length.
    pub trail: u32,
    /// Mutation quantum in ticks: `max(1, round(mutation_ms / tick_ms))`.
    pub mq: u32,
    /// Dither quantum: smallest multiple of `mq` at or above `ticks_of(350, tick_ms)`.
    pub dq: u32,
    /// Stammer quantum (`ticks_of(500, tick_ms)`), holding bright heads once every 8 windows.
    pub sq: u32,
}

/// Round `ms` to whole ticks of `tick_ms`, floored at 1.
#[must_use]
#[inline]
pub const fn ticks_of(ms: u32, tick_ms: u32) -> u32 {
    let t = if tick_ms > 1 { tick_ms } else { 1 };
    let n = (ms + t / 2) / t;
    if n > 1 { n } else { 1 }
}

impl FieldParams {
    /// Derive `(mq, dq, sq)` in ticks from `tick_ms` and `mutation_ms`.
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

/// Per-column derived parameters from `(seed32, col, FieldParams)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ColParams {
    /// Column hash root `rain_hash32(seed32 ^ ((col << 1) | 1))`.
    pub h: u32,
    /// Step period in ticks per row (`2..=16`).
    pub p: u32,
    /// Trail length in rows (`3..=rows`).
    pub l: u32,
    /// Off-screen gap in rows, extended to satisfy the 1 Hz flash floor.
    pub g: u32,
    /// Cycle length `rows + l + g`.
    pub c: u32,
    /// Phase offset in ticks (`< c * p`).
    pub phi: u32,
}

/// Compute column parameters with `g` extended so `c * p * tick_ms >= 1000` (WCAG 2.3.1 `< 1 Hz` re-ignition).
#[must_use]
#[inline]
pub const fn col_params(fp: &FieldParams, col: u32) -> ColParams {
    let rows = if fp.rows > 3 { fp.rows } else { 3 };
    let h = rain_hash32(fp.seed32 ^ ((col << 1) | 1));
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
    let g_style = l / 2 + bits(h, 4, 7) * l / 8;
    let tick_ms = if fp.tick_ms > 1 { fp.tick_ms } else { 1 };
    let floor_c = 1000u32.div_ceil(p * tick_ms);
    let min_g = floor_c.saturating_sub(rows + l);
    let g = if g_style > min_g { g_style } else { min_g };
    let c = rows + l + g;
    let phi = rain_hash32(h) % (c * p);
    ColParams { h, p, l, g, c, phi }
}

/// Hold bright-head columns one extra tick in the 8th stammer window: `tick - ((tick / sq) % 8 == 7)`.
#[must_use]
#[inline]
pub const fn stammer_tick(tick: u64, sq: u32) -> u64 {
    let sq = if sq > 1 { sq } else { 1 } as u64;
    tick - ((tick / sq) % 8 == 7) as u64
}

/// Resolved cycle state for a column at `tick`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CycleView {
    /// Effective tick after bright-head stammer adjustment.
    pub eff_tick: u64,
    /// Head position within the cycle (`0..c`, on-screen while `< rows`).
    pub head_row: u32,
    /// Cycle index.
    pub k: u64,
    /// Per-cycle re-roll hash `rain_hash32(h ^ k)`.
    pub hk: u32,
    /// Whether this cycle has a bright head (`(hk & 7) < 2`).
    pub bright: bool,
}

/// Resolve a column's cycle at `tick`, probing initial brightness before applying [`stammer_tick`].
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

/// Tick at which cycle `k` started (head at row 0), used as the [`DensityRing`] lookup key.
#[must_use]
#[inline]
pub const fn cycle_start_tick(cp: &ColParams, k: u64) -> u64 {
    (k * cp.c as u64 * cp.p as u64).saturating_sub(cp.phi as u64)
}

/// Return whether a column rains in cycle `k` (`((hk >> 8) & 0xFF) < density_byte`).
#[must_use]
#[inline]
pub const fn cycle_active(hk: u32, density_byte: u8) -> bool {
    ((hk >> 8) & 0xFF) < density_byte as u32
}

/// Time-based trail level `0..=15` at `row` from elapsed ticks since head ignition, or `None` when unlit.
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

/// Per-row subtractive twinkle `0..=3` hashed from `(h, row, epoch)`.
#[must_use]
#[inline]
pub const fn dither(h: u32, row: u32, epoch: u32) -> u32 {
    rain_hash32(h ^ row ^ epoch) & 3
}

/// Frame-invariant dither epoch `tick / dq`.
#[must_use]
#[inline]
pub const fn dither_epoch(tick: u64, dq: u32) -> u32 {
    let dq = if dq > 1 { dq } else { 1 } as u64;
    (tick / dq) as u32
}

/// Glyph index (`0..GLYPH_COUNT`) and horizontal mirror bit (bit 8) for `(row, col, glyph_epoch)`.
#[must_use]
#[inline]
pub const fn glyph_from_epoch(seed32: u32, row: u32, col: u32, glyph_epoch: u32) -> (u32, bool) {
    let g = rain_hash32(seed32 ^ pack(row, col) ^ glyph_epoch);
    (g % GLYPH_COUNT, (g >> 8) & 1 == 1)
}

/// Frame-invariant glyph mutation epoch `tick / mq`.
#[must_use]
#[inline]
pub const fn glyph_epoch(tick: u64, mq: u32) -> u32 {
    let mq = if mq > 1 { mq } else { 1 } as u64;
    (tick / mq) as u32
}

/// Effective per-frame quad cap: `min(MAX_RAIN_QUADS, max(256, MAX_RAIN_TEXELS / (cell_w * cell_h)))`.
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

/// Bounded ring of density-staircase change points `(tick, density_byte)` for cycle-start lookups.
#[derive(Clone, Copy, Debug)]
pub struct DensityRing {
    entries: [(u64, u8); DENSITY_RING],
    head: usize,
    len: usize,
}

impl Default for DensityRing {
    #[inline]
    fn default() -> Self {
        Self::EMPTY
    }
}

impl DensityRing {
    /// Empty density-change ring before any ticks are recorded.
    pub const EMPTY: Self = Self {
        entries: [(0, 0); DENSITY_RING],
        head: 0,
        len: 0,
    };
    /// Record a `(tick, density)` change point once deduplicated by the caller.
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

    /// Return the newest density change point at or before `tick` (or the oldest retained entry, or `0` if empty).
    #[must_use]
    #[inline]
    pub const fn at(&self, tick: u64) -> u8 {
        let mut oldest = 0u8;
        let mut idx = self.head;
        let mut i = 0;
        while i < self.len {
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

    /// Clear all recorded change points.
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

    /// Verify `c * p * tick_ms >= 1000` across clamped configs and viewport heights.
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

    /// Verify that 1–2-row viewports clamp cleanly to a 3-row virtual cycle.
    #[test]
    fn tiny_viewports_never_panic_and_keep_invariants() {
        for rows in [1u32, 2, 3] {
            for speed in [1u32, 5, 10] {
                for trail in [1u32, 5, 10] {
                    let fp = params(rows, 83, speed, trail);
                    for col in 0..100 {
                        let cp = col_params(&fp, col);
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

    /// Verify that `dq` is a multiple of `mq` at or above the 350 ms floor.
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
        for i in 0..12u64 {
            ring.push(100 + i, i as u8);
        }
        assert_eq!(ring.at(u64::MAX), 11);
        assert_eq!(ring.at(0), 4, "evicted history resolves to the oldest kept");
    }
}
