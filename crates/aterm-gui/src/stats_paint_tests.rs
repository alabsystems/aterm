// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_vitals::jobs::{Class, Consumer, JobKind, JobRow, JobsView, Tick, Verdict, ZombieParent};

/// A job of `verdict` in tab 7, quiet for `quiet_ms` at `phi_milli`.
pub(crate) fn job_row(
    id: &str,
    verdict: Verdict,
    phi_milli: Option<u64>,
    quiet_ms: Option<u64>,
) -> JobRow {
    JobRow {
        id: id.into(),
        tab: 7,
        kind: JobKind::Shell,
        orphan_of: None,
        consumer: Consumer::default(),
        promoted_ms: None,
        task: None,
        pgid: 300,
        leader: (300, None),
        exe: "make".into(),
        members: 1,
        class: Class::Running,
        wait: None,
        named: None,
        stop: None,
        input: None,
        cpu_mc: None,
        zombies: None,
        standing: None,
        quiet_ms,
        phi_milli,
        mu_ms: None,
        mu_from: None,
        verdict,
        no_progress: false,
        working_descendant: None,
        episode: 0,
        exit: None,
        tick: Tick::default(),
    }
}

/// Zombies unreaped by `ppid`, in tab `tab` (or outside aterm).
pub(crate) fn zombies(ppid: u32, count: u32, tab: Option<u64>) -> ZombieParent {
    ZombieParent {
        ppid,
        name: "node".into(),
        count,
        per_min_deci: 0,
        tab,
    }
}

#[test]
fn the_marks_count_the_engines_verdicts_worst_first() {
    let calm = JobsView {
        rows: vec![
            job_row("j1", Verdict::Fine, None, None),
            job_row("j2", Verdict::Busy, Some(1_000), Some(4_000)),
        ],
        findings: aterm_vitals::jobs::Findings {
            zombies: vec![zombies(1, 4, None)],
            ..Default::default()
        },
        ..Default::default()
    };
    assert!(marks_of(&calm).is_empty(), "a calm machine has no marks");

    let busy = JobsView {
        rows: vec![
            job_row("j1", Verdict::Parked, None, None),
            job_row("j2", Verdict::Quiet, Some(4_000), Some(30_000)),
            job_row("j3", Verdict::Stuck, Some(9_000), Some(90_000)),
            job_row("j4", Verdict::WaitingYou, Some(0), Some(5_000)),
            job_row("j5", Verdict::Stuck, Some(12_000), Some(200_000)),
        ],
        findings: aterm_vitals::jobs::Findings {
            zombies: vec![zombies(1, 4, None), zombies(300, 3, Some(7))],
            ..Default::default()
        },
        ..Default::default()
    };
    let marks: Vec<(WellMark, u32)> = marks_of(&busy).iter().map(|m| (m.kind, m.count)).collect();
    assert_eq!(
        marks,
        [
            (WellMark::Stuck, 2),
            (WellMark::Zombies, 3),
            (WellMark::Quiet, 1),
            (WellMark::Waiting, 1),
            (WellMark::Stopped, 1),
        ],
        "only the zombies a tab's process holds count"
    );
}

#[test]
fn the_quiet_arc_fills_from_the_quiet_line_to_the_stuck_line() {
    assert_eq!(quiet_arc(aterm_vitals::jobs::PHI_QUIET_MILLI), 0.0);
    assert_eq!(quiet_arc(aterm_vitals::jobs::PHI_STUCK_MILLI), 1.0);
    assert_eq!(quiet_arc(20_000), 1.0);
    assert_eq!(quiet_arc(0), 0.0);
    let view = JobsView {
        rows: vec![
            job_row("j1", Verdict::Quiet, Some(4_000), None),
            job_row("j2", Verdict::Quiet, Some(5_500), None),
        ],
        ..Default::default()
    };
    let quiet = marks_of(&view);
    assert_eq!(quiet.len(), 1);
    assert_eq!(quiet[0].frac, quiet_arc(5_500), "the worst φ sets the arc");
    assert!(quiet[0].frac > quiet_arc(4_000));
}

fn inks() -> VitalsInks {
    aterm_vitals::ink::vitals_inks([0x30, 0x31, 0x35], [0xF8, 0xF8, 0xF2], [0x50, 0xFA, 0x7B])
}

#[test]
fn the_ladder_is_keyed_to_drawn_size() {
    assert_eq!(Rung::of(48.0), Rung::Full);
    assert_eq!(Rung::of(32.0), Rung::Full);
    assert_eq!(Rung::of(16.0), Rung::Mid);
    assert_eq!(Rung::of(10.0), Rung::Mid);
    assert_eq!(Rung::of(7.0), Rung::Low);
}

#[test]
fn a_tiles_bands_keep_its_rounded_silhouette() {
    let inks = inks();
    let mut p = Painter::new(&inks, 2.0, 24.0, false);
    let m = Mark {
        level: 0.6,
        ghost: 0.3,
        edge: 0,
        glow: 2,
        motion: Motion::Rising,
    };
    p.tile(100.0, 10.0, 48.0, m, 10.0);
    let tile_panels: Vec<f32> = p
        .prims
        .iter()
        .filter_map(|prim| match prim {
            DrawPrim::Panel {
                x, y, w, h, radius, ..
            } if *x == 100.0 && *y == 10.0 && *w == 48.0 && *h == 48.0 => Some(*radius),
            _ => None,
        })
        .collect();
    assert!(
        tile_panels.len() >= 4,
        "track, fill, sliver, tide: {tile_panels:?}"
    );
    assert!(
        tile_panels
            .iter()
            .all(|r| *r == tile_panels[0] && *r >= 3.0)
    );
    for prim in &p.prims {
        if let DrawPrim::Panel { w, radius, y, .. } = prim
            && *radius == 0.0
            && *y >= 10.0
        {
            assert!(
                *w < 48.0 - 2.0 * tile_panels[0] + 1.0 || *w < 16.0,
                "{prim:?}"
            );
        }
    }
}

#[test]
fn a_small_tile_shows_the_glow_as_a_top_rim_and_a_tiny_one_only_its_level() {
    let inks = inks();
    let m = Mark {
        level: 0.5,
        ghost: 0.2,
        edge: 2,
        glow: 3,
        motion: Motion::Rising,
    };
    let mut mid = Painter::new(&inks, 2.0, 24.0, false);
    mid.tile(0.0, 0.0, 16.0, m, 6.0);
    let rise = a(inks.rise, 0.85);
    assert!(
        mid.prims
            .iter()
            .any(|p| matches!(p, DrawPrim::Panel { fill, .. } if *fill == rise))
    );
    assert!(
        !mid.prims
            .iter()
            .any(|p| matches!(p, DrawPrim::Stroke { color, .. } if color[3] < 200)),
        "no feather at 16 px"
    );
    let mut low = Painter::new(&inks, 1.0, 12.0, false);
    low.tile(0.0, 0.0, 7.0, m, 3.0);
    let panels = low
        .prims
        .iter()
        .filter(|p| matches!(p, DrawPrim::Panel { .. }))
        .count();
    assert_eq!(panels, 2, "{:?}", low.prims);
}

#[test]
fn differentiate_without_color_gives_falling_a_shape() {
    let inks = inks();
    let m = Mark {
        level: 0.2,
        ghost: 0.6,
        edge: 0,
        glow: 0,
        motion: Motion::Falling,
    };
    let count = |dwc: bool| {
        let mut p = Painter::new(&inks, 2.0, 24.0, dwc);
        p.tile(0.0, 0.0, 48.0, m, 10.0);
        p.lane(
            0.0,
            80.0,
            300.0,
            10.0,
            m,
            inks.aterm,
            (0.0, 70.0, 300.0, 30.0),
            false,
        );
        p.prims.len()
    };
    assert_eq!(
        count(true),
        count(false) + 2,
        "a bottom notch and a hollow cap"
    );
}
