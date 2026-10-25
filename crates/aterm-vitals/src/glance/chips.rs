// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Processes block's chips (`docs/DESIGN-top-deck-merged-2026-10-21.md` §7): each scope
//! row's biggest programs, from the sweep's program groups ([`Snapshot::programs`]) and its
//! process rows ([`Snapshot::procs`]).
//!
//! - The Mac row is the whole Mac's top programs whatever their scope, each tagged with its own
//!   scope so it is named in that scope's ink (busy: this tab's rustc, then Spotlight).
//! - The all-tabs row holds aterm's other tabs, each as one unit (`Tab 1`), and aterm itself;
//!   never this tab, whose programs are the row below and whose share the bars show.
//! - The this-tab row holds the front tab's programs, all its panes'.
//!
//! Every row ranks by one rule ([`RankBy`]): the share of the Mac's CPU or of its memory,
//! whichever score is nearer its capacity, held until the other leads by [`RANK_HOLD`].

use crate::jobs::Verdict;
use crate::model::{ProcScope, ProcView, ProgramGroup, Resource, RowScope, ScopeView, Snapshot};
use crate::words::{DeckScope, program_age};

/// The most chips a row lists, more than the widest form draws.
pub const ROW_CHIPS: usize = 8;
/// The most tiles a chip lists, one per process; the merged deck's planner sizes a chip's room
/// for this many.
pub const CHIP_TILES: usize = 9;
/// How far the other score must lead the one the chips rank by before they turn to it: 5
/// points, so two close scores do not reorder the chips back and forth.
pub const RANK_HOLD: f32 = 0.05;
/// What the all-tabs row calls aterm's own processes (the app and its helpers).
pub const ATERM: &str = "aterm";

/// What every row ranks its chips by: their share of the Mac's CPU or of its memory.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RankBy {
    /// The CPU, by cores.
    Cpu,
    /// Memory, by footprint.
    Memory,
}

impl RankBy {
    /// Whichever of the Mac's CPU and memory scores is nearer its capacity, except that `held`
    /// (what the last reading ranked by) stays until the other score leads it by more than
    /// [`RANK_HOLD`]. Where one score is unread, the other; CPU on a tie, and where neither is
    /// read and nothing is held.
    #[must_use]
    pub fn of(snap: &Snapshot, held: Option<Self>) -> Self {
        let score = |r: Resource| snap.resources[r.index()].heat_now;
        Self::choose(score(Resource::Cpu), score(Resource::Memory), held)
    }

    fn choose(cpu: Option<f32>, mem: Option<f32>, held: Option<Self>) -> Self {
        match (cpu, mem, held) {
            (Some(cpu), Some(mem), Some(Self::Cpu)) if mem > cpu + RANK_HOLD => Self::Memory,
            (Some(cpu), Some(mem), Some(Self::Memory)) if cpu > mem + RANK_HOLD => Self::Cpu,
            (Some(_), Some(_), Some(held)) => held,
            (Some(cpu), Some(mem), None) if mem > cpu => Self::Memory,
            (None, Some(_), _) => Self::Memory,
            (None, None, Some(held)) => held,
            _ => Self::Cpu,
        }
    }

    /// `a` before `b` in a row: the larger share first, then the other resource's, then by name.
    fn order(self, a: &Unit<'_>, b: &Unit<'_>) -> std::cmp::Ordering {
        let cores = b.cores.total_cmp(&a.cores);
        let memory = b.footprint.unwrap_or(0).cmp(&a.footprint.unwrap_or(0));
        match self {
            Self::Cpu => cores.then(memory),
            Self::Memory => memory.then(cores),
        }
        .then_with(|| a.name.cmp(b.name))
    }
}

/// One chip: a program, or in the all-tabs row a whole tab or aterm.
#[derive(Clone, Debug, PartialEq)]
pub struct Chip {
    /// Its name: the program's short name as its processes chose it (data, never an
    /// instruction), a tab's label (`Tab 1`) or [`ATERM`].
    pub name: String,
    /// The scope it runs in, whose ink names it.
    pub scope: DeckScope,
    /// Each process's CPU, cores, busiest first, at most [`CHIP_TILES`]; `None` for a process
    /// whose rate is unread or that the sweep kept no row for (outside aterm, among neither the
    /// machine's busiest nor its largest, [`crate::model::TOP_ROWS`]).
    pub tiles: Vec<Option<f32>>,
    /// How many processes it holds.
    pub n: u32,
    /// Their CPU, cores, summed.
    pub cores: f32,
    /// Their memory, bytes, summed over those read; `None` where none was.
    pub footprint: Option<u64>,
    /// Its age once it has run long (`up 47 min`, [`program_age`]), in its own scope's row
    /// only; a tab or aterm has none.
    pub age: Option<String>,
    /// Whether it holds a stuck job of the front tab (one of its processes leads the job or is
    /// in its process group).
    pub stuck: bool,
}

/// One row of the Processes block.
#[derive(Clone, Debug, PartialEq)]
pub struct ChipRow {
    /// Its scope.
    pub scope: DeckScope,
    /// How many processes it counts: the whole Mac's ([`crate::model::RowFacts::pid_counts`]),
    /// or the scope's in its sweep ([`crate::model::ScopeView::procs`]); `None` where unread.
    pub count: Option<u64>,
    /// Its chips, first the biggest, at most [`ROW_CHIPS`].
    pub chips: Vec<Chip>,
}

/// The Processes block's rows.
#[derive(Clone, Debug, PartialEq)]
pub struct Chips {
    /// What they rank by: held by the caller for the next reading's [`chips`].
    pub rank: RankBy,
    /// The Mac's, all tabs' and this tab's ([`DeckScope::ALL`]).
    pub rows: [ChipRow; 3],
}

/// The Processes block's rows for a window whose front tab is the sessions `tab_panes` (one
/// tab's panes, [`crate::model::TabRoot::tab`]), ranked by [`RankBy::of`] with `held`.
#[must_use]
pub fn chips(snap: &Snapshot, tab_panes: &[u64], held: Option<RankBy>) -> Chips {
    let rank = RankBy::of(snap, held);
    let front: Vec<u64> = snap
        .tabs
        .iter()
        .filter(|t| tab_panes.contains(&t.key))
        .map(|t| t.tab)
        .collect();
    let stuck: Vec<(u32, u32)> = snap
        .jobs
        .as_deref()
        .map(|j| {
            j.rows
                .iter()
                .filter(|r| r.verdict == Verdict::Stuck && tab_panes.contains(&r.tab))
                .map(|r| (r.pgid, r.leader.0))
                .collect()
        })
        .unwrap_or_default();
    let rows = snap
        .procs
        .iter()
        .map(|r| (proc_scope(snap, r), r))
        .collect();
    let deck = Deck {
        snap,
        rank,
        front,
        stuck,
        rows,
    };
    let count = |scope: Option<&ScopeView>| scope.map(|s| u64::from(s.procs));
    Chips {
        rank,
        rows: [
            ChipRow {
                scope: DeckScope::Mac,
                count: snap.facts.pid_counts.0,
                chips: deck.row(DeckScope::Mac, deck.programs(|_| true)),
            },
            ChipRow {
                scope: DeckScope::AllTabs,
                count: count(snap.aterm.as_ref()),
                chips: deck.row(DeckScope::AllTabs, deck.units()),
            },
            ChipRow {
                scope: DeckScope::ThisTab,
                count: count(snap.tab_of(tab_panes).as_ref().map(|t| &t.scope)),
                chips: deck.row(
                    DeckScope::ThisTab,
                    deck.programs(|s| deck.scope_of(s) == DeckScope::ThisTab),
                ),
            },
        ],
    }
}

/// The scope a process row counts in, as [`ProgramGroup::scope`] names it: a pane's is its
/// tab's.
fn proc_scope(snap: &Snapshot, r: &ProcView) -> ProcScope {
    match r.scope {
        RowScope::Tab(k) => ProcScope::Tab(snap.tab(k).map_or(k, |t| t.tab)),
        RowScope::SelfProc | RowScope::Helper => ProcScope::Aterm,
        RowScope::Others | RowScope::Services | RowScope::Unknown => ProcScope::Other,
    }
}

/// A chip before it is built: a program group, or a whole scope.
struct Unit<'a> {
    name: &'a str,
    scope: ProcScope,
    /// The program; `None` for a whole tab or aterm.
    group: Option<&'a ProgramGroup>,
    n: u32,
    cores: f32,
    footprint: Option<u64>,
}

impl<'a> Unit<'a> {
    fn program(g: &'a ProgramGroup) -> Self {
        Self {
            name: &g.name,
            scope: g.scope,
            group: Some(g),
            n: g.n,
            cores: g.cores,
            footprint: g.footprint,
        }
    }
}

/// What every row reads.
struct Deck<'a> {
    snap: &'a Snapshot,
    rank: RankBy,
    /// The front tab's key (its panes' [`crate::model::TabView::tab`]).
    front: Vec<u64>,
    /// The front tab's stuck jobs: their process group and leader.
    stuck: Vec<(u32, u32)>,
    /// Every process row with its scope.
    rows: Vec<(ProcScope, &'a ProcView)>,
}

impl<'a> Deck<'a> {
    fn scope_of(&self, s: ProcScope) -> DeckScope {
        match s {
            ProcScope::Tab(t) if self.front.contains(&t) => DeckScope::ThisTab,
            ProcScope::Tab(_) | ProcScope::Aterm => DeckScope::AllTabs,
            ProcScope::Other => DeckScope::Mac,
        }
    }

    /// The programs whose scope `keep` holds.
    fn programs(&self, keep: impl Fn(ProcScope) -> bool) -> Vec<Unit<'a>> {
        self.snap
            .programs
            .iter()
            .filter(|g| keep(g.scope))
            .map(Unit::program)
            .collect()
    }

    /// The all-tabs row's units: aterm, and each tab but the front one as a whole.
    fn units(&self) -> Vec<Unit<'a>> {
        let mut out: Vec<Unit<'a>> = Vec::new();
        for g in &self.snap.programs {
            if !matches!(g.scope, ProcScope::Aterm | ProcScope::Tab(_))
                || self.scope_of(g.scope) == DeckScope::ThisTab
            {
                continue;
            }
            if let Some(u) = out.iter_mut().find(|u| u.scope == g.scope) {
                u.n += g.n;
                u.cores += g.cores;
                u.footprint = match (u.footprint, g.footprint) {
                    (Some(a), Some(b)) => Some(a.saturating_add(b)),
                    (a, b) => a.or(b),
                };
                continue;
            }
            let name = match g.scope {
                ProcScope::Tab(t) => self
                    .snap
                    .tabs
                    .iter()
                    .find(|v| v.tab == t)
                    .map(|v| v.label.as_str()),
                _ => Some(ATERM),
            };
            if let Some(name) = name {
                out.push(Unit {
                    name,
                    group: None,
                    ..Unit::program(g)
                });
            }
        }
        out
    }

    /// `units` ranked, the first [`ROW_CHIPS`] as `row`'s chips.
    fn row(&self, row: DeckScope, mut units: Vec<Unit<'_>>) -> Vec<Chip> {
        units.sort_by(|a, b| self.rank.order(a, b));
        units
            .iter()
            .take(ROW_CHIPS)
            .map(|u| self.chip(row, u))
            .collect()
    }

    fn chip(&self, row: DeckScope, u: &Unit<'_>) -> Chip {
        let members: Vec<&ProcView> = self
            .rows
            .iter()
            .filter(|(s, r)| *s == u.scope && u.group.is_none_or(|g| r.name == g.name))
            .map(|(_, r)| *r)
            .collect();
        let mut tiles: Vec<Option<f32>> = members.iter().map(|r| r.cores).collect();
        tiles.resize(tiles.len().max(u.n as usize), None);
        tiles.sort_by(|a, b| {
            let busy = |c: &Option<f32>| c.unwrap_or(f32::NEG_INFINITY);
            busy(b).total_cmp(&busy(a))
        });
        tiles.truncate(CHIP_TILES);
        let scope = self.scope_of(u.scope);
        Chip {
            name: u.name.to_owned(),
            scope,
            tiles,
            n: u.n,
            cores: u.cores,
            footprint: u.footprint,
            age: u
                .group
                .filter(|_| scope == row)
                .and_then(|g| program_age(g, self.snap.at_unix_ms)),
            stuck: members.iter().any(|r| {
                self.stuck.iter().any(|&(pgid, leader)| {
                    (pgid != 0 && r.pgid == pgid) || (leader != 0 && r.pid == leader)
                })
            }),
        }
    }
}

#[cfg(test)]
#[path = "chips_tests.rs"]
mod tests;
