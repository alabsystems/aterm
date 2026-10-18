// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The wake census at the seam.

use super::*;

/// One gesture of the census schedule.
#[derive(Clone, Copy, Debug)]
enum CensusGesture {
    /// One typed glyph, echoed one cell right.
    Key,
    /// A keyed same-row jump to this column.
    JumpTo(u16),
    /// One Backspace: the caret retreats a cell.
    Backspace,
    /// Enter: the caret drops a row to column 0.
    Enter,
}

/// The census schedule, in ms from the first key: 35 keys at 70 ms, a 1.5 s pause, 9 keys at
/// 35 ms, a 1 s pause, four Ctrl-A / Ctrl-E jumps 500 ms apart, four backspaces at 100 ms, Enter.
fn census_schedule() -> Vec<(u64, CensusGesture)> {
    let mut s = Vec::new();
    let mut t = 0u64;
    for _ in 0..35 {
        s.push((t, CensusGesture::Key));
        t += 70;
    }
    t += 1_500 - 70;
    for _ in 0..9 {
        s.push((t, CensusGesture::Key));
        t += 35;
    }
    t += 1_000 - 35;
    for k in 0..4u64 {
        let to = if k % 2 == 0 { 0 } else { 48 };
        s.push((t, CensusGesture::JumpTo(to)));
        t += 500;
    }
    for _ in 0..4 {
        s.push((t, CensusGesture::Backspace));
        t += 100;
    }
    t += 400;
    s.push((t, CensusGesture::Enter));
    s
}

/// Who owned one seam wake — the term whose answer set the deadline.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum SeamOwner {
    /// v2's brisk light: the frame train, by the engine's own word.
    V2Brisk,
    /// v2's next visible step — a tail or an edge, never the train.
    V2Tail,
    /// v1's crown window alone held the frame train (no v2 brisk light).
    V1Crown,
    /// Another v1 brisk pool held the train (sparks, ring, pops…).
    V1Other,
    /// A v1 coarse poll: the kill smoke or a pending poof hint.
    V1Poll,
}

/// The census of one scripted session through the seam: every wake the
/// seam asked for, by owner, plus the idle marks after the last key.
#[derive(Debug, Default)]
struct SeamCensus {
    wakes: std::collections::BTreeMap<SeamOwner, u32>,
    /// Ticks on which `is_active` held with no deadline offered: the fingerprint-only pin the
    /// host's train would otherwise re-arm on.
    fp_only: u32,
    /// ms after the last gesture at which `needs_frame_cadence` first read
    /// `false` (and stayed so).
    cadence_false_ms: Option<u64>,
    /// ms after the last gesture at which the engine's own brisk light first
    /// released (`Engine::needs_frame_cadence` false, and stayed so); the seam
    /// may not hold the train past this.
    engine_brisk_false_ms: Option<u64>,
    /// ms after the last gesture at which `next_change_deadline` first read
    /// `None`.
    deadline_none_ms: Option<u64>,
}

/// Drive the seam through [`census_schedule`] as the host's lane would: a wake
/// at every gesture and every deadline the seam offers, one tick per wake,
/// recording the owner of each offered deadline. `lane` is the host's
/// effect-present interval.
fn seam_wake_census(lane: Duration) -> SeamCensus {
    let g = Geom {
        cw: 18,
        ch: 40,
        rows: 26,
        cols: 190,
        origin_x: 0,
        origin_y: 0,
        win_w: 3420,
        win_h: 1040,
        head: 0,
    };
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    let mut out = Vec::new();
    let script = census_schedule();
    let last_at = t0 + Duration::from_millis(script.last().map_or(0, |s| s.0));
    let end = last_at + Duration::from_secs(6);
    let mut census = SeamCensus::default();
    let mut cell = (12u16, 4u16);
    let mut next_ev = 0usize;
    // Seed: one dark tick so the engine has a source cell.
    let mut now = t0 - Duration::from_millis(50);
    glow.tick(Some(cell), now, &c, g, &mut out);
    let mut due = glow.next_change_deadline(now, lane);
    loop {
        let ev_at = script
            .get(next_ev)
            .map(|(ms, _)| t0 + Duration::from_millis(*ms));
        let wake = match (due, ev_at) {
            (Some(d), Some(e)) => d.min(e),
            (Some(d), None) => d,
            (None, Some(e)) => e,
            (None, None) => break,
        };
        if wake > end {
            break;
        }
        assert!(
            wake > now || ev_at == Some(wake),
            "the seam offered a deadline at `now` — a busy re-arm, not a next change"
        );
        now = wake;
        if ev_at == Some(now) {
            match script[next_ev].1 {
                CensusGesture::Key => {
                    glow.note_synthetic_typed(now, 1);
                    cell.1 += 1;
                }
                CensusGesture::JumpTo(col) => {
                    glow.note_motion(now);
                    cell.1 = col;
                }
                CensusGesture::Backspace => {
                    glow.note_backspace(now);
                    cell.1 = cell.1.saturating_sub(1);
                }
                CensusGesture::Enter => {
                    glow.note_return(now);
                    cell = (cell.0 + 1, 0);
                }
            }
            next_ev += 1;
        }
        glow.tick(Some(cell), now, &c, g, &mut out);
        glow.drain_sound_cues().for_each(drop);
        // Classify the deadline the seam offers from this state.
        due = glow.next_change_deadline(now, lane);
        let brisk_v2 = glow.v2.needs_frame_cadence();
        let crown = glow.crown_until.is_some_and(|t| now < t);
        let cadence = glow.needs_frame_cadence();
        let since_last = now.saturating_duration_since(last_at).as_millis() as u64;
        if now >= last_at {
            if cadence {
                census.cadence_false_ms = None;
            } else if census.cadence_false_ms.is_none() {
                census.cadence_false_ms = Some(since_last);
            }
            if brisk_v2 {
                census.engine_brisk_false_ms = None;
            } else if census.engine_brisk_false_ms.is_none() {
                census.engine_brisk_false_ms = Some(since_last);
            }
            if due.is_some() {
                census.deadline_none_ms = None;
            } else if census.deadline_none_ms.is_none() {
                census.deadline_none_ms = Some(since_last);
            }
        }
        if glow.is_active() && due.is_none() {
            census.fp_only += 1;
        }
        let owner = if cadence {
            if brisk_v2 {
                SeamOwner::V2Brisk
            } else if crown {
                SeamOwner::V1Crown
            } else {
                SeamOwner::V1Other
            }
        } else if let Some(d) = due {
            let v2_due = glow
                .v2
                .engaged()
                .then(|| glow.v2.next_change_deadline(now))
                .flatten();
            if v2_due == Some(d) {
                SeamOwner::V2Tail
            } else {
                SeamOwner::V1Poll
            }
        } else {
            continue;
        };
        *census.wakes.entry(owner).or_default() += 1;
    }
    census
}

/// The seam's wake census: wakes by owner over the census schedule. No v1 pool holds the frame
/// train under v2, and the seam's cadence releases when the engine's does.
#[test]
fn under_v2_the_seam_never_paces_the_frame_train_for_v1s_crown() {
    let lane = Duration::from_micros(16_667);
    let v2 = seam_wake_census(lane);
    assert_eq!(
        v2.wakes.get(&SeamOwner::V1Crown).copied().unwrap_or(0),
        0,
        "v1's crown held the frame train under v2"
    );
    assert_eq!(
        v2.wakes.get(&SeamOwner::V1Other).copied().unwrap_or(0),
        0,
        "a v1 brisk pool held the frame train under v2"
    );
    // The seam releases the train on the engine's word: the retracting mark
    // after Enter is brisk light by the ribbon's own law.
    let cadence_false = v2
        .cadence_false_ms
        .expect("the seam's cadence never released");
    let engine_false = v2.engine_brisk_false_ms.expect("the engine never released");
    assert_eq!(
        cadence_false, engine_false,
        "the seam held frame cadence {cadence_false} ms after the last key where the \
         engine released at {engine_false} ms"
    );
    let none = v2.deadline_none_ms.expect("the seam never went idle");
    assert!(
        none <= 2_000,
        "the seam offered wakes {none} ms after the last key"
    );
    assert_eq!(v2.fp_only, 0, "a fingerprint-only pin with no deadline");
    // v2's brisk wakes are its only frame-cadence wakes (tail wakes are paced `TAIL_FLOOR` apart
    // and counted separately), so the train is at most one wake per lane tick over the session;
    // an unticked brisk wake is a busy re-arm. No glyph probe is fed, so no stardust is born; the
    // exhaust's train cost is bounded in
    // `rk::stardust::tests::the_exhaust_costs_the_frame_train_its_own_window_and_no_more`.
    let train_v2 = v2.wakes.get(&SeamOwner::V2Brisk).copied().unwrap_or(0);
    let last_ms = census_schedule().last().map_or(0, |s| s.0);
    let lane_ticks =
        ((last_ms + cadence_false) as f64 / (lane.as_secs_f64() * 1_000.0)).ceil() as u32 + 1;
    assert!(
        train_v2 > 0 && train_v2 <= lane_ticks,
        "v2 held the frame train for {train_v2} wakes where the lane ticked {lane_ticks} \
         times"
    );
}
