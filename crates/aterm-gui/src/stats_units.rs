// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The deck's units as the planner measures them (`docs/DESIGN-stats-deck-units-2026-11-04.md`
//! §2.4 and §4, A1; `docs/DESIGN-stats-deck-resources-2026-11-04.md` §3.1 and §3.5, A2;
//! `docs/DESIGN-stats-deck-instruments-2026-11-06.md` §4, §5 and §7, A4): each row that draws
//! units has a ladder of forms, widest first, and each form a width at a scale. Block A's GPU,
//! Memory, Disk and Heat rows draw them, and block B's Network and Processes rows; the CPU row
//! draws none, because the Cores row is its unit view (A4 §7).
//!
//! The planner counts widths at 1× ([`PLAN_SCALE`]), where a device px is a point, so the
//! hairlines (a wall, a rim, a seam) and the tiles of a half point cost the most, and a form fits
//! its column at every scale above. The tiles inside each form, and their paint, are A4's step N5.

use aterm_vitals::Snapshot;
use aterm_vitals::units::DISK_UNITS;

/// The scale the planner measures forms at: 1×, where hairlines cost the most (A1 §2.4).
pub(crate) const PLAN_SCALE: f64 = 1.0;

/// Between two tiles of a row's run, pt.
const TILE_GAP: f64 = 1.0;
/// Clear each side of a wall's hairline, pt (A1 §4.0).
const WALL_CLEAR: f64 = 1.0;
/// A GPU compartment, pt: rows of up to [`GPU_ROW`] (A1 §4.2).
const COMPARTMENT: f64 = 2.0;
/// The narrower compartment where the rows the cell holds take more than [`GPU_ROW`] each, pt:
/// at 1×, a GPU of 21 to 40 cores in two rows of up to [`GPU_FINE_ROW`].
const FINE_COMPARTMENT: f64 = 1.0;
/// Compartments in a row of [`COMPARTMENT`]s.
const GPU_ROW: u32 = 10;
/// Compartments in a row of [`FINE_COMPARTMENT`]s.
const GPU_FINE_ROW: u32 = 20;
/// The most GPU cores the tank divides; more draw it undivided (A1 ruling 11).
const GPU_COLUMN_MAX: u32 = 40;
/// The least height of a tank row, device px: a level needs three (A1 §4.2).
const GPU_ROW_PX: f64 = 3.0;
/// The undivided tank, pt.
const UNDIVIDED: f64 = 8.0;
/// The Neural Engine tile left of the tank, pt (A2 §3.1): 6 device px at 2×, 3 at 1×.
const NEURAL_ENGINE_TILE: f64 = 3.0;
/// From the Neural Engine tile to the tank's rim, pt: 4 device px at 2×, 2 at 1×.
const NEURAL_ENGINE_CLEAR: f64 = 2.0;
/// A memory tile, pt (A1 §4.3).
const MEMORY_TILE: f64 = 4.0;
/// The most memory tiles: the unit doubles until the total fits in these.
const MEMORY_TILES: u64 = 16;
/// The swap column past the waffle's wall, pt: up to two tiles stacked.
const SWAP_COLUMN: f64 = 4.0;
/// Between two device tiles, pt (A1 §4.4).
const DISK_GAP: f64 = 1.0;
/// The thermal-pressure ladder, pt (A4 §4): one column of four steps lit from the foot.
const LADDER: f64 = 3.0;
/// From the ladder to the first clock tile, and from the last watt tile to the work-per-joule
/// tile, pt: the chain's three groups read apart.
const CHAIN_GAP: f64 = 2.0;
/// The first CPU kind's clock tile (the Super cores', or a CPU of one kind's), pt.
const FIRST_CLOCK: f64 = 3.0;
/// The other kind's clock tile (the Efficiency cores'), pt: kinds are told apart by width, as
/// the Cores row's clusters were in A1 §4.1.
const LATER_CLOCK: f64 = 2.0;
/// The most clock tiles: the coalition split names the Super cores' cycles and the rest's
/// (A4 §4), and CPU Stats, which names each cluster, is read only on demand.
const CLOCKS_MAX: u8 = 2;
/// A watt tile (CPU, GPU, Neural Engine), pt.
const WATT_TILE: f64 = 3.0;
/// The most watt tiles: CPU, GPU and Neural Engine.
const WATTS_MAX: u8 = 3;
/// The work-per-joule tile, pt.
const WORK_TILE: f64 = 3.0;
/// A link's interface tile, pt (A4 §5.1).
const LINK_TILE: f64 = 3.0;
/// The interface tiles the Network form keeps the place of: the busiest four links that are up.
/// Links come and go with the machine's place, so the form keeps all four's place whether or not
/// they draw, as Disk keeps its second device's (A1 §2.4's as-built note), and no link moves a
/// column.
pub(crate) const LINK_TILES: u64 = 4;
/// The Processes flow tile, starts over exits at its seam, pt (A4 §6.3).
const FLOW_TILE: f64 = 5.0;
/// The unreaped pile's column, and the stuck column's, pt: 5 device px at 2×, so 3 at 1×.
const PILE_TILE: f64 = 2.5;

/// What the planner's measures read of the machine: what it has, which changes seldom or never,
/// not the readings its tiles draw.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct UnitFacts {
    /// The GPU, where its row reads one; without it the row draws no units, and no Neural
    /// Engine tile stands alone (A2 §3.1).
    pub(crate) gpu: Option<Gpu>,
    /// Memory's total bytes, where read.
    pub(crate) memory: Option<u64>,
    /// Whether the disk row reads devices here: its reader runs on this platform (A1 §4.4: macOS
    /// and Linux, not Windows). Not how many tiles a reading draws: the front tab's device comes
    /// and goes with the tab, so the Disk form keeps [`DISK_UNITS`] tiles' place whatever the
    /// reading draws, as a wall keeps its space (A1 §4.0), and a front-tab change moves no
    /// column (A1 §2.4).
    pub(crate) disk: bool,
    /// The heat chain's readers (A4 §4), where any runs; without them the Heat row draws no
    /// units and says it has no heat reading.
    pub(crate) heat: Option<Heat>,
    /// Whether the network row reads its links here (A4 §5.1): its reader runs on this platform
    /// (macOS's `NET_RT_IFLIST2`, Linux's `/proc/net/dev`), not which links are up.
    pub(crate) network: bool,
    /// Whether the processes row reads its flow, pile and stuck columns here (A4 §5.3).
    pub(crate) processes: bool,
}

/// The GPU's facts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Gpu {
    /// `gpu-core-count`, where read: the tank's compartments.
    pub(crate) cores: Option<u32>,
    /// Whether the Neural Engine tile stands left of the tank: its reader runs and its
    /// self-check holds (A2 §3.1). Where it fails the tile keeps no space.
    pub(crate) neural_engine: bool,
}

/// Which links of the heat chain have a reader here (A4 §4's units, left to right): a unit with
/// no reader is absent and keeps no width (A3's rule). A unit whose reader fails its self-check
/// (M-COAL) still reads, hollow, and keeps its width.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Heat {
    /// The pressure ladder: macOS's documented thermal-pressure level, or Linux's heuristic
    /// level in machine ink (A3).
    pub(crate) ladder: bool,
    /// The clock tiles, one a CPU kind the coalition split names: the Super cores' and the
    /// rest's on Apple silicon, one on a CPU of one kind, none where no clock is derived. More
    /// than [`CLOCKS_MAX`] draw that many.
    pub(crate) clocks: u8,
    /// The watt tiles past the wall: CPU, GPU and Neural Engine, each where read. More than
    /// [`WATTS_MAX`] draw that many.
    pub(crate) watts: u8,
    /// The work-per-joule tile: instructions and energy both read.
    pub(crate) work: bool,
}

/// The rows that draw units (A1 §2.1, A4 §5), in the order of a units column's forms
/// ([`Forms`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum UnitRow {
    Gpu,
    Memory,
    Disk,
    Heat,
    Network,
    Processes,
}

/// Each [`UnitRow`]'s form in a table's units column, in [`UnitRow::ALL`]'s order; `None` draws
/// no units on that row, or the row is not the table's.
pub(crate) type Forms = [Option<Form>; UnitRow::ALL.len()];

impl UnitRow {
    /// Every row that draws units, block A's top to bottom, then block B's.
    pub(crate) const ALL: [Self; 6] = [
        Self::Gpu,
        Self::Memory,
        Self::Disk,
        Self::Heat,
        Self::Network,
        Self::Processes,
    ];
    /// Folded block A's rows that draw units, top to bottom: the CPU row draws none (A4 §7).
    pub(crate) const BLOCK_A: [Self; 4] = [Self::Gpu, Self::Memory, Self::Disk, Self::Heat];
    /// Folded block B's rows that draw units, top to bottom (A4 §5): Latency's TCP waits tile
    /// lands with F8's Latency row.
    pub(crate) const BLOCK_B: [Self; 2] = [Self::Network, Self::Processes];

    /// Its place in [`Forms`].
    pub(crate) const fn index(self) -> usize {
        self as usize
    }

    /// Its forms, widest first (A1 §4.2–§4.4, A4 §4, §5): the row takes the first that its
    /// facts draw and that fits. None grows past [`Form::Column`] (A1 ruling 17).
    pub(crate) const fn ladder(self) -> &'static [Form] {
        match self {
            Self::Gpu => &[Form::Column, Form::Undivided],
            Self::Memory => &[Form::Column, Form::Half],
            Self::Disk | Self::Heat | Self::Network | Self::Processes => &[Form::Column],
        }
    }
}

/// How a row draws its units.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Form {
    /// A tile per unit of capacity or mechanism: a GPU core's compartment, a memory tile in the
    /// waffle, a device, each link of the heat chain, a link's interface tile, the processes'
    /// flow, pile and stuck columns.
    Column,
    /// GPU: one tank with no compartments.
    Undivided,
    /// Memory: the waffle at twice the tile, at most eight of them.
    Half,
}

/// The memory tile (A1 §4.3): u = 2^k GiB for the least integer k, negative allowed, with
/// ⌈`total` / u⌉ ≤ 16, and that many tiles; `None` for no memory. The unit is a power of two
/// bytes at least ⌈`total` / 16⌉, and 2^k GiB is every power of two bytes from 2^-30 GiB up.
pub(crate) fn memory_tiles(total: u64) -> Option<(u64, u64)> {
    (total > 0).then(|| {
        let unit = total.div_ceil(MEMORY_TILES).next_power_of_two();
        (unit, total.div_ceil(unit))
    })
}

/// A device px, pt, at `scale`: what a hairline costs.
fn hairline(scale: f64) -> f64 {
    1.0 / scale
}

/// `pt` in whole device px at `scale`, rounded up, pt: a tile of a half point is 3 px at 1×.
fn whole_px(pt: f64, scale: f64) -> f64 {
    (pt * scale).ceil() / scale
}

/// A wall past a row's units, pt (A1 §4.0): a device px in `words2` with a point clear each
/// side, 3 pt at 1× and 2.5 at 2×.
fn wall(scale: f64) -> f64 {
    2.0 * WALL_CLEAR + hairline(scale)
}

/// `n` items of `each` pt in a run with `gap` pt between them.
fn run(n: u64, each: f64, gap: f64) -> f64 {
    if n == 0 {
        return 0.0;
    }
    let n = n as f64;
    n * each + (n - 1.0) * gap
}

/// Two groups of a form beside each other, a wall between them where both stand, pt.
fn walled(left: f64, right: f64, scale: f64) -> f64 {
    if left > 0.0 && right > 0.0 {
        left + wall(scale) + right
    } else {
        left + right
    }
}

/// The units' height in whole device px at `scale` for a cell `cell` pt tall: 17 at 2× and 9 at
/// 1× in Folded's 8.5 pt cell (A1 §2.2).
fn units_px(cell: f64, scale: f64) -> f64 {
    (cell * scale).ceil()
}

impl UnitFacts {
    /// No facts: a deck that draws no units.
    pub(crate) const NONE: Self = Self {
        gpu: None,
        memory: None,
        disk: false,
        heat: None,
        network: false,
        processes: false,
    };

    /// What `snap` and this platform's readers give the units' widths: the GPU's core count,
    /// and the Neural Engine tile where its reader's self-check held (`Snapshot::ane` read, A2
    /// §3.1); memory's total; the block reader's platforms (macOS and Linux, A1 §4.4); the heat
    /// chain's readers (A4 §4: on macOS the pressure level, a clock a CPU kind the coalition
    /// split names, the CPU's watts and the GPU's and the Neural Engine's where read, and the
    /// work per joule; on Linux the heuristic level alone; on Windows none); and the network and
    /// processes rows, whose readers run on macOS and Linux. Each changes
    /// seldom or never: none is a reading the units draw. A4's step N5 plans the painted deck
    /// with these as it paints the units; its step N2 builds the heat readers they name.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "A4 step N5 plans the painted deck with the host's facts"
        )
    )]
    pub(crate) fn of(snap: &Snapshot) -> Self {
        let kinds = snap
            .cpu
            .as_ref()
            .map_or(0, |cpu| {
                cpu.clusters.iter().filter(|c| !c.cores.is_empty()).count()
            })
            .max(usize::from(snap.total_cores().is_some_and(|n| n > 0)));
        let neural_engine = matches!(snap.ane, Some(Ok(_)));
        let gpu = snap.gpu.as_ref().map(|gpu| Gpu {
            cores: gpu.cores.map(u32::from),
            neural_engine,
        });
        let heat = if cfg!(target_os = "macos") {
            Some(Heat {
                ladder: true,
                clocks: u8::try_from(kinds).unwrap_or(u8::MAX).min(CLOCKS_MAX),
                watts: 1 + u8::from(gpu.is_some()) + u8::from(neural_engine),
                work: true,
            })
        } else if cfg!(target_os = "linux") {
            // No watt tile yet: N2 reads RAPL's package energy and NVML's board power into
            // `snap.heat.watts`, and N5 plans their tiles here from those readings (A4 §4).
            Some(Heat {
                ladder: true,
                clocks: 0,
                watts: 0,
                work: false,
            })
        } else {
            None
        };
        // The block, link and process readers' platforms (`aterm_sysprobe::blockio`, `iflist`,
        // the sweep): elsewhere those rows draw no units.
        let readers = cfg!(any(target_os = "macos", target_os = "linux"));
        Self {
            gpu,
            memory: snap
                .mem
                .as_ref()
                .map(|mem| mem.sample.total)
                .filter(|&total| total > 0),
            disk: readers,
            heat,
            network: readers,
            processes: readers,
        }
    }

    /// How wide `row` draws `form` at `scale` in a cell `cell` pt tall, pt; `None` where its
    /// facts give that form nothing to draw, or the form is not on the row's ladder. A wall and
    /// what stands past it keep their space whether or not they draw (A1 §4.0).
    pub(crate) fn width(&self, row: UnitRow, form: Form, scale: f64, cell: f64) -> Option<f64> {
        if !row.ladder().contains(&form) {
            return None;
        }
        match row {
            UnitRow::Gpu => self.gpu_width(form, scale, cell),
            UnitRow::Memory => self.memory_width(form, scale),
            UnitRow::Disk => self.disk_width(scale, cell),
            UnitRow::Heat => self.heat_width(scale),
            UnitRow::Network => self.network.then(|| run(LINK_TILES, LINK_TILE, TILE_GAP)),
            UnitRow::Processes => self.processes.then(|| {
                let pile = whole_px(PILE_TILE, scale);
                walled(FLOW_TILE, pile + TILE_GAP + pile, scale)
            }),
        }
    }

    /// Each of `rows`' forms for a column at most `cap` pt wide at [`PLAN_SCALE`]: the first of
    /// its ladder that its facts draw within `cap`, else `None`, as every row not in `rows`.
    pub(crate) fn forms_within(&self, cap: f64, cell: f64, rows: &[UnitRow]) -> Forms {
        let mut forms = [None; UnitRow::ALL.len()];
        for &row in rows {
            forms[row.index()] = row.ladder().iter().copied().find(|&form| {
                self.width(row, form, PLAN_SCALE, cell)
                    .is_some_and(|w| w <= cap)
            });
        }
        forms
    }

    /// Whether any of `rows`' facts draw any form at all.
    pub(crate) fn draw_any(&self, cell: f64, rows: &[UnitRow]) -> bool {
        self.forms_within(f64::INFINITY, cell, rows)
            .iter()
            .any(Option::is_some)
    }

    /// The widest of `forms` at [`PLAN_SCALE`], pt; 0 where none draws.
    pub(crate) fn widest(&self, forms: &Forms, cell: f64) -> f64 {
        UnitRow::ALL
            .into_iter()
            .filter_map(|row| self.width(row, forms[row.index()]?, PLAN_SCALE, cell))
            .fold(0.0, f64::max)
    }

    /// The GPU (A1 §4.2, A2 §3.1): the tank, and the Neural Engine tile and its clear left of it
    /// where the tile stands.
    fn gpu_width(&self, form: Form, scale: f64, cell: f64) -> Option<f64> {
        let gpu = self.gpu?;
        let tank = match form {
            Form::Column => {
                let n = gpu.cores.filter(|n| (1..=GPU_COLUMN_MAX).contains(n))?;
                // Rows of three device px or more, seams of one, inside a one px rim.
                let interior = units_px(cell, scale) - 2.0;
                let most_rows = ((interior + 1.0) / (GPU_ROW_PX + 1.0)).floor();
                let rows = f64::from(n.div_ceil(GPU_ROW)).min(most_rows);
                if rows < 1.0 {
                    return None;
                }
                let cols = (f64::from(n) / rows).ceil();
                let compartment = if cols <= f64::from(GPU_ROW) {
                    COMPARTMENT
                } else if cols <= f64::from(GPU_FINE_ROW) {
                    FINE_COMPARTMENT
                } else {
                    return None;
                };
                cols * compartment + (cols + 1.0) * hairline(scale)
            }
            Form::Undivided => UNDIVIDED,
            Form::Half => return None,
        };
        let tile = if gpu.neural_engine {
            NEURAL_ENGINE_TILE + NEURAL_ENGINE_CLEAR
        } else {
            0.0
        };
        Some(tile + tank)
    }

    /// Memory (A1 §4.3): the waffle's two rows, tile k in column ⌊k/2⌋, then the wall and the
    /// swap column.
    fn memory_width(&self, form: Form, scale: f64) -> Option<f64> {
        let (unit, tiles) = memory_tiles(self.memory?)?;
        let tiles = match form {
            Form::Column => tiles,
            Form::Half => self.memory?.div_ceil(2 * unit),
            Form::Undivided => return None,
        };
        Some(run(tiles.div_ceil(2), MEMORY_TILE, TILE_GAP) + wall(scale) + SWAP_COLUMN)
    }

    /// Disk (A1 §4.4): a square tile a device, the cell's height, a point apart, and the place of
    /// [`DISK_UNITS`] of them, home's first, whether or not the front tab's device draws.
    fn disk_width(&self, scale: f64, cell: f64) -> Option<f64> {
        let tile = units_px(cell, scale) / scale;
        self.disk.then(|| run(DISK_UNITS as u64, tile, DISK_GAP))
    }

    /// Heat (A4 §4), read left to right as the thermal control chain: the pressure ladder, the
    /// clock tiles (the first kind's wider), a wall, the watt tiles and the work-per-joule tile.
    /// Each stands where its reader runs; the wall stands where something stands each side of it.
    fn heat_width(&self, scale: f64) -> Option<f64> {
        let heat = self.heat?;
        let clocks = match heat.clocks.min(CLOCKS_MAX) {
            0 => 0.0,
            1 => FIRST_CLOCK,
            _ => FIRST_CLOCK + TILE_GAP + LATER_CLOCK,
        };
        let ladder = if heat.ladder { LADDER } else { 0.0 };
        let watts = run(u64::from(heat.watts.min(WATTS_MAX)), WATT_TILE, TILE_GAP);
        let work = if heat.work { WORK_TILE } else { 0.0 };
        let apart = |a: f64, b: f64| {
            if a > 0.0 && b > 0.0 {
                a + CHAIN_GAP + b
            } else {
                a + b
            }
        };
        let w = walled(apart(ladder, clocks), apart(watts, work), scale);
        (w > 0.0).then_some(w)
    }
}

#[cfg(test)]
#[path = "stats_units_tests.rs"]
mod tests;
