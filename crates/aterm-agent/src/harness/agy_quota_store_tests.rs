// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::cell::Cell;

use super::*;

const ASKED: crate::harness::provider_read::InUse = crate::harness::provider_read::InUse::Asked;

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
}

impl Recorded {
    fn new(doc: Result<String, String>) -> Self {
        Self {
            doc,
            reads: Cell::new(0),
            default: None,
        }
    }
}

impl Source for Recorded {
    fn read(&self) -> Result<String, String> {
        self.reads.set(self.reads.get() + 1);
        self.doc.clone()
    }

    fn default_model(&self) -> Result<String, String> {
        self.default
            .map(str::to_owned)
            .ok_or_else(|| "no default".to_owned())
    }
}

const NOW: i64 = 1_792_803_313;

#[test]
fn a_fresh_reading_is_kept_and_a_stale_one_read_again() {
    let dir = tmp("fresh");
    let store = Store::new(dir.path(), "home");
    let agy = Recorded::new(Ok(doc(0.8, 0.9, 0.9)));
    let (r, got) = current(&store, &agy, NOW, 0.1, false, ASKED).unwrap();
    assert_eq!((got, agy.reads.get(), r.as_of), (Got::Read, 1, NOW));
    let (_, got) = current(&store, &agy, NOW + FRESH_OPEN_S - 1, 0.1, false, ASKED).unwrap();
    assert_eq!(
        (got, agy.reads.get()),
        (Got::Kept, 1),
        "every pool open: 15 minutes current"
    );
    let (_, got) = current(&store, &agy, NOW + FRESH_OPEN_S, 0.1, false, ASKED).unwrap();
    assert_eq!((got, agy.reads.get()), (Got::Read, 2));
    let (_, got) = current(&store, &agy, NOW + FRESH_OPEN_S, 0.1, true, ASKED).unwrap();
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
    current(&store, &agy, NOW, 0.1, false, ASKED).unwrap();
    let (_, got) = current(&store, &agy, NOW + FRESH_TIGHT_S, 0.1, false, ASKED).unwrap();
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
    current(&low_store, &low_agy, reset_5h - 30, 0.1, false, ASKED).unwrap();
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
        current(&store, &bad, NOW, 0.1, false, ASKED).unwrap_err(),
        "`agy -p /usage` exited 1"
    );
    current(
        &store,
        &Recorded::new(Ok(doc(0.5, 0.5, 0.5))),
        NOW,
        0.1,
        true,
        ASKED,
    )
    .unwrap();
    let (r, got) = current(&store, &bad, NOW + FRESH_OPEN_S, 0.1, false, ASKED).unwrap();
    assert_eq!((got, r.as_of), (Got::Stale, NOW));
}

#[test]
fn a_failed_read_is_not_retried_until_its_backoff_passes() {
    let dir = tmp("backoff");
    let store = Store::new(dir.path(), "home");
    let bad = Recorded::new(Err("signed out".to_owned()));
    assert!(current(&store, &bad, NOW, 0.1, false, ASKED).is_err());
    let again = current(&store, &bad, NOW + 60, 0.1, false, ASKED).unwrap_err();
    assert!(
        again.contains("signed out") && again.contains("waits"),
        "{again}"
    );
    assert_eq!(bad.reads.get(), 1, "inside the backoff agy is not asked");
    let _ = current(&store, &bad, NOW + 60, 0.1, true, ASKED);
    assert_eq!(bad.reads.get(), 2, "a forced read asks anyway");
    let good = Recorded::new(Ok(doc(0.5, 0.5, 0.5)));
    assert_eq!(
        current(&store, &good, NOW + FAIL_BACKOFF_S + 60, 0.1, false, ASKED)
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
    current(
        &store,
        &Recorded::new(Ok(doc(0.5, 0.5, 0.5))),
        NOW,
        0.1,
        false,
        ASKED,
    )
    .unwrap();
    let held = store.try_lock().expect("the first lock");
    assert!(store.try_lock().is_none(), "one reader at a time");
    let agy = Recorded::new(Ok(doc(0.4, 0.4, 0.4)));
    let (_, got) = current(&store, &agy, NOW + FRESH_OPEN_S, 0.1, true, ASKED).unwrap();
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
        current(
            &store,
            &Recorded::new(Ok(doc(0.9, 0.9, *left))),
            at,
            0.1,
            true,
            ASKED,
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
    let (reading, _) = current(&spent_store, &spent, NOW, r.low, false, ASKED).unwrap();
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
    let (shown, got) = current(&spent_store, &mute, settled, r.low, false, ASKED).unwrap();
    assert_eq!((got, shown.as_of), (Got::Stale, NOW));
    assert_eq!(
        shown.quota, reading.quota,
        "the old reading remains available for display"
    );

    // When one pool's 5h bucket settles while another pool's weekly bucket is still spent for days,
    // a stale launch pick still rotates off the weekly-spent pool onto the open pool.
    let partial_store = Store::new(dir.path(), "partial-stale");
    let partial = Recorded::new(Ok(doc(0.0, 0.9, 0.0)));
    let _ = current(&partial_store, &partial, NOW, r.low, false, ASKED).unwrap();
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
    let (r, got) = current(&store, &agy, NOW, 0.1, false, ASKED).unwrap();
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

/// A running session between turns reads nothing, however old the record: it rotates on what
/// launches and walls last read. CONTROL: a launch on the same aged record does ask agy.
#[test]
fn a_running_session_rotates_on_the_record_and_asks_agy_nothing() {
    let dir = tmp("on-record");
    let store = Store::new(dir.path(), "running");
    let r = agy_quota::Rotation::default();
    let asked = Some("claude-opus-5-5-medium");
    let (none, events) =
        crate::harness::provider_read::captured(|| rotate_on_record(asked, &store, &r, NOW));
    assert_eq!(none, None, "nothing on record, and nothing asked");
    assert!(events.is_empty());
    let spent = Recorded::new(Ok(doc(0.95, 0.0, 0.0)));
    let _ = rotate_launch(asked, &store, &spent, &r, NOW);
    assert_eq!(spent.reads.get(), 1);
    let aged = NOW + FRESH_OPEN_S * 2;
    let (pick, events) =
        crate::harness::provider_read::captured(|| rotate_on_record(asked, &store, &r, aged));
    let pick = pick.expect("the record speaks");
    assert!(
        matches!(pick.pick, agy_quota::Pick::Rotate { .. }),
        "{pick:?}"
    );
    assert_eq!(pick.got, Got::Stale);
    assert!(events.is_empty(), "no read, no report");
    let (_, events) =
        crate::harness::provider_read::captured(|| rotate_launch(asked, &store, &spent, &r, aged));
    assert_eq!(spent.reads.get(), 2, "CONTROL: a launch reads");
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0].why,
        crate::harness::provider_read::InUse::Launching
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
