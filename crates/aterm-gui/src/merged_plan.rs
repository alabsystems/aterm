// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The merged deck's planner (`docs/DESIGN-top-deck-merged-2026-10-21.md` §3, §11): where every
//! block, lane, rule and mark of the merged deck sits for a card width, a deck height and a
//! machine. It never reads a reading. A slot's width comes from worst-case templates (the busy
//! pills, `Processes` and its legend, `~88 waiting`, `88 GiB swap`, `100%`, one reference chip
//! per lane, a minimum run pitch) measured in the deck's face ([`FaceMetrics`]), so no slot moves
//! when the first snapshot lands, when a reading changes or when the front tab's volume does; the
//! machine ([`MachineShape`]) only subdivides a slot into tiles. The form is the first of Full,
//! Mid, Narrow and Rail whose least width fits ([`MergedPlanner::thresholds`]), never a constant.
//! Width past that least goes to the chips, then to the runs' pitch up to a cap, then to the
//! right margin.
//!
//! Lengths are points at the mockups' 51 pt deck, scaled with the deck's height; text is 9–11 pt.

use aterm_vitals::glance::chips::CHIP_TILES;
use aterm_vitals::model::Snapshot;
use aterm_vitals::words::{self, DeckScope, FormWords, Platform, RunSpan};

use crate::deck_face::FaceMetrics;

/// The mockups' deck height in points (102 device px at 2×, three 17 pt rows): every length
/// below is the mockups' at this height and scales with the deck's.
const REF_PT: f64 = 51.0;
/// The mockups' deck in device px, the bottoms of row A, the Mac lane, all tabs and this tab in
/// it, and the seam under them.
const REF_PX: f64 = 102.0;
const LANE_ENDS_PX: [f64; 4] = [23.0, 57.0, 78.0, 99.0];
const SEAM_PX: f64 = 2.0;
/// The deck heights the three-row band allows (45–60 pt), as a share of [`REF_PT`].
const K_RANGE: (f64, f64) = (45.0 / REF_PT, 60.0 / REF_PT);
/// Words and figures, and the block names, at [`REF_PT`]; every size stays within [`TEXT_PT`].
const WORDS_PT: f64 = 10.0;
const NAMES_PT: f64 = 10.5;
const TEXT_PT: (f64, f64) = (9.0, 11.0);

/// The card's edge to the first and from the last mark.
const MARGIN: f64 = 6.0;
/// A pill's icon side before its words, and its far end after them.
const PILL_ICON: f64 = 11.0;
const PILL_END: f64 = 3.5;
/// The calm rail's check and its gap before `Nothing needs you`.
const CHECK: f64 = 11.0;
/// The rail's widest pill to its rule, and the rule to the first block.
const RAIL_TO_RULE: f64 = 2.5;
const RULE_TO_BLOCK: f64 = 2.0;
/// A block's last mark to its rule, each side; a rule's width; the divider's, where the bands end.
const RULE_GAP: f64 = 0.75;
const RULE_W: f64 = 0.75;
const DIVIDER_W: f64 = 1.0;

/// A score gauge's width, the gauge to its figure, and the figure to its run.
const GAUGE_W: f64 = 9.5;
const GAUGE_TO_FIG: f64 = 2.5;
const FIG_TO_RUN: f64 = 3.0;
/// Disk's figure clears its gauge's needs ring by this much more.
const RING: f64 = 2.0;
/// The widest figure a gauge names.
const FIG_TEMPLATE: &str = "100%";
/// A run's oldest-end word starts this far left of its first bar (Disk's further, over its
/// figure); row A's words end this far before that word; the two end words keep this apart at
/// every deck height, as the run's mark keeps them ([`crate::merged_marks::Marks::run_ends`]).
const END_OVERHANG: f64 = 3.0;
const DAY_OVERHANG: f64 = 8.0;
const HEAD_TO_END: f64 = 4.0;
const ENDS_GAP: f64 = 2.0;
/// The device px a run's two ends may lose to the painter's snapping: each edge of the run
/// rounds, and its oldest word's start rounds up.
const ENDS_SNAP_PX: f64 = 2.0;
/// Network names its run's ends beside it: the figure to `10m`, and `10m` to the run and the
/// run to `now`.
const BESIDE_FIG: f64 = 2.5;
const BESIDE_RUN: f64 = 1.5;
/// Disk's projection past now: the gap after the run, and its width.
const PROJ_GAP: f64 = 1.25;
const PROJ_W: f64 = 2.5;
/// A run's least and most pitch, and its gap as a share of the pitch; the least is never under
/// two device px (a bar and a gap).
const RUN_PITCH: (f64, f64) = (1.0, 2.5);
const RUN_GAP: f64 = 0.3;
const RUN_BARS: usize = 20;
const DAY_BARS: usize = 24;

/// A tile's gap as a share of its pitch, in every grid.
const TILE_GAP: f64 = 0.25;
/// A tile narrower than this many device px, or less than a whole device px from its
/// neighbour, is not drawn: the parts are one tank.
const TILE_FLOOR_PX: f64 = 2.0;
const TILE_APART_PX: f64 = 1.0;
/// The fastest cluster's tiles are this much wider, and the others this much as tall, where a
/// CPU has more than one kind.
const BIG_WEIGHT: f64 = 1.5;
const SMALL_TALL: f64 = 0.78;
/// A cluster's letter to its tiles.
const LETTER_GAP: f64 = 1.5;
/// Memory's and Disk's tiles fill two rows; Memory's unit keeps them within 16 columns, Disk's
/// within 10.
const GRID_ROWS: usize = 2;
const MEM_COLS: usize = 16;
const DISK_COLS: usize = 10;
const GPU_COLS: usize = 20;
/// Memory's unit ladder in GiB (16 and 32 hold a 512 GiB or 1 TiB machine to 16 columns), and
/// Disk's in GB.
const MEM_UNITS_GIB: [u64; 6] = [1, 2, 4, 8, 16, 32];
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "Disk's tiles (slice E9) divide by it")
)]
const DISK_UNITS_GB: [u64; 6] = [25, 50, 100, 250, 500, 1000];
const GIB: u64 = 1 << 30;
const GB: u64 = 1_000_000_000;

/// The Processes block: the legend's tile, `Processes` to it and the tile to its words, the
/// legend to `per-user limit`, which ends this far before the figure's end.
const LEGEND_GAP: f64 = 6.0;
const LEGEND_TILE: f64 = 1.35;
const LEGEND_TILE_GAP: f64 = 3.0;
const LEGEND_TO_LIMIT: f64 = 12.0;
const LIMIT_TO_FIG_END: f64 = 4.0;
/// The scope names' tree indent, a name to its count, `procs` to the Mac's count, the count
/// rule's gap each side and its width.
pub(crate) const INDENT: f64 = 3.5;
const NAME_TO_COUNT: f64 = 3.0;
const PROCS_TO_COUNT: f64 = 2.0;
const COUNT_RULE_GAP: f64 = 1.5;
const COUNT_RULE_W: f64 = 0.75;
/// The Mac lane's chips end this far before the Processes gauge.
const CHIPS_TO_GAUGE: f64 = 6.0;
/// Mid's scope name to its chip; Narrow's names under the CPU cores to the scopes' figures.
const NAME_TO_CHIP: f64 = 4.5;
const NAME_TO_FIG: f64 = 3.0;
/// Narrow: Memory's row A words run on over the Processes head, which starts this far after.
const HEAD_GAP: f64 = 6.0;
/// Mid: the fold note's gap after Disk's name.
const FOLD_GAP: f64 = 6.0;
/// Narrow: a chip whose program writes the disk that needs you carries the disk's mark.
const DISK_MARK: f64 = 11.5;

/// The busy count templates: four digits for the machine (five where it counts threads), three
/// for a scope.
const MACHINE_COUNT: &str = "8888";
const THREADS_COUNT: &str = "88888";
const SCOPE_COUNT: &str = "888";
/// The reference chip: a program's name, its tiles (as many as a chip lists, [`CHIP_TILES`]),
/// its memory and, at Full in its own scope's lane, its age.
const CHIP_NAME: &str = "rustc";
const CHIP_GIB: u64 = 16 * GIB;
const CHIP_AGE_MS: u64 = 47 * 60_000;
/// Row A's busy capacity and orange templates: CPU's kinds, the widest memory, GPU, disk and
/// link capacities, and what each says past its wall.
const CPU_KINDS: &str = "Super, Perf";
const MEM_TEMPLATE: u64 = 888 * GIB;
const SWAP_TEMPLATE: u64 = 88 * GIB;
const GPU_TEMPLATE: u16 = 88;
const DISK_TEMPLATE: u64 = 88_000_000_000_000;
const LINK_TEMPLATE: f64 = 111_000_000.0;
const WAIT_TEMPLATE: f64 = 88.0;
const FREE_TEMPLATE: u64 = 888 * GB;
const WRITES_TEMPLATE: f64 = 888e9;

/// The Processes block's head in Full, Mid and Narrow (§3, the mockups): its name in bold, then
/// the key to its chips (Full's after a legend tile), so a GiB never reads as disk.
pub(crate) const PROCESSES_HEAD: [(&str, &str); 3] = [
    ("Processes", "CPU, GiB memory"),
    ("Top procs", "· GiB mem"),
    ("GiB = mem", ""),
];
/// Row A's orange words past capacity (`~8 waiting`, `6 GiB swap`) start this far after the
/// words before them.
pub(crate) const OVER_GAP: f64 = 3.0;
/// Memory's name where the deck is narrower than Full.
pub(crate) const MEMORY_SHORT: &str = "Mem";
/// Mid's row A at its right: what a wider window shows (Tokens is out of the deck, §9).
pub(crate) const FOLD_NOTE: &str = "wider: GPU, Net";

/// The merged deck's form (§3): the first of [`MergedForm::LADDER`] whose least width fits the
/// card, or row A alone under about 10 terminal rows (decided from the grid's rows, not here).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum MergedForm {
    /// Every block.
    Full,
    /// Needs, the top programs, CPU, Memory and Disk; no runs.
    Mid,
    /// Needs, CPU and Memory with each scope's %, and each band's top program.
    Narrow,
    /// The needs rail alone.
    Rail,
    /// Row A alone, its slots where the width's form lays them.
    RowA,
}

impl MergedForm {
    /// The forms a width picks from, widest first.
    pub(crate) const LADDER: [Self; 4] = [Self::Full, Self::Mid, Self::Narrow, Self::Rail];

    /// The words a pill says in this form: Narrow's serve the rail and row A too.
    pub(crate) fn pill(self, words: &FormWords) -> &str {
        match self {
            Self::Full => &words.full,
            Self::Mid => &words.mid,
            Self::Narrow | Self::Rail | Self::RowA => &words.narrow,
        }
    }

    const fn ix(self) -> usize {
        match self {
            Self::Full => 0,
            Self::Mid => 1,
            Self::Narrow | Self::Rail | Self::RowA => 2,
        }
    }

    /// The blocks this form lays, left to right (Full's are every block, in the deck's order).
    pub(crate) fn blocks(self) -> &'static [BlockKind] {
        use BlockKind::{Cpu, Disk, Gpu, Memory, Network, Processes};
        match self {
            Self::Full => &[Processes, Cpu, Memory, Gpu, Disk, Network],
            Self::Mid => &[Processes, Cpu, Memory, Disk],
            Self::Narrow => &[Cpu, Memory, Processes],
            Self::Rail | Self::RowA => &[],
        }
    }

    /// The block the bands end after, at the divider.
    const fn divided_after(self) -> Option<BlockKind> {
        match self {
            Self::Full => Some(BlockKind::Gpu),
            Self::Mid => Some(BlockKind::Memory),
            Self::Narrow | Self::Rail | Self::RowA => None,
        }
    }
}

/// The gap in row A between a block's name and the words after it in `form` (its capacity, or
/// Processes' key, past Full's legend tile), in points at the mockups' height: the templates
/// measure each head with it, so the painter's words fit the room the plan gives them.
pub(crate) const fn after_name(kind: BlockKind, form: MergedForm) -> f64 {
    match (kind, form) {
        (BlockKind::Processes, MergedForm::Full) => LEGEND_GAP + LEGEND_TILE + LEGEND_TILE_GAP,
        (BlockKind::Processes, _) => LEGEND_TILE_GAP / 2.0,
        (BlockKind::Cpu | BlockKind::Memory, _) => 2.5,
        (BlockKind::Gpu, _) => 3.0,
        (BlockKind::Disk | BlockKind::Network, _) => 3.5,
    }
}

/// What the planner knows of the machine: its core clusters, its memory, its GPU's cores and its
/// platform. Never the volume: the front tab's changes at a tab switch, so Disk's slot is fixed
/// and [`MergedPlanner::disk_grid`] subdivides it as the painter draws.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) struct MachineShape {
    /// Each cluster's letter (`S`, `P`, `E`, or `CPU` for a lone one) and core count, fastest
    /// first.
    pub(crate) clusters: Vec<(String, usize)>,
    /// Physical memory in bytes, 0 before the first reading.
    pub(crate) memory: u64,
    /// The GPU's cores, where the OS says.
    pub(crate) gpu_cores: Option<u16>,
    /// Whose words the deck says.
    pub(crate) platform: Platform,
}

impl MachineShape {
    /// The machine as `snap` read it, or nothing known before the first reading.
    pub(crate) fn of(snap: Option<&Snapshot>, platform: Platform) -> Self {
        let Some(snap) = snap else {
            return Self {
                platform,
                ..Self::default()
            };
        };
        let clusters = snap.cpu.as_ref().map_or_else(Vec::new, |cpu| {
            if cpu.clusters.is_empty() && cpu.total() > 0 {
                return vec![("CPU".to_owned(), cpu.total())];
            }
            cpu.clusters
                .iter()
                .filter(|c| !c.cores.is_empty())
                .map(|c| (c.short.clone(), c.cores.len()))
                .collect()
        });
        Self {
            clusters,
            memory: snap.mem.as_ref().map_or(0, |m| m.sample.total),
            gpu_cores: snap.gpu.as_ref().and_then(|g| g.cores),
            platform,
        }
    }

    /// The mockups' machine on `platform`: 6 Super and 12 Performance cores, 128 GiB and a
    /// 40-core GPU. The CPU's slot is sized for its parts.
    pub(crate) fn reference(platform: Platform) -> Self {
        Self {
            clusters: vec![("S".to_owned(), 6), ("P".to_owned(), 12)],
            memory: 128 * GIB,
            gpu_cores: Some(40),
            platform,
        }
    }
}

/// A horizontal extent in points.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Span {
    pub(crate) x: f64,
    pub(crate) w: f64,
}

impl Span {
    pub(crate) const fn new(x: f64, w: f64) -> Self {
        Self { x, w }
    }

    pub(crate) fn end(self) -> f64 {
        self.x + self.w
    }
}

/// A vertical extent in points, whole device px at the plan's scale.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Band {
    pub(crate) y: f64,
    pub(crate) h: f64,
}

impl Band {
    pub(crate) fn end(self) -> f64 {
        self.y + self.h
    }
}

/// The deck's lanes: row A, then the Mac, all tabs and this tab (none in [`MergedForm::RowA`]),
/// then the seam.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Lanes {
    pub(crate) a: Band,
    pub(crate) scopes: Option<[Band; 3]>,
    pub(crate) seam: Band,
}

/// The deck's text sizes in points: words and figures, and block names (bold).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Text {
    pub(crate) words: f64,
    pub(crate) names: f64,
}

/// A chip's geometry in its form (E7 draws to it): its tiles' width and gap, its name to its
/// tiles, its tiles to its memory and its memory to its age, the gap between chips, and what a
/// stuck chip's ring adds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Chip {
    pub(crate) tile: f64,
    pub(crate) tile_gap: f64,
    pub(crate) name_gap: f64,
    pub(crate) mem_gap: f64,
    pub(crate) age_gap: f64,
    pub(crate) between: f64,
    pub(crate) ring: f64,
}

impl Chip {
    fn of(form: MergedForm, k: f64) -> Self {
        let (tile, tile_gap) = match form {
            MergedForm::Full => (1.35, 0.4),
            MergedForm::Mid => (1.3, 0.4),
            MergedForm::Narrow | MergedForm::Rail | MergedForm::RowA => (1.0, 0.25),
        };
        Self {
            tile: tile * k,
            tile_gap: tile_gap * k,
            name_gap: 1.5 * k,
            mem_gap: 2.5 * k,
            age_gap: 4.0 * k,
            between: 3.5 * k,
            ring: 6.0 * k,
        }
    }
}

/// A block of the merged deck.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum BlockKind {
    Processes,
    Cpu,
    Memory,
    Gpu,
    Disk,
    Network,
}

/// A run: `bars` bars, oldest at its left, each `pitch` from the last and `gap` short of it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Run {
    pub(crate) at: Span,
    pub(crate) bars: usize,
    pub(crate) pitch: f64,
    pub(crate) gap: f64,
    pub(crate) span: RunSpan,
    /// The words naming its two ends (§4): over it in row A, or beside it in the Mac lane.
    pub(crate) ends: [Span; 2],
    pub(crate) ends_beside: bool,
}

/// One kind of tile: tile `i` of a group sits in column `i / rows` of the grid's rows.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Tiles {
    /// The cluster's letter and where it sits, before its tiles, where a CPU has more than one
    /// kind.
    pub(crate) letter: Option<(String, Span)>,
    /// The first column's left edge.
    pub(crate) x: f64,
    pub(crate) n: usize,
    pub(crate) cols: usize,
    /// Column to column, and a tile's width.
    pub(crate) pitch: f64,
    pub(crate) w: f64,
    /// A tile's height as a share of the lane's.
    pub(crate) tall: f64,
}

impl Tiles {
    pub(crate) fn end(&self) -> f64 {
        self.x + self.pitch * (self.cols.max(1) - 1) as f64 + self.w
    }
}

/// How a slot's parts divide the machine (§4: a tile is a unit of capacity).
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Grid {
    /// `rows` rows of tiles, each `unit` of capacity (a core, or bytes).
    Tiles {
        groups: Vec<Tiles>,
        rows: usize,
        unit: u64,
    },
    /// Tiles under [`TILE_FLOOR_PX`] device px, or under [`TILE_APART_PX`] apart (or none to
    /// count): one tank across the parts, filled to the whole level.
    Tank,
}

/// A block's slot and the marks in it. Its parts are the capacity: a scope's share bar runs on a
/// track as wide.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Block {
    pub(crate) kind: BlockKind,
    pub(crate) at: Span,
    /// Row A's room for the block's name, capacity and orange words (Narrow's Memory runs on
    /// past its slot, over the Processes head).
    pub(crate) head: Span,
    pub(crate) parts: Option<Span>,
    pub(crate) grid: Option<Grid>,
    /// The dashed capacity wall's centre, and the column past it (CPU's slats, Memory's swap).
    pub(crate) wall: Option<f64>,
    pub(crate) over: Option<Span>,
    pub(crate) gauge: Option<Span>,
    /// The Mac's figure, and each scope's % under it.
    pub(crate) fig: Option<Span>,
    pub(crate) run: Option<Run>,
    /// Disk's projection past now.
    pub(crate) proj: Option<Span>,
    /// The scope names' column: Mid's Processes, and Narrow's under the CPU cores.
    pub(crate) names: Option<Span>,
    /// Full's Processes: the scope names and counts (digits right-aligned at its end), and the
    /// rule after them.
    pub(crate) counts: Option<Span>,
    pub(crate) count_rule: Option<Span>,
    /// Processes: the chips' room in the Mac, all-tabs and this-tab lanes.
    pub(crate) chips: Option<[Span; 3]>,
}

impl Block {
    fn new(kind: BlockKind, at: Span) -> Self {
        Self {
            kind,
            at,
            head: at,
            parts: None,
            grid: None,
            wall: None,
            over: None,
            gauge: None,
            fig: None,
            run: None,
            proj: None,
            names: None,
            counts: None,
            count_rule: None,
            chips: None,
        }
    }
}

/// A lane of the deck, top to bottom.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum Lane {
    A,
    Mac,
    AllTabs,
    ThisTab,
}

impl Lane {
    /// Every lane, top to bottom.
    pub(crate) const ALL: [Self; 4] = [Self::A, Self::Mac, Self::AllTabs, Self::ThisTab];
}

/// What a rule parts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum RuleKind {
    /// The needs rail from the blocks.
    Rail,
    /// Two blocks.
    Block,
    /// The bands' end: past it Disk and Network use the height for their own lines.
    Divider,
}

/// A rule from the top of lane `top` to the bottom of lane `bottom`: the bands run on unbroken
/// under a rule between two scoped blocks, and row A's words over one they run on past.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Rule {
    pub(crate) at: Span,
    pub(crate) kind: RuleKind,
    pub(crate) top: Lane,
    pub(crate) bottom: Lane,
}

/// Where everything of the merged deck sits.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct MergedPlan {
    pub(crate) form: MergedForm,
    /// The width's form, whose slots row A keeps in [`MergedForm::RowA`].
    pub(crate) laid: MergedForm,
    /// The card's width and the band's height, in points.
    pub(crate) w: f64,
    pub(crate) h: f64,
    pub(crate) scale: f64,
    /// The deck's height against the mockups' (51 pt): what every length was scaled by.
    pub(crate) k: f64,
    pub(crate) text: Text,
    pub(crate) lanes: Lanes,
    pub(crate) chip: Chip,
    pub(crate) rail: Span,
    pub(crate) blocks: Vec<Block>,
    pub(crate) rules: Vec<Rule>,
    /// The scope lanes' tint, from the rail's rule to the divider (Narrow: to the last block).
    pub(crate) bands: Option<Span>,
    /// Mid's [`FOLD_NOTE`], right-aligned in row A.
    pub(crate) fold: Option<Span>,
}

impl MergedPlan {
    /// The block of `kind`, where this form draws one.
    pub(crate) fn block(&self, kind: BlockKind) -> Option<&Block> {
        self.blocks.iter().find(|b| b.kind == kind)
    }
}

/// The three-row deck's row A alone, with its seam, in device px: the band [`MergedForm::RowA`]
/// draws in.
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "row A alone's band (slice B2) is sized from it")
)]
pub(crate) fn row_a_px(deck_px: usize) -> usize {
    let h = deck_px as f64;
    (h * LANE_ENDS_PX[0] / REF_PX + seam_px(h)).round() as usize
}

fn seam_px(h: f64) -> f64 {
    (h * SEAM_PX / REF_PX).round().max(1.0)
}

/// Every word a slot is sized for, measured once.
#[derive(Clone, Debug, Default, PartialEq)]
struct Templates {
    fig: f64,
    rail: [f64; 3],
    procs_head: [f64; 3],
    /// Mid's scope names column, and Narrow's under the CPU cores (all tabs and this tab).
    names: f64,
    scope_names: f64,
    /// Full's count column, its tree-indented names included.
    counts: f64,
    /// A scope lane's reference chip in each form, and Full's Mac lane's (no age there).
    chip: [f64; 3],
    chip_mac: f64,
    cpu_head: [f64; 3],
    mem_head: [f64; 3],
    gpu_head: f64,
    disk_head: [f64; 2],
    disk_lines: f64,
    net_head: f64,
    ten: (f64, f64),
    day: (f64, f64),
    fold: f64,
    cpu_parts: [f64; 3],
    mem_parts: [f64; 3],
    gpu_parts: f64,
    disk_parts: [f64; 2],
}

/// Measures words in the deck's face at the deck's sizes.
struct Pen<'a> {
    m: &'a FaceMetrics,
    k: f64,
    text: Text,
}

impl Pen<'_> {
    fn sp(&self, pt: f64) -> f64 {
        pt * self.k
    }

    fn words(&self, s: &str) -> f64 {
        self.m.width(s, self.text.words, false)
    }

    fn bold(&self, s: &str) -> f64 {
        self.m.width(s, self.text.words, true)
    }

    fn name(&self, s: &str) -> f64 {
        self.m.width(s, self.text.names, true)
    }

    fn templates(&self, platform: Platform) -> Templates {
        let busy = [
            words::disk_full_pill(34.0 * 60.0),
            words::waiting_pill("Tab 1", "password"),
            words::stuck_pill("node", Some(12 * 60_000), "Tab 3"),
        ];
        let rail = [MergedForm::Full, MergedForm::Mid, MergedForm::Narrow].map(|form| {
            busy.iter()
                .map(|p| self.sp(PILL_ICON + PILL_END) + self.bold(form.pill(p)))
                .chain([
                    self.bold(words::NEEDS_RAIL_HEAD),
                    self.sp(CHECK) + self.words(words::NEEDS_RAIL_CALM),
                ])
                .fold(0.0, f64::max)
        });
        let scope = |s: DeckScope| self.bold(s.word(platform));
        let count = words::count_word(platform);
        let machine_count = if count == "threads" {
            THREADS_COUNT
        } else {
            MACHINE_COUNT
        };
        let scope_count = |indent: f64, s: DeckScope| {
            self.sp(indent) + scope(s) + self.sp(NAME_TO_COUNT) + self.words(SCOPE_COUNT)
        };
        let counts = (scope(DeckScope::Mac)
            + self.sp(NAME_TO_COUNT + PROCS_TO_COUNT)
            + self.words(count)
            + self.words(machine_count))
        .max(scope_count(INDENT, DeckScope::AllTabs))
        .max(scope_count(2.0 * INDENT, DeckScope::ThisTab));
        let [full, mid, narrow] = PROCESSES_HEAD;
        let age = words::up_words(Some(CHIP_AGE_MS), false).unwrap_or_default();
        let chip = |form: MergedForm, age: &str| {
            let c = Chip::of(form, self.k);
            let tiles = CHIP_TILES as f64 * (c.tile + c.tile_gap) - c.tile_gap;
            let aged = if age.is_empty() {
                0.0
            } else {
                c.age_gap + self.words(age)
            };
            self.words(CHIP_NAME)
                + c.name_gap
                + tiles
                + c.mem_gap
                + self.words(&words::gib_words(CHIP_GIB))
                + aged
        };
        let orange = |w: Option<String>| w.map_or(0.0, |w| self.sp(OVER_GAP) + self.words(&w));
        let waiting = orange(words::waiting_words(WAIT_TEMPLATE, platform));
        let swap = orange(words::swap_words(SWAP_TEMPLATE));
        let reference = MachineShape::reference(platform);
        let ten = RunSpan::TenMinutes.ends();
        let day = RunSpan::Day.ends();
        let writer = self.bold(CHIP_NAME)
            + self.words(" ")
            + self.words(&words::writes_words(WRITES_TEMPLATE));
        Templates {
            fig: self.bold(FIG_TEMPLATE),
            rail,
            procs_head: [
                self.name(full.0)
                    + self.sp(after_name(BlockKind::Processes, MergedForm::Full) + LEGEND_TO_LIMIT)
                    + self.words(full.1)
                    + self.words(words::PER_USER_LIMIT),
                self.name(mid.0)
                    + self.sp(after_name(BlockKind::Processes, MergedForm::Mid))
                    + self.words(mid.1),
                self.name(narrow.0),
            ],
            names: DeckScope::ALL.map(scope).into_iter().fold(0.0, f64::max),
            scope_names: scope(DeckScope::AllTabs).max(scope(DeckScope::ThisTab)),
            counts,
            chip: [
                chip(MergedForm::Full, &age),
                chip(MergedForm::Mid, ""),
                chip(MergedForm::Narrow, "") + self.sp(DISK_MARK),
            ],
            chip_mac: chip(MergedForm::Full, ""),
            cpu_head: [
                self.name("CPU")
                    + self.sp(after_name(BlockKind::Cpu, MergedForm::Full))
                    + self.words(CPU_KINDS)
                    + waiting,
                self.name("CPU") + waiting,
                self.name("CPU") + waiting,
            ],
            mem_head: [
                self.name("Memory")
                    + self.sp(after_name(BlockKind::Memory, MergedForm::Full))
                    + self.words(&words::memory_capacity(MEM_TEMPLATE))
                    + swap,
                self.name(MEMORY_SHORT) + swap,
                self.name(MEMORY_SHORT) + swap,
            ],
            gpu_head: self.name("GPU")
                + self.sp(after_name(BlockKind::Gpu, MergedForm::Full))
                + self.words(&words::gpu_capacity(GPU_TEMPLATE)),
            disk_head: [
                self.name("Disk")
                    + self.sp(after_name(BlockKind::Disk, MergedForm::Full))
                    + self.words(&words::disk_capacity(DISK_TEMPLATE)),
                self.name("Disk"),
            ],
            disk_lines: self.bold(&words::free_words(FREE_TEMPLATE)).max(writer),
            net_head: self.name("Network")
                + self.sp(after_name(BlockKind::Network, MergedForm::Full))
                + self.words(&words::link_capacity(LINK_TEMPLATE)),
            ten: (self.words(ten.0), self.words(ten.1)),
            day: (self.words(day.0), self.words(day.1)),
            fold: self.words(FOLD_NOTE),
            cpu_parts: [MergedForm::Full, MergedForm::Mid, MergedForm::Narrow].map(|form| {
                let groups = cpu_groups(&reference.clusters);
                let letters: Vec<f64> = groups
                    .iter()
                    .map(|g| g.letter.map_or(0.0, |l| self.bold(l)))
                    .collect();
                let fixed = fixed_w(&letters, self.sp(LETTER_GAP), self.sp(kind_gap(form)));
                fixed + self.sp(CPU_PITCH[form.ix()]) * weighted(&groups)
            }),
            mem_parts: MEM_PITCH.map(|p| self.sp(p) * (MEM_COLS as f64 - TILE_GAP)),
            gpu_parts: self.sp(GPU_PITCH) * (GPU_COLS as f64 - TILE_GAP),
            disk_parts: DISK_PITCH.map(|p| self.sp(p) * (DISK_COLS as f64 - TILE_GAP)),
        }
    }
}

/// The CPU's reference pitch of a weight-1 column in Full, Mid and Narrow, and the gap between
/// two kinds; Memory's, GPU's and Disk's reference pitches.
const CPU_PITCH: [f64; 3] = [3.0, 1.6, 1.3];
const MEM_PITCH: [f64; 3] = [4.6, 2.15, 1.5];
const GPU_PITCH: f64 = 1.8;
const DISK_PITCH: [f64; 2] = [2.4, 2.3];

const fn kind_gap(form: MergedForm) -> f64 {
    match form {
        MergedForm::Full => 3.0,
        MergedForm::Mid => 2.0,
        MergedForm::Narrow | MergedForm::Rail | MergedForm::RowA => 1.5,
    }
}

/// The letters (each with its gap) and the gaps between kinds of a CPU's groups.
fn fixed_w(letters: &[f64], letter_gap: f64, kind_gap: f64) -> f64 {
    let lettered: f64 = letters
        .iter()
        .filter(|&&w| w > 0.0)
        .map(|w| w + letter_gap)
        .sum();
    lettered + kind_gap * letters.len().saturating_sub(1) as f64
}

/// The groups' width in weight-1 pitches, the last gap of each left out.
fn weighted(groups: &[Group<'_>]) -> f64 {
    groups
        .iter()
        .map(|g| g.weight * (g.cols as f64 - TILE_GAP))
        .sum()
}

/// The merged deck's planner for one deck height, scale, machine and face: it measures the
/// templates once, so a plan at any width is arithmetic.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct MergedPlanner {
    shape: MachineShape,
    /// The shape's cluster letters' widths, bold, in cluster order (0 where none is drawn).
    letters: Vec<f64>,
    deck_px: usize,
    scale: f64,
    k: f64,
    text: Text,
    t: Templates,
}

impl MergedPlanner {
    /// A planner for a three-row deck `deck_px` device px tall at `scale` device px per point.
    pub(crate) fn new(deck_px: usize, scale: f64, shape: MachineShape, m: &FaceMetrics) -> Self {
        let scale = if scale.is_finite() && scale > 0.0 {
            scale
        } else {
            1.0
        };
        let k = (deck_px as f64 / scale / REF_PT).clamp(K_RANGE.0, K_RANGE.1);
        let text = Text {
            words: (WORDS_PT * k).clamp(TEXT_PT.0, TEXT_PT.1),
            names: (NAMES_PT * k).clamp(TEXT_PT.0, TEXT_PT.1),
        };
        let pen = Pen { m, k, text };
        let letters = cpu_groups(&shape.clusters)
            .iter()
            .map(|g| g.letter.map_or(0.0, |l| pen.bold(l)))
            .collect();
        Self {
            t: pen.templates(shape.platform),
            letters,
            shape,
            deck_px,
            scale,
            k,
            text,
        }
    }

    fn sp(&self, pt: f64) -> f64 {
        pt * self.k
    }

    fn p_min(&self) -> f64 {
        self.sp(RUN_PITCH.0).max(TILE_FLOOR_PX / self.scale)
    }

    fn p_max(&self) -> f64 {
        self.sp(RUN_PITCH.1).max(self.p_min())
    }

    /// A 10-minute run's width at pitch `p`: never short of its two end words, which overhang
    /// its first bar, once the painter has snapped it to device px.
    fn ten_w(&self, p: f64) -> f64 {
        (p * (RUN_BARS as f64 - RUN_GAP)).max(self.ends_w(self.t.ten, END_OVERHANG))
    }

    fn day_w(&self, p: f64) -> f64 {
        (p * (DAY_BARS as f64 - RUN_GAP)).max(self.ends_w(self.t.day, DAY_OVERHANG))
    }

    /// The least run under its two end words `words`, the oldest overhanging it by `overhang`.
    fn ends_w(&self, words: (f64, f64), overhang: f64) -> f64 {
        words.0 + ENDS_GAP + words.1 - self.sp(overhang) + ENDS_SNAP_PX / self.scale
    }

    /// The gauge, figure and run after a scored block's parts, or after Processes' Mac chips.
    fn score(&self, run: f64) -> f64 {
        self.sp(GAUGE_W + GAUGE_TO_FIG + FIG_TO_RUN) + self.t.fig + run
    }

    /// What follows a block's parts up to its gauge (Full) or figure: the wall's centre and the
    /// column past it (CPU's slats, Memory's swap), from the parts' end, and the whole length.
    fn post(&self, kind: BlockKind, form: MergedForm) -> (f64, Span, f64) {
        let (wall, over, w, after) = match (kind, form) {
            (BlockKind::Cpu, MergedForm::Full) => (3.0, 5.0, 6.0, 3.5),
            (BlockKind::Cpu, MergedForm::Mid) => (2.0, 3.5, 3.0, 3.0),
            (BlockKind::Cpu, _) => (2.0, 3.5, 2.5, 2.5),
            (BlockKind::Memory, MergedForm::Full) => (2.5, 5.0, 9.1, 3.5),
            (BlockKind::Memory, MergedForm::Mid) => (2.0, 4.0, 4.5, 3.0),
            (BlockKind::Memory, _) => (2.0, 4.0, 4.5, 2.5),
            (_, MergedForm::Full) => (0.0, 0.0, 0.0, 3.5),
            _ => (0.0, 0.0, 0.0, 3.0),
        };
        (
            self.sp(wall),
            Span::new(self.sp(over), self.sp(w)),
            self.sp(over + w + after),
        )
    }

    /// A block's least width in `form` at run pitch `p`, with room for one reference chip.
    fn need(&self, kind: BlockKind, form: MergedForm, p: f64) -> f64 {
        let t = &self.t;
        let ix = form.ix();
        let over_run =
            |head: f64, overhang: f64, run: f64| head + self.sp(HEAD_TO_END + overhang) + run;
        match (kind, form) {
            (BlockKind::Processes, MergedForm::Full) => {
                let run = self.ten_w(p);
                let rule = self.sp(2.0 * COUNT_RULE_GAP + COUNT_RULE_W);
                let head = t.procs_head[0] + self.sp(LIMIT_TO_FIG_END + FIG_TO_RUN) + run;
                let mac = t.counts + rule + t.chip_mac + self.sp(CHIPS_TO_GAUGE) + self.score(run);
                head.max(mac).max(t.counts + rule + t.chip[0])
            }
            (BlockKind::Processes, MergedForm::Mid) => {
                t.procs_head[1].max(t.names + self.sp(NAME_TO_CHIP) + t.chip[1])
            }
            (BlockKind::Processes, _) => t.chip[2],
            (BlockKind::Cpu | BlockKind::Memory | BlockKind::Gpu, MergedForm::Full) => {
                let run = self.ten_w(p);
                let (parts, head) = match kind {
                    BlockKind::Cpu => (t.cpu_parts[0], t.cpu_head[0]),
                    BlockKind::Memory => (t.mem_parts[0], t.mem_head[0]),
                    _ => (t.gpu_parts, t.gpu_head),
                };
                (parts + self.post(kind, form).2 + self.score(run)).max(over_run(
                    head,
                    END_OVERHANG,
                    run,
                ))
            }
            (BlockKind::Cpu, _) => {
                let lane = t.cpu_parts[ix] + self.post(kind, form).2;
                let lane = if form == MergedForm::Narrow {
                    lane.max(t.scope_names + self.sp(NAME_TO_FIG))
                } else {
                    lane
                };
                (lane + t.fig).max(t.cpu_head[ix])
            }
            (BlockKind::Memory, _) => {
                let lane = t.mem_parts[ix] + self.post(kind, form).2 + t.fig;
                // Narrow's head runs on over the Processes head ([`Self::slots`] holds it).
                if form == MergedForm::Narrow {
                    lane
                } else {
                    lane.max(t.mem_head[ix])
                }
            }
            (BlockKind::Disk, MergedForm::Full) => {
                let tail = self.day_w(p) + self.sp(PROJ_GAP + PROJ_W);
                let lane =
                    t.disk_parts[0] + self.post(kind, form).2 + self.sp(RING) + self.score(tail);
                lane.max(over_run(t.disk_head[0], DAY_OVERHANG, tail))
                    .max(t.disk_lines)
            }
            (BlockKind::Disk, _) => (t.disk_parts[1] + self.post(kind, form).2 + t.fig)
                .max(t.disk_lines)
                .max(t.disk_head[1]),
            (BlockKind::Network, _) => (self
                .sp(GAUGE_W + GAUGE_TO_FIG + BESIDE_FIG + 2.0 * BESIDE_RUN)
                + t.fig
                + t.ten.0
                + p * (RUN_BARS as f64 - RUN_GAP)
                + t.ten.1)
                .max(t.net_head),
            (BlockKind::Gpu, _) => 0.0,
        }
    }

    /// The room past the least the chips may take: one more reference chip in each lane.
    fn chips_cap(&self, form: MergedForm) -> f64 {
        let chip = match form {
            MergedForm::Full => self.t.chip_mac,
            _ => self.t.chip[form.ix()],
        };
        Chip::of(form, self.k).between + chip
    }

    fn rule_w(&self, form: MergedForm, after: BlockKind) -> f64 {
        self.sp(if form.divided_after() == Some(after) {
            DIVIDER_W
        } else {
            RULE_W
        })
    }

    /// Lays `form`'s slots at run pitch `p` with `chips` more room for the chips: each block's
    /// slot, and the right edge of everything drawn (row A's run-on words and the fold note
    /// included).
    fn slots(&self, form: MergedForm, p: f64, chips: f64) -> (Vec<(BlockKind, Span)>, f64) {
        let rail_end = self.sp(MARGIN) + self.t.rail[form.ix()];
        let mut x = rail_end + self.sp(RAIL_TO_RULE + RULE_W + RULE_TO_BLOCK);
        let mut out: Vec<(BlockKind, Span)> = Vec::new();
        for &kind in form.blocks() {
            if let Some(&(prev, _)) = out.last() {
                x += self.sp(2.0 * RULE_GAP) + self.rule_w(form, prev);
            }
            let extra = if kind == BlockKind::Processes {
                chips
            } else {
                0.0
            };
            let at = Span::new(x, self.need(kind, form, p) + extra);
            out.push((kind, at));
            x = at.end();
        }
        let mut right = out.last().map_or(rail_end, |s| s.1.end());
        let at = |kind| out.iter().find(|s| s.0 == kind).map(|s| s.1);
        if form == MergedForm::Mid
            && let Some(disk) = at(BlockKind::Disk)
        {
            right = right.max(disk.x + self.t.disk_head[1] + self.sp(FOLD_GAP) + self.t.fold);
        }
        if form == MergedForm::Narrow
            && let (Some(mem), Some(procs)) = (at(BlockKind::Memory), at(BlockKind::Processes))
        {
            let head = procs.x.max(mem.x + self.t.mem_head[2] + self.sp(HEAD_GAP));
            right = right.max(head + self.t.procs_head[2]);
        }
        (out, right)
    }

    /// The least card width Full, Mid and Narrow each fit, in points: the first-fit ladder's
    /// thresholds for this deck, platform and face.
    pub(crate) fn thresholds(&self) -> [f64; 3] {
        [MergedForm::Full, MergedForm::Mid, MergedForm::Narrow]
            .map(|form| self.slots(form, self.p_min(), 0.0).1 + self.sp(MARGIN))
    }

    /// The plan for a card `w_pt` points wide; `row_a_only` keeps row A alone, its slots where
    /// the width lays them.
    pub(crate) fn plan(&self, w_pt: f64, row_a_only: bool) -> MergedPlan {
        let w = if w_pt.is_finite() { w_pt.max(0.0) } else { 0.0 };
        let need = self.thresholds();
        let laid = MergedForm::LADDER
            .into_iter()
            .zip(need)
            .find_map(|(form, need)| (w >= need).then_some(form))
            .unwrap_or(MergedForm::Rail);
        let lanes = self.lanes(row_a_only);
        let margin = self.sp(MARGIN);
        let rail_w = match laid {
            MergedForm::Rail => self.t.rail[laid.ix()].min(w - 2.0 * margin).max(0.0),
            _ => self.t.rail[laid.ix()],
        };
        let rail = Span::new(margin.min(w), rail_w);
        let mut plan = MergedPlan {
            form: if row_a_only { MergedForm::RowA } else { laid },
            laid,
            w,
            h: lanes.seam.end(),
            scale: self.scale,
            k: self.k,
            text: self.text,
            lanes,
            chip: Chip::of(laid, self.k),
            rail,
            blocks: Vec::new(),
            rules: Vec::new(),
            bands: None,
            fold: None,
        };
        if laid == MergedForm::Rail {
            return plan;
        }
        let mut slack = w - need[laid.ix()];
        let chips = slack.clamp(0.0, self.chips_cap(laid));
        slack -= chips;
        let p = self.pitch(laid, chips, w, slack);
        let rail_rule = Span::new(rail.end() + self.sp(RAIL_TO_RULE), self.sp(RULE_W));
        plan.rules.push(Rule {
            at: rail_rule,
            kind: RuleKind::Rail,
            top: Lane::A,
            bottom: Lane::ThisTab,
        });
        let (slots, _) = self.slots(laid, p, chips);
        let mut mem_head_end = 0.0;
        for (i, &(kind, at)) in slots.iter().enumerate() {
            let block = self.block(kind, laid, at, p, mem_head_end, w);
            if kind == BlockKind::Memory {
                mem_head_end = block.head.end();
            }
            if let Some(&(next, _)) = slots.get(i + 1) {
                plan.rules.push(self.rule_after(laid, kind, next, at.end()));
            }
            plan.blocks.push(block);
        }
        let band_end = match plan.rules.iter().find(|r| r.kind == RuleKind::Divider) {
            Some(divider) => divider.at.end(),
            None => plan
                .blocks
                .last()
                .map_or(rail_rule.end(), |b| b.at.end() + self.sp(RULE_GAP)),
        };
        plan.bands = Some(Span::new(rail_rule.end(), band_end - rail_rule.end()));
        if laid == MergedForm::Mid {
            plan.fold = Some(Span::new(w - margin - self.t.fold, self.t.fold));
        }
        plan
    }

    /// The widest run pitch up to the cap that keeps everything inside `w`.
    fn pitch(&self, form: MergedForm, chips: f64, w: f64, slack: f64) -> f64 {
        let (mut lo, mut hi) = (self.p_min(), self.p_max());
        if form != MergedForm::Full || slack <= 0.0 {
            return lo;
        }
        let fits = |p: f64| self.slots(form, p, chips).1 + self.sp(MARGIN) <= w;
        if fits(hi) {
            return hi;
        }
        for _ in 0..40 {
            let mid = 0.5 * (lo + hi);
            if fits(mid) {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        lo
    }

    /// The rule after `before`'s slot ending at `end`, `after` being the next block.
    fn rule_after(&self, form: MergedForm, before: BlockKind, after: BlockKind, end: f64) -> Rule {
        let at = Span::new(end + self.sp(RULE_GAP), self.rule_w(form, before));
        let scoped = |k| matches!(k, BlockKind::Cpu | BlockKind::Memory | BlockKind::Gpu);
        let (kind, top, bottom) = if form.divided_after() == Some(before) {
            (RuleKind::Divider, Lane::A, Lane::ThisTab)
        } else if form == MergedForm::Narrow && before == BlockKind::Memory {
            (RuleKind::Block, Lane::Mac, Lane::ThisTab)
        } else if scoped(before) && scoped(after) {
            (RuleKind::Block, Lane::A, Lane::Mac)
        } else {
            (RuleKind::Block, Lane::A, Lane::ThisTab)
        };
        Rule {
            at,
            kind,
            top,
            bottom,
        }
    }

    /// The run ending at `end`, `w` wide, its ends named over it in row A.
    fn run_over(&self, end: f64, w: f64, span: RunSpan) -> Run {
        let (bars, overhang, words) = match span {
            RunSpan::TenMinutes => (RUN_BARS, END_OVERHANG, self.t.ten),
            RunSpan::Day => (DAY_BARS, DAY_OVERHANG, self.t.day),
        };
        let at = Span::new(end - w, w);
        let pitch = w / (bars as f64 - RUN_GAP);
        Run {
            at,
            bars,
            pitch,
            gap: pitch * RUN_GAP,
            span,
            ends: [
                Span::new(at.x - self.sp(overhang), words.0),
                Span::new(at.end() - words.1, words.1),
            ],
            ends_beside: false,
        }
    }

    /// The Mac's gauge and figure before a run starting at `run_x`, `ring` further apart.
    fn gauge_fig(&self, run_x: f64, ring: f64) -> (Span, Span) {
        let fig = Span::new(run_x - self.sp(FIG_TO_RUN) - self.t.fig, self.t.fig);
        let gauge_x = fig.x - self.sp(GAUGE_TO_FIG + GAUGE_W) - ring;
        (Span::new(gauge_x, self.sp(GAUGE_W)), fig)
    }

    fn block(
        &self,
        kind: BlockKind,
        form: MergedForm,
        at: Span,
        p: f64,
        mem_head_end: f64,
        w: f64,
    ) -> Block {
        let t = &self.t;
        let mut b = Block::new(kind, at);
        let fig_right = Span::new(at.end() - t.fig, t.fig);
        let head_to = |run: &Run| Span::new(at.x, run.ends[0].x - self.sp(HEAD_TO_END) - at.x);
        match (kind, form) {
            (BlockKind::Processes, MergedForm::Full) => {
                let run = self.run_over(at.end(), self.ten_w(p), RunSpan::TenMinutes);
                let (gauge, fig) = self.gauge_fig(run.at.x, 0.0);
                b.head = head_to(&run);
                let rule = Span::new(
                    at.x + t.counts + self.sp(COUNT_RULE_GAP),
                    self.sp(COUNT_RULE_W),
                );
                let x = rule.end() + self.sp(COUNT_RULE_GAP);
                let lane = Span::new(x, at.end() - x);
                b.counts = Some(Span::new(at.x, t.counts));
                b.count_rule = Some(rule);
                b.chips = Some([
                    Span::new(x, gauge.x - self.sp(CHIPS_TO_GAUGE) - x),
                    lane,
                    lane,
                ]);
                (b.gauge, b.fig, b.run) = (Some(gauge), Some(fig), Some(run));
            }
            (BlockKind::Processes, MergedForm::Mid) => {
                let names = Span::new(at.x, t.names);
                let x = names.end() + self.sp(NAME_TO_CHIP);
                b.names = Some(names);
                b.chips = Some([Span::new(x, at.end() - x); 3]);
            }
            (BlockKind::Processes, _) => {
                b.head = Span::new(at.x.max(mem_head_end + self.sp(HEAD_GAP)), t.procs_head[2]);
                b.chips = Some([at; 3]);
            }
            (BlockKind::Cpu | BlockKind::Memory | BlockKind::Gpu, MergedForm::Full) => {
                let run = self.run_over(at.end(), self.ten_w(p), RunSpan::TenMinutes);
                let (gauge, fig) = self.gauge_fig(run.at.x, 0.0);
                b.head = head_to(&run);
                self.parts(&mut b, form, gauge.x);
                (b.gauge, b.fig, b.run) = (Some(gauge), Some(fig), Some(run));
            }
            (BlockKind::Cpu | BlockKind::Memory, _) => {
                if kind == BlockKind::Memory && form == MergedForm::Narrow {
                    b.head = Span::new(at.x, t.mem_head[2]);
                }
                if kind == BlockKind::Cpu && form == MergedForm::Narrow {
                    b.names = Some(Span::new(at.x, t.scope_names));
                }
                self.parts(&mut b, form, fig_right.x);
                b.fig = Some(fig_right);
            }
            (BlockKind::Disk, MergedForm::Full) => {
                let proj = Span::new(at.end() - self.sp(PROJ_W), self.sp(PROJ_W));
                let run = self.run_over(proj.x - self.sp(PROJ_GAP), self.day_w(p), RunSpan::Day);
                let (gauge, fig) = self.gauge_fig(run.at.x, self.sp(RING));
                b.head = head_to(&run);
                self.parts(&mut b, form, gauge.x);
                (b.gauge, b.fig, b.run, b.proj) = (Some(gauge), Some(fig), Some(run), Some(proj));
            }
            (BlockKind::Disk, _) => {
                let fold = w - self.sp(MARGIN) - t.fold;
                b.head = Span::new(at.x, fold - self.sp(FOLD_GAP) - at.x);
                self.parts(&mut b, form, fig_right.x);
                b.fig = Some(fig_right);
            }
            (BlockKind::Network, _) => {
                let gauge = Span::new(at.x, self.sp(GAUGE_W));
                let fig = Span::new(gauge.end() + self.sp(GAUGE_TO_FIG), t.fig);
                let left = Span::new(fig.end() + self.sp(BESIDE_FIG), t.ten.0);
                let right = Span::new(at.end() - t.ten.1, t.ten.1);
                let x = left.end() + self.sp(BESIDE_RUN);
                let run_w = right.x - self.sp(BESIDE_RUN) - x;
                let pitch = run_w / (RUN_BARS as f64 - RUN_GAP);
                b.run = Some(Run {
                    at: Span::new(x, run_w),
                    bars: RUN_BARS,
                    pitch,
                    gap: pitch * RUN_GAP,
                    span: RunSpan::TenMinutes,
                    ends: [left, right],
                    ends_beside: true,
                });
                (b.gauge, b.fig) = (Some(gauge), Some(fig));
            }
            (BlockKind::Gpu, _) => {}
        }
        b
    }

    /// Lays a block's parts from its start to `until` (its gauge, or its figure) less what
    /// follows them, with the wall and the column past it, and divides them for the machine.
    fn parts(&self, b: &mut Block, form: MergedForm, until: f64) {
        let (wall, over, post) = self.post(b.kind, form);
        let parts = Span::new(b.at.x, until - post - b.at.x);
        if over.w > 0.0 {
            b.wall = Some(parts.end() + wall);
            b.over = Some(Span::new(parts.end() + over.x, over.w));
        }
        b.parts = Some(parts);
        b.grid = match b.kind {
            BlockKind::Cpu => {
                let groups = cpu_groups(&self.shape.clusters);
                (!groups.is_empty())
                    .then(|| self.divide(parts, &groups, &self.letters, kind_gap(form), 1, 1))
            }
            BlockKind::Memory => (self.shape.memory > 0)
                .then(|| self.grid(parts, self.shape.memory, &MEM_UNITS_GIB, GIB, MEM_COLS)),
            BlockKind::Gpu => Some(
                self.shape
                    .gpu_cores
                    .map_or(Grid::Tank, |n| self.two_rows(parts, usize::from(n), 1)),
            ),
            BlockKind::Processes | BlockKind::Disk | BlockKind::Network => None,
        };
    }

    /// `total` bytes as tiles of the first unit of `units` (times `base`) that keeps them
    /// within `cols` columns of two rows, across `parts`.
    fn grid(&self, parts: Span, total: u64, units: &[u64], base: u64, cols: usize) -> Grid {
        let unit = units
            .iter()
            .map(|&u| u * base)
            .find(|&u| total.div_ceil(u) <= (GRID_ROWS * cols) as u64)
            .or_else(|| units.last().map(|&u| u * base))
            .unwrap_or(base);
        self.two_rows(
            parts,
            usize::try_from(total.div_ceil(unit)).unwrap_or(usize::MAX),
            unit,
        )
    }

    /// `n` tiles of `unit` in two rows across `parts`, the last column holding what is left.
    fn two_rows(&self, parts: Span, n: usize, unit: u64) -> Grid {
        let groups = [Group::columns(n.div_ceil(GRID_ROWS))];
        match self.divide(parts, &groups, &[0.0], 0.0, GRID_ROWS, unit) {
            Grid::Tiles {
                mut groups,
                rows,
                unit,
            } => {
                groups.iter_mut().for_each(|g| g.n = n);
                Grid::Tiles { groups, rows, unit }
            }
            Grid::Tank => Grid::Tank,
        }
    }

    /// Divides `parts` among `groups` of columns, each led by its letter (`letters`' widths;
    /// `kind_gap` points at the mockups' height between two groups), a weight-1 column `u`
    /// apart; a tank where the thinnest tile would be under [`TILE_FLOOR_PX`], or its pitch
    /// under that and [`TILE_APART_PX`] more, so the painter's whole-px tiles keep a gap.
    fn divide(
        &self,
        parts: Span,
        groups: &[Group<'_>],
        letters: &[f64],
        kind_gap: f64,
        rows: usize,
        unit: u64,
    ) -> Grid {
        let kind_gap = self.sp(kind_gap);
        let fixed = fixed_w(letters, self.sp(LETTER_GAP), kind_gap);
        let u = (parts.w - fixed) / weighted(groups);
        let pitch = groups
            .iter()
            .map(|g| g.weight)
            .fold(f64::INFINITY, f64::min)
            * u
            * self.scale;
        let fits =
            pitch * (1.0 - TILE_GAP) >= TILE_FLOOR_PX && pitch >= TILE_FLOOR_PX + TILE_APART_PX;
        if !(u.is_finite() && u > 0.0 && fits) {
            return Grid::Tank;
        }
        let mut x = parts.x;
        let mut out = Vec::with_capacity(groups.len());
        for (g, &lw) in groups.iter().zip(letters) {
            let letter = g.letter.filter(|_| lw > 0.0).map(|l| {
                let span = Span::new(x, lw);
                x = span.end() + self.sp(LETTER_GAP);
                (l.to_owned(), span)
            });
            let pitch = g.weight * u;
            let tiles = Tiles {
                letter,
                x,
                n: g.cols * rows,
                cols: g.cols,
                pitch,
                w: pitch * (1.0 - TILE_GAP),
                tall: g.tall,
            };
            x = tiles.end() + kind_gap;
            out.push(tiles);
        }
        Grid::Tiles {
            groups: out,
            rows,
            unit,
        }
    }

    /// The deck's lanes in points, each edge on a whole device px.
    fn lanes(&self, row_a_only: bool) -> Lanes {
        let h = self.deck_px as f64;
        let seam = seam_px(h);
        let edge = |i: usize| (h * LANE_ENDS_PX[i] / REF_PX).round();
        let band = |y0: f64, y1: f64| Band {
            y: y0 / self.scale,
            h: (y1 - y0) / self.scale,
        };
        let a = band(0.0, edge(0));
        if row_a_only {
            return Lanes {
                a,
                scopes: None,
                seam: band(edge(0), edge(0) + seam),
            };
        }
        Lanes {
            a,
            scopes: Some([
                band(edge(0), edge(1)),
                band(edge(1), edge(2)),
                band(edge(2), edge(3)),
            ]),
            seam: band(h - seam, h),
        }
    }

    /// Disk's tiles for the front tab's volume of `total` bytes in its fixed `parts` (§5):
    /// units from 25 GB to 1 TB keep them within 20 tiles in two rows; a tank where they would
    /// be too thin, none before the first reading.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "the Disk block (slice E9) draws its tiles from it"
        )
    )]
    pub(crate) fn disk_grid(&self, parts: Span, total: u64) -> Option<Grid> {
        (total > 0).then(|| self.grid(parts, total, &DISK_UNITS_GB, GB, DISK_COLS))
    }
}

/// A run of tile columns: its letter, columns, width weight and height share.
struct Group<'a> {
    letter: Option<&'a str>,
    cols: usize,
    weight: f64,
    tall: f64,
}

impl Group<'_> {
    const fn columns(cols: usize) -> Self {
        Self {
            letter: None,
            cols,
            weight: 1.0,
            tall: 1.0,
        }
    }
}

/// The CPU's clusters that hold a core, as tile groups in one row: where it has more than one
/// kind, each led by its letter, the fastest wider and the others shorter.
fn cpu_groups(clusters: &[(String, usize)]) -> Vec<Group<'_>> {
    let kinds: Vec<&(String, usize)> = clusters.iter().filter(|c| c.1 > 0).collect();
    let many = kinds.len() > 1;
    kinds
        .into_iter()
        .enumerate()
        .map(|(i, (letter, n))| Group {
            letter: many.then_some(letter.as_str()),
            cols: *n,
            weight: if many && i == 0 { BIG_WEIGHT } else { 1.0 },
            tall: if many && i > 0 { SMALL_TALL } else { 1.0 },
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use aterm_vitals::fixtures::{self, DeckScene};

    use super::*;
    use crate::deck_face::{fit, txts};

    /// The mockups' deck at 2×: 102 device px, 51 pt.
    const MOCKUP_PX: usize = 102;
    /// The three-row band's extremes at 2× (45 and 60 pt) and the mockups'.
    const DECKS_2X: [usize; 3] = [90, MOCKUP_PX, 120];
    const EPS: f64 = 1e-6;

    fn faces() -> [(&'static str, FaceMetrics); 2] {
        [
            ("Helvetica Neue", FaceMetrics::helvetica_neue()),
            ("DejaVu", FaceMetrics::test_stack()),
        ]
    }

    fn reference(deck_px: usize, scale: f64, m: &FaceMetrics) -> MergedPlanner {
        MergedPlanner::new(deck_px, scale, MachineShape::reference(Platform::MacOs), m)
    }

    /// Every box the plan draws in, by lane (row A, Mac, all tabs, this tab), named.
    fn boxes(plan: &MergedPlan) -> [Vec<(String, Span)>; 4] {
        let mut out: [Vec<(String, Span)>; 4] = Default::default();
        let mut put = |lanes: &[usize], name: String, s: Span| {
            for &l in lanes {
                out[l].push((name.clone(), s));
            }
        };
        const ALL: [usize; 4] = [0, 1, 2, 3];
        const SCOPES: [usize; 2] = [2, 3];
        put(&ALL, "rail".into(), plan.rail);
        for r in &plan.rules {
            let lanes: Vec<usize> = (0..Lane::ALL.len())
                .filter(|&l| (r.top..=r.bottom).contains(&Lane::ALL[l]))
                .collect();
            put(&lanes, format!("{:?} rule", r.kind), r.at);
        }
        if let Some(fold) = plan.fold {
            put(&[0], "fold note".into(), fold);
        }
        for b in &plan.blocks {
            let n = |part: &str| format!("{:?} {part}", b.kind);
            put(&[0], n("head"), b.head);
            if let Some(run) = b.run {
                let lanes: &[usize] = if run.ends_beside { &[1] } else { &[0] };
                put(lanes, n("oldest end"), run.ends[0]);
                put(lanes, n("now end"), run.ends[1]);
            }
            let scoped_run = matches!(b.kind, BlockKind::Cpu | BlockKind::Memory | BlockKind::Gpu);
            if let Some(run) = b.run {
                put(if scoped_run { &[1, 2, 3] } else { &[1] }, n("run"), run.at);
            }
            if let Some(counts) = b.counts {
                put(&[1, 2, 3], n("counts"), counts);
            }
            if let Some(rule) = b.count_rule {
                put(&[1, 2, 3], n("count rule"), rule);
            }
            if let Some(chips) = b.chips {
                for (l, s) in chips.into_iter().enumerate() {
                    put(&[l + 1], n("chips"), s);
                }
            }
            if let Some(names) = b.names {
                let lanes: &[usize] = if b.kind == BlockKind::Cpu {
                    &SCOPES
                } else {
                    &[1, 2, 3]
                };
                put(lanes, n("names"), names);
            }
            if let Some(parts) = b.parts {
                let shared = b.kind != BlockKind::Disk && b.names.is_none();
                put(if shared { &[1, 2, 3] } else { &[1] }, n("parts"), parts);
            }
            if let Some(wall) = b.wall {
                put(&[1], n("wall"), Span::new(wall - 0.5 * plan.k, plan.k));
            }
            if let Some(over) = b.over {
                put(&[1], n("over"), over);
            }
            if let Some(gauge) = b.gauge {
                put(&[1], n("gauge"), gauge);
            }
            if let Some(fig) = b.fig {
                let lanes: &[usize] = match b.kind {
                    BlockKind::Disk | BlockKind::Network | BlockKind::Processes => &[1],
                    _ => &[1, 2, 3],
                };
                put(lanes, n("figure"), fig);
            }
            if let Some(proj) = b.proj {
                put(&[1], n("projection"), proj);
            }
            if b.kind == BlockKind::Disk {
                put(&SCOPES, n("lines"), b.at);
            }
        }
        out
    }

    /// The plan stays inside the card, no two boxes of a lane overlap, and each grid stays
    /// inside its parts with tiles at least two device px wide.
    fn assert_laid(plan: &MergedPlan, ctx: &str) {
        let (lo, hi) = if plan.laid == MergedForm::Rail {
            (0.0, plan.w)
        } else {
            (MARGIN * plan.k, plan.w - MARGIN * plan.k)
        };
        for (lane, mut boxes) in boxes(plan).into_iter().enumerate() {
            boxes.sort_by(|a, b| a.1.x.total_cmp(&b.1.x));
            for (name, s) in &boxes {
                assert!(
                    s.w >= -EPS && s.x >= lo - EPS && s.end() <= hi + EPS,
                    "{ctx}: lane {lane} {name} {s:?} leaves [{lo}, {hi}]"
                );
            }
            for pair in boxes.windows(2) {
                let [(a, sa), (b, sb)] = pair else { continue };
                assert!(
                    sa.end() <= sb.x + EPS,
                    "{ctx}: lane {lane}: {a} {sa:?} overlaps {b} {sb:?}"
                );
            }
        }
        for b in &plan.blocks {
            if let (Some(parts), Some(grid)) = (b.parts, &b.grid) {
                assert_grid_inside(grid, parts, plan.scale, &format!("{ctx} {:?}", b.kind));
            }
        }
    }

    fn assert_grid_inside(grid: &Grid, parts: Span, scale: f64, ctx: &str) {
        let Grid::Tiles { groups, .. } = grid else {
            return;
        };
        let mut x = parts.x;
        for g in groups {
            if let Some((_, letter)) = &g.letter {
                assert!(letter.x >= x - EPS, "{ctx}: letter {letter:?} before {x}");
                x = letter.end();
            }
            assert!(
                g.x >= x - EPS && g.end() <= parts.end() + EPS,
                "{ctx}: {g:?} leaves {parts:?}"
            );
            assert!(
                g.w * scale >= TILE_FLOOR_PX - EPS
                    && g.pitch * scale >= TILE_FLOOR_PX + TILE_APART_PX - EPS,
                "{ctx}: {g:?} is under the floor"
            );
            x = g.end();
        }
    }

    /// Each room the plan gives words holds the worst-case template it was sized for: the
    /// rail's head and calm words, each block's row A words, the chips in each lane, Full's
    /// counts, the scope names and Disk's lines.
    fn assert_rooms(planner: &MergedPlanner, plan: &MergedPlan, ctx: &str) {
        let t = &planner.t;
        let ix = plan.laid.ix();
        let room = |what: &str, room: f64, need: f64| {
            assert!(
                room + EPS >= need,
                "{ctx}: {what} has {room} pt, its words need {need}"
            );
        };
        // The rail alone is cut to a card narrower than it.
        if plan.laid != MergedForm::Rail {
            room("the rail", plan.rail.w, t.rail[ix]);
        }
        for b in &plan.blocks {
            let head = match (b.kind, plan.laid) {
                (BlockKind::Processes, _) => t.procs_head[ix],
                (BlockKind::Cpu, _) => t.cpu_head[ix],
                (BlockKind::Memory, _) => t.mem_head[ix],
                (BlockKind::Gpu, _) => t.gpu_head,
                (BlockKind::Disk, MergedForm::Full) => t.disk_head[0],
                (BlockKind::Disk, _) => t.disk_head[1],
                (BlockKind::Network, _) => t.net_head,
            };
            room(&format!("{:?}'s head", b.kind), b.head.w, head);
            if let Some(chips) = b.chips {
                let need = match plan.laid {
                    MergedForm::Full => [t.chip_mac, t.chip[0], t.chip[0]],
                    _ => [t.chip[ix]; 3],
                };
                for (lane, (s, need)) in chips.into_iter().zip(need).enumerate() {
                    room(&format!("lane {}'s chips", lane + 1), s.w, need);
                }
            }
            if let Some(counts) = b.counts {
                room("the counts", counts.w, t.counts);
            }
            if let Some(names) = b.names {
                let need = if b.kind == BlockKind::Cpu {
                    t.scope_names
                } else {
                    t.names
                };
                room(&format!("{:?}'s names", b.kind), names.w, need);
            }
            if b.kind == BlockKind::Disk {
                room("Disk's lines", b.at.w, t.disk_lines);
            }
        }
    }

    /// Prints the plan at the mockups' widths beside their edges (device px at 2×) and the
    /// derived thresholds beside the spec's; the mockup measured with SF and the deck draws
    /// Helvetica Neue, so the edges are compared by eye, never asserted.
    #[test]
    fn the_reference_machine_fits_the_mockups_widths() {
        for (face, m) in faces() {
            for deck_px in DECKS_2X {
                for scale in [1.0, 2.0] {
                    let px = (deck_px as f64 * scale / 2.0).round() as usize;
                    let t = reference(px, scale, &m).thresholds();
                    println!(
                        "{face}, deck {} pt, {scale}x: Full from {:.1} pt (spec 1100), Mid from {:.1} (542), Narrow from {:.1} (331)",
                        deck_px / 2,
                        t[0],
                        t[1],
                        t[2]
                    );
                }
            }
        }
        let m = FaceMetrics::helvetica_neue();
        let planner = reference(MOCKUP_PX, 2.0, &m);
        let px = |x: f64| (x * 2.0).round();
        let edge = |plan: &MergedPlan, kind| plan.block(kind).map_or(f64::NAN, |b| px(b.at.end()));
        let rail = |plan: &MergedPlan| plan.rules.first().map_or(f64::NAN, |r| px(r.at.end()));
        type Edges = Vec<(&'static str, f64, f64)>;
        let full = |plan: &MergedPlan| -> Edges {
            let net = plan.block(BlockKind::Network);
            vec![
                ("rail rule", rail(plan), 313.0),
                ("Processes", edge(plan, BlockKind::Processes), 856.0),
                ("CPU", edge(plan, BlockKind::Cpu), 1207.0),
                ("Memory", edge(plan, BlockKind::Memory), 1572.0),
                ("GPU (divider)", edge(plan, BlockKind::Gpu), 1801.0),
                ("Disk", edge(plan, BlockKind::Disk), 2011.0),
                ("Network from", net.map_or(f64::NAN, |b| px(b.at.x)), 2015.0),
                (
                    "right edge",
                    net.map_or(f64::NAN, |b| px(b.at.end())),
                    2226.0,
                ),
            ]
        };
        let mid = |plan: &MergedPlan| -> Edges {
            vec![
                ("rail rule", rail(plan), 244.0),
                ("Processes", edge(plan, BlockKind::Processes), 478.0),
                ("CPU", edge(plan, BlockKind::Cpu), 652.0),
                ("Memory (divider)", edge(plan, BlockKind::Memory), 818.0),
                ("Disk", edge(plan, BlockKind::Disk), 1017.0),
                (
                    "fold note from",
                    plan.fold.map_or(f64::NAN, |f| px(f.x)),
                    895.0,
                ),
            ]
        };
        let narrow = |plan: &MergedPlan| -> Edges {
            vec![
                ("rail rule", rail(plan), 225.0),
                ("CPU", edge(plan, BlockKind::Cpu), 384.0),
                ("Memory", edge(plan, BlockKind::Memory), 506.0),
                ("Processes", edge(plan, BlockKind::Processes), 671.0),
            ]
        };
        for (w, form) in [
            (1119.0, MergedForm::Full),
            (550.0, MergedForm::Mid),
            (350.0, MergedForm::Narrow),
        ] {
            let plan = planner.plan(w, false);
            let edges = match form {
                MergedForm::Full => full(&plan),
                MergedForm::Mid => mid(&plan),
                _ => narrow(&plan),
            };
            let pitch = plan
                .block(BlockKind::Cpu)
                .and_then(|b| b.run)
                .map(|r| r.pitch);
            println!(
                "Helvetica Neue at {w} pt ({} px, 2x, deck 51 pt): {:?}, run pitch {pitch:?} pt",
                w * 2.0,
                plan.form
            );
            println!("    {:<18} {:>8} {:>8}", "edge", "ours px", "mockup");
            for (name, ours, mockup) in edges {
                println!("    {name:<18} {ours:>8} {mockup:>8}");
            }
            assert_eq!(plan.form, form, "{w} pt");
            assert_laid(&plan, &format!("{w} pt"));
        }
    }

    #[test]
    fn every_width_plans_inside_the_card_with_no_box_overlap() {
        for (face, m) in faces() {
            for scale in [1.0, 2.0] {
                let deck_px = (51.0 * scale) as usize;
                let planner = reference(deck_px, scale, &m);
                let t = planner.thresholds();
                assert!(t[0] > t[1] && t[1] > t[2], "{face} {scale}x: {t:?}");
                for w in (160..=5120).map(f64::from) {
                    let plan = planner.plan(w, false);
                    let ctx = format!("{face} {scale}x W {w} {:?}", plan.form);
                    assert_laid(&plan, &ctx);
                    assert_rooms(&planner, &plan, &ctx);
                    let want = MergedForm::LADDER
                        .into_iter()
                        .zip(t)
                        .find_map(|(f, t)| (w >= t).then_some(f))
                        .unwrap_or(MergedForm::Rail);
                    assert_eq!(plan.form, want, "{face} {scale}x W {w}");
                    let row_a = planner.plan(w, true);
                    assert_eq!((row_a.form, row_a.laid), (MergedForm::RowA, plan.form));
                    assert_eq!((&row_a.blocks, &row_a.rules), (&plan.blocks, &plan.rules));
                }
            }
        }
    }

    #[test]
    fn form_follows_the_window_never_the_readings() {
        let m = FaceMetrics::helvetica_neue();
        let slots = |mut plan: MergedPlan| {
            plan.blocks.iter_mut().for_each(|b| b.grid = None);
            plan
        };
        let planners = DeckScene::ALL.map(|scene| {
            let snap = fixtures::snapshot(scene);
            MergedPlanner::new(
                MOCKUP_PX,
                2.0,
                MachineShape::of(Some(&snap), Platform::MacOs),
                &m,
            )
        });
        let unread =
            MergedPlanner::new(MOCKUP_PX, 2.0, MachineShape::of(None, Platform::MacOs), &m);
        for w in (160..=5120).map(f64::from) {
            let busy = slots(planners[0].plan(w, false));
            for planner in planners.iter().chain([&unread]) {
                assert_eq!(slots(planner.plan(w, false)), busy, "W {w}");
            }
        }
        for w in [1119.0, 550.0, 2000.0] {
            let plan = planners[0].plan(w, false);
            let parts = plan
                .block(BlockKind::Disk)
                .and_then(|b| b.parts)
                .expect("Disk's parts");
            for gb in [
                64, 128, 256, 500, 512, 1000, 2000, 4000, 8000, 16000, 20000, 64000, 100_000,
            ] {
                let total = gb * GB;
                let grid = planners[0].disk_grid(parts, total).expect("a read volume");
                if let Grid::Tiles { groups, unit, .. } = &grid {
                    let n: usize = groups.iter().map(|g| g.n).sum();
                    assert!(
                        n as u64 * unit >= total && n <= 20 || gb > 20_000,
                        "{gb} GB: {n} of {unit}"
                    );
                } else {
                    assert!(gb > 20_000, "{gb} GB fits 20 tiles, not a tank");
                }
                assert_grid_inside(&grid, parts, 2.0, &format!("{gb} GB at {w}"));
            }
            assert_eq!(planners[0].disk_grid(parts, 0), None);
        }
    }

    #[test]
    fn the_machine_only_subdivides_its_slots() {
        let m = FaceMetrics::helvetica_neue();
        let mut shapes = Vec::new();
        for n in (8..=32).step_by(2) {
            for kinds in [
                vec![("S", 6), ("P", n - 6)],
                vec![("P", n - 4), ("E", 4)],
                vec![("S", 4), ("P", n.max(10) - 8), ("E", 4)],
                vec![("CPU", n)],
            ] {
                let linux = kinds.len() == 1;
                shapes.push(MachineShape {
                    clusters: kinds.into_iter().map(|(l, c)| (l.to_owned(), c)).collect(),
                    platform: if linux {
                        Platform::Linux
                    } else {
                        Platform::MacOs
                    },
                    ..MachineShape::reference(Platform::MacOs)
                });
            }
        }
        for gib in [
            8.0, 15.6, 16.0, 18.0, 24.0, 32.0, 36.0, 48.0, 64.0, 96.0, 128.0, 192.0, 256.0, 512.0,
        ] {
            shapes.push(MachineShape {
                memory: (gib * GIB as f64) as u64,
                ..MachineShape::reference(Platform::MacOs)
            });
        }
        for gpu in [
            None,
            Some(0),
            Some(8),
            Some(10),
            Some(16),
            Some(19),
            Some(38),
            Some(40),
            Some(60),
            Some(80),
        ] {
            shapes.push(MachineShape {
                gpu_cores: gpu,
                ..MachineShape::reference(Platform::MacOs)
            });
        }
        for scale in [1.0, 2.0] {
            let deck_px = (51.0 * scale) as usize;
            let mut tanks = 0;
            for shape in &shapes {
                let ours = MergedPlanner::new(deck_px, scale, shape.clone(), &m);
                let theirs =
                    MergedPlanner::new(deck_px, scale, MachineShape::reference(shape.platform), &m);
                for w in ours.thresholds().into_iter().chain([2000.0]) {
                    let (plan, base) = (ours.plan(w, false), theirs.plan(w, false));
                    let ctx = format!("{scale}x W {w} {shape:?}");
                    assert_laid(&plan, &ctx);
                    let undivided = |plan: &MergedPlan| {
                        let mut plan = plan.clone();
                        plan.blocks.iter_mut().for_each(|b| b.grid = None);
                        plan
                    };
                    assert_eq!(undivided(&plan), undivided(&base), "{ctx}");
                    for b in &plan.blocks {
                        match (&b.grid, b.kind) {
                            (Some(Grid::Tiles { groups, rows, unit }), BlockKind::Cpu) => {
                                let cores: usize = shape.clusters.iter().map(|c| c.1).sum();
                                assert_eq!(
                                    groups.iter().map(|g| g.n).sum::<usize>(),
                                    cores,
                                    "{ctx}"
                                );
                                assert_eq!((*rows, *unit), (1, 1), "{ctx}");
                                assert_eq!(
                                    groups.iter().any(|g| g.letter.is_some()),
                                    shape.clusters.len() > 1,
                                    "{ctx}"
                                );
                            }
                            (Some(Grid::Tiles { groups, rows, unit }), BlockKind::Memory) => {
                                let n: usize = groups.iter().map(|g| g.n).sum();
                                assert_eq!(*rows, 2, "{ctx}");
                                assert!(
                                    n <= 32 && n as u64 * unit >= shape.memory,
                                    "{ctx}: {n} × {unit}"
                                );
                            }
                            (Some(Grid::Tiles { groups, .. }), BlockKind::Gpu) => {
                                let n: usize = groups.iter().map(|g| g.n).sum();
                                assert_eq!(Some(n), shape.gpu_cores.map(usize::from), "{ctx}");
                            }
                            (Some(Grid::Tank), _) => tanks += 1,
                            (None, kind) => assert!(
                                !matches!(
                                    kind,
                                    BlockKind::Cpu | BlockKind::Memory | BlockKind::Gpu
                                ),
                                "{ctx}: {kind:?} is undivided"
                            ),
                            _ => {}
                        }
                    }
                }
            }
            println!("{scale}x: {tanks} parts drawn as a tank over the sweep");
        }
    }

    #[test]
    fn lanes_snap_to_device_px() {
        let m = FaceMetrics::helvetica_neue();
        for scale in [1.0, 2.0, 3.0] {
            for pt in 45..=60 {
                let deck_px = (f64::from(pt) * scale).round() as usize;
                let planner = reference(deck_px, scale, &m);
                let whole = |y: f64| ((y * scale) - (y * scale).round()).abs() < EPS;
                let three = planner.plan(1119.0, false).lanes;
                let scopes = three.scopes.expect("three rows");
                let mut y = 0.0;
                for band in [three.a].into_iter().chain(scopes) {
                    assert!(
                        (band.y - y).abs() < EPS && band.h * scale >= 1.0 - EPS,
                        "{pt} pt {scale}x: {band:?}"
                    );
                    assert!(
                        whole(band.y) && whole(band.end()),
                        "{pt} pt {scale}x: {band:?}"
                    );
                    y = band.end();
                }
                assert!(
                    y <= three.seam.y + EPS && whole(three.seam.y),
                    "{pt} pt {scale}x"
                );
                assert!(
                    (three.seam.end() * scale - deck_px as f64).abs() < EPS
                        && three.seam.h * scale >= 1.0 - EPS
                );
                let row_a = planner.plan(1119.0, true);
                assert_eq!(row_a.lanes.a, three.a, "{pt} pt {scale}x");
                assert!((row_a.lanes.seam.y - three.a.end()).abs() < EPS);
                assert!(
                    (row_a.h * scale - row_a_px(deck_px) as f64).abs() < EPS,
                    "{pt} pt {scale}x"
                );
                let t = planner.text;
                assert!(
                    (TEXT_PT.0..=TEXT_PT.1).contains(&t.words)
                        && (TEXT_PT.0..=TEXT_PT.1).contains(&t.names)
                );
            }
        }
    }

    /// Each form's worst-case words fit the rooms planned for them as rule F measures them, so a
    /// painter fitting them ([`fit`]) draws them whole at the threshold itself.
    #[test]
    fn every_template_fits_its_room_as_rule_f_measures_it() {
        for (face, m) in faces() {
            let planner = reference(MOCKUP_PX, 2.0, &m);
            let pills = [
                words::disk_full_pill(34.0 * 60.0),
                words::waiting_pill("Tab 1", "password"),
                words::stuck_pill("node", Some(12 * 60_000), "Tab 3"),
            ];
            for (form, w) in MergedForm::LADDER.into_iter().zip(planner.thresholds()) {
                let plan = planner.plan(w, false);
                assert_eq!(plan.form, form, "{face} at {w}");
                let words_pt = plan.text.words;
                let whole = |s: &str, room: f64, bold: bool| {
                    assert_eq!(
                        fit(&txts([s.to_owned()], &m), room, words_pt, bold, &m),
                        s,
                        "{face} {form:?}"
                    );
                };
                for pill in &pills {
                    whole(
                        form.pill(pill),
                        plan.rail.w - planner.sp(PILL_ICON + PILL_END),
                        true,
                    );
                }
                whole(words::NEEDS_RAIL_HEAD, plan.rail.w, true);
                whole(
                    words::NEEDS_RAIL_CALM,
                    plan.rail.w - planner.sp(CHECK),
                    false,
                );
                assert_rooms(&planner, &plan, &format!("{face} {form:?} at {w}"));
                for b in &plan.blocks {
                    if let Some(fig) = b.fig {
                        whole(FIG_TEMPLATE, fig.w, true);
                    }
                    if let Some(run) = b.run {
                        let (l, r) = run.span.ends();
                        whole(l, run.ends[0].w, false);
                        whole(r, run.ends[1].w, false);
                    }
                }
                if let Some(fold) = plan.fold {
                    whole(FOLD_NOTE, fold.w, false);
                }
            }
        }
    }

    #[test]
    fn slack_goes_to_the_chips_then_the_runs_then_the_margin() {
        let m = FaceMetrics::helvetica_neue();
        let planner = reference(MOCKUP_PX, 2.0, &m);
        let full = planner.thresholds()[0];
        let cap = planner.chips_cap(MergedForm::Full);
        let procs = |w: f64| {
            planner
                .plan(w, false)
                .block(BlockKind::Processes)
                .map(|b| b.at.w)
        };
        let pitch = |w: f64| {
            planner
                .plan(w, false)
                .block(BlockKind::Network)
                .and_then(|b| b.run)
                .map(|r| r.pitch)
        };
        let least = procs(full).expect("Full lays Processes");
        let grown = procs(full + 0.5 * cap).expect("Full lays Processes") - least;
        assert!(
            (grown - 0.5 * cap).abs() < EPS,
            "the chips take the first slack: {grown}"
        );
        assert!((pitch(full + 0.5 * cap).expect("a run") - planner.p_min()).abs() < EPS);
        assert!(
            pitch(full + cap + 20.0).expect("a run") > planner.p_min() + EPS,
            "then the runs"
        );
        let wide = planner.plan(5120.0, false);
        let net = wide
            .block(BlockKind::Network)
            .and_then(|b| b.run)
            .expect("a run");
        assert!(
            (net.pitch - planner.p_max()).abs() < EPS,
            "the runs stop at their cap"
        );
        let right = wide.blocks.last().map_or(0.0, |b| b.at.end());
        assert!(
            right < 5120.0 - 1000.0,
            "the margin takes the rest: content ends at {right}"
        );
    }
}
