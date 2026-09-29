// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the PRESSURE pass (decided 2026-09-28): below the automatic
//! floor, profiles inside the idle window give up their `incremental/`
//! least recently used first, free space measured again before each, until
//! the volume is back at the floor plus [`PRESSURE_MARGIN_GIB`].
//!
//! Every fixture is a build directory laid out as cargo lays one out
//! ([`lay_target`], [`lay_profile`]: locks in each profile, none at the
//! root), each profile's compile outputs dated by hand relative to the real
//! clock — EVERY profile recent (hours old, inside the one-day window), the
//! machine the measurement of 2026-09-27/28 found. The re-measure is a
//! script of free figures, so the stop is decided by the test, not by the
//! machine's own disk. Nothing here reads or writes anything of the
//! owner's.

use super::tests::{Tmp, lay_profile, lay_target, no_pressure, now_after_write};
use super::*;

const HOUR: i64 = 3_600;

/// Set the mtime of the directory or file at `path` to `at` (unix seconds).
fn set_mtime(path: &Path, at: i64) {
    let when = std::time::UNIX_EPOCH
        + std::time::Duration::from_secs(u64::try_from(at).expect("after the epoch"));
    std::fs::File::open(path)
        .and_then(|f| f.set_modified(when))
        .expect("set mtime");
}

/// Date `profile` LAST USED at `at`: what a compile writes, written then.
pub(crate) fn last_used(profile: &Path, at: i64) {
    for c in target::CLOCK_DIRS {
        let d = profile.join(c);
        if d.exists() {
            set_mtime(&d, at);
        }
    }
}

/// A config with `disk.apply` on, for the owner's verb.
fn applying() -> Config {
    Config {
        apply: true,
        ..Config::default()
    }
}

/// The bytes the blocks of the file at `p` take.
fn blocks(p: &Path) -> u64 {
    use std::os::unix::fs::MetadataExt as _;
    std::fs::symlink_metadata(p).expect("stat").blocks() * 512
}

/// Three recent profiles in two build directories, in an order where PATH
/// order and LEAST-RECENTLY-USED order disagree:
/// `a/target/debug` 3 h old, `a/target/release` 1 h, `b/target/debug` 5 h
/// (the oldest, and last by path), under `root`. Answers the CANONICAL
/// build directories and `now`.
pub(crate) fn three_recent(root: &Path) -> (PathBuf, PathBuf, i64) {
    let now = now_after_write(0);
    let a = lay_target(&root.join("a/target"));
    lay_profile(&a.join("release"));
    let b = lay_target(&root.join("b/target"));
    last_used(&a.join("debug"), now - 3 * HOUR);
    last_used(&a.join("release"), now - HOUR);
    last_used(&b.join("debug"), now - 5 * HOUR);
    let canon = |p: PathBuf| std::fs::canonicalize(p).expect("canonical");
    (canon(a), canon(b), now)
}

/// The report the tick would take on `targets` at `now` with `free` bytes
/// free on the scratch directory's volume (the default config).
fn surveyed(tmp: &Tmp, targets: &[PathBuf], now: i64, free: u64) -> (Survey, Report) {
    surveyed_with(tmp, targets, now, free, Config::default())
}

/// [`surveyed`] under `config`.
fn surveyed_with(
    tmp: &Tmp,
    targets: &[PathBuf],
    now: i64,
    free: u64,
    config: Config,
) -> (Survey, Report) {
    let roots = Roots {
        volume: Some(tmp.path().to_path_buf()),
        targets: targets.to_vec(),
        ..Roots::default()
    };
    let survey = scan(&roots, now, Trigger::Tick, config, Some(free));
    let rep = report(&survey, config);
    (survey, rep)
}

/// The real remover at `now`, on any device (a pressure row carries its own).
fn real(now: i64) -> impl FnMut(&Row) -> std::io::Result<Removed> {
    remover(Judge {
        now,
        threshold_days: DEFAULT_TARGET_STALE_DAYS,
        device: None,
    })
}

/// A re-measure that answers `script` in turn (its last figure after that),
/// counting how often it was read.
fn scripted(script: Vec<u64>, reads: &mut usize) -> impl FnMut() -> Option<u64> + '_ {
    move || {
        let got = script
            .get(*reads)
            .or(script.last())
            .copied()
            .expect("a script");
        *reads += 1;
        Some(got)
    }
}

/// The pressure rows of `rep`, as the profiles they name.
fn pressure_order(rep: &Report) -> Vec<PathBuf> {
    rep.rows
        .iter()
        .filter(|r| matches!(r.witness, Witness::LeastRecentlyUsed { .. }))
        .map(|r| r.path.clone())
        .collect()
}

/// BELOW THE FLOOR WITH EVERY PROFILE RECENT, THE OLDEST UNLOCKED PROFILE
/// GOES FIRST AND THE PASS STOPS ONCE FREE SPACE IS BACK. At the idle window
/// alone the tick took nothing here (every profile is hours old). Under
/// pressure the least recently used profile — `b/target/debug`, five hours
/// — gives up its `incremental/`; free space measured again is at the
/// floor plus the margin, so the pass stops there and the two more recent
/// profiles keep theirs. The journal says each step as it happened —
/// intent, outcome (the witness, the bytes the unlinks released), and the
/// stop (the figure it stopped on and how many it left) — and the bytes are
/// the query cache's blocks, counted, not the row's estimate.
#[test]
fn below_the_floor_with_every_profile_recent_the_oldest_goes_first_and_the_pass_stops() {
    let tmp = Tmp::new("pressure-stop");
    let (a, b, now) = three_recent(tmp.path());
    let config = Config::default();
    let target = config.pressure_target_bytes();
    assert_eq!(target, (DEFAULT_AUTO_FREE_GIB + PRESSURE_MARGIN_GIB) * GIB);
    let (survey, rep) = surveyed(&tmp, &[a.clone(), b.clone()], now, GIB);
    assert!(
        rep.rows
            .iter()
            .all(|r| matches!(r.witness, Witness::LeastRecentlyUsed { .. })),
        "every profile is recent, so no idle row: {rep:?}"
    );
    let oldest_cache = b.join("debug/incremental/lay-1x2y3z/s-abc-def/query-cache.bin");
    let expect = blocks(&oldest_cache);

    let mut reads = 0;
    let mut journal = Vec::new();
    let done = apply_auto(
        &rep,
        survey.transcripts_root.as_deref(),
        &mut real(now),
        &mut scripted(vec![GIB, target], &mut reads),
        &mut |step| {
            journal.push(step_row(now, "", step));
            true
        },
    )
    .expect("below the floor");

    assert_eq!(reads, 2, "measured before each pressure row it reached");
    assert_eq!(done.removed.len(), 1, "{done:?}");
    assert_eq!(done.removed[0].0.path, b.join("debug"), "{done:?}");
    assert_eq!(done.removed[0].1.units, vec![b.join("debug/incremental")]);
    assert_eq!(done.freed_bytes, expect, "the blocks the unlinks released");
    assert_eq!(
        done.stopped,
        Some(Stop {
            by: StopBy::Measured,
            free: Some(target),
            target,
            short: target - GIB,
            released: expect,
            left: 2
        })
    );
    assert!(!b.join("debug/incremental").exists());
    assert!(
        b.join("debug/deps/liblay.rlib").exists(),
        "only incremental/"
    );
    assert!(a.join("debug/incremental").exists(), "more recent: kept");
    assert!(a.join("release/incremental").exists(), "more recent: kept");

    let kinds: Vec<&str> = journal
        .iter()
        .map(|l| {
            ["removing", "removed", "stopped", "denial"]
                .into_iter()
                .find(|k| l.contains(&format!("\"kind\":\"{k}\"")))
                .unwrap_or("?")
        })
        .collect();
    assert_eq!(kinds, ["removing", "removed", "stopped"], "{journal:?}");
    assert!(
        journal[1].contains("\"witness\":\"least-recently-used\""),
        "{}",
        journal[1]
    );
    assert!(
        journal[1].contains(&format!("\"freed_bytes\":{expect}")),
        "{}",
        journal[1]
    );
    assert!(
        journal[2].contains(&format!("\"target_free_bytes\":{target}"))
            && journal[2].contains("\"left\":2"),
        "{}",
        journal[2]
    );
}

/// THE ORDER IS LEAST RECENTLY USED, NOT PATH ORDER. With free space never
/// back, every recent profile goes, and the intents are journalled oldest
/// first: `b/target/debug` (5 h), `a/target/debug` (3 h), then
/// `a/target/release` (1 h) — where path order would start in `a/`. The
/// report lists them in that order too, the order the verb prints.
#[test]
fn the_pressure_pass_takes_profiles_least_recently_used_first() {
    let tmp = Tmp::new("pressure-order");
    let (a, b, now) = three_recent(tmp.path());
    let (survey, rep) = surveyed(&tmp, &[a.clone(), b.clone()], now, GIB);
    let lru = vec![b.join("debug"), a.join("debug"), a.join("release")];
    assert_eq!(pressure_order(&rep), lru, "{rep:?}");

    let mut intents = Vec::new();
    let mut reads = 0;
    let done = apply_auto(
        &rep,
        survey.transcripts_root.as_deref(),
        &mut real(now),
        &mut scripted(vec![GIB], &mut reads),
        &mut |step| {
            if let Step::Removing(row) = step {
                intents.push(row.path.clone());
            }
            true
        },
    )
    .expect("below the floor");
    assert_eq!(intents, lru);
    assert_eq!(reads, 3);
    assert_eq!(done.removed.len(), 3, "{done:?}");
    assert_eq!(done.stopped, None, "never back above the floor");
    for p in &lru {
        assert!(!p.join(INCREMENTAL).exists(), "{}", p.display());
    }
}

/// A LOCKED PROFILE IS NEVER TOUCHED, EVEN WHEN IT IS THE OLDEST. A build
/// holding any one of `b/target/debug`'s locks at the survey keeps it off
/// the report (a note says why), and the pass starts at the next oldest. A
/// build that takes the lock AFTER the survey is met by the reclaim's own
/// lock: the row is refused, its cache byte for byte intact, and the pass
/// goes on to the next.
#[test]
fn a_locked_profile_is_never_taken_under_pressure_even_when_it_is_the_oldest() {
    for lock in CARGO_LOCKS {
        // Held at the survey.
        let tmp = Tmp::new("pressure-held");
        let (a, b, now) = three_recent(tmp.path());
        let hold = target::Build::holds(&b.join("debug").join(lock));
        let (survey, rep) = surveyed(&tmp, &[a.clone(), b.clone()], now, GIB);
        assert_eq!(
            pressure_order(&rep),
            vec![a.join("debug"), a.join("release")],
            "{lock}: {rep:?}"
        );
        assert!(
            rep.notes
                .iter()
                .any(|n| n.contains("kept") && n.contains("a build holds") && n.contains(lock)),
            "{lock}: {:?}",
            rep.notes
        );
        let mut reads = 0;
        let done = apply_auto(
            &rep,
            survey.transcripts_root.as_deref(),
            &mut real(now),
            &mut scripted(vec![GIB, 20 * GIB], &mut reads),
            &mut |_| true,
        )
        .expect("below the floor");
        assert_eq!(done.removed.len(), 1, "{lock}: {done:?}");
        assert_eq!(done.removed[0].0.path, a.join("debug"), "{lock}");
        assert!(b.join("debug").join(INCREMENTAL).exists(), "{lock}: held");
        drop(hold);

        // Taken after the survey, before the reclaim.
        let tmp = Tmp::new("pressure-held-late");
        let (a, b, now) = three_recent(tmp.path());
        let (survey, rep) = surveyed(&tmp, &[a.clone(), b.clone()], now, GIB);
        assert_eq!(pressure_order(&rep)[0], b.join("debug"), "{lock}");
        let hold = target::Build::holds(&b.join("debug").join(lock));
        let cache = b.join("debug/incremental/lay-1x2y3z/s-abc-def/query-cache.bin");
        let before = std::fs::read(&cache).expect("cache");
        let mut reads = 0;
        let done = apply_auto(
            &rep,
            survey.transcripts_root.as_deref(),
            &mut real(now),
            &mut scripted(vec![GIB, GIB, 20 * GIB], &mut reads),
            &mut |_| true,
        )
        .expect("below the floor");
        assert!(
            done.denials
                .iter()
                .any(|d| d.code() == "remove-failed" && d.describe().contains("a build holds")),
            "{lock}: {done:?}"
        );
        assert_eq!(done.removed.len(), 1, "{lock}: {done:?}");
        assert_eq!(done.removed[0].0.path, a.join("debug"), "{lock}");
        assert_eq!(std::fs::read(&cache).expect("cache"), before, "{lock}");
        drop(hold);
    }
}

/// AT OR ABOVE THE FLOOR NOTHING HAPPENS: no pressure row is reported (a
/// note says a recent profile would not be taken), nothing is planned, the
/// re-measure is never read and every cache stays — and the owner's verb,
/// with `disk.apply` on, takes nothing either, since no profile is idle.
#[test]
fn above_the_floor_no_recent_profile_is_taken() {
    let tmp = Tmp::new("pressure-above");
    let (a, b, now) = three_recent(tmp.path());
    for free in [DEFAULT_AUTO_FREE_GIB * GIB, 500 * GIB] {
        let (survey, rep) = surveyed(&tmp, &[a.clone(), b.clone()], now, free);
        assert!(pressure_order(&rep).is_empty(), "{rep:?}");
        assert!(
            rep.notes
                .iter()
                .any(|n| n.contains("not under the 10 GiB floor")
                    && n.contains("no profile written into within 1d would be taken")),
            "{:?}",
            rep.notes
        );
        assert_eq!(auto_plan(&rep), None);
        let done = apply_auto(
            &rep,
            survey.transcripts_root.as_deref(),
            &mut |row| panic!("removed {} above the floor", row.path.display()),
            &mut no_pressure,
            &mut |_| true,
        );
        assert_eq!(done, None);
        let rep = report(&survey, applying());
        let done = apply(
            &rep,
            None,
            Some(Class::CargoTargets),
            &mut |row| panic!("removed {} above the floor", row.path.display()),
            &mut no_pressure,
            &mut |_| true,
        );
        assert!(
            done.removed.is_empty() && done.denials.is_empty(),
            "{done:?}"
        );
    }
    for p in [a.join("debug"), a.join("release"), b.join("debug")] {
        assert!(p.join(INCREMENTAL).exists(), "{}", p.display());
    }
}

/// A PROFILE BUILT WITHIN THE LAST MINUTES IS KEPT EVEN UNDER PRESSURE: a
/// build that just finished is usually followed by another, which would
/// write the cache straight back. Five minutes old, `b/target/debug` is no
/// row (a note says why) however low free space is; at exactly
/// [`PRESSURE_MIN_IDLE_S`] it is one again.
#[test]
fn a_profile_built_within_the_last_minutes_is_kept_under_pressure() {
    let tmp = Tmp::new("pressure-recent");
    let (a, b, now) = three_recent(tmp.path());
    last_used(&b.join("debug"), now - 5 * 60);
    let (_, rep) = surveyed(&tmp, &[a.clone(), b.clone()], now, GIB);
    assert_eq!(
        pressure_order(&rep),
        vec![a.join("debug"), a.join("release")],
        "{rep:?}"
    );
    assert!(
        rep.notes
            .iter()
            .any(|n| n.contains("written into 5 min ago, inside the 10 min")),
        "{:?}",
        rep.notes
    );
    last_used(&b.join("debug"), now - PRESSURE_MIN_IDLE_S);
    let (_, rep) = surveyed(&tmp, &[a.clone(), b.clone()], now, GIB);
    assert_eq!(
        pressure_order(&rep),
        vec![a.join("debug"), a.join("release"), b.join("debug")],
        "{rep:?}"
    );
}

/// IDLE PROFILES STILL GO FIRST, AND WHOLE. `target_stale_days` keeps its
/// meaning: a profile past it is idle and its build directory's row takes
/// it at once, before any recent profile and without a re-measure; only
/// then does the pressure pass measure and stop. Here the idle profile
/// alone is enough: the stop comes before any recent one.
#[test]
fn idle_profiles_go_first_and_whole_before_any_recent_one() {
    let tmp = Tmp::new("pressure-idle-first");
    let (a, b, now) = three_recent(tmp.path());
    last_used(&a.join("release"), now - 3 * 86_400);
    let (survey, rep) = surveyed(&tmp, &[a.clone(), b.clone()], now, GIB);
    let kinds: Vec<&str> = rep
        .rows_of(Class::CargoTargets)
        .iter()
        .map(|r| r.witness.kind())
        .collect();
    assert_eq!(
        kinds,
        [
            "stale-build-dir",
            "least-recently-used",
            "least-recently-used"
        ],
        "{rep:?}"
    );
    let target = Config::default().pressure_target_bytes();
    let mut reads = 0;
    let done = apply_auto(
        &rep,
        survey.transcripts_root.as_deref(),
        &mut real(now),
        &mut scripted(vec![target], &mut reads),
        &mut |_| true,
    )
    .expect("below the floor");
    assert_eq!(reads, 1, "the idle row needs no measurement");
    assert_eq!(done.removed.len(), 1, "{done:?}");
    assert_eq!(done.removed[0].1.units, vec![a.join("release/incremental")]);
    assert_eq!(done.stopped.as_ref().map(|s| s.left), Some(2));
    assert!(b.join("debug/incremental").exists());
    assert!(a.join("debug/incremental").exists());
}

/// A RE-MEASURE THAT FAILS STOPS THE PASS: nothing is removed on a free
/// figure nobody read, and the stop row says so.
#[test]
fn a_failed_re_measure_stops_the_pressure_pass() {
    let tmp = Tmp::new("pressure-unmeasured");
    let (a, b, now) = three_recent(tmp.path());
    let (survey, rep) = surveyed(&tmp, &[a.clone(), b.clone()], now, GIB);
    let mut journal = Vec::new();
    let done = apply_auto(
        &rep,
        survey.transcripts_root.as_deref(),
        &mut |row| panic!("removed {} on no figure", row.path.display()),
        &mut || None,
        &mut |step| {
            journal.push(step_row(now, "", step));
            true
        },
    )
    .expect("below the floor");
    assert!(done.removed.is_empty(), "{done:?}");
    assert_eq!(
        done.stopped.as_ref().map(|s| (s.free, s.left)),
        Some((None, 3))
    );
    assert_eq!(journal.len(), 1, "{journal:?}");
    assert!(
        journal[0].contains("\"kind\":\"stopped\"")
            && journal[0].contains("\"free_bytes\":null")
            && journal[0].contains("could not be measured again"),
        "{}",
        journal[0]
    );
}

/// ONLY THE MEASURED VOLUME: a build directory whose device is not the
/// measured volume's gets no pressure row (reclaiming it frees nothing
/// there), and a volume whose device could not be read gets none at all.
#[test]
fn a_build_directory_on_another_volume_gets_no_pressure_row() {
    let tmp = Tmp::new("pressure-volume");
    let (a, b, now) = three_recent(tmp.path());
    let (mut survey, _) = surveyed(&tmp, &[a.clone(), b.clone()], now, GIB);
    survey.volume_device = survey.volume_device.map(|d| d.wrapping_add(1));
    let rep = report(&survey, Config::default());
    assert!(pressure_order(&rep).is_empty(), "{rep:?}");
    assert!(
        rep.notes.iter().any(|n| n.contains("another volume")),
        "{:?}",
        rep.notes
    );
    survey.volume_device = None;
    let rep = report(&survey, Config::default());
    assert!(pressure_order(&rep).is_empty(), "{rep:?}");
}

/// A RE-MEASURE THAT NEVER RISES STOPS THE PASS ONCE THE COUNTED BYTES
/// COVER WHAT WAS SHORT. A local snapshot (or a clone, or a deferred free)
/// can keep an unlink out of `statvfs` for a while; if only the figure could
/// stop the pass, it would take every profile it has while the volume
/// stayed under the floor. Here free space reads short of the target by
/// exactly the oldest profile's cache and never moves: that profile goes,
/// and the pass stops at the next row on the bytes its unlink released —
/// the stop row says which stop fired, what was short and what was
/// released — and the two more recent profiles keep theirs.
#[test]
fn a_re_measure_that_never_rises_stops_once_the_counted_bytes_cover_the_shortfall() {
    let tmp = Tmp::new("pressure-counted");
    let (a, b, now) = three_recent(tmp.path());
    let (survey, rep) = surveyed(&tmp, &[a.clone(), b.clone()], now, GIB);
    let first = rep
        .rows
        .iter()
        .find(|r| r.path == b.join("debug"))
        .expect("the oldest profile's row")
        .bytes;
    assert!(first > 0, "{rep:?}");
    let target = Config::default().pressure_target_bytes();
    let stuck = target - first;

    let mut reads = 0;
    let mut journal = Vec::new();
    let done = apply_auto(
        &rep,
        survey.transcripts_root.as_deref(),
        &mut real(now),
        &mut scripted(vec![stuck], &mut reads),
        &mut |step| {
            journal.push(step_row(now, "", step));
            true
        },
    )
    .expect("below the floor");

    assert_eq!(reads, 2, "{done:?}");
    assert_eq!(done.removed.len(), 1, "{done:?}");
    assert_eq!(done.removed[0].0.path, b.join("debug"));
    assert_eq!(done.freed_bytes, first, "the blocks the unlinks released");
    assert_eq!(
        done.stopped,
        Some(Stop {
            by: StopBy::Counted,
            free: Some(stuck),
            target,
            short: first,
            released: first,
            left: 2,
        })
    );
    assert!(a.join("debug/incremental").exists(), "more recent: kept");
    assert!(a.join("release/incremental").exists(), "more recent: kept");
    let stop = journal.last().expect("a stop row");
    for said in [
        "\"kind\":\"stopped\"".to_owned(),
        "\"by\":\"counted\"".to_owned(),
        format!("\"short_bytes\":{first}"),
        format!("\"released_bytes\":{first}"),
        "\"left\":2".to_owned(),
    ] {
        assert!(stop.contains(&said), "{said}: {stop}");
    }
}

/// WITH `target_stale_days = 0`, A PROFILE BUILT MINUTES AGO IS STILL NEVER
/// TAKEN. A zero window makes every profile idle, and idle rows are taken
/// whole with no re-measure — so the ten-minute rule has to hold in the idle
/// pass too, or the docs' "never one built in the last 10 min" would be
/// false for that setting. `b/target/debug`, built a minute ago, is no row
/// (a note says why), the tick takes the older profiles in `a/` and not it,
/// and the reclaim itself, which judges each profile again, takes nothing.
#[test]
fn with_a_zero_day_window_a_profile_built_minutes_ago_is_still_never_taken() {
    let tmp = Tmp::new("pressure-zero-window");
    let (a, b, now) = three_recent(tmp.path());
    last_used(&b.join("debug"), now - 60);
    let config = Config {
        target_stale_days: 0,
        ..Config::default()
    };
    let (survey, rep) = surveyed_with(&tmp, &[a.clone(), b.clone()], now, GIB, config);
    assert!(
        rep.rows
            .iter()
            .all(|r| r.path != b && r.path != b.join("debug")),
        "{rep:?}"
    );
    assert!(
        rep.notes
            .iter()
            .any(|n| n.contains(&b.display().to_string()) && n.contains("within the last 10 min")),
        "{:?}",
        rep.notes
    );
    let done = apply_auto(
        &rep,
        survey.transcripts_root.as_deref(),
        &mut remover(Judge {
            now,
            threshold_days: 0,
            device: None,
        }),
        &mut no_pressure,
        &mut |_| true,
    )
    .expect("below the floor");
    assert!(done.denials.is_empty(), "{done:?}");
    assert!(
        !a.join("debug/incremental").exists(),
        "idle at 0 days: taken"
    );
    assert!(
        !a.join("release/incremental").exists(),
        "idle at 0 days: taken"
    );
    assert!(b.join("debug/incremental").exists(), "a minute old: kept");

    let refused = target::reclaim(
        &b,
        &Judge {
            now,
            threshold_days: 0,
            device: None,
        },
    )
    .expect_err("a minute old, under its locks too");
    assert!(
        refused.to_string().contains("nothing reclaimed"),
        "{refused}"
    );
    assert!(b.join("debug/incremental").exists());
}

/// A SURVEY THAT DID NOT PROBE THE RECENT PROFILES MAKES NO PRESSURE ROW.
/// Surveyed as if above the floor, only idle profiles are probed for a
/// build's locks and sized; reported below it, a recent profile's row would
/// say "no build holding its locks" of a profile nobody probed — here one a
/// build holds — and 0 bytes. So the directory gets a note instead.
#[test]
fn a_survey_that_did_not_probe_the_recent_profiles_makes_no_pressure_row() {
    let tmp = Tmp::new("pressure-unprobed");
    let (a, b, now) = three_recent(tmp.path());
    let hold = target::Build::holds(&b.join("debug/.cargo-lock"));
    let (mut survey, _) = surveyed(&tmp, &[a.clone(), b.clone()], now, 500 * GIB);
    survey.free_bytes = Some(GIB);
    let rep = report(&survey, Config::default());
    assert!(pressure_order(&rep).is_empty(), "{rep:?}");
    assert_eq!(rep.reclaimable_under_pressure(), 0);
    for dir in [&a, &b] {
        assert!(
            rep.notes
                .iter()
                .any(|n| n.contains(&dir.display().to_string()) && n.contains("neither probed")),
            "{}: {:?}",
            dir.display(),
            rep.notes
        );
    }
    drop(hold);
}

/// THE HEADLINE KEEPS THE PRESSURE ROWS APART, AS AN UPPER BOUND. The pass
/// stops at the floor plus the margin, so adding every pressure row into
/// `reclaimable=` would promise far more than it takes. `reclaimable=` is
/// what goes whole (the idle row); `under_pressure_up_to=` is every
/// pressure row's bytes — in the headline, the JSON and the ledger's report
/// row alike.
#[test]
fn the_headline_keeps_the_pressure_rows_apart_as_an_upper_bound() {
    let tmp = Tmp::new("pressure-headline");
    let (a, b, now) = three_recent(tmp.path());
    last_used(&a.join("release"), now - 3 * 86_400);
    let (_, rep) = surveyed(&tmp, &[a.clone(), b.clone()], now, GIB);
    let sum = |kind: &str| {
        rep.rows
            .iter()
            .filter(|r| r.witness.kind() == kind)
            .map(|r| r.bytes)
            .sum::<u64>()
    };
    let (whole, pressure) = (sum("stale-build-dir"), sum("least-recently-used"));
    assert!(whole > 0 && pressure > 0, "{rep:?}");
    assert_eq!(rep.reclaimable_total(), whole);
    assert_eq!(rep.reclaimable_under_pressure(), pressure);
    let headline = rep.headline();
    assert!(
        headline.contains(&format!(
            "reclaimable={} under_pressure_up_to={}",
            human_bytes(whole),
            human_bytes(pressure)
        )),
        "{headline}"
    );
    for line in [rep.to_json(), report_row(now, "", &rep)] {
        assert!(
            line.contains(&format!("\"reclaimable_bytes\":{whole}"))
                && line.contains(&format!(
                    "\"reclaimable_under_pressure_up_to_bytes\":{pressure}"
                )),
            "{line}"
        );
    }
}

/// A BUILD DIRECTORY INSIDE ANOTHER GIVES EACH PROFILE ONE PRESSURE ROW.
/// Both are named, and the outer walk finds the inner one's profile too:
/// that profile is listed once (as the inner directory's), counted once,
/// and taken once — no second attempt that fails as a denial.
#[test]
fn a_build_directory_inside_another_gives_each_profile_one_pressure_row() {
    let tmp = Tmp::new("pressure-nested");
    let now = now_after_write(0);
    let outer = lay_target(&tmp.path().join("o/target"));
    let inner = lay_target(&outer.join("nested/target"));
    last_used(&outer.join("debug"), now - 2 * HOUR);
    last_used(&inner.join("debug"), now - 4 * HOUR);
    let canon = |p: &Path| std::fs::canonicalize(p).expect("canonical");
    let (outer, inner) = (canon(&outer), canon(&inner));
    let (survey, rep) = surveyed(&tmp, &[outer.clone(), inner.clone()], now, GIB);
    assert_eq!(
        pressure_order(&rep),
        vec![inner.join("debug"), outer.join("debug")],
        "{rep:?}"
    );
    let inner_row = rep
        .rows
        .iter()
        .find(|r| r.path == inner.join("debug"))
        .expect("the inner profile's row");
    assert!(
        matches!(&inner_row.witness, Witness::LeastRecentlyUsed { build_dir, .. } if *build_dir == inner),
        "{inner_row:?}"
    );
    let bytes: u64 = rep
        .rows
        .iter()
        .filter(|r| matches!(r.witness, Witness::LeastRecentlyUsed { .. }))
        .map(|r| r.bytes)
        .sum();
    assert_eq!(rep.reclaimable_under_pressure(), bytes);

    let mut reads = 0;
    let done = apply_auto(
        &rep,
        survey.transcripts_root.as_deref(),
        &mut real(now),
        &mut scripted(vec![GIB], &mut reads),
        &mut |_| true,
    )
    .expect("below the floor");
    assert!(done.denials.is_empty(), "{done:?}");
    assert_eq!(done.removed.len(), 2, "{done:?}");
    assert!(!inner.join("debug/incremental").exists());
    assert!(!outer.join("debug/incremental").exists());
}

/// ONE VERDICT PER PROFILE, THE INNERMOST BUILD DIRECTORY'S. A build
/// directory named inside another is surveyed twice, one survey after the
/// other, and each probes the nested profile's locks: a build that holds one
/// at only one of the two surveys is read free by one and held by the other.
/// Only the inner directory's survey decides that profile.
///
/// * Held at the inner survey (free at the outer): no row lists the profile
///   and a note keeps it — never, as before 2026-09-28, a removable row
///   naming the OUTER directory beside a note keeping the profile because "a
///   build holds" it.
/// * The mirror, held at the outer survey (free at the inner): the inner
///   directory's row, and no note keeping it.
///
/// No clock decides which survey sees the build: the two surveys run one
/// after the other, and the lock is taken and released around exactly one.
///
/// NEGATIVE CONTROL: with no build at either survey the nested profile has
/// its row, once, as the inner directory's, and no note keeps anything.
#[test]
fn the_innermost_build_directory_decides_a_nested_profile_between_two_surveys() {
    /// Which survey a build holds the nested profile's `.cargo-lock` at.
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Build {
        Neither,
        AtOuter,
        AtInner,
    }
    let now = now_after_write(0);
    // `o/target` holding `o/target/nested/target`, each with a recent
    // profile, surveyed as the tick does below the floor: the outer build
    // directory, then the inner one.
    let surveys = |name: &str, build: Build| {
        let tmp = Tmp::new(name);
        let outer = lay_target(&tmp.path().join("o/target"));
        let inner = lay_target(&outer.join("nested/target"));
        last_used(&outer.join("debug"), now - 2 * HOUR);
        last_used(&inner.join("debug"), now - 4 * HOUR);
        let canon = |p: &Path| std::fs::canonicalize(p).expect("canonical");
        let (outer, inner) = (canon(&outer), canon(&inner));
        // Released by `LOCK_UN` when dropped ([`target::Build`]), a panic's
        // unwind included, so no child a sibling test is spawning keeps it
        // for the next survey.
        let holding = |at: Build| {
            (build == at).then(|| target::Build::holds(&inner.join("debug/.cargo-lock")))
        };
        let lock = holding(Build::AtOuter);
        let (mut survey, _) = surveyed(&tmp, std::slice::from_ref(&outer), now, GIB);
        drop(lock);
        let lock = holding(Build::AtInner);
        let (second, _) = surveyed(&tmp, std::slice::from_ref(&inner), now, GIB);
        drop(lock);
        survey.targets.extend(second.targets);
        assert_eq!(survey.targets.len(), 2);
        let rep = report(&survey, Config::default());
        (tmp, outer, inner, rep)
    };
    // What the notes keeping a profile say, from the profile each names on.
    let kept = |rep: &Report| -> Vec<String> {
        rep.notes
            .iter()
            .filter_map(|n| n.strip_prefix("cargo-targets under pressure: kept "))
            .map(str::to_owned)
            .collect()
    };
    // No removable row lists a profile a note keeps.
    let consistent = |rep: &Report| {
        let kept = kept(rep);
        for r in rep.rows.iter().filter(|r| r.removable) {
            let named = format!("{}: ", r.path.display());
            assert!(
                !kept.iter().any(|k| k.starts_with(&named)),
                "{} is both removable and kept: {rep:?}",
                r.path.display()
            );
        }
    };
    // The build directory of each pressure row listing `profile`.
    let row_dirs = |rep: &Report, profile: &Path| -> Vec<PathBuf> {
        rep.rows
            .iter()
            .filter(|r| r.path == profile)
            .filter_map(|r| match &r.witness {
                Witness::LeastRecentlyUsed { build_dir, .. } => Some(build_dir.clone()),
                _ => None,
            })
            .collect()
    };

    // NEGATIVE CONTROL: no build at either survey.
    let (_tmp, outer, inner, rep) = surveys("pressure-verdict-none", Build::Neither);
    assert_eq!(
        pressure_order(&rep),
        vec![inner.join("debug"), outer.join("debug")],
        "{rep:?}"
    );
    assert_eq!(row_dirs(&rep, &inner.join("debug")), vec![inner.clone()]);
    assert!(kept(&rep).is_empty(), "{:?}", rep.notes);
    consistent(&rep);

    // Held at the inner survey: the inner verdict keeps it, and no row
    // lists it.
    let (_tmp, outer, inner, rep) = surveys("pressure-verdict-inner", Build::AtInner);
    assert_eq!(pressure_order(&rep), vec![outer.join("debug")], "{rep:?}");
    assert_eq!(
        kept(&rep),
        vec![format!(
            "{}: a build holds {}",
            inner.join("debug").display(),
            inner.join("debug/.cargo-lock").display()
        )],
        "{:?}",
        rep.notes
    );
    consistent(&rep);

    // Held at the outer survey: the inner verdict gives its row, and no note
    // keeps it.
    let (_tmp, outer, inner, rep) = surveys("pressure-verdict-outer", Build::AtOuter);
    assert_eq!(
        pressure_order(&rep),
        vec![inner.join("debug"), outer.join("debug")],
        "{rep:?}"
    );
    assert_eq!(row_dirs(&rep, &inner.join("debug")), vec![inner.clone()]);
    assert!(kept(&rep).is_empty(), "{:?}", rep.notes);
    consistent(&rep);
}
