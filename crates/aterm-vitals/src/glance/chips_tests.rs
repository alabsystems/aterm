// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::fixtures::{self, DeckScene, GIB};
use crate::model::{ProcState, ScopeView, TabView};

fn scene(scene: DeckScene) -> (Snapshot, &'static [u64]) {
    let panes = fixtures::tabs(scene)[fixtures::front(scene)].panes;
    (fixtures::snapshot(scene), panes)
}

fn names(row: &ChipRow) -> Vec<&str> {
    row.chips.iter().map(|c| c.name.as_str()).collect()
}

fn chip<'a>(row: &'a ChipRow, name: &str) -> &'a Chip {
    row.chips
        .iter()
        .find(|c| c.name == name)
        .unwrap_or_else(|| panic!("no {name} in {:?}", names(row)))
}

/// gen.py's whole Mac: busy ranks by CPU (99% over memory's 88%), so the Mac row starts with
/// this tab's rustc, then Spotlight; calm ranks by memory (24% over 6%), and Safari leads.
#[test]
fn the_mac_row_is_the_whole_macs_top_programs_each_in_its_scope() {
    let (busy, panes) = scene(DeckScene::Busy);
    let c = chips(&busy, panes, None);
    let mac = &c.rows[0];
    assert_eq!(c.rank, RankBy::Cpu);
    assert_eq!((mac.scope, mac.count), (DeckScope::Mac, Some(897)));
    let lead: Vec<(&str, DeckScope)> = mac.chips[..3]
        .iter()
        .map(|c| (c.name.as_str(), c.scope))
        .collect();
    assert_eq!(
        lead,
        [
            ("rustc", DeckScope::ThisTab),
            ("Spotlight", DeckScope::Mac),
            ("Docker", DeckScope::Mac)
        ]
    );
    assert_eq!(
        chip(mac, "aterm").scope,
        DeckScope::AllTabs,
        "aterm's own process is all tabs'"
    );
    assert!(
        mac.chips
            .iter()
            .any(|c| c.name == "claude" && c.scope == DeckScope::AllTabs),
        "another tab's program keeps its own name in the Mac row: {:?}",
        names(mac)
    );

    let (calm, panes) = scene(DeckScene::Calm);
    let c = chips(&calm, panes, None);
    let mac = &c.rows[0];
    assert_eq!(c.rank, RankBy::Memory);
    assert_eq!(mac.count, Some(612));
    assert_eq!(
        (mac.chips[0].name.as_str(), mac.chips[0].scope),
        ("Safari", DeckScope::Mac)
    );
    assert_eq!(names(mac)[..3], ["Safari", "claude", "claude"]);
    assert_eq!(
        mac.chips[1..3].iter().map(|c| c.scope).collect::<Vec<_>>(),
        [DeckScope::AllTabs, DeckScope::ThisTab],
        "Tab 1's claude (3 GiB) before this tab's (1.4 GiB)"
    );
}

/// The all-tabs row lists the other tabs, each as one, and aterm: never this tab, whichever
/// tab is in front.
#[test]
fn all_tabs_lists_the_other_tabs_and_aterm_never_this_tab() {
    let (busy, panes) = scene(DeckScene::Busy);
    let tabs = &chips(&busy, panes, None).rows[1];
    assert_eq!((tabs.scope, tabs.count), (DeckScope::AllTabs, Some(21)));
    assert_eq!(names(tabs), ["Tab 1", "Tab 4", "aterm", "Tab 2"]);
    assert!(tabs.chips.iter().all(|c| c.scope == DeckScope::AllTabs));
    let tab1 = chip(tabs, "Tab 1");
    assert_eq!(tab1.tiles, [Some(0.30), Some(0.12), Some(0.05)]);
    assert_eq!(tab1.n, 3);
    assert_eq!(
        (tab1.footprint.unwrap_or(0) as f64 / GIB).round(),
        22.0,
        "Tab 1 22 GiB"
    );
    assert!(
        tabs.chips.iter().all(|c| c.age.is_none()),
        "a unit has no age"
    );

    let (calm, panes) = scene(DeckScene::Calm);
    let tabs = &chips(&calm, panes, None).rows[1];
    assert_eq!(tabs.count, Some(18));
    assert_eq!(names(tabs), ["Tab 1", "aterm", "Tab 3", "Tab 4"]);

    // Tab 1 in front: it leaves the row and busy's Tab 3 (both its panes) joins it.
    let tabs = &chips(&busy, &[1], None).rows[1];
    assert_eq!(names(tabs), ["Tab 3", "Tab 4", "aterm", "Tab 2"]);
    assert_eq!(chip(tabs, "Tab 3").n, 14, "both of Tab 3's panes");
}

/// This tab's row covers the front tab's every pane, its count the whole tab's, and gives a
/// long-running program its age in its own row only.
#[test]
fn this_tab_is_the_whole_front_tab_and_its_ages() {
    let (busy, panes) = scene(DeckScene::Busy);
    let c = chips(&busy, panes, None);
    let tab = &c.rows[2];
    assert_eq!((tab.scope, tab.count), (DeckScope::ThisTab, Some(14)));
    assert_eq!(names(tab), ["rustc", "node", "cargo", "zsh"]);
    assert!(tab.chips.iter().all(|c| c.scope == DeckScope::ThisTab));
    let rustc = chip(tab, "rustc");
    assert_eq!((rustc.n, rustc.tiles.len()), (9, CHIP_TILES));
    assert_eq!(rustc.tiles[0], Some(1.0), "busiest first");
    assert_eq!(rustc.age.as_deref(), Some("up 47 min"));
    assert_eq!(chip(tab, "zsh").n, 2, "a shell in each pane: one program");
    let node = chip(tab, "node");
    assert_eq!(
        node.tiles,
        [Some(0.01), Some(0.0)],
        "the other pane's processes are the tab's"
    );
    assert_eq!(node.age, None, "its birth is unread");
    assert_eq!(
        chip(&c.rows[0], "rustc").age,
        None,
        "the Mac row is not rustc's own scope's"
    );

    let (calm, panes) = scene(DeckScene::Calm);
    let tab = &chips(&calm, panes, None).rows[2];
    assert_eq!(tab.count, Some(6));
    assert_eq!(names(tab), ["claude", "node", "zsh", "caffeinate"]);
    assert_eq!(chip(tab, "claude").age.as_deref(), Some("up 3 h"));
}

/// The Mac's two scores pick the rule; the held one stays until the other leads it by more
/// than five points.
#[test]
fn the_ranking_flips_only_past_the_hold() {
    use RankBy::{Cpu, Memory};
    assert_eq!(RankBy::choose(Some(0.50), Some(0.52), None), Memory);
    assert_eq!(RankBy::choose(Some(0.52), Some(0.52), None), Cpu, "a tie");
    assert_eq!(RankBy::choose(Some(0.50), Some(0.54), Some(Cpu)), Cpu);
    assert_eq!(RankBy::choose(Some(0.50), Some(0.56), Some(Cpu)), Memory);
    assert_eq!(RankBy::choose(Some(0.54), Some(0.50), Some(Memory)), Memory);
    assert_eq!(RankBy::choose(Some(0.56), Some(0.50), Some(Memory)), Cpu);
    assert_eq!(RankBy::choose(None, Some(0.10), Some(Cpu)), Memory);
    assert_eq!(RankBy::choose(Some(0.10), None, Some(Memory)), Cpu);
    assert_eq!(RankBy::choose(None, None, Some(Memory)), Memory);
    assert_eq!(RankBy::choose(None, None, None), Cpu);

    // A busy-on-CPU program and a big one: the scores cross back and forth inside the hold
    // and the chips stay put; past it they turn, and stay turned on the way back.
    let mut s = two_programs();
    let mut held = None;
    let mut seen = Vec::new();
    for (cpu, mem) in [
        (0.50, 0.48),
        (0.50, 0.53),
        (0.50, 0.48),
        (0.50, 0.56),
        (0.52, 0.54),
    ] {
        s.resources[Resource::Cpu.index()].heat_now = Some(cpu);
        s.resources[Resource::Memory.index()].heat_now = Some(mem);
        let c = chips(&s, &[7], held);
        held = Some(c.rank);
        seen.push(names(&c.rows[2]).concat());
    }
    assert_eq!(seen, ["hotbig", "hotbig", "hotbig", "bighot", "bighot"]);
}

/// A tab holding `hot` (busy, small) and `big` (idle, large) in front.
fn two_programs() -> Snapshot {
    let row = |pid: u32, name: &str, cores: f32, gib: f64| ProcView {
        pid,
        ppid: 1,
        pgid: pid,
        name: name.into(),
        cores: Some(cores),
        wait: None,
        footprint: Some((gib * GIB) as u64),
        peak: None,
        write_bps: None,
        state: ProcState::Run,
        scope: RowScope::Tab(7),
        refused: false,
        gpu: None,
        birth_ms: None,
    };
    let group = |name: &str, cores: f32, gib: f64| ProgramGroup {
        name: name.into(),
        scope: ProcScope::Tab(7),
        n: 1,
        cores,
        footprint: Some((gib * GIB) as u64),
        born_ms: None,
        unborn: 1,
    };
    Snapshot {
        tabs: vec![TabView {
            key: 7,
            tab: 7,
            label: "Tab 1".into(),
            scope: ScopeView {
                procs: 2,
                ..ScopeView::default()
            },
            ..TabView::default()
        }],
        procs: vec![row(10, "hot", 2.0, 0.1), row(11, "big", 0.0, 40.0)],
        programs: vec![group("hot", 2.0, 0.1), group("big", 0.0, 40.0)],
        ..Snapshot::default()
    }
}

/// The ring goes on the chip of the front tab's stuck job, found by its process group or its
/// leader, and on no other: not for a stuck job in another tab, nor for one that is not stuck.
#[test]
fn the_stuck_chip_is_the_front_tabs_stuck_job() {
    let (busy, panes) = scene(DeckScene::Busy);
    let ringed = |s: &Snapshot, panes: &[u64]| -> Vec<(DeckScope, String)> {
        let c = chips(s, panes, None);
        c.rows
            .iter()
            .flat_map(|r| r.chips.iter().filter(|c| c.stuck))
            .map(|c| (c.scope, c.name.clone()))
            .collect()
    };
    assert_eq!(
        ringed(&busy, panes),
        [(DeckScope::ThisTab, "node".to_owned())]
    );
    let with = |edit: &dyn Fn(&mut crate::jobs::JobRow)| {
        let mut s = busy.clone();
        let mut jobs = s.jobs.as_deref().cloned().unwrap_or_default();
        for r in jobs.rows.iter_mut().filter(|r| r.exe == "node") {
            edit(r);
        }
        s.jobs = Some(std::sync::Arc::new(jobs));
        s
    };
    let by_group = with(&|r| r.leader.0 = 0);
    assert_eq!(ringed(&by_group, panes).len(), 1, "by its process group");
    let by_leader = with(&|r| r.pgid = 0);
    assert_eq!(ringed(&by_leader, panes).len(), 1, "by its leader");
    let neither = with(&|r| (r.pgid, r.leader.0) = (0, 0));
    assert!(ringed(&neither, panes).is_empty());
    let fine = with(&|r| r.verdict = Verdict::Fine);
    assert!(ringed(&fine, panes).is_empty(), "not stuck");
    assert!(
        ringed(&busy, &[1]).is_empty(),
        "Tab 3's stuck node, with Tab 1 in front"
    );
}

/// A chip gives one tile per process however many rows the sweep kept: a process it kept no
/// row for is unread, never idle, and the tiles stop at [`CHIP_TILES`].
#[test]
fn a_member_without_a_row_is_an_unread_tile() {
    let (mut busy, panes) = scene(DeckScene::Busy);
    let mut seen = 0;
    busy.procs.retain(|p| {
        let keep = p.name != "Spotlight" || seen < 2;
        seen += usize::from(p.name == "Spotlight");
        keep
    });
    let spotlight = chip(&chips(&busy, panes, None).rows[0], "Spotlight").clone();
    assert_eq!(spotlight.n, 7);
    assert_eq!(
        spotlight.tiles,
        [Some(0.98), Some(0.96), None, None, None, None, None]
    );
    for p in busy.programs.iter_mut().filter(|g| g.name == "Spotlight") {
        p.n = 40;
    }
    let spotlight = chip(&chips(&busy, panes, None).rows[0], "Spotlight").clone();
    assert_eq!((spotlight.n, spotlight.tiles.len()), (40, CHIP_TILES));
}

/// A row lists at most [`ROW_CHIPS`], and a snapshot before the first sweep has rows with no
/// chips and no counts.
#[test]
fn rows_are_bounded_and_empty_before_a_sweep() {
    let (busy, panes) = scene(DeckScene::Busy);
    let c = chips(&busy, panes, None);
    assert_eq!(c.rows[0].chips.len(), ROW_CHIPS, "busy has more programs");
    let none = chips(&fixtures::snapshot(DeckScene::Absent), panes, None);
    for (row, scope) in none.rows.iter().zip(DeckScope::ALL) {
        assert_eq!(row.scope, scope);
        assert!(row.chips.is_empty() && row.count.is_none(), "{row:?}");
    }
}

/// The proof: busy's and calm's rows as the deck would lay them, printed beside the mockups'
/// (gen.py's `who` rows ranked by `rank_by`).
#[test]
#[ignore = "print-only proof: --ignored --nocapture"]
fn print_rows_beside_gen_py() {
    let gen_py = [
        (
            DeckScene::Busy,
            [
                "rustc 15.6 GiB, Spotlight 1.2 GiB, Docker 24 GiB, Tab 1, WindowServer",
                "Tab 1 22 GiB, Tab 4 20 GiB, aterm 1.1 GiB",
                "rustc 15.6 GiB up 47 min, node 1.1 GiB (stuck 12 min)",
            ],
        ),
        (
            DeckScene::Calm,
            [
                "Safari 6.1 GiB, Tab 1 3.6 GiB, claude 1.4 GiB, aterm 1.1 GiB, WindowServer",
                "Tab 1 3.6 GiB, aterm 1.1 GiB, Tab 3 0.2 GiB",
                "claude 1.4 GiB up 3 h, node 0.3 GiB",
            ],
        ),
    ];
    for (sc, want) in gen_py {
        let (s, panes) = scene(sc);
        let c = chips(&s, panes, None);
        println!("{} (ranked by {:?})", sc.name(), c.rank);
        for (row, want) in c.rows.iter().zip(want) {
            let ours: Vec<String> = row
                .chips
                .iter()
                .take(5)
                .map(|c| {
                    let tiles: Vec<String> = c
                        .tiles
                        .iter()
                        .map(|t| t.map_or("-".into(), |t| format!("{t:.2}")))
                        .collect();
                    format!(
                        "{} [{}] {}{}{}{}",
                        c.name,
                        tiles.join(" "),
                        crate::words::gib_words(c.footprint.unwrap_or(0)),
                        c.age
                            .as_deref()
                            .map(|a| format!(" {a}"))
                            .unwrap_or_default(),
                        if c.stuck { " (stuck)" } else { "" },
                        if c.scope == row.scope {
                            String::new()
                        } else {
                            format!(" <{:?}>", c.scope)
                        },
                    )
                })
                .collect();
            println!("  {:?} {:?}", row.scope, row.count);
            println!("    ours:   {}", ours.join(", "));
            println!("    gen.py: {want}");
        }
    }
}
