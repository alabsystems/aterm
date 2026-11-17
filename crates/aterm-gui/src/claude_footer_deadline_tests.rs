// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! LAW L4, DEADLINE COMPLETE: a time a footer fact holds says when it can next say otherwise
//! (`changes_after`), and nothing it says moves before that second. The footer's own terms are
//! held each second for 26 hours (a weekly figure's age limits and reset, a wall's day words and
//! end), a whole read of the weekly fixtures between each deadline it posts, and the outlook each
//! minute over the quota scenes, as the replay says it at the minute while a pace stands
//! ([`outlook::DRIFT_S`]).
//!
//! NEGATIVE CONTROLS, each run 2026-11-15 and restored:
//! - `Wall::changes_after` without `resets_at − 86 399`: the 7d wall's `Oct 16 at 8:40am` turned
//!   `8:40am` at +3601, before its deadline +90000 (`seven_day: said otherwise at +3601`).
//! - `profile::reading_terms` without `at + 901`: the figure read at −100's `weekly 63% left`
//!   turned `weekly ≤63% left` at +801, before its deadline +3501 (`read: said otherwise at
//!   +801`).
//! - `Outlook::changes_after` without the pace's [`outlook::DRIFT_S`]: case (a)'s `~43% left at
//!   its reset Sat 10:00` turned `~44%` at +2640, before the deadline +3601 the replay at +960
//!   gave (`case a: said otherwise at +2640`).

use std::fmt::Debug;
use std::time::{Duration, Instant, UNIX_EPOCH};

use aterm_agent::harness::footer;
use aterm_agent::harness::outlook;
use aterm_agent::harness::profile::WeeklyFigure;
use aterm_agent::harness::quota_scenes;
use aterm_agent::harness::session_usage::wall_of;
use aterm_agent::harness::usage::LimitNoticeRow;

use super::super::{Asks, Identity, MachineMemo, fingerprint, resolve_facts_at};
use super::{
    NOW, PID, RESET, armed, asks, fresh, profile, reading, records, save_main, stored, tab, tapped,
    unread, untapped, weekly_wall,
};
use crate::session_timeline::SessionTimeline;

/// How long each sweep runs: a day and the two hours a reset can stand past it.
const SPAN: i64 = 26 * 3600;

/// Walk `said` from `from` over [`SPAN`], a sample each `step` seconds: until a sample at or past
/// the deadline `changes` gave at the last one taken, every sample says what that one said. How
/// many deadlines it met at which `said` moved.
fn sweep<W: PartialEq + Debug>(
    what: &str,
    (from, step): (i64, usize),
    mut said: impl FnMut(i64) -> W,
    mut changes: impl FnMut(i64) -> Option<i64>,
) -> usize {
    let (mut since, mut words, mut due) = (from, said(from), changes(from));
    let mut moved = 0;
    for t in (from..=from + SPAN).step_by(step).skip(1) {
        assert!(
            due.is_none_or(|due| due > since),
            "{what}: a deadline after its second"
        );
        let now = said(t);
        if due.is_some_and(|due| t >= due) {
            moved += usize::from(now != words);
            (since, words, due) = (t, now, changes(t));
            continue;
        }
        assert_eq!(
            now,
            words,
            "{what}: said otherwise at +{} before its deadline {:?}, computed at +{}",
            t - from,
            due.map(|due| due - from),
            since - from
        );
    }
    moved
}

/// One session read as the resolver keeps it between reads, judged at any second.
struct Reader {
    timeline: std::sync::Arc<std::sync::Mutex<SessionTimeline>>,
    identity: Identity,
    tail: footer::TailCache,
    fold: footer::FooterCache,
    machine: MachineMemo,
    asks: Asks,
}

impl Reader {
    fn new(asks: Asks) -> Reader {
        Reader {
            timeline: tab(),
            identity: unread(&records()),
            tail: footer::TailCache::default(),
            fold: footer::FooterCache::default(),
            machine: MachineMemo::default(),
            asks,
        }
    }

    /// A read at wall second `now`: the footer's fingerprint and the deadline the read posted.
    fn at(&mut self, now: i64) -> (u64, Option<i64>) {
        let read_at = UNIX_EPOCH + Duration::from_secs(u64::try_from(now).expect("after 1970"));
        let (_, found) = resolve_facts_at(
            &self.timeline,
            &mut self.identity,
            &mut self.tail,
            &mut self.fold,
            &mut self.machine,
            self.asks,
            (
                &footer::ReadClock {
                    read_at,
                    now,
                    offset_at: &|_, _| Some(0),
                },
                Instant::now(),
            ),
        );
        let held = self.timeline.lock().unwrap();
        let facts = held.claude_footer();
        assert!(facts.is_some(), "+{}: a footer", now - NOW);
        (
            fingerprint(facts, None, true, footer::FactMask::default()),
            found.changes_after,
        )
    }
}

/// [`sweep`] of whole reads: from [`NOW`], each read's fingerprint holds at every ten minutes and
/// at the last second before the deadline it posted, then the read at that deadline starts the
/// next span. How many deadlines moved the fingerprint.
fn read_sweep(what: &str, reader: &mut Reader) -> usize {
    let end = NOW + SPAN;
    let (mut t, mut moved) = (NOW, 0);
    let (mut held, mut due) = reader.at(t);
    loop {
        assert!(
            due.is_none_or(|due| due > t),
            "{what}: a deadline after its read"
        );
        let next = due.map_or(end, |due| due.min(end));
        let samples: Vec<i64> = (t + 600..next).step_by(600).chain([next - 1]).collect();
        for s in samples.into_iter().filter(|s| *s > t) {
            assert_eq!(
                reader.at(s).0,
                held,
                "{what}: the footer moved at +{} before its deadline +{}, posted at +{}",
                s - NOW,
                next - NOW,
                t - NOW
            );
        }
        if next >= end {
            return moved;
        }
        t = next;
        let (now, posted) = reader.at(t);
        moved += usize::from(now != held);
        (held, due) = (now, posted);
    }
}

#[test]
fn time_facts_change_at_their_computed_second() {
    // A weekly figure, each second: read now, read stamped ahead of the skew, and one whose
    // window resets within the span.
    for (name, figure) in [
        ("read", (RESET, NOW - 100)),
        ("ahead", (RESET, NOW + 200)),
        ("resets", (NOW + 5000, NOW - 100)),
    ] {
        let figure = WeeklyFigure {
            used_x10: 370,
            resets_at: figure.0,
            at: figure.1,
        };
        let moved = sweep(
            name,
            (NOW, 1),
            |t| (figure.words(t), figure.current(t)),
            |t| figure.changes_after(t),
        );
        assert!(moved >= 2, "{name}: the figure's words move: {moved}");
    }

    // A wall, each second: a 7d one whose notice gave its reset as a span (its date drops within
    // a day of it) and a 5h one, as the transcript's fold holds them.
    for (window, span, at) in [
        ("seven_day", "in 1d 1h", NOW),
        ("five_hour", "in 3h", NOW - 600),
    ] {
        let rows = [(
            window.to_owned(),
            LimitNoticeRow {
                reset_text: Some(span.to_owned()),
                at,
            },
        )]
        .into_iter()
        .collect();
        let utc = |_: Option<&str>, _: i64| Some(0);
        let moved = sweep(
            window,
            (NOW, 1),
            |t| wall_of(&rows, None, t, &utc).map(|wall| (wall.label, wall.resets)),
            |t| wall_of(&rows, None, t, &utc).and_then(|wall| wall.changes_after(t)),
        );
        assert!(moved >= 1, "{window}: the wall's words move: {moved}");
    }

    // Whole reads of the weekly fixtures: the tap's own week, the profile's `/usage` cache, the
    // store's week, the store's wall and the transcript's.
    let pid = u32::try_from(PID).expect("a pid");
    fresh("footer-deadline-tap");
    profile(None);
    reading(pid, NOW - 100, (370, RESET), None, None);
    let moved = read_sweep("tap", &mut Reader::new(asks(tapped, armed)));
    assert!(moved >= 2, "tap: exact, bound, stale: {moved}");

    fresh("footer-deadline-profile");
    profile(Some((63, NOW - 100)));
    let moved = read_sweep("profile", &mut Reader::new(asks(untapped, armed)));
    assert!(moved >= 2, "profile: exact, bound, gone: {moved}");

    fresh("footer-deadline-store");
    profile(None);
    save_main(Some(NOW - 100), false);
    let moved = read_sweep("store", &mut Reader::new(asks(untapped, stored)));
    assert!(moved >= 2, "store: exact, bound, gone: {moved}");

    fresh("footer-deadline-store-wall");
    profile(None);
    save_main(None, true);
    let moved = read_sweep("store wall", &mut Reader::new(asks(untapped, stored)));
    assert!(moved >= 1, "store wall: it ends at its reset: {moved}");

    // The transcript's wall names a dated reset three days on: it holds the span.
    fresh("footer-deadline-transcript-wall");
    profile(None);
    weekly_wall();
    read_sweep("transcript wall", &mut Reader::new(asks(untapped, stored)));
    let _ = std::fs::remove_dir_all(super::root());

    // The outlook each minute, over the quota scenes and the same scenes with no series behind
    // them (no pace: only the readings' and the resets' seconds).
    let mut paced = 0;
    for (name, make) in [
        ("a", quota_scenes::case_a as fn() -> quota_scenes::Fx),
        ("b", quota_scenes::case_b),
        ("c", quota_scenes::case_c),
        ("d", quota_scenes::case_d),
        ("e", quota_scenes::case_e),
    ] {
        for unpaced in [false, true] {
            let mut fx = make();
            if unpaced {
                fx.series.clear();
            }
            let from = fx.now;
            let view = |fx: &mut quota_scenes::Fx, t: i64| {
                fx.now = t;
                let shown = fx.outlook().quota("main", None, true);
                (
                    shown.named,
                    shown.folded,
                    shown.names,
                    shown.tail,
                    shown.title,
                    shown.spoken,
                )
            };
            let what = format!("case {name}{}", if unpaced { " unpaced" } else { "" });
            let fx = std::cell::RefCell::new(fx);
            let moved = sweep(
                &what,
                (from, 60),
                |t| view(&mut fx.borrow_mut(), t),
                |t| {
                    let mut fx = fx.borrow_mut();
                    fx.now = t;
                    let inputs = fx.inputs();
                    outlook::outlook(&inputs).changes_after(&inputs)
                },
            );
            paced += usize::from(!unpaced) * moved;
        }
    }
    assert!(paced > 0, "the paced scenes' forecasts move: {paced}");
}
