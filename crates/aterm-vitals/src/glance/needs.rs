// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What needs the owner (`docs/DESIGN-top-deck-merged-2026-10-21.md` §6): the needs rail's
//! pills, worst first, read from the deck's own window. A tab waits for the owner when the jobs
//! engine reads `waiting-you` on any of its panes or the needs system publishes a need for one;
//! a job in the front tab is `stuck` by the engine's verdict; the front tab's volume fills
//! within the hour by the filling clause. A stuck job in a background tab, a disk filling within
//! the day but not the hour, and anything in another window's tabs are no need here. The needs
//! system's words for a stuck program ([`Ask::stuck`]) keep the stuck rule, so one stall is
//! never two pills or a background tab's.
//!
//! These are deck marks, never alerts: nothing here posts.

use std::cmp::Reverse;

use crate::history::URGENT_S;
use crate::jobs::Verdict;
use crate::model::Snapshot;
use crate::words::{self, DeckScope, FormWords};

/// The need word of a password prompt, the one the jobs engine reads as `waiting-you`.
pub const PASSWORD: &str = "password";
/// The need word of a disk full within the hour.
pub const FULL: &str = "full";

/// What a need is about, and so its pill's icon; worst first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum NeedKind {
    /// The front tab's volume is full within the hour: a drive. Worst, since once it is, every
    /// tab's writes fail, and the pill names when.
    Drive,
    /// A tab waits for the owner (a password, an approval, an answer, a login): a lock. Nothing
    /// in it moves until they act.
    Lock,
    /// A job in the front tab is stuck: an hourglass.
    Hourglass,
}

/// One need, as the needs rail draws its pill.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct DeckNeed {
    /// What it is about.
    pub kind: NeedKind,
    /// The rail's lane it sits in, the row it is about: the Mac for the disk, this tab for the
    /// front tab's, all tabs for another tab's.
    pub lane: DeckScope,
    /// Its tab by its place in the window's strip, from 1 (`Tab 1`); `None` for the disk.
    pub tab: Option<usize>,
    /// Its pane's session key; `None` for the disk.
    pub pane: Option<u64>,
    /// The job's program (`sudo`, `node`), where the jobs engine names one.
    pub program: Option<String>,
    /// A stuck job's quiet age, ms.
    pub quiet_ms: Option<u64>,
    /// How soon the disk is full, ms.
    pub full_in_ms: Option<u64>,
    /// The need's own word: [`PASSWORD`]; the needs system's (`approval`, `question`, `login`,
    /// …); the jobs engine's `stuck`; or [`FULL`].
    pub word: &'static str,
}

impl DeckNeed {
    /// Its pill's words at each width (§6, [`words::disk_full_pill`], [`words::waiting_pill`],
    /// [`words::stuck_pill`]): `Disk full in 34 min`, `Tab 1 password`,
    /// `node stuck 12 min · this tab`.
    #[must_use]
    pub fn pill(&self) -> FormWords {
        let tab = self.tab.map_or_else(String::new, |t| format!("Tab {t}"));
        match self.kind {
            NeedKind::Drive => words::disk_full_pill(self.full_in_ms.unwrap_or(0) as f64 / 1000.0),
            NeedKind::Lock => words::waiting_pill(&tab, self.word),
            NeedKind::Hourglass => {
                words::stuck_pill(self.program.as_deref().unwrap_or(""), self.quiet_ms, &tab)
            }
        }
    }
}

/// The deck's window as its needs read it.
#[derive(Clone, Copy, Debug)]
pub struct DeckWindow<'a> {
    /// Every tab's panes (session keys), in the strip's order.
    pub tabs: &'a [Vec<u64>],
    /// The front tab's index in [`Self::tabs`].
    pub front: Option<usize>,
    /// Each pane's need as the needs system publishes it.
    pub asks: &'a [Ask],
}

/// A pane's need as the needs system publishes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ask {
    /// Its pane's session key.
    pub pane: u64,
    /// The need's word: `approval`, `question`, `login`, …
    pub word: &'static str,
    /// The word names a stuck program (`frozen`, `stopped`: the input gate's published stall, or
    /// an agent's screen that stopped answering), which the jobs engine reads as `stuck`. It
    /// keeps the stuck rule: a pill only in the front tab, and none on a pane the engine already
    /// reads stuck, whose pill says it in the engine's word.
    pub stuck: bool,
}

/// What needs the owner in `window` over `snap` (`None` before the first reading: the needs
/// system's alone), worst first: by [`NeedKind`], then the sooner full disk, the longer quiet
/// stuck job, the earlier tab.
#[must_use]
pub fn of(snap: Option<&Snapshot>, window: &DeckWindow<'_>) -> Vec<DeckNeed> {
    let place = |pane: u64| window.tabs.iter().position(|t| t.contains(&pane));
    let need = |kind, i: usize, pane, word| DeckNeed {
        kind,
        lane: if Some(i) == window.front {
            DeckScope::ThisTab
        } else {
            DeckScope::AllTabs
        },
        tab: Some(i + 1),
        pane: Some(pane),
        program: None,
        quiet_ms: None,
        full_in_ms: None,
        word,
    };
    let mut out = Vec::new();
    if let Some(filling) = snap
        .and_then(|s| s.facts.filling)
        .filter(|f| f.secs < URGENT_S)
    {
        out.push(DeckNeed {
            kind: NeedKind::Drive,
            lane: DeckScope::Mac,
            tab: None,
            pane: None,
            program: None,
            quiet_ms: None,
            full_in_ms: Some((filling.secs.max(0.0) * 1000.0) as u64),
            word: FULL,
        });
    }
    let rows = snap
        .and_then(|s| s.jobs.as_deref())
        .map_or(&[][..], |j| &j.rows);
    for row in rows {
        let Some(i) = place(row.tab) else {
            continue;
        };
        let mut job = match row.verdict {
            Verdict::WaitingYou => need(NeedKind::Lock, i, row.tab, PASSWORD),
            Verdict::Stuck if Some(i) == window.front => {
                let mut stuck = need(NeedKind::Hourglass, i, row.tab, Verdict::Stuck.word());
                stuck.quiet_ms = row.quiet_ms;
                stuck
            }
            _ => continue,
        };
        job.program = Some(row.exe.clone());
        out.push(job);
    }
    for ask in window.asks {
        let Some(i) = place(ask.pane) else {
            continue;
        };
        let engine_reads_it = || {
            out.iter()
                .any(|n| n.kind == NeedKind::Hourglass && n.pane == Some(ask.pane))
        };
        if ask.stuck && (Some(i) != window.front || engine_reads_it()) {
            continue;
        }
        out.push(need(NeedKind::Lock, i, ask.pane, ask.word));
    }
    out.sort_by_key(|n| (n.kind, n.full_in_ms, Reverse(n.quiet_ms), n.tab));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::{self, DeckScene};
    use crate::history::Filling;
    use crate::jobs::{JobRow, JobsView};

    fn strip(scene: DeckScene) -> Vec<Vec<u64>> {
        fixtures::tabs(scene)
            .iter()
            .map(|t| t.panes.to_vec())
            .collect()
    }

    /// The needs system's need `word` for `pane`, not a stuck program's.
    fn ask(pane: u64, word: &'static str) -> Ask {
        Ask {
            pane,
            word,
            stuck: false,
        }
    }

    fn scene_needs(scene: DeckScene, front: usize, asks: &[Ask]) -> Vec<DeckNeed> {
        let snap = fixtures::snapshot(scene);
        of(
            Some(&snap),
            &DeckWindow {
                tabs: &strip(scene),
                front: Some(front),
                asks,
            },
        )
    }

    fn busy(snap: &Snapshot, front: usize) -> Vec<DeckNeed> {
        of(
            Some(snap),
            &DeckWindow {
                tabs: &strip(DeckScene::Busy),
                front: Some(front),
                asks: &[],
            },
        )
    }

    /// Busy needs the owner three times, one in each lane, as the mockup's rail draws them: the
    /// disk full in 34 min (the Mac), Tab 1's password (all tabs), the front tab's stuck `node`
    /// (this tab), worst first. Calm and absent need nothing.
    #[test]
    fn busy_needs_one_in_each_lane_and_calm_none() {
        let front = fixtures::front(DeckScene::Busy);
        let needs = scene_needs(DeckScene::Busy, front, &[]);
        for n in &needs {
            println!(
                "{:?} {:?} tab={:?} pane={:?} program={:?} quiet_ms={:?} full_in_ms={:?} word={} \
                 pill={:?}",
                n.kind,
                n.lane,
                n.tab,
                n.pane,
                n.program,
                n.quiet_ms,
                n.full_in_ms,
                n.word,
                n.pill().forms()
            );
        }
        let lanes: Vec<DeckScope> = needs.iter().map(|n| n.lane).collect();
        assert_eq!(lanes, DeckScope::ALL);
        let kinds: Vec<NeedKind> = needs.iter().map(|n| n.kind).collect();
        assert_eq!(
            kinds,
            [NeedKind::Drive, NeedKind::Lock, NeedKind::Hourglass]
        );
        let pills: Vec<String> = needs.iter().map(|n| n.pill().full).collect();
        assert_eq!(
            pills,
            [
                "Disk full in 34 min",
                "Tab 1 password",
                "node stuck 12 min · this tab"
            ]
        );
        assert_eq!(needs[1].tab, Some(1));
        assert_eq!(needs[1].pane, Some(1));
        assert_eq!(needs[2].tab, Some(front + 1));
        assert_eq!(
            needs[2].pane,
            Some(5),
            "the stuck node is in the unfocused pane"
        );
        assert_eq!(
            needs[2].word, "stuck",
            "the engine's verdict, never \"hung\""
        );
        for scene in [DeckScene::Calm, DeckScene::Absent] {
            let none = scene_needs(scene, fixtures::front(scene), &[]);
            assert!(none.is_empty(), "{scene:?}: {none:?}");
        }
    }

    /// A stuck job in a background tab, and a disk filling within the day but not the hour, are
    /// no need (§6); a password prompt is one in whichever tab, in this tab's lane once its tab
    /// is the front one. CONTROL: the same stuck job in the front tab, and the disk at 59 min,
    /// are.
    #[test]
    fn a_background_stuck_job_and_a_days_filling_are_no_need() {
        let mut snap = fixtures::snapshot(DeckScene::Busy);
        let tab1 = busy(&snap, 0);
        assert!(
            tab1.iter().all(|n| n.kind != NeedKind::Hourglass),
            "{tab1:?}"
        );
        let lock = tab1.iter().find(|n| n.kind == NeedKind::Lock);
        assert_eq!(lock.map(|n| n.lane), Some(DeckScope::ThisTab));
        assert!(
            busy(&snap, 2).iter().any(|n| n.kind == NeedKind::Hourglass),
            "CONTROL: the front tab's stuck node"
        );
        let filling = snap.facts.filling.expect("busy fills");
        for (secs, needed) in [
            (2.0 * 3_600.0, false),
            (URGENT_S, false),
            (f64::NAN, false),
            (59.0 * 60.0, true),
        ] {
            snap.facts.filling = Some(Filling { secs, ..filling });
            let drive = busy(&snap, 2).iter().any(|n| n.kind == NeedKind::Drive);
            assert_eq!(drive, needed, "full in {secs} s");
        }
        snap.facts.filling = None;
        assert!(busy(&snap, 2).iter().all(|n| n.kind != NeedKind::Drive));
    }

    /// Only this window's tabs are read: a prompt or a need in a pane no tab of the window holds
    /// is another window's. The needs system's need is a lock in its tab's lane, in its own word.
    #[test]
    fn only_this_windows_tabs_need_it() {
        let mut snap = fixtures::snapshot(DeckScene::Busy);
        snap.facts.filling = None;
        let mut jobs = JobsView::clone(snap.jobs.as_deref().expect("busy's jobs"));
        for row in &mut jobs.rows {
            row.tab = 9;
        }
        snap.jobs = Some(std::sync::Arc::new(jobs));
        let asks = [ask(9, "approval"), ask(4, "question"), ask(3, "login")];
        let needs = of(
            Some(&snap),
            &DeckWindow {
                tabs: &strip(DeckScene::Busy),
                front: Some(2),
                asks: &asks,
            },
        );
        let read: Vec<(DeckScope, Option<usize>, &str, String)> = needs
            .iter()
            .map(|n| (n.lane, n.tab, n.word, n.pill().full))
            .collect();
        assert_eq!(
            read,
            [
                (DeckScope::ThisTab, Some(3), "login", "Tab 3 login".into()),
                (
                    DeckScope::AllTabs,
                    Some(4),
                    "question",
                    "Tab 4 question".into()
                ),
            ]
        );
        assert!(needs.iter().all(|n| n.program.is_none()));
    }

    /// Before the first reading the needs system's needs still show, and nothing else does.
    #[test]
    fn before_a_reading_only_the_needs_systems_show() {
        let tabs = strip(DeckScene::Busy);
        let window = |asks| DeckWindow {
            tabs: &tabs,
            front: Some(2),
            asks,
        };
        assert!(of(None, &window(&[])).is_empty());
        let one = of(None, &window(&[ask(1, "approval")]));
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].pill().full, "Tab 1 approval");
    }

    /// The needs system's word for a stuck program keeps the stuck rule (§6, §9): on a pane the
    /// jobs engine reads stuck it is no second pill, the engine's `stuck` being the one word; in
    /// a background tab it is none; in the front tab before the engine reads the job stuck it is
    /// the tab's lock, in its own word. CONTROL: an approval on each of those panes is a pill.
    #[test]
    fn a_stall_is_one_pill_in_the_engines_word() {
        let snap = fixtures::snapshot(DeckScene::Busy);
        let tabs = strip(DeckScene::Busy);
        let read = |asks: &[Ask]| -> Vec<(NeedKind, Option<u64>, &'static str)> {
            let window = DeckWindow {
                tabs: &tabs,
                front: Some(2),
                asks,
            };
            of(Some(&snap), &window)
                .iter()
                .map(|n| (n.kind, n.pane, n.word))
                .collect()
        };
        let stuck = |pane, word| Ask {
            pane,
            word,
            stuck: true,
        };
        let busy = read(&[]);
        assert!(
            busy.contains(&(NeedKind::Hourglass, Some(5), "stuck")),
            "{busy:?}"
        );
        assert_eq!(
            read(&[stuck(5, "frozen"), stuck(4, "frozen")]),
            busy,
            "the front tab's stuck node and Tab 4's stall are no lock"
        );
        let front = read(&[stuck(3, "stopped")]);
        assert_eq!(front.len(), busy.len() + 1, "{front:?}");
        assert!(
            front.contains(&(NeedKind::Lock, Some(3), "stopped")),
            "{front:?}"
        );
        let control = read(&[ask(5, "approval"), ask(4, "approval")]);
        assert_eq!(control.len(), busy.len() + 2, "CONTROL: {control:?}");
    }

    /// Worst first: the disk, then each waiting tab in strip order, then the front tab's stuck
    /// jobs, the longest quiet first.
    #[test]
    fn needs_come_worst_first() {
        let mut snap = fixtures::snapshot(DeckScene::Busy);
        let mut jobs = JobsView::clone(snap.jobs.as_deref().expect("busy's jobs"));
        let stuck = jobs
            .rows
            .iter()
            .find(|r| r.verdict == Verdict::Stuck)
            .cloned()
            .expect("busy's stuck node");
        let longer = JobRow {
            tab: 3,
            exe: "rg".into(),
            quiet_ms: Some(40 * 60_000),
            ..stuck
        };
        jobs.rows.insert(0, longer);
        snap.jobs = Some(std::sync::Arc::new(jobs));
        let needs = of(
            Some(&snap),
            &DeckWindow {
                tabs: &strip(DeckScene::Busy),
                front: Some(2),
                asks: &[ask(4, "approval")],
            },
        );
        let pills: Vec<String> = needs.iter().map(|n| n.pill().full).collect();
        assert_eq!(
            pills,
            [
                "Disk full in 34 min",
                "Tab 1 password",
                "Tab 4 approval",
                "rg stuck 40 min · this tab",
                "node stuck 12 min · this tab",
            ]
        );
    }
}
