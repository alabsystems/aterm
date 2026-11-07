// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The deck's units as the planner measures them (`docs/DESIGN-stats-deck-units-2026-11-04.md`
//! §2.4 and §4, A1; `docs/DESIGN-stats-deck-resources-2026-11-04.md` §3.1 and §3.5, A2): each of
//! block A's rows has a ladder of forms, widest first, and each form a width at a scale.
//!
//! The planner counts widths at 1× ([`PLAN_SCALE`]), where a device px is a point, so the
//! hairlines (a wall, a rim, a seam) cost the most and a form fits its column at every scale
//! above. The tiles inside each form, and their paint, are A1's steps R5 and R6.

use aterm_vitals::Snapshot;
use aterm_vitals::units::DISK_UNITS;

/// The scale the planner measures forms at: 1×, where hairlines cost the most (A1 §2.4).
pub(crate) const PLAN_SCALE: f64 = 1.0;

/// A Super (`hw.perflevel0`) core's tile, pt (A1 §4.1); the tile of a machine with one cluster.
const FIRST_CLUSTER_TILE: f64 = 3.0;
/// Every other cluster's core tile, pt: core types are told apart by width (A1 ruling 4).
const LATER_CLUSTER_TILE: f64 = 2.0;
/// Between two tiles of a row's run, pt.
const TILE_GAP: f64 = 1.0;
/// Between two CPU clusters, pt.
const CLUSTER_GAP: f64 = 2.0;
/// The CPU's Cluster form's tile, one a cluster, pt.
const CLUSTER_TILE: f64 = 3.0;
/// The CPU's queue tile past its wall, pt.
const QUEUE_TILE: f64 = 3.0;
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

/// What the units' widths read of the machine: what it has, which changes seldom or never, not
/// the readings its tiles draw.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct UnitFacts<'a> {
    /// Logical cores per CPU cluster, fastest first (`hw.perflevel0` first); empty where the CPU
    /// is unread. A cluster of no cores draws nothing.
    pub(crate) clusters: &'a [usize],
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
}

/// What the host's reading gives the units' widths, owned so a deck keeps it beside its plan
/// ([`Self::facts`] lends the [`UnitFacts`] the planner reads): the cores per cluster in the
/// reading's cluster order (`hw.perflevel0` first) or, with no cluster read, every core in one;
/// the GPU's core count, and the Neural Engine tile where its reader's self-check held
/// (`Snapshot::ane` read, A2 §3.1); memory's total; and whether this platform's block reader
/// reads devices (macOS and Linux, A1 §4.4). Each changes seldom or never: none is a reading the
/// units draw. A1's step R5 plans both deck forms from these facts in its laws; step R6 plans the
/// painted deck with them as it paints the units.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "A1 step R6 plans the painted deck with the host's facts"
    )
)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HostFacts {
    clusters: Vec<usize>,
    gpu: Option<Gpu>,
    memory: Option<u64>,
    disk: bool,
}

#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "A1 step R6 plans the painted deck with the host's facts"
    )
)]
impl HostFacts {
    /// What `snap` reads of the machine.
    pub(crate) fn of(snap: &Snapshot) -> Self {
        let clusters = snap
            .cpu
            .as_ref()
            .map(|cpu| {
                cpu.clusters
                    .iter()
                    .map(|c| c.cores.len())
                    .collect::<Vec<_>>()
            })
            .filter(|counts| counts.iter().any(|&n| n > 0))
            .or_else(|| snap.total_cores().map(|n| vec![n]))
            .unwrap_or_default();
        Self {
            clusters,
            gpu: snap.gpu.as_ref().map(|gpu| Gpu {
                cores: gpu.cores.map(u32::from),
                neural_engine: matches!(snap.ane, Some(Ok(_))),
            }),
            memory: snap
                .mem
                .as_ref()
                .map(|mem| mem.sample.total)
                .filter(|&total| total > 0),
            // The block reader's platforms (`aterm_sysprobe::blockio`): elsewhere the Disk row
            // draws no units.
            disk: cfg!(any(target_os = "macos", target_os = "linux")),
        }
    }

    /// The facts the planner reads.
    pub(crate) fn facts(&self) -> UnitFacts<'_> {
        UnitFacts {
            clusters: &self.clusters,
            gpu: self.gpu,
            memory: self.memory,
            disk: self.disk,
        }
    }
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

/// Block A's rows, the ones that draw units (A1 §2.1); block B draws none (A1 ruling 15).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum UnitRow {
    Cpu,
    Gpu,
    Memory,
    Disk,
}

impl UnitRow {
    /// Block A's rows, top to bottom.
    pub(crate) const ALL: [Self; 4] = [Self::Cpu, Self::Gpu, Self::Memory, Self::Disk];

    /// Its forms, widest first (A1 §4.1–§4.4): the row takes the first that its facts draw and
    /// that fits. None grows past [`Form::Column`] (A1 ruling 17).
    pub(crate) const fn ladder(self) -> &'static [Form] {
        match self {
            Self::Cpu => &[Form::Column, Form::Stack, Form::Cluster],
            Self::Gpu => &[Form::Column, Form::Undivided],
            Self::Memory => &[Form::Column, Form::Half],
            Self::Disk => &[Form::Column],
        }
    }
}

/// How a row draws its units.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Form {
    /// A tile per unit of capacity: a core, a GPU core's compartment, a memory tile in the
    /// waffle, a device.
    Column,
    /// CPU: each cluster's cores in two half-height rows, core k in column ⌊k/2⌋.
    Stack,
    /// CPU: one tile per cluster, at its mean.
    Cluster,
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

/// The units' height in whole device px at `scale` for a cell `cell` pt tall: 17 at 2× and 9 at
/// 1× in Folded's 8.5 pt cell (A1 §2.2).
fn units_px(cell: f64, scale: f64) -> f64 {
    (cell * scale).ceil()
}

impl UnitFacts<'static> {
    /// No facts: a deck that draws no units.
    pub(crate) const NONE: Self = Self {
        clusters: &[],
        gpu: None,
        memory: None,
        disk: false,
    };
}

impl UnitFacts<'_> {
    /// How wide `row` draws `form` at `scale` in a cell `cell` pt tall, pt; `None` where its
    /// facts give that form nothing to draw, or the form is not on the row's ladder. A wall and
    /// what stands past it keep their space whether or not they draw (A1 §4.0).
    pub(crate) fn width(&self, row: UnitRow, form: Form, scale: f64, cell: f64) -> Option<f64> {
        if !row.ladder().contains(&form) {
            return None;
        }
        match row {
            UnitRow::Cpu => self.cpu_width(form, scale),
            UnitRow::Gpu => self.gpu_width(form, scale, cell),
            UnitRow::Memory => self.memory_width(form, scale),
            UnitRow::Disk => self.disk_width(scale, cell),
        }
    }

    /// Each row's form for a column at most `cap` pt wide at [`PLAN_SCALE`]: the first of its
    /// ladder that its facts draw within `cap`, else `None`.
    pub(crate) fn forms_within(&self, cap: f64, cell: f64) -> [Option<Form>; 4] {
        UnitRow::ALL.map(|row| {
            row.ladder().iter().copied().find(|&form| {
                self.width(row, form, PLAN_SCALE, cell)
                    .is_some_and(|w| w <= cap)
            })
        })
    }

    /// Whether any row's facts draw any form at all.
    pub(crate) fn draw_any(&self, cell: f64) -> bool {
        self.forms_within(f64::INFINITY, cell)
            .iter()
            .any(Option::is_some)
    }

    /// The CPU (A1 §4.1): its clusters' tiles, fastest first, then the wall and the queue tile.
    fn cpu_width(&self, form: Form, scale: f64) -> Option<f64> {
        let counts = || self.clusters.iter().filter(|&&n| n > 0).map(|&n| n as u64);
        let k = counts().count();
        if k == 0 {
            return None;
        }
        // The first cluster's tiles are the wider: core types are told apart by width.
        let tile = |i: usize| {
            if i == 0 {
                FIRST_CLUSTER_TILE
            } else {
                LATER_CLUSTER_TILE
            }
        };
        let clusters: f64 = match form {
            Form::Column => counts()
                .enumerate()
                .map(|(i, n)| run(n, tile(i), TILE_GAP))
                .sum(),
            Form::Stack => counts()
                .enumerate()
                .map(|(i, n)| run(n.div_ceil(2), tile(i), TILE_GAP))
                .sum(),
            Form::Cluster => k as f64 * CLUSTER_TILE,
            Form::Undivided | Form::Half => return None,
        };
        let gaps = (k - 1) as f64 * CLUSTER_GAP;
        Some(clusters + gaps + wall(scale) + QUEUE_TILE)
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
            Form::Stack | Form::Cluster | Form::Half => return None,
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
            Form::Stack | Form::Cluster | Form::Undivided => return None,
        };
        Some(run(tiles.div_ceil(2), MEMORY_TILE, TILE_GAP) + wall(scale) + SWAP_COLUMN)
    }

    /// Disk (A1 §4.4): a square tile a device, the cell's height, a point apart, and the place of
    /// [`DISK_UNITS`] of them, home's first, whether or not the front tab's device draws.
    fn disk_width(&self, scale: f64, cell: f64) -> Option<f64> {
        let tile = units_px(cell, scale) / scale;
        self.disk.then(|| run(DISK_UNITS as u64, tile, DISK_GAP))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: u64 = 1 << 30;
    /// The Folded cell, pt.
    const CELL: f64 = 8.5;

    /// This Mac (A1 §1.3): 4 Super and 6 Efficiency cores, a 10-core GPU with a Neural Engine,
    /// 32 GiB and a disk reader.
    fn this_mac() -> UnitFacts<'static> {
        UnitFacts {
            clusters: &[4, 6],
            gpu: Some(Gpu {
                cores: Some(10),
                neural_engine: true,
            }),
            memory: Some(32 * GIB),
            disk: true,
        }
    }

    fn at(facts: &UnitFacts<'_>, row: UnitRow, form: Form) -> [Option<f64>; 2] {
        [1.0, 2.0].map(|s| facts.width(row, form, s, CELL))
    }

    /// The forms measure what A1 §2.4, §4.1–§4.4 and A2 §3.1 say: this Mac at 1× (CPU 40, GPU
    /// 31 and 36 with the Neural Engine, Memory 46, Disk 19 for two devices' place) and at 2× (a
    /// hairline is half a point there), a 12 + 4 core Max's Stack at 36, an Ultra's Cluster at
    /// 14, the tank's rows at 16 and 40 cores, and the undivided tank over 40 or with no count.
    #[test]
    fn the_forms_measure_what_the_design_says() {
        use Form::{Cluster, Column, Half, Stack, Undivided};
        use UnitRow::{Cpu, Disk, Memory};
        let mac = this_mac();
        assert_eq!(at(&mac, Cpu, Column), [Some(40.0), Some(39.5)]);
        assert_eq!(at(&mac, Cpu, Stack), [Some(23.0), Some(22.5)]);
        assert_eq!(at(&mac, Cpu, Cluster), [Some(14.0), Some(13.5)]);
        assert_eq!(at(&mac, UnitRow::Gpu, Column), [Some(36.0), Some(30.5)]);
        assert_eq!(at(&mac, UnitRow::Gpu, Undivided), [Some(13.0), Some(13.0)]);
        assert_eq!(at(&mac, Memory, Column), [Some(46.0), Some(45.5)]);
        assert_eq!(at(&mac, Memory, Half), [Some(26.0), Some(25.5)]);
        // Two square tiles a point apart: 9 + 1 + 9 px at 1×, 17 + 2 + 17 at 2× (A1 §4.4).
        assert_eq!(at(&mac, Disk, Column), [Some(19.0), Some(18.0)]);

        let max = UnitFacts {
            clusters: &[12, 4],
            ..mac
        };
        assert_eq!(at(&max, Cpu, Column), [Some(66.0), Some(65.5)]);
        assert_eq!(at(&max, Cpu, Stack), [Some(36.0), Some(35.5)]);
        let ultra = UnitFacts {
            clusters: &[24, 8],
            ..mac
        };
        assert_eq!(at(&ultra, Cpu, Stack), [Some(66.0), Some(65.5)]);
        assert_eq!(at(&ultra, Cpu, Cluster), [Some(14.0), Some(13.5)]);

        let tank = |cores: Option<u32>| UnitFacts {
            gpu: Some(Gpu {
                cores,
                neural_engine: false,
            }),
            ..mac
        };
        // 10 cores: 10 × 4 px + 9 seams + the rim = 51 px at 2×; 31 px at 1×.
        assert_eq!(
            at(&tank(Some(10)), UnitRow::Gpu, Column),
            [Some(31.0), Some(25.5)]
        );
        assert_eq!(
            at(&tank(Some(16)), UnitRow::Gpu, Column),
            [Some(25.0), Some(20.5)]
        );
        // 40 cores: 4 × 10 at 2×, 2 × 20 of a point at 1×.
        assert_eq!(
            at(&tank(Some(40)), UnitRow::Gpu, Column),
            [Some(41.0), Some(25.5)]
        );
        for cores in [Some(41), Some(80), Some(0), None] {
            let gpu = tank(cores);
            assert_eq!(at(&gpu, UnitRow::Gpu, Column), [None; 2], "{cores:?}");
            assert_eq!(
                at(&gpu, UnitRow::Gpu, Undivided),
                [Some(8.0); 2],
                "{cores:?}"
            );
        }

        // Where no reader runs, the Disk row draws no units.
        let diskless = UnitFacts { disk: false, ..mac };
        assert_eq!(at(&diskless, Disk, Column), [None; 2]);

        // Unread, a row draws nothing, and no form off its ladder draws.
        let none = UnitFacts::NONE;
        for row in UnitRow::ALL {
            for form in [Column, Stack, Cluster, Undivided, Half] {
                assert_eq!(none.width(row, form, 1.0, CELL), None, "{row:?} {form:?}");
                if !row.ladder().contains(&form) {
                    assert_eq!(mac.width(row, form, 1.0, CELL), None, "{row:?} {form:?}");
                }
            }
        }
        assert!(!none.draw_any(CELL) && mac.draw_any(CELL));
        let gpuless = UnitFacts { gpu: None, ..mac };
        assert_eq!(at(&gpuless, UnitRow::Gpu, Undivided), [None; 2]);
    }

    /// A1 §2.4's choice, before the planner: for every machine shape (1 to 128 cores in one to
    /// four clusters, GPUs of no count and 0 to 80 cores with and without the Neural Engine, 1
    /// to 512 GiB, a disk reader or none), as the cap grows no row's form steps down its ladder,
    /// every form taken is within the cap at 1× and no wider at 2×, and a row takes nothing
    /// only where no form of its ladder fits. CONTROL: each form of each ladder is taken
    /// somewhere in the sweep.
    #[test]
    fn a_wider_cap_never_takes_a_smaller_form() {
        let rank = |row: UnitRow, form: Option<Form>| {
            form.and_then(|f| row.ladder().iter().position(|&g| g == f))
                .unwrap_or(row.ladder().len())
        };
        let mut shapes: Vec<Vec<usize>> = Vec::new();
        for n in 1..=128_usize {
            for k in 1..=4_usize.min(n) {
                shapes.push((0..k).map(|i| n / k + usize::from(i < n % k)).collect());
            }
        }
        let gpus = (0..=80)
            .map(Some)
            .chain([None])
            .flat_map(|cores| {
                [false, true].map(|neural_engine| Gpu {
                    cores,
                    neural_engine,
                })
            })
            .map(Some)
            .chain([None]);
        let mut taken = std::collections::HashSet::new();
        let mut check = |facts: UnitFacts<'_>| {
            let mut last = [usize::MAX; 4];
            for cap in 0..=140 {
                let cap = f64::from(cap);
                let forms = facts.forms_within(cap, CELL);
                for (i, (row, form)) in UnitRow::ALL.into_iter().zip(forms).enumerate() {
                    let r = rank(row, form);
                    assert!(
                        r <= last[i],
                        "{facts:?}: {row:?} steps down to {form:?} at {cap}"
                    );
                    last[i] = r;
                    let Some(form) = form else {
                        assert!(
                            row.ladder().iter().all(|&f| facts
                                .width(row, f, PLAN_SCALE, CELL)
                                .is_none_or(|w| w > cap)),
                            "{facts:?}: {row:?} takes nothing in {cap}"
                        );
                        continue;
                    };
                    taken.insert((row, form));
                    let [w1, w2] = at(&facts, row, form).map(|w| w.expect("taken, so drawn"));
                    assert!(
                        w1 <= cap && w2 <= w1,
                        "{facts:?}: {row:?} {form:?} in {cap}"
                    );
                }
            }
        };
        let mac = this_mac();
        for clusters in &shapes {
            check(UnitFacts {
                clusters,
                ..UnitFacts::NONE
            });
        }
        for gpu in gpus {
            check(UnitFacts {
                gpu,
                ..UnitFacts::NONE
            });
        }
        for gib in 1..=512 {
            check(UnitFacts {
                memory: Some(gib * GIB),
                ..UnitFacts::NONE
            });
        }
        for disk in [false, true] {
            check(UnitFacts { disk, ..mac });
        }
        for row in UnitRow::ALL {
            for &form in row.ladder() {
                assert!(
                    taken.contains(&(row, form)),
                    "CONTROL: {row:?} {form:?} never taken"
                );
            }
        }
    }

    /// A1 §4.3's table: the tile is a power of two GiB, the least whose 16 tiles hold the total,
    /// so there are never more than 16 and the tile is never needlessly large; Half doubles it.
    #[test]
    fn the_memory_unit_is_a_power_of_two_gib_and_tiles_never_exceed_sixteen() {
        let table = [
            (8, (GIB / 2, 16)),
            (16, (GIB, 16)),
            (18, (2 * GIB, 9)),
            (24, (2 * GIB, 12)),
            (32, (2 * GIB, 16)),
            (64, (4 * GIB, 16)),
            (128, (8 * GIB, 16)),
        ];
        for (gib, want) in table {
            assert_eq!(memory_tiles(gib * GIB), Some(want), "{gib} GiB");
        }
        assert_eq!(memory_tiles(0), None);
        let totals =
            (1..=4096_u64)
                .map(|mib| mib * (1 << 20) * 37)
                .chain([1, 15, 16, 17, u64::MAX]);
        for total in totals {
            let (unit, tiles) = memory_tiles(total).expect("some memory");
            assert!(unit.is_power_of_two(), "{total}: {unit}");
            assert!((1..=16).contains(&tiles), "{total}: {tiles} tiles");
            assert_eq!(tiles, total.div_ceil(unit), "{total}");
            if unit > 1 {
                assert!(
                    total.div_ceil(unit / 2) > 16,
                    "{total}: {unit} is not the least"
                );
            }
        }
    }

    /// The host's reading supplies every fact the units plan reads (A1 step R5): the deck's
    /// three-reading snapshot of this Mac reads its 4 + 6 cores, its 10-core GPU and 32 GiB,
    /// the Neural Engine tile only once its reader reads, and the disk row on macOS and Linux;
    /// planned from them, both deck forms lay the units column the laws plan for this Mac at
    /// 1039 pt. A machine with no cluster read draws every core in one, and an empty snapshot
    /// gives the planner nothing to draw.
    #[test]
    fn the_hosts_reading_supplies_the_facts_the_units_plan_reads() {
        use crate::stats_plan::{plan_folded_with, plan_full_with};
        let mut snap = crate::stats_deck::tests::snapshot();
        let disk = cfg!(any(target_os = "macos", target_os = "linux"));
        let read = HostFacts::of(&snap);
        let want = UnitFacts {
            gpu: Some(Gpu {
                cores: Some(10),
                neural_engine: false,
            }),
            disk,
            ..this_mac()
        };
        assert_eq!(read.facts(), want);
        snap.ane = Some(Ok(aterm_vitals::AneSample {
            cores: 16,
            powered: false,
        }));
        let read = HostFacts::of(&snap);
        assert_eq!(read.facts(), UnitFacts { disk, ..this_mac() });

        let m = crate::deck_face::FaceMetrics::helvetica_neue();
        let folded = plan_folded_with(1039.0, &m, &read.facts()).blocks[0].units;
        let full = plan_full_with(1039.0, &m, &read.facts()).units;
        assert_eq!(folded.map(|u| u.at.w), Some(46.0));
        let column = Some(Form::Column);
        assert_eq!(
            full.map(|u| u.forms),
            Some([column, column, column, disk.then_some(Form::Column)]),
            "{full:?}"
        );

        if let Some(cpu) = snap.cpu.as_mut() {
            cpu.clusters.clear();
        }
        assert_eq!(HostFacts::of(&snap).facts().clusters, &[10]);
        let empty = HostFacts::of(&Snapshot::default());
        assert_eq!(
            empty.facts(),
            UnitFacts {
                disk,
                ..UnitFacts::NONE
            }
        );
        let disk_alone = empty.facts().draw_any(CELL);
        assert_eq!(disk_alone, disk);
        assert!(
            !UnitFacts {
                disk: false,
                ..empty.facts()
            }
            .draw_any(CELL)
        );
    }
}
