// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The CPU block's facts in the Mac lane (`docs/DESIGN-top-deck-merged-2026-10-21.md` §3–§5): a
//! tile per core, kind by kind in the order the OS lists them (fastest first); the cores' worth
//! of work waiting past the wall, which the deck says in orange words and draws as slats; and
//! the Mac's score through the last 10 minutes.

use crate::model::{HeatCell, Resource, Snapshot};
use crate::trend::TrendView;
use crate::words::{self, Platform};

/// The seconds the Mac's run covers: the last 10 minutes.
const RUN_S: u32 = 600;

/// One kind of core: its letter and each of its cores' busy fraction, a tile each.
#[derive(Clone, Debug, PartialEq)]
pub struct CoreKind {
    /// The OS's short name for the kind (`S`, `P`, `E`), or `CPU` where it names none.
    pub short: String,
    /// Each core's busy fraction (`0..=1`), in the order the OS lists the kind's cores.
    pub loads: Vec<TrendView>,
}

/// What the CPU block draws for the whole Mac.
#[derive(Clone, Debug, PartialEq)]
pub struct CpuGlance {
    /// Each kind of core that holds one, fastest first; a lone `CPU` where the OS names none.
    pub kinds: Vec<CoreKind>,
    /// Cores' worth busy over the whole Mac, of [`Self::cores`]: the score gauge.
    pub busy: TrendView,
    /// Logical cores.
    pub cores: usize,
    /// The share of the cores busy as the deck's figure says it (`0..=1`), where read.
    pub score: Option<f32>,
    /// Cores' worth of work waiting past the wall (§5): the 1-minute load average less the
    /// cores, a slat each; 0 wherever the deck says no words for it ([`words::waiting_words`]:
    /// under 0.05, and on Linux, whose load average counts disk waits).
    pub waiting: f32,
    /// The orange words over the slats, `~8 waiting`, where the deck says them.
    pub words: Option<String>,
}

impl CpuGlance {
    /// The CPU as `snap` read it on `platform`, or `None` before its first reading.
    #[must_use]
    pub fn of(snap: &Snapshot, platform: Platform) -> Option<Self> {
        let cpu = snap.cpu.as_ref().filter(|c| c.total() > 0)?;
        let kinds = if cpu.clusters.is_empty() {
            vec![CoreKind {
                short: "CPU".to_owned(),
                loads: cpu.cores.iter().map(|c| c.load).collect(),
            }]
        } else {
            cpu.clusters
                .iter()
                .filter(|c| !c.cores.is_empty())
                .map(|c| CoreKind {
                    short: c.short.clone(),
                    loads: c
                        .cores
                        .iter()
                        .filter_map(|&i| cpu.cores.get(i).map(|c| c.load))
                        .collect(),
                })
                .collect()
        };
        let wait = snap
            .facts
            .load1
            .map_or(f64::NAN, |load| load - cpu.total() as f64);
        let words = words::waiting_words(wait, platform);
        Some(Self {
            kinds,
            busy: cpu.busy,
            cores: cpu.total(),
            score: snap.resources[Resource::Cpu.index()]
                .heat_now
                .filter(|v| !v.is_nan()),
            waiting: if words.is_some() { wait as f32 } else { 0.0 },
            words,
        })
    }

    /// Past capacity: the gauge's orange top, only ever beside its words (§4).
    #[must_use]
    pub const fn over(&self) -> bool {
        self.words.is_some()
    }
}

/// The Mac's run (§4): the last 10 minutes in `columns` columns, oldest first, each the share
/// of the cores busy, a column with no reading hollow. A column is past capacity only on a
/// platform whose deck can say what waits past the wall: never on Linux, which says no waiting
/// words, so no orange shows there without them (§4).
#[must_use]
pub fn run(snap: &Snapshot, platform: Platform, columns: usize) -> Vec<HeatCell> {
    let says_waiting = words::waiting_words(1.0, platform).is_some();
    let per_s = (RUN_S / u32::try_from(columns).unwrap_or(u32::MAX).max(1)).max(1);
    let mut cells =
        snap.resources[Resource::Cpu.index()].heat_columns(per_s, columns, snap.at_unix_ms);
    for c in &mut cells {
        c.over &= says_waiting;
    }
    cells
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::{self, DeckScene};
    use crate::model::{ClusterView, CoreView, CpuView};

    /// The deck's run: a bar per 30 s.
    const BARS: usize = 20;

    fn loads(n: usize, from: f32) -> Vec<CoreView> {
        (0..n)
            .map(|i| CoreView {
                load: TrendView::flat(from + 0.01 * i as f32),
            })
            .collect()
    }

    fn cluster(name: &str, short: &str, cores: impl IntoIterator<Item = usize>) -> ClusterView {
        ClusterView {
            name: name.to_owned(),
            short: short.to_owned(),
            cores: cores.into_iter().collect(),
        }
    }

    fn shape(g: &CpuGlance) -> Vec<(&str, usize)> {
        g.kinds
            .iter()
            .map(|k| (k.short.as_str(), k.loads.len()))
            .collect()
    }

    /// The busy Mac: 6 Super then 12 Performance tiles, each its core's load; 25.7 on 18
    /// cores is about 8 waiting, said and past capacity.
    #[test]
    fn busy_reads_its_kinds_and_what_waits() {
        let snap = fixtures::snapshot(DeckScene::Busy);
        let g = CpuGlance::of(&snap, Platform::MacOs).expect("busy reads its CPU");
        assert_eq!(shape(&g), [("S", 6), ("P", 12)]);
        let cpu = snap.cpu.as_ref().expect("the CPU");
        for (kind, cluster) in g.kinds.iter().zip(&cpu.clusters) {
            let want: Vec<TrendView> = cluster.cores.iter().map(|&i| cpu.cores[i].load).collect();
            assert_eq!(kind.loads, want, "{}", kind.short);
        }
        assert_eq!(g.cores, 18);
        assert!((g.waiting - 7.7).abs() < 1e-3, "{}", g.waiting);
        assert_eq!(g.words.as_deref(), Some("~8 waiting"));
        assert!(g.over());
        assert!(
            g.score.is_some_and(|v| (v - 0.99).abs() < 1e-4),
            "{:?}",
            g.score
        );
        let cells = run(&snap, Platform::MacOs, BARS);
        assert_eq!(cells.len(), BARS);
        assert!(cells.iter().all(|c| c.heat.is_some()), "{cells:?}");
        assert!(cells[BARS - 1].over && !cells[0].over, "{cells:?}");
    }

    /// The calm Mac: the same tiles, nothing waiting, nothing said, nothing over.
    #[test]
    fn calm_waits_for_nothing() {
        let snap = fixtures::snapshot(DeckScene::Calm);
        let g = CpuGlance::of(&snap, Platform::MacOs).expect("calm reads its CPU");
        assert_eq!(shape(&g), [("S", 6), ("P", 12)]);
        assert_eq!(
            (g.waiting, g.words.as_deref(), g.over()),
            (0.0, None, false)
        );
        assert!(run(&snap, Platform::MacOs, BARS).iter().all(|c| !c.over));
    }

    /// Linux's load average counts threads waiting on the disk, so its deck says no waiting
    /// words, draws no slats and no orange: neither the gauge nor any run column is over.
    #[test]
    fn linux_says_no_waiting_words() {
        let snap = fixtures::snapshot(DeckScene::Busy);
        let g = CpuGlance::of(&snap, Platform::Linux).expect("busy reads its CPU");
        assert_eq!(
            (g.waiting, g.words.as_deref(), g.over()),
            (0.0, None, false)
        );
        assert!(run(&snap, Platform::Linux, BARS).iter().all(|c| !c.over));
        assert!(
            run(&snap, Platform::MacOs, BARS).iter().any(|c| c.over),
            "CONTROL: the same run is over on macOS"
        );
    }

    /// The words count every core's worth waiting, past the ten slats the wall's column holds;
    /// under 0.05 waits nothing.
    #[test]
    fn the_words_count_what_the_slats_cannot() {
        let mut snap = fixtures::snapshot(DeckScene::Busy);
        snap.facts.load1 = Some(40.2);
        let g = CpuGlance::of(&snap, Platform::MacOs).expect("the CPU");
        assert!((g.waiting - 22.2).abs() < 1e-3, "{}", g.waiting);
        assert_eq!(g.words.as_deref(), Some("~22 waiting"));
        snap.facts.load1 = Some(18.04);
        let g = CpuGlance::of(&snap, Platform::MacOs).expect("the CPU");
        assert_eq!((g.waiting, g.words.as_deref()), (0.0, None));
        snap.facts.load1 = None;
        let g = CpuGlance::of(&snap, Platform::MacOs).expect("the CPU");
        assert_eq!(
            (g.waiting, g.words.as_deref()),
            (0.0, None),
            "an unread load"
        );
    }

    /// Kinds follow the OS's cluster order, fastest first, whatever the cores' ids; a cluster
    /// with no core draws nothing; a CPU whose OS names no kind is one `CPU` of every core; an
    /// unread CPU, or one of no cores, is no glance.
    #[test]
    fn tiles_follow_the_clusters_in_their_order() {
        let mut snap = fixtures::snapshot(DeckScene::Calm);
        let mut cores = loads(10, 0.1);
        cores[8].load = TrendView::flat(0.9);
        snap.cpu = Some(CpuView {
            cores,
            clusters: vec![
                cluster("Performance", "P", [8, 9]),
                cluster("Unused", "U", []),
                cluster("Efficiency", "E", 0..8),
            ],
            ..CpuView::default()
        });
        let g = CpuGlance::of(&snap, Platform::MacOs).expect("the CPU");
        assert_eq!(shape(&g), [("P", 2), ("E", 8)]);
        assert_eq!(
            g.kinds[0].loads[0],
            TrendView::flat(0.9),
            "P's first core is id 8"
        );
        snap.cpu = Some(CpuView {
            cores: loads(12, 0.2),
            ..CpuView::default()
        });
        let g = CpuGlance::of(&snap, Platform::Linux).expect("the CPU");
        assert_eq!(shape(&g), [("CPU", 12)]);
        snap.cpu = Some(CpuView::default());
        assert_eq!(CpuGlance::of(&snap, Platform::MacOs), None, "no cores");
        let absent = fixtures::snapshot(DeckScene::Absent);
        assert_eq!(
            CpuGlance::of(&absent, Platform::MacOs),
            None,
            "not read yet"
        );
        let hollow = run(&absent, Platform::MacOs, BARS);
        assert_eq!(hollow.len(), BARS);
        assert!(
            hollow.iter().all(|c| c.heat.is_none()),
            "an unread run is hollow"
        );
    }
}
