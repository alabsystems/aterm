// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::super::{Layer, Phase};
use super::*;
use crate::state_words::{StateWords, Word};
use aterm_time::Duration;

fn words(r: &Ribbon, t0: Instant) -> Vec<Word> {
    let mut w = StateWords::new(t0);
    r.state_words(&mut w);
    w.into_words()
}

fn laid(t0: Instant) -> Ribbon {
    let born = t0 + Duration::from_millis(8);
    let mut rib = Ribbon::new();
    rib.cells.push(Cell {
        row: 2,
        col: 5,
        cohort: 1,
        t: 0.25,
        born,
        attack_at: born,
        life_s: 1.0,
        cov0: 0.5,
        typing: true,
        retract_at: None,
        retire_at: None,
        birth_disp: 0.4,
        edge_cells: 1.0,
        layer: Layer::Base,
        rearm: None,
        released_at: None,
    });
    rib.cohorts.push(Cohort {
        id: 1,
        row: 2,
        col0: 5,
        col1: 6,
        anchor_col: 5,
        t0: 0.25,
        d0: 0.0,
        born,
        alive_at: born,
        abandoned: false,
        wake: false,
        retract_col: None,
        drain_right_first: false,
        phase: Phase::Laying,
        rejoinable: true,
        phrase: 0,
        grace_s: 1.0,
        flow: None,
        flow_drift: 0.0,
        release_clock: None,
        parts_revoked_at: None,
        line_killed: false,
        printed: None,
    });
    rib.holds.push((2, 0.5, born));
    rib.next_cohort = 2;
    rib
}

#[test]
fn a_hashed_ribbon_field_moves_the_ribbon_words() {
    let t0 = Instant::now();
    let rib = laid(t0);
    let base = words(&rib, t0);
    assert_eq!(base, words(&rib.clone(), t0));

    let mut released = rib.clone();
    released.cells[0].released_at = Some(t0 + Duration::from_millis(30));
    assert_ne!(
        words(&released, t0),
        base,
        "a cell's partial custody is hashed"
    );
    let mut revoked = rib.clone();
    revoked.cohorts[0].parts_revoked_at = Some(t0 + Duration::from_millis(30));
    assert_ne!(
        words(&revoked, t0),
        base,
        "a cohort's private custody is hashed"
    );
    let mut eased = rib.clone();
    eased.holds[0].1 = 0.75;
    assert_ne!(words(&eased, t0), base, "a row's hold is hashed");
    let mut clocked = rib.clone();
    clocked.ember_at = Some(t0);
    assert_ne!(words(&clocked, t0), base, "the ember's clock is hashed");
    let mut planned = rib.clone();
    planned.slabs += 3;
    planned.ink_quads += 1;
    assert_eq!(
        words(&planned, t0),
        base,
        "the frame's plan is output, not state"
    );
}

#[test]
fn every_bed_table_field_moves_the_bed_words() {
    let t0 = Instant::now();
    let words = |bed: &BedInkLut| {
        let mut w = StateWords::new(t0);
        bed.state_words(&mut w);
        w.into_words()
    };
    let cfg =
        super::super::Config::from_glow(&crate::cursor_glow::GlowConfig::sample_for_test(), false);
    let bed = BedInkLut::synced(&cfg);
    let base = words(&bed);
    assert_eq!(
        base,
        words(&BedInkLut::synced(&cfg)),
        "the same table, the same words"
    );
    assert_ne!(words(&BedInkLut::EMPTY), base, "an unsynced table is apart");
    let (fg, bg, dark, tall) = bed.key.expect("a synced table has its key");
    let mut mutants = Vec::new();
    for (what, key) in [
        ("fg", (fg ^ 1, bg, dark, tall)),
        ("bg", (fg, bg ^ 1, dark, tall)),
        ("dark", (fg, bg, !dark, tall)),
        ("tall", (fg, bg, dark, !tall)),
    ] {
        let mut keyed = bed;
        keyed.key = Some(key);
        mutants.push((what, keyed));
    }
    let mut inked = bed;
    inked.lut[BED_INK_LUT_LEN - 1] ^= 1;
    mutants.push(("last ink", inked));
    let mut seen = bed;
    seen.seen[0] = f32::from_bits(seen.seen[0].to_bits() ^ 1);
    mutants.push(("first seen", seen));
    for (what, mutant) in mutants {
        assert_ne!(words(&mutant), base, "the table's {what} is hashed");
    }
}
