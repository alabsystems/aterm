// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::cell::Cell;

use super::*;

fn current_fixture(
    store: &Store,
    source: &dyn Source,
    now: i64,
    low: f64,
    force: bool,
) -> Result<(Reading, Got), String> {
    let token =
        provider_read::Use::fixture(provider_read::Read::AgyUsage, Path::new(&store.key), None);
    super::current(store, source, now, low, force, &mut Asking::new(token))
}

#[cfg(unix)]
fn read(source: &Agy) -> Result<String, String> {
    let home = source
        .env
        .iter()
        .rev()
        .find(|(name, _)| name == super::super::upgrade_agy::HOME_ENV)
        .map(|(_, value)| value.as_str());
    let key = launch_key(home, &source.flags);
    let token = provider_read::Use::fixture(provider_read::Read::AgyUsage, Path::new(&key), None);
    let permit = token
        .begin(
            provider_read::Account::in_home(provider_read::Read::AgyUsage, Path::new(&key), None),
            NOW,
            None,
        )
        .unwrap();
    source.read(&permit, source.admit(NOW)?)
}

/// Why the gate refuses a read of `source` now, if it does.
#[cfg(unix)]
fn refused(source: &Agy) -> Option<String> {
    source.admit(NOW).err()
}

fn tmp(tag: &str) -> aterm_tempfile::TempDir {
    aterm_tempfile::Builder::new()
        .prefix(&format!("aterm-agy-quota-{tag}-"))
        .tempdir()
        .expect("temp dir")
}

fn doc(gemini_week: f64, third_week: f64, third_5h: f64) -> String {
    format!(
        r#"{{"status":"SUCCESS","command":{{"name":"usage","data":{{"groups":[{{"name":"Gemini Models","description":"Models within this group: Gemini Flash, Gemini Pro","buckets":[{{"id":"gemini-weekly","window":"weekly","remaining_fraction":{gemini_week},"reset_time":"2026-10-28T00:19:03Z"}},{{"id":"gemini-5h","window":"5h","remaining_fraction":1}}]}},{{"name":"Claude and GPT models","description":"Models within this group: Claude Opus, Claude Sonnet, GPT-OSS","buckets":[{{"id":"3p-weekly","window":"weekly","remaining_fraction":{third_week},"reset_time":"2026-10-31T00:49:13Z"}},{{"id":"3p-5h","window":"5h","remaining_fraction":{third_5h},"reset_time":"2026-10-24T05:49:13Z"}}]}}]}}}}}}"#
    )
}

struct Recorded {
    doc: Result<String, String>,
    reads: Cell<u32>,
    default: Option<&'static str>,
    /// How often its default model was asked.
    defaults: Cell<u32>,
    /// Why its gate refuses, where it does.
    out: Option<String>,
}

impl Recorded {
    fn new(doc: Result<String, String>) -> Self {
        Self {
            doc,
            reads: Cell::new(0),
            default: None,
            defaults: Cell::new(0),
            out: None,
        }
    }
}

impl Source for Recorded {
    fn admit(&self, now: i64) -> Result<Pass, String> {
        self.out.clone().map_or_else(|| Ok(Pass::fixture(now)), Err)
    }

    fn read(&self, _: &provider_read::Permit, _: Pass) -> Result<String, String> {
        self.reads.set(self.reads.get() + 1);
        self.doc.clone()
    }

    fn default_model(&self, _: &provider_read::Permit, _: Pass) -> Result<String, String> {
        self.defaults.set(self.defaults.get() + 1);
        self.default
            .map(str::to_owned)
            .ok_or_else(|| "no default".to_owned())
    }
}

/// A login not known signed in has no reading at all: not the fresh one kept of it, not a stale
/// one, and nothing is read or backed off.
#[test]
fn a_login_the_gate_refuses_has_no_reading_kept_or_read() {
    let dir = tmp("gate-first");
    let store = Store::new(dir.path(), "home");
    let mut agy = Recorded::new(Ok(doc(0.8, 0.9, 0.9)));
    current_fixture(&store, &agy, NOW, 0.1, false).expect("a reading is kept");
    // CONTROL: admitted, the kept reading answers.
    assert_eq!(
        current_fixture(&store, &agy, NOW + 1, 0.1, false).map(|(_, got)| got),
        Ok(Got::Kept)
    );
    let why = format!("{}: not known", super::super::agy_gate::NOT_KNOWN);
    agy.out = Some(why.clone());
    for (at, force) in [
        (NOW + 2, false),
        (NOW + FRESH_OPEN_S, false),
        (NOW + 3, true),
    ] {
        assert_eq!(
            current_fixture(&store, &agy, at, 0.1, force).map(|_| ()),
            Err(why.clone())
        );
    }
    assert_eq!(agy.reads.get(), 1, "the refused source was never read");
    assert!(
        store.recent_failure(NOW + 3).is_none(),
        "nothing ran to back off"
    );
    let rotation = agy_quota::Rotation::default();
    assert_eq!(
        rotate_launch_at(None, &store, &agy, &rotation, NOW + 4, false),
        None
    );
    assert_eq!(agy.defaults.get(), 0, "nor asked its default model");
}

/// A `/model` read that failed is not asked again at the next launch, while its backoff lasts.
#[test]
fn a_failed_default_model_read_is_backed_off() {
    let dir = tmp("model-backoff");
    let store = Store::new(dir.path(), "home");
    let agy = Recorded::new(Ok(doc(0.8, 0.9, 0.9)));
    let rotation = agy_quota::Rotation::default();
    for at in [NOW, NOW + 1, NOW + FAIL_BACKOFF_S - 1] {
        assert_eq!(
            rotate_launch_at(None, &store, &agy, &rotation, at, false),
            None
        );
    }
    assert_eq!(agy.defaults.get(), 1, "one ask, then the backoff");
    assert!(
        store.recent_failure(NOW + 1).is_none(),
        "the usage read's own backoff is untouched"
    );
    // CONTROL: once the backoff has run out, the next launch asks again.
    assert_eq!(
        rotate_launch_at(None, &store, &agy, &rotation, NOW + FAIL_BACKOFF_S, false),
        None
    );
    assert_eq!(agy.defaults.get(), 2);
}

const NOW: i64 = 1_792_803_313;

#[test]
fn another_home_token_is_refused_even_when_the_requested_reading_is_cached() {
    let tmp = tmp("token-home");
    let store = Store::new(tmp.path(), "right-home");
    let source = Recorded::new(Ok(doc(1.0, 1.0, 1.0)));
    current_fixture(&store, &source, NOW, 0.1, false).unwrap();
    let token =
        provider_read::Use::fixture(provider_read::Read::AgyUsage, Path::new("wrong-home"), None);
    let (result, probes) = provider_read::captured(|| {
        super::current(&store, &source, NOW, 0.1, false, &mut Asking::new(token))
    });
    assert!(result.is_err());
    assert!(probes.is_empty());
    assert_eq!(
        source.reads.get(),
        1,
        "a refused token makes no second source read"
    );
}

#[test]
fn a_fresh_reading_is_kept_and_a_stale_one_read_again() {
    let dir = tmp("fresh");
    let store = Store::new(dir.path(), "home");
    let agy = Recorded::new(Ok(doc(0.8, 0.9, 0.9)));
    let (r, got) = current_fixture(&store, &agy, NOW, 0.1, false).unwrap();
    assert_eq!((got, agy.reads.get(), r.as_of), (Got::Read, 1, NOW));
    let (_, got) = current_fixture(&store, &agy, NOW + FRESH_OPEN_S - 1, 0.1, false).unwrap();
    assert_eq!(
        (got, agy.reads.get()),
        (Got::Kept, 1),
        "every pool open: 15 minutes current"
    );
    let (_, got) = current_fixture(&store, &agy, NOW + FRESH_OPEN_S, 0.1, false).unwrap();
    assert_eq!((got, agy.reads.get()), (Got::Read, 2));
    let (_, got) = current_fixture(&store, &agy, NOW + FRESH_OPEN_S, 0.1, true).unwrap();
    assert_eq!(
        (got, agy.reads.get()),
        (Got::Read, 3),
        "force always asks agy"
    );
    assert_eq!(store.history().len(), 3, "every read is a history line");
}

#[test]
fn a_tight_pool_is_read_more_often_and_a_passed_refill_at_once() {
    let dir = tmp("tight");
    let store = Store::new(dir.path(), "home");
    let agy = Recorded::new(Ok(doc(0.0, 0.9, 0.9)));
    current_fixture(&store, &agy, NOW, 0.1, false).unwrap();
    let (_, got) = current_fixture(&store, &agy, NOW + FRESH_TIGHT_S, 0.1, false).unwrap();
    assert_eq!(got, Got::Read, "a spent pool: 5 minutes current");
    let reset = aterm_types::rfc3339::parse_utc_fractional("2026-10-28T00:19:03Z").unwrap();
    let r = store.last().unwrap();
    assert!(
        !r.fresh(reset + RESET_SETTLE_S, 0.1),
        "past the spent limit's refill"
    );
    let low_store = Store::new(dir.path(), "low-refill");
    let reset_5h = aterm_types::rfc3339::parse_utc_fractional("2026-10-24T05:49:13Z").unwrap();
    let low_agy = Recorded::new(Ok(doc(0.9, 0.9, 0.05)));
    current_fixture(&low_store, &low_agy, reset_5h - 30, 0.1, false).unwrap();
    let low_r = low_store.last().unwrap();
    assert!(
        low_r.fresh(reset_5h + RESET_SETTLE_S - 1, 0.1),
        "still fresh inside FRESH_TIGHT_S before the low bucket's refill settles"
    );
    assert!(
        !low_r.fresh(reset_5h + RESET_SETTLE_S, 0.1),
        "a low bucket's settled refill expires the reading before FRESH_TIGHT_S"
    );
}

#[test]
fn a_failed_read_falls_back_to_the_last_kept_one() {
    let dir = tmp("fail");
    let store = Store::new(dir.path(), "home");
    let bad = Recorded::new(Err("`agy -p /usage` exited 1".to_owned()));
    assert_eq!(
        current_fixture(&store, &bad, NOW, 0.1, false).unwrap_err(),
        "`agy -p /usage` exited 1"
    );
    current_fixture(
        &store,
        &Recorded::new(Ok(doc(0.5, 0.5, 0.5))),
        NOW,
        0.1,
        true,
    )
    .unwrap();
    let (r, got) = current_fixture(&store, &bad, NOW + FRESH_OPEN_S, 0.1, false).unwrap();
    assert_eq!((got, r.as_of), (Got::Stale, NOW));
}

#[test]
fn a_failed_read_is_not_retried_until_its_backoff_passes() {
    let dir = tmp("backoff");
    let store = Store::new(dir.path(), "home");
    let bad = Recorded::new(Err("signed out".to_owned()));
    assert!(current_fixture(&store, &bad, NOW, 0.1, false).is_err());
    let again = current_fixture(&store, &bad, NOW + 60, 0.1, false).unwrap_err();
    assert!(
        again.contains("signed out") && again.contains("waits"),
        "{again}"
    );
    assert_eq!(bad.reads.get(), 1, "inside the backoff agy is not asked");
    let _ = current_fixture(&store, &bad, NOW + 60, 0.1, true);
    assert_eq!(bad.reads.get(), 2, "a forced read asks anyway");
    let good = Recorded::new(Ok(doc(0.5, 0.5, 0.5)));
    assert_eq!(
        current_fixture(&store, &good, NOW + FAIL_BACKOFF_S + 60, 0.1, false)
            .unwrap()
            .1,
        Got::Read
    );
    assert!(
        store.recent_failure(NOW + FAIL_BACKOFF_S + 61).is_none(),
        "a good read clears it"
    );
}

#[test]
fn a_held_lock_hands_out_the_kept_reading_and_asks_agy_nothing() {
    let dir = tmp("lock");
    let store = Store::new(dir.path(), "home");
    current_fixture(
        &store,
        &Recorded::new(Ok(doc(0.5, 0.5, 0.5))),
        NOW,
        0.1,
        false,
    )
    .unwrap();
    let held = store.try_lock().expect("the first lock");
    assert!(store.try_lock().is_none(), "one reader at a time");
    let agy = Recorded::new(Ok(doc(0.4, 0.4, 0.4)));
    let (_, got) = current_fixture(&store, &agy, NOW + FRESH_OPEN_S, 0.1, true).unwrap();
    assert_eq!((got, agy.reads.get()), (Got::Stale, 0));
    drop(held);
    assert!(
        store.try_lock().is_some(),
        "a dropped lock lets the next reader in"
    );
}

#[test]
fn homes_never_share_a_reading() {
    assert_eq!(launch_key(None, &[]), "home");
    let a = launch_key(Some("/Users//me/agy-a"), &[]);
    let b = launch_key(Some("/Users//me/agy-b"), &[]);
    let c = launch_key(None, &["--gemini_dir=/Users//me/alt".to_owned()]);
    assert!(a.starts_with('h') && a.len() == 17, "{a}");
    assert!(a != b && a != c && b != c, "{a} {b} {c}");
    assert_eq!(a, launch_key(Some("/Users//me/agy-a"), &[]), "stable");
}

#[test]
fn the_history_gives_each_limit_its_burn_and_when_it_runs_out() {
    let dir = tmp("burn");
    let store = Store::new(dir.path(), "home");
    for (i, left) in [0.50, 0.45, 0.40].iter().enumerate() {
        let at = NOW + i as i64 * 1_800;
        current_fixture(
            &store,
            &Recorded::new(Ok(doc(0.9, 0.9, *left))),
            at,
            0.1,
            true,
        )
        .unwrap();
    }
    let history = store.history();
    let b = burn(&history, "3p-5h").expect("an hour of falling samples");
    assert!((b.per_hour - 0.10).abs() < 1e-9, "{b:?}");
    assert_eq!(
        b.empty_at, None,
        "0.40 left at 0.10/h runs out after the 5h limit refills, so it never empties first"
    );
    let w = burn(&history, "3p-weekly").unwrap();
    assert_eq!(
        (w.per_hour, w.empty_at),
        (0.0, None),
        "a flat limit burns nothing"
    );
    assert!(
        burn(&history[..1], "3p-5h").is_none(),
        "one sample is no rate"
    );
}

#[test]
fn a_burn_that_beats_the_refill_names_the_moment() {
    let mk = |at: i64, left: f64| Sample {
        at,
        buckets: vec![(
            "3p-weekly".to_owned(),
            Point {
                left: Some(left),
                reset: Some(NOW + 7 * 86_400),
                off: false,
            },
        )],
    };
    let history = [mk(NOW, 0.30), mk(NOW + 3_600, 0.20)];
    let b = burn(&history, "3p-weekly").unwrap();
    assert_eq!(b.empty_at, Some(NOW + 3_600 + 2 * 3_600));
    let other_window = [
        Sample {
            buckets: vec![(
                "3p-weekly".to_owned(),
                Point {
                    left: Some(0.9),
                    reset: Some(NOW),
                    off: false,
                },
            )],
            ..mk(NOW - 3_600, 0.0)
        },
        mk(NOW + 3_600, 0.20),
    ];
    assert!(
        burn(&other_window, "3p-weekly").is_none(),
        "a refill between samples is no burn"
    );
}

#[test]
fn a_launch_rotates_off_the_spent_pool_on_the_kept_reading() {
    let dir = tmp("launch");
    let store = Store::new(dir.path(), "home");
    let agy = Recorded {
        default: Some("gemini-4-argon-high"),
        ..Recorded::new(Ok(doc(0.0, 0.96, 0.92)))
    };
    let r = agy_quota::Rotation::default();
    let got = rotate_launch(None, &store, &agy, &r, NOW).unwrap();
    assert_eq!(
        got.pick.model(),
        Some("claude-opus-5-5-medium"),
        "{}",
        got.pick.describe()
    );
    assert!(!got.named);
    assert_eq!(got.got, Got::Read);
    assert_eq!(
        store.kept_default(NOW + 60).as_deref(),
        Some("gemini-4-argon-high"),
        "the default is kept"
    );
    assert_eq!(
        store.kept_default(NOW + FRESH_OPEN_S),
        None,
        "and read again after 15 minutes"
    );
    let named = rotate_launch(Some("claude-sonnet-5-5-low"), &store, &agy, &r, NOW + 60).unwrap();
    assert_eq!(named.pick.model(), Some("claude-sonnet-5-5-low"));
    assert_eq!(
        (named.got, agy.reads.get()),
        (Got::Kept, 1),
        "the second launch reads nothing"
    );
    let off = agy_quota::Rotation {
        enabled: false,
        ..agy_quota::Rotation::default()
    };
    assert_eq!(
        rotate_launch(None, &store, &agy, &off, NOW),
        None,
        "rotation off leaves the launch alone"
    );
    let mute = Recorded::new(Err("no agy".to_owned()));
    assert_eq!(
        rotate_launch(None, &Store::new(dir.path(), "other"), &mute, &r, NOW),
        None,
        "no reading: the launch runs as asked"
    );

    let spent = Recorded::new(Ok(doc(0.0, 0.0, 0.0)));
    let spent_store = Store::new(dir.path(), "spent");
    let (reading, _) = current_fixture(&spent_store, &spent, NOW, r.low, false).unwrap();
    let reset = reading
        .quota
        .groups
        .iter()
        .flat_map(|g| &g.buckets)
        .filter_map(agy_quota::Bucket::reset_at)
        .min()
        .expect("recorded refill");
    let settled = reset + RESET_SETTLE_S;
    let asked = Some("gemini-4-argon-high");
    let before = rotate_launch(asked, &spent_store, &mute, &r, settled - 1)
        .expect("the unexpired stale reading keeps its policy");
    assert_eq!(before.got, Got::Stale);
    assert!(matches!(before.pick, agy_quota::Pick::Wait { .. }));
    assert_eq!(
        rotate_launch(asked, &spent_store, &mute, &r, settled),
        None,
        "a failed refresh after refill leaves launch availability unknown"
    );
    let (shown, got) = current_fixture(&spent_store, &mute, settled, r.low, false).unwrap();
    assert_eq!((got, shown.as_of), (Got::Stale, NOW));
    assert_eq!(
        shown.quota, reading.quota,
        "the old reading remains available for display"
    );

    // When one pool's 5h bucket settles while another pool's weekly bucket is still spent for days,
    // a stale launch pick still rotates off the weekly-spent pool onto the open pool.
    let partial_store = Store::new(dir.path(), "partial-stale");
    let partial = Recorded::new(Ok(doc(0.0, 0.9, 0.0)));
    let _ = current_fixture(&partial_store, &partial, NOW, r.low, false).unwrap();
    let stale_rot = rotate_launch(asked, &partial_store, &mute, &r, settled)
        .expect("weekly-spent pool still rotates to the pool whose 5h refill settled");
    assert_eq!(stale_rot.got, Got::Stale);
    assert_eq!(
        stale_rot.pick,
        agy_quota::Pick::Rotate {
            from: "gemini-4-argon-high".to_owned(),
            from_standing: Standing::Spent,
            to: "claude-opus-5-5-medium".to_owned(),
            to_standing: Standing::Open,
        }
    );
    assert_eq!(
        rotate_launch(
            Some("claude-opus-5-5-medium"),
            &partial_store,
            &mute,
            &r,
            settled
        ),
        None,
        "when the asked model's pool settled on a stale reading, launch passes through without claiming fresh standing"
    );

    let wall_store = Store::new(dir.path(), "wall");
    let open = Recorded::new(Ok(doc(0.95, 0.95, 0.95)));
    let _ = rotate_launch_at(
        Some("claude-opus-5-5-medium"),
        &wall_store,
        &open,
        &r,
        NOW,
        false,
    )
    .unwrap();
    let exhausted = Recorded::new(Ok(doc(0.95, 0.05, 0.05)));
    let kept = rotate_launch_at(
        Some("claude-opus-5-5-medium"),
        &wall_store,
        &exhausted,
        &r,
        NOW + WALL_FORCE_FLOOR_S - 1,
        true,
    )
    .unwrap();
    assert_eq!(
        (kept.got, exhausted.reads.get()),
        (Got::Kept, 0),
        "inside WALL_FORCE_FLOOR_S the kept reading is used"
    );
    let refreshed = rotate_launch_at(
        Some("claude-opus-5-5-medium"),
        &wall_store,
        &exhausted,
        &r,
        NOW + WALL_FORCE_FLOOR_S,
        true,
    )
    .unwrap();
    assert_eq!((refreshed.got, exhausted.reads.get()), (Got::Read, 1));
    assert_eq!(refreshed.pick.model(), Some("gemini-4-argon-high"));

    // An unnamed session (`asked = None`) whose default model changed via `/model` inside
    // `FRESH_OPEN_S` refreshes `default_model()` when `at_wall` forces a fresh reading.
    let switch_store = Store::new(dir.path(), "switch");
    let before_switch = Recorded {
        default: Some("gemini-4-argon-high"),
        ..Recorded::new(Ok(doc(0.95, 0.95, 0.95)))
    };
    let start_pick = rotate_launch(None, &switch_store, &before_switch, &r, NOW).unwrap();
    assert_eq!(start_pick.pick.model(), Some("gemini-4-argon-high"));
    let after_switch = Recorded {
        default: Some("claude-opus-5-5-medium"),
        ..Recorded::new(Ok(doc(0.95, 0.95, 0.0)))
    };
    let wall_switched = rotate_launch_at(
        None,
        &switch_store,
        &after_switch,
        &r,
        NOW + WALL_FORCE_FLOOR_S,
        true,
    )
    .unwrap();
    assert_eq!(wall_switched.got, Got::Read);
    assert_eq!(
        wall_switched.pick,
        agy_quota::Pick::Rotate {
            from: "claude-opus-5-5-medium".to_owned(),
            from_standing: Standing::Spent,
            to: "gemini-4-argon-high".to_owned(),
            to_standing: Standing::Open,
        }
    );

    // A spent bucket whose reset_at (reset_5h) is crossed < WALL_FORCE_FLOOR_S after a
    // pre-refill reading (reset_5h - 10) still forces a fresh read at the wall once `now >= reset_at`.
    let reset_5h = aterm_types::rfc3339::parse_utc_fractional("2026-10-24T05:49:13Z").unwrap();
    let cross_store = Store::new(dir.path(), "cross");
    let pre_refill = Recorded::new(Ok(doc(0.0, 0.0, 0.0)));
    let _ = rotate_launch_at(
        Some("claude-opus-5-5-medium"),
        &cross_store,
        &pre_refill,
        &r,
        reset_5h - 10,
        true,
    )
    .unwrap();
    let mute_before = mute.reads.get();
    assert_eq!(
        rotate_launch_at(
            Some("claude-opus-5-5-medium"),
            &cross_store,
            &mute,
            &r,
            reset_5h + 5,
            true,
        ),
        None,
        "a failed refresh after crossing reset_at at a wall refuses the pre-refill reading"
    );
    assert_eq!(mute.reads.get(), mute_before + 1);
    // That failed read was this wall's one read until the settle point (`reset_5h + RESET_SETTLE_S`),
    // and still refuses the pre-refill reading without polling agy again inside the settle window.
    let _ = std::fs::remove_file(cross_store.failed_path());
    let post_refill = Recorded::new(Ok(doc(0.0, 1.0, 1.0)));
    assert_eq!(
        rotate_launch_at(
            Some("claude-opus-5-5-medium"),
            &cross_store,
            &post_refill,
            &r,
            reset_5h + 40,
            true,
        ),
        None,
        "spent wall inside the settle window neither re-reads agy nor trusts the pre-refill reading"
    );
    assert_eq!(post_refill.reads.get(), 0);
    let crossed = rotate_launch_at(
        Some("claude-opus-5-5-medium"),
        &cross_store,
        &post_refill,
        &r,
        reset_5h + 5 + WALL_READ_SPENT_S,
        true,
    )
    .unwrap();
    assert_eq!((crossed.got, post_refill.reads.get()), (Got::Read, 1));
    assert!(matches!(crossed.pick, agy_quota::Pick::Keep { .. }));

    // `launch_opening` reads the kept reading without asking agy: `Some(until)` while every pool
    // is spent and `until > now` (even after `FRESH_OPEN_S` using `last_default`), and `None` once
    // `now >= until`, rotation is off, or a pool opens.
    let hold_store = Store::new(dir.path(), "hold");
    let hold_agy = Recorded {
        default: Some("gemini-4-argon-high"),
        ..Recorded::new(Ok(doc(0.0, 0.8, 0.0)))
    };
    let initial = rotate_launch(None, &hold_store, &hold_agy, &r, NOW).unwrap();
    assert_eq!(
        initial.pick,
        agy_quota::Pick::Wait {
            model: "gemini-4-argon-high".to_owned(),
            until: Some(reset_5h),
        }
    );
    assert_eq!(
        launch_opening(None, &hold_store, &r, NOW + FRESH_OPEN_S + 60),
        Some(reset_5h),
        "held past FRESH_OPEN_S still knows the default model and earliest refill"
    );
    assert_eq!(launch_opening(None, &hold_store, &off, NOW), None);
    assert_eq!(
        launch_opening(None, &hold_store, &r, reset_5h),
        None,
        "once `now >= until` the hold ends so the next attempt reads fresh usage"
    );
    // If the fresh read at `reset_5h` still sees the pre-refill zero before the server settles,
    // `launch_opening` holds until `reset_5h + RESET_SETTLE_S`, then releases at the settle point.
    let lagging = rotate_launch(None, &hold_store, &hold_agy, &r, reset_5h).unwrap();
    assert_eq!(lagging.got, Got::Read);
    assert_eq!(
        launch_opening(None, &hold_store, &r, reset_5h),
        Some(reset_5h + RESET_SETTLE_S)
    );
    assert_eq!(
        launch_opening(None, &hold_store, &r, reset_5h + RESET_SETTLE_S),
        None
    );
    let refilled = Recorded {
        default: Some("gemini-4-argon-high"),
        ..Recorded::new(Ok(doc(0.0, 0.8, 1.0)))
    };
    let resumed =
        rotate_launch(None, &hold_store, &refilled, &r, reset_5h + RESET_SETTLE_S).unwrap();
    assert_eq!((resumed.got, refilled.reads.get()), (Got::Read, 1));
    assert_eq!(resumed.pick.model(), Some("claude-opus-5-5-medium"));
}

#[test]
fn the_model_document_names_agys_default() {
    let doc = r#"{"status":"SUCCESS","command":{"name":"model","data":{"id":"claude-opus-5-5-medium","label":"Claude Opus 5.5 (Medium)"}}}"#;
    assert_eq!(
        agy_quota::parse_model(doc).unwrap(),
        "claude-opus-5-5-medium"
    );
    assert!(agy_quota::parse_model(r#"{"command":{"name":"usage","data":{}}}"#).is_err());
    assert!(
        agy_quota::parse_model(
            r#"{"status":"ERROR","response":"not signed in","command":{"name":"model","data":{"id":"claude-opus-5-5-medium"}}}"#
        )
        .is_err()
    );
}

#[cfg(unix)]
#[test]
fn store_rejects_fifos_and_replaces_them_atomically() {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt as _;

    let dir = tmp("fifo");
    let store = Store::new(dir.path(), "home");
    std::fs::create_dir_all(dir.path().join(DIR)).unwrap();
    for path in [
        store.last_path(),
        store.history_path(),
        store.model_path(),
        store.failed_path(),
        store.lock_path(),
    ] {
        let c = CString::new(path.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
    }
    assert!(store.last().is_none());
    assert!(store.history().is_empty());
    assert!(store.kept_default(NOW).is_none());
    assert!(store.recent_failure(NOW).is_none());
    let lock = store
        .try_lock()
        .expect("non-regular lock file is treated as stale and replaced");
    drop(lock);
    let agy = Recorded::new(Ok(doc(0.75, 0.9, 0.9)));
    let (r, got) = current_fixture(&store, &agy, NOW, 0.1, false).unwrap();
    assert_eq!((got, r.as_of), (Got::Read, NOW));
    assert_eq!(store.history().len(), 1);
    assert!(!r.fresh(i64::MAX, 0.1));
    assert_eq!(
        burn(
            &[
                Sample {
                    at: i64::MIN,
                    buckets: vec![(
                        "gemini-weekly".to_owned(),
                        Point {
                            left: Some(0.9),
                            reset: None,
                            off: false,
                        },
                    )],
                },
                Sample {
                    at: i64::MAX,
                    buckets: vec![(
                        "gemini-weekly".to_owned(),
                        Point {
                            left: Some(0.5),
                            reset: None,
                            off: false,
                        },
                    )],
                },
            ],
            "gemini-weekly"
        ),
        None,
        "extreme sample timestamps must not overflow i64 subtraction"
    );
}

/// However many times the loop looks at a wall, it reads once; a refill the reading records, or a
/// wall standing past [`WALL_READ_SPENT_S`], reads once more. CONTROL: the first look and those
/// two do read, on the same store, so the spent stamp is what holds the other looks back.
#[test]
fn a_wall_makes_one_read_however_often_it_is_looked_at() {
    let dir = tmp("one-wall");
    let store = Store::new(dir.path(), "wall");
    let r = agy_quota::Rotation::default();
    let asked = Some("claude-opus-5-5-medium");
    let agy = Recorded::new(Ok(doc(0.95, 0.5, 0.0)));
    let wall = |at: i64| rotate_launch_at(asked, &store, &agy, &r, at, true);
    let reset = wall(NOW).expect("a pick").quota.groups[1].buckets[1]
        .reset_at()
        .expect("the wall's reset");
    assert_eq!(agy.reads.get(), 1, "CONTROL: a wall's first look reads");
    let shortly = reset.min(NOW + WALL_READ_SPENT_S) - 1;
    let mut at = NOW + WALL_FORCE_FLOOR_S;
    while at < shortly {
        let _ = wall(at);
        at += WALL_FORCE_FLOOR_S;
    }
    assert_eq!(agy.reads.get(), 1, "a standing wall has had its read");
    let _ = wall(reset.max(NOW + WALL_READ_SPENT_S));
    assert_eq!(
        agy.reads.get(),
        2,
        "CONTROL: the refill arrived, or the hour passed"
    );
}

/// NEGATIVE CONTROL: a failed wall read on a cold store or past a settled refill still spends the
/// wall's one-hour read budget instead of re-polling every [`FAIL_BACKOFF_S`], while a newer
/// launch reading supersedes an older cold-wall stamp so a later session's wall can refresh.
#[test]
fn failed_wall_reads_spend_the_wall_budget_without_repolling_after_backoff() {
    let dir = tmp("failed-wall");
    let r = agy_quota::Rotation::default();
    let asked = Some("claude-opus-5-5-medium");

    // 1. Cold store: a failed wall read writes `.wall` and makes zero extra reads past
    // `FAIL_BACKOFF_S` until `WALL_READ_SPENT_S` elapses (or a newer launch reading arrives).
    let cold_store = Store::new(dir.path(), "cold");
    let failing = Recorded::new(Err("not signed in".to_owned()));
    assert_eq!(
        rotate_launch_at(asked, &cold_store, &failing, &r, NOW, true),
        None
    );
    assert_eq!(failing.reads.get(), 1);
    for at in [NOW + FAIL_BACKOFF_S + 10, NOW + WALL_READ_SPENT_S - 1] {
        assert_eq!(
            rotate_launch_at(asked, &cold_store, &failing, &r, at, true),
            None
        );
    }
    assert_eq!(
        failing.reads.get(),
        1,
        "cold wall failure must spend the wall's one-hour read budget"
    );
    let signed_in = Recorded::new(Ok(doc(0.95, 0.95, 0.95)));
    let launched = rotate_launch(
        asked,
        &cold_store,
        &signed_in,
        &r,
        NOW + FAIL_BACKOFF_S + 20,
    )
    .expect("a later launch after sign-in still reads once backoff expires");
    assert_eq!((launched.got, signed_in.reads.get()), (Got::Read, 1));
    let later_wall = Recorded::new(Ok(doc(0.95, 0.5, 0.0)));
    let rotated = rotate_launch_at(
        asked,
        &cold_store,
        &later_wall,
        &r,
        NOW + FAIL_BACKOFF_S + 20 + WALL_FORCE_FLOOR_S,
        true,
    )
    .expect("a new launch reading supersedes the earlier cold-wall stamp");
    assert_eq!((rotated.got, later_wall.reads.get()), (Got::Read, 1));

    // 2. Post-settle wall failure over a pre-refill reading (`reset_5h`,
    // `settled = reset_5h + RESET_SETTLE_S`) holds the spent stamp for the full hour past
    // `post_settle`.
    let reset_5h = aterm_types::rfc3339::parse_utc_fractional("2026-10-24T05:49:13Z").unwrap();
    let post_settle = reset_5h + RESET_SETTLE_S + 10;
    let settled_store = Store::new(dir.path(), "settled");
    let pre = Recorded::new(Ok(doc(0.0, 0.0, 0.0)));
    let _ = rotate_launch_at(asked, &settled_store, &pre, &r, reset_5h - 10, true).unwrap();
    assert_eq!(
        rotate_launch_at(asked, &settled_store, &failing, &r, post_settle, true),
        None
    );
    assert_eq!(failing.reads.get(), 2);
    for at in [
        post_settle + FAIL_BACKOFF_S + 10,
        post_settle + WALL_READ_SPENT_S - 1,
    ] {
        assert_eq!(
            rotate_launch_at(asked, &settled_store, &failing, &r, at, true),
            None
        );
    }
    assert_eq!(
        failing.reads.get(),
        2,
        "post-settle failed wall read must not re-poll until WALL_READ_SPENT_S"
    );
    let recovered = Recorded::new(Ok(doc(0.0, 1.0, 1.0)));
    let after_hour = rotate_launch_at(
        asked,
        &settled_store,
        &recovered,
        &r,
        post_settle + WALL_READ_SPENT_S,
        true,
    )
    .expect("CONTROL: once WALL_READ_SPENT_S elapses, the next look reads again");
    assert_eq!((after_hour.got, recovered.reads.get()), (Got::Read, 1));
}

/// The SSH-style environment that selects agy's native file backend, as `aterm harness agy login`
/// and its account workers run it.
#[cfg(unix)]
const FILE_BACKEND: (&str, &str) = ("SSH_CONNECTION", "127.0.0.1 1 127.0.0.1 22");

/// A stand-in agy at `<dir>/agy` (already run once, so a bounded read meets an assessed file)
/// that marks `<dir>/agy.ran` whenever a read runs it, then does `body`.
#[cfg(unix)]
fn stand_in_agy(dir: &Path, body: &str) -> (PathBuf, PathBuf) {
    let agy = dir.join("agy");
    let ran = dir.join("agy.ran");
    crate::harness::stand_in::proven(&agy, &format!(": > \"$0.ran\"\n{body}"));
    (agy, ran)
}

/// An agy whose `HOME` (in `env`) is a login directory no account answers to, behind the gate
/// of a pool kept beside `program`: such a home passes on its grant file alone.
#[cfg(unix)]
fn agy_at(program: &Path, env: &[(&str, &str)], flags: &[String]) -> Agy {
    let root = program.parent().expect("the stand-in's directory");
    let login = env
        .iter()
        .rev()
        .find(|(name, _)| *name == "HOME")
        .map(|(_, home)| Path::new(home));
    Agy {
        program: program.as_os_str().to_owned(),
        env: env
            .iter()
            .map(|(n, v)| ((*n).to_owned(), (*v).to_owned()))
            .collect(),
        flags: flags.to_vec(),
        budget: LAUNCH_READ_BUDGET,
        inherited_home: std::env::var(crate::harness::upgrade_agy::HOME_ENV).ok(),
        credentials_set: Vec::new(),
        gate: Some(Gate::at(
            &root.join("gate-state"),
            &root.join("gate-home"),
            None,
            login,
        )),
    }
}

/// The person's own agy in `home` (agy's keyring backend: no grant file speaks), behind the gate
/// of this machine's own agy login.
#[cfg(unix)]
fn own_agy(program: &Path, home: &Path) -> Agy {
    let root = program.parent().expect("the stand-in's directory");
    let mut agy = agy_at(program, &[("HOME", home.to_str().expect("utf-8"))], &[]);
    agy.gate = Some(Gate::at(&root.join("gate-state"), home, None, None));
    agy
}

/// Put a served turn on record for `agy`'s login, as the footer's hook does.
#[cfg(unix)]
fn serve(agy: &Agy, at: i64) {
    let gate = agy.gate.as_ref().expect("a gate");
    assert!(gate.served(Some("person@example.com"), at, at));
}

#[cfg(unix)]
fn put_grant(config: &Path) {
    std::fs::create_dir_all(config).unwrap();
    // A stand-in grant: what agy keeps there is never read, only whether the file is there.
    std::fs::write(config.join(crate::harness::upgrade_agy::GRANT_FILE), b"{}").unwrap();
}

#[cfg(unix)]
#[test]
fn a_signed_out_file_profile_answers_at_once_and_never_runs_agy() {
    let dir = tmp("signed-out");
    let profile = dir.path().join("profile");
    let home = profile.display().to_string();
    let (program, ran) = stand_in_agy(dir.path(), &format!("printf '%s' '{}'", doc(0.8, 0.9, 0.9)));
    let agy = agy_at(&program, &[("HOME", &home), FILE_BACKEND], &[]);
    let why = read(&agy).expect_err("no grant file, no reading");
    assert!(why.starts_with("signed-out: agy's login"), "{why}");
    assert!(why.contains("is signed out on this machine"), "{why}");
    assert!(why.contains("antigravity-oauth-token"), "{why}");
    assert!(why.contains("aterm harness agy login <account>"), "{why}");
    assert!(is_signed_out(&why));

    let store = Store::new(dir.path(), &launch_key(Some(home.as_str()), &[]));
    let (model, probes) = provider_read::captured(|| {
        default_model(&store, &agy, &mut Asking::new(token_for(&store)), NOW)
    });
    assert_eq!(
        model,
        Err(why.clone()),
        "the default model is answered locally"
    );
    assert!(probes.is_empty(), "a local answer notes no probe");
    let got = current_fixture(&store, &agy, NOW, 0.1, true);
    assert_eq!(
        got.map(|_| ()),
        Err(why.clone()),
        "a forced read is answered the same"
    );
    assert!(
        store.recent_failure(NOW).is_none(),
        "a local answer sets no backoff"
    );
    let rotation = agy_quota::Rotation::default();
    assert_eq!(
        rotate_launch_at(None, &store, &agy, &rotation, NOW, false),
        None,
        "a worker launch on a signed-out profile launches as asked, at once"
    );

    // An empty grant file is no grant, and neither is a directory where it should be.
    let config = profile.join(".gemini/antigravity-cli");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(config.join(crate::harness::upgrade_agy::GRANT_FILE), b"").unwrap();
    assert!(refused(&agy).is_some(), "an empty grant file");
    std::fs::remove_file(config.join(crate::harness::upgrade_agy::GRANT_FILE)).unwrap();
    std::fs::create_dir(config.join(crate::harness::upgrade_agy::GRANT_FILE)).unwrap();
    assert!(refused(&agy).is_some(), "a directory named like the grant");
    assert!(!ran.exists(), "agy ran on a profile with no grant");
}

#[cfg(unix)]
#[test]
fn a_profile_with_its_grant_reads_as_before() {
    let dir = tmp("signed-in");
    let profile = dir.path().join("profile");
    let home = profile.display().to_string();
    put_grant(&profile.join(".gemini/antigravity-cli"));
    let (program, ran) = stand_in_agy(dir.path(), &format!("printf '%s' '{}'", doc(0.8, 0.9, 0.9)));
    let agy = agy_at(&program, &[("HOME", &home), FILE_BACKEND], &[]);
    assert_eq!(refused(&agy), None);
    let wrong = provider_read::Use::fixture(
        provider_read::Read::AgyUsage,
        Path::new("another-home"),
        None,
    )
    .begin(
        provider_read::Account::in_home(
            provider_read::Read::AgyUsage,
            Path::new("another-home"),
            None,
        ),
        NOW,
        None,
    )
    .unwrap();
    assert!(agy.read(&wrong, agy.admit(NOW).unwrap()).is_err());
    assert!(
        !ran.exists(),
        "a permit for another home cannot run the provider"
    );
    let store = Store::new(dir.path(), &launch_key(Some(home.as_str()), &[]));
    let (reading, got) = current_fixture(&store, &agy, NOW, 0.1, true).expect("it reads");
    assert_eq!((got, reading.as_of), (Got::Read, NOW));
    assert!(ran.exists(), "the read ran agy");
}

#[cfg(unix)]
fn token_for(store: &Store) -> provider_read::Use {
    provider_read::Use::fixture(provider_read::Read::AgyUsage, Path::new(&store.key), None)
}

/// A signed-in file-backend home whose stand-in agy answers `/usage` with a doc and `/model`
/// with a default model, marking `<dir>/agy.model` whenever `/model` runs.
#[cfg(unix)]
fn model_rig(tag: &str) -> (aterm_tempfile::TempDir, Agy, Store, PathBuf) {
    let dir = tmp(tag);
    let profile = dir.path().join("profile");
    let home = profile.display().to_string();
    put_grant(&profile.join(".gemini/antigravity-cli"));
    let model =
        r#"{"status":"SUCCESS","command":{"name":"model","data":{"id":"gemini-4-argon-high"}}}"#;
    let (program, _) = stand_in_agy(
        dir.path(),
        &format!(
            "case \"$2\" in\n/model) : > \"$0.model\"; printf '%s' '{model}' ;;\n*) printf '%s' '{}' ;;\nesac",
            doc(0.0, 1.0, 1.0)
        ),
    );
    let marker = dir.path().join("agy.model");
    let agy = agy_at(&program, &[("HOME", &home), FILE_BACKEND], &[]);
    let store = Store::new(dir.path(), &launch_key(Some(home.as_str()), &[]));
    (dir, agy, store, marker)
}

#[cfg(unix)]
fn agy_probe(why: provider_read::InUse) -> provider_read::ProbeEvent {
    provider_read::ProbeEvent {
        at: NOW,
        read: provider_read::Read::AgyUsage,
        account: None,
        login: None,
        why,
    }
}

/// A launch on a kept reading still asks agy its default model: that ask spends the launch's
/// use of the home and is on record as its probe.
#[cfg(unix)]
#[test]
fn a_launchs_default_model_read_spends_its_use_and_notes_its_probe() {
    let (_dir, agy, store, marker) = model_rig("model-kept");
    current_fixture(
        &store,
        &Recorded::new(Ok(doc(0.8, 0.9, 0.9))),
        NOW,
        0.1,
        false,
    )
    .unwrap();
    let rotation = agy_quota::Rotation::default();
    let (pick, probes) =
        provider_read::captured(|| rotate_launch(None, &store, &agy, &rotation, NOW));
    let pick = pick.expect("a pick from the kept reading and the default read now");
    assert_eq!((pick.got, pick.named), (Got::Kept, false));
    assert!(marker.exists(), "the default model was read from agy");
    assert_eq!(probes, vec![agy_probe(provider_read::InUse::Launching)]);
    assert_eq!(
        store.kept_default(NOW).as_deref(),
        Some("gemini-4-argon-high")
    );
}

/// One use that asks agy twice (`/usage`, then `/model`) notes two probes.
#[cfg(unix)]
#[test]
fn a_model_read_after_the_usage_read_notes_its_own_probe() {
    let (_dir, agy, store, marker) = model_rig("model-after-usage");
    let rotation = agy_quota::Rotation::default();
    let (pick, probes) =
        provider_read::captured(|| rotate_launch(None, &store, &agy, &rotation, NOW));
    assert_eq!(pick.map(|p| p.got), Some(Got::Read));
    assert!(marker.exists());
    assert_eq!(
        probes,
        vec![
            agy_probe(provider_read::InUse::Launching),
            agy_probe(provider_read::InUse::Launching)
        ]
    );
}

/// CONTROL: with a later ask riding the first ask's probe, the two-ask use is on record once,
/// so `a_model_read_after_the_usage_read_notes_its_own_probe` fails.
#[cfg(unix)]
#[test]
fn control_a_model_read_riding_the_usage_probe_is_missing_from_the_record() {
    let (_dir, agy, store, marker) = model_rig("model-control");
    let rotation = agy_quota::Rotation::default();
    UNNOTED_LATER_ASK.set(true);
    let (pick, probes) =
        provider_read::captured(|| rotate_launch(None, &store, &agy, &rotation, NOW));
    UNNOTED_LATER_ASK.set(false);
    assert_eq!(pick.map(|p| p.got), Some(Got::Read));
    assert!(marker.exists(), "agy was asked its default model");
    assert_eq!(probes, vec![agy_probe(provider_read::InUse::Launching)]);
}

/// The `/model` read never runs agy without this home's authority: not for a token of another
/// home, not for a spent use, not for another home's permit.
#[cfg(unix)]
#[test]
fn the_model_read_never_runs_agy_without_this_homes_authority() {
    let (_dir, agy, store, marker) = model_rig("model-token");
    let (model, probes) = provider_read::captured(|| {
        let other = provider_read::Use::fixture(
            provider_read::Read::AgyUsage,
            Path::new("another-home"),
            None,
        );
        let refused = default_model(&store, &agy, &mut Asking::new(other), NOW);
        let spent = default_model(&store, &agy, &mut Asking::spent(), NOW);
        (refused, spent)
    });
    assert!(model.0.is_err() && model.1.is_err(), "{model:?}");
    assert!(probes.is_empty(), "a refused ask notes no probe");
    let wrong = provider_read::Use::fixture(
        provider_read::Read::AgyUsage,
        Path::new("another-home"),
        None,
    )
    .begin_home(
        provider_read::Read::AgyUsage,
        Path::new("another-home"),
        NOW,
    )
    .unwrap();
    assert!(agy.default_model(&wrong, agy.admit(NOW).unwrap()).is_err());
    assert!(
        !marker.exists(),
        "agy's /model ran without this home's authority"
    );

    let (model, probes) = provider_read::captured(|| {
        default_model(&store, &agy, &mut Asking::new(token_for(&store)), NOW)
    });
    assert_eq!(model.as_deref(), Ok("gemini-4-argon-high"));
    assert_eq!(probes, vec![agy_probe(provider_read::InUse::Launching)]);
    assert!(marker.exists(), "CONTROL: this home's token runs /model");
}

/// A signed-out file-backend home never runs agy for its default model, at a launch or a wall:
/// the missing grant answers here, and nothing is on record.
#[cfg(unix)]
#[test]
fn a_signed_out_home_never_runs_agy_for_its_default_model() {
    let (dir, agy, store, marker) = model_rig("model-signed-out");
    current_fixture(
        &store,
        &Recorded::new(Ok(doc(0.0, 1.0, 1.0))),
        NOW,
        0.1,
        false,
    )
    .unwrap();
    std::fs::remove_file(
        dir.path()
            .join("profile/.gemini/antigravity-cli")
            .join(crate::harness::upgrade_agy::GRANT_FILE),
    )
    .unwrap();
    assert!(refused(&agy).is_some());
    let rotation = agy_quota::Rotation::default();
    let (picks, probes) = provider_read::captured(|| {
        (
            rotate_launch_at(None, &store, &agy, &rotation, NOW, false),
            rotate_launch_at(None, &store, &agy, &rotation, NOW, true),
        )
    });
    assert_eq!(picks, (None, None), "no default model is known here");
    assert!(probes.is_empty(), "{probes:?}");
    assert!(!marker.exists(), "agy ran on a signed-out home");
    assert!(!dir.path().join("agy.ran").exists());
}

#[cfg(unix)]
#[test]
fn a_carried_gemini_dir_names_the_grant_it_reads() {
    let dir = tmp("gemini-dir");
    let profile = dir.path().join("profile");
    let home = profile.display().to_string();
    let alt = dir.path().join("alt-gemini");
    put_grant(&profile.join(".gemini/antigravity-cli"));
    let flags = [format!("--gemini_dir={}", alt.display())];
    let agy = agy_at(
        &dir.path().join("no-such-agy"),
        &[("HOME", &home), FILE_BACKEND],
        &flags,
    );
    assert_eq!(
        agy.grant_file(),
        Some(alt.join("antigravity-cli/antigravity-oauth-token"))
    );
    let why = refused(&agy).expect("the home's grant is not the carried dir's");
    assert!(
        why.contains(&alt.join("antigravity-cli").display().to_string()),
        "{why}"
    );
    put_grant(&alt.join("antigravity-cli"));
    assert_eq!(refused(&agy), None);
}

/// Only agy's file backend makes the grant file the answer: under the OS keyring (no
/// `SSH_CONNECTION`), or with another credential or an unplaced config, no file speaks, and the
/// account's record alone admits the read. agy is never run to find out.
#[cfg(unix)]
#[test]
fn without_the_file_backend_only_the_accounts_record_admits_a_read() {
    let dir = tmp("keyring");
    let profile = dir.path().join("profile");
    let home = profile.display().to_string();
    let (program, ran) = stand_in_agy(dir.path(), &format!("printf '%s' '{}'", doc(0.8, 0.9, 0.9)));
    let keyring = own_agy(&program, &profile);
    assert_eq!((keyring.grant_file(), keyring.proof()), (None, Proof::None));
    let why = read(&keyring).expect_err("nothing on record of the login");
    assert!(why.starts_with(super::super::agy_gate::NOT_KNOWN), "{why}");
    assert!(!ran.exists(), "agy was run to find out");
    // A turn served on it is on record: now it is read.
    serve(&keyring, NOW - 1);
    read(&keyring).expect("a keyring login known signed in is read");
    assert!(ran.exists());
    for other in [
        ("GEMINI_API_KEY", "k"),
        ("AGY_LLM_GATEWAY_URL", "http://127.0.0.1:1"),
        ("AGY_CLI_CONFIG_DIR", "/elsewhere"),
    ] {
        let agy = agy_at(&program, &[("HOME", &home), FILE_BACKEND, other], &[]);
        assert_eq!(agy.proof(), Proof::None, "{other:?}");
    }
    let blank = agy_at(&program, &[("HOME", &home), ("SSH_CONNECTION", " ")], &[]);
    assert_eq!(
        blank.grant_file(),
        None,
        "a blank SSH_CONNECTION selects nothing"
    );
}

/// A grant agy no longer accepts: agy prints its sign-in prompt and would wait a minute for a
/// person; the read stops at the prompt and says signed-out (expired).
#[cfg(unix)]
#[test]
fn a_sign_in_prompt_ends_the_read_as_signed_out() {
    let dir = tmp("expired");
    let profile = dir.path().join("profile");
    let home = profile.display().to_string();
    put_grant(&profile.join(".gemini/antigravity-cli"));
    let parked = aterm_spec::wait::PARKED_SECS;
    let (program, ran) = stand_in_agy(
        dir.path(),
        &format!(
            "printf 'Authentication required. Please visit the URL to log in: https://accounts.example.invalid/\\n'\n\
             printf 'Waiting for authentication (timeout 60s)...\\n' >&2\n\
             exec /bin/sleep {parked}"
        ),
    );
    let agy = agy_at(&program, &[("HOME", &home), FILE_BACKEND], &[]);
    // A read that waited for the parked stand-in would end at its budget, saying so.
    let why = read(&agy).expect_err("a prompt is no reading");
    assert!(ran.exists(), "the read ran agy");
    assert!(why.starts_with("signed-out (expired):"), "{why}");

    // Under the keyring the same prompt (here on stderr) is plain signed-out.
    let keyring = dir.path().join("keyring");
    std::fs::create_dir(&keyring).unwrap();
    let (program, _) = stand_in_agy(
        &keyring,
        &format!(
            "printf 'Authentication required. Please visit the URL to log in:\\n' >&2\n\
             exec /bin/sleep {parked}"
        ),
    );
    let own = own_agy(&program, &profile);
    serve(&own, NOW - 1);
    let why = read(&own).expect_err("a prompt is no reading");
    assert!(
        why.starts_with("signed-out: agy asked for a Google sign-in"),
        "{why}"
    );
    // The prompt is the account's record: the next read is refused at the gate.
    let why = refused(&own).expect("the gate holds");
    assert!(why.starts_with("signed-out: agy's login"), "{why}");
}

/// A read that meets the prompt answers no stale reading, and no later read runs agy, forced or
/// past the backoff: the login's account says it is signed out.
#[cfg(unix)]
#[test]
fn a_prompt_leaves_no_stale_reading_and_no_later_read_runs_agy() {
    let dir = tmp("prompt-current");
    let profile = dir.path().join("profile");
    let parked = aterm_spec::wait::PARKED_SECS;
    let agy = dir.path().join("agy");
    crate::harness::stand_in::proven(
        &agy,
        &format!(
            "echo run >> \"$0.runs\"\n\
             printf 'Authentication required. Please visit the URL to log in:\\n' >&2\n\
             exec /bin/sleep {parked}"
        ),
    );
    let own = own_agy(&agy, &profile);
    serve(&own, NOW - 1);
    let store = Store::new(dir.path(), &launch_key(profile.to_str(), &[]));
    current_fixture(
        &store,
        &Recorded::new(Ok(doc(0.8, 0.9, 0.9))),
        NOW,
        0.1,
        false,
    )
    .expect("a reading is kept");
    let runs = || {
        std::fs::read_to_string(dir.path().join("agy.runs")).map_or(0, |text| text.lines().count())
    };
    let why = current_fixture(&store, &own, NOW + 1, 0.1, true).expect_err("no stale answer");
    assert!(is_signed_out(&why), "{why}");
    assert_eq!(runs(), 1);
    for at in [NOW + 2, NOW + FAIL_BACKOFF_S + 1, NOW + 3_700] {
        let why = current_fixture(&store, &own, at, 0.1, true).expect_err("the gate holds");
        assert!(why.starts_with("signed-out: agy's login"), "{why}");
    }
    assert_eq!(runs(), 1, "no later read started agy");
    // CONTROL: signed in again by a turn served since, the next read runs agy.
    serve(&own, NOW + 3_800);
    let _ = current_fixture(&store, &own, NOW + 3_900, 0.1, true);
    assert_eq!(runs(), 2);
}
