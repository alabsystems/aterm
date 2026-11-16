// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::cell::Cell;

use super::*;

#[test]
fn a_borrowed_lifetime_guard_stamps_without_releasing_or_reacquiring_it() {
    let home = aterm_tempfile::tempdir().unwrap();
    let path = super::super::reserve::pass_lock_for(home.path());
    let held = super::super::reserve::take_pass(&path)
        .unwrap()
        .expect("lifetime guard");
    let dir = path.parent().unwrap();
    assert!(claim_held(dir, 1_790_000_000, &held).unwrap());
    assert!(
        !claim_held(dir, 1_790_000_001, &held).unwrap(),
        "one daily stamp"
    );
    assert!(
        super::super::reserve::take_pass(&path).unwrap().is_none(),
        "borrowed claim kept actual exclusion"
    );
    let stamp_before = read_stamp(dir, STAMP).unwrap();
    let other = aterm_tempfile::tempdir().unwrap();
    let other_path = super::super::reserve::pass_lock_for(other.path());
    let other_guard = super::super::reserve::take_pass(&other_path)
        .unwrap()
        .unwrap();
    assert!(
        claim_held(dir, 1_790_086_400, &other_guard).is_err(),
        "a real guard for another path is no authority here"
    );
    assert_eq!(read_stamp(dir, STAMP).unwrap(), stamp_before);
    std::fs::rename(&path, dir.join("old-pass.lock")).unwrap();
    let replacement = super::super::reserve::take_pass(&path).unwrap().unwrap();
    assert!(
        held.validate_for(&path).is_err(),
        "a replaced name is not the held inode"
    );
    assert!(claim_held(dir, 1_790_086_400, &held).is_err());
    assert_eq!(
        read_stamp(dir, STAMP).unwrap(),
        stamp_before,
        "replacement refused before a new stamp"
    );
    drop(replacement);
    drop(held);
}

/// AN EARLY CLAIM KEEPS AN HOUR FROM ANY SWEEP AND NEVER TAKES THE DAY'S TURN. After a daily
/// stamp, an early claim under the same guard is refused inside the hour and admitted an hour
/// on, under its own stamp: a second early claim is refused within the hour of it, and the
/// daily claim is due a day after its own stamp however often the early one ran (one an hour
/// before it included), so the holders' measure and the branch-name step keep their day. An
/// early stamp within the hour also refuses an early claim made right after a daily one ran.
/// NEGATIVE CONTROLS: an early claim that wrote the daily stamp would leave the daily claim
/// refused at a day on; one that did not read the daily stamp would be admitted inside its hour.
#[test]
fn an_early_claim_keeps_an_hour_from_any_sweep_and_never_takes_the_days_turn() {
    let home = aterm_tempfile::tempdir().unwrap();
    let path = super::super::reserve::pass_lock_for(home.path());
    let held = super::super::reserve::take_pass(&path)
        .unwrap()
        .expect("lifetime guard");
    let dir = path.parent().unwrap();
    let t0 = 1_790_000_000;
    assert!(claim_held(dir, t0, &held).unwrap());
    let daily = read_stamp(dir, STAMP).unwrap();
    assert!(
        !claim_held_early(dir, t0 + 3_599, &held).unwrap(),
        "inside the hour of the daily sweep"
    );
    assert!(claim_held_early(dir, t0 + 3_600, &held).unwrap());
    assert!(
        !claim_held_early(dir, t0 + 3_601, &held).unwrap(),
        "the early sweep stamped its own hour"
    );
    assert_eq!(
        read_stamp(dir, STAMP).unwrap(),
        daily,
        "the day's stamp stays"
    );
    let mut at = t0 + 3_600;
    while at + 3_600 < t0 + 86_400 {
        at += 3_600;
        assert!(claim_held_early(dir, at, &held).unwrap(), "hourly at {at}");
    }
    assert!(
        claim_held(dir, t0 + 86_400, &held).unwrap(),
        "the daily turn comes a day after its own stamp"
    );
    assert!(
        !claim_held_early(dir, t0 + 86_400 + 3_599, &held).unwrap(),
        "inside the hour of the daily sweep again"
    );
}

#[test]
fn daily_and_pressure_passes_exclude_each_other_on_the_same_real_lock() {
    let home = aterm_tempfile::tempdir().unwrap();
    let path = super::super::reserve::pass_lock_for(home.path());
    let pressure = super::super::reserve::take_pass(&path)
        .unwrap()
        .expect("pressure guard");
    let dir = path.parent().expect("pass directory");
    assert!(
        claim(dir, 1_790_000_000).unwrap().is_none(),
        "daily cannot overlap pressure"
    );
    assert!(
        !dir.join(STAMP).exists(),
        "a contended daily attempt writes no stamp"
    );
    drop(pressure);
    let daily = claim(dir, 1_790_000_000).unwrap().expect("daily guard");
    assert!(
        super::super::reserve::take_pass(&path).unwrap().is_none(),
        "pressure cannot overlap daily"
    );
    drop(daily);
    assert!(
        super::super::reserve::take_pass(&path).unwrap().is_some(),
        "both guards release normally"
    );
}

#[test]
fn a_long_lived_ample_watch_claims_one_cache_sweep_each_day_across_instances() {
    let root = aterm_tempfile::tempdir().unwrap();
    let cache = root.path().join("aterm");
    let dir = cache.join("disk-reserve.noindex");
    assert!(!cache.exists());
    assert!(!dir.exists());
    let first = Mutex::new(Schedule::new());
    let second = Mutex::new(Schedule::new());
    let t0 = Instant::now();
    let unix = 1_790_000_000;
    let sweeps = Cell::new(0);
    let allowed = eligible(&Config::default(), Level::Ok, true, true, false);
    let look = |schedule, at, seconds| {
        run(
            schedule,
            at,
            allowed,
            || claim(&dir, seconds),
            || {
                assert!(PrivateLock::take(&dir, "pass.lock", Duration::ZERO, "busy").is_err());
                sweeps.set(sweeps.get() + 1);
            },
        )
        .unwrap()
    };
    assert!(look(&first, t0, unix));
    private_file::check_private_dir(&cache).unwrap();
    private_file::check_private_dir(&dir).unwrap();
    assert_eq!(
        private_file::read_private(&dir, STAMP, 12).unwrap().len(),
        12
    );
    assert!(!look(
        &first,
        t0 + DAY - Duration::from_secs(1),
        unix + 86_399
    ));
    assert!(!look(&second, t0, unix));
    assert_eq!(sweeps.get(), 1);
    assert!(look(&first, t0 + DAY, unix + 86_400));
    assert!(!look(&second, t0 + DAY, unix + 86_400));
    assert!(!look(&first, t0 + DAY + RETRY, unix + 87_000));
    assert_eq!(sweeps.get(), 2);
}

#[test]
fn a_failed_or_busy_claim_never_sweeps_and_backs_off() {
    let root = aterm_tempfile::tempdir().unwrap();
    let cache = root.path().join("aterm");
    let dir = cache.join("disk-reserve.noindex");
    private_file::create_private_dir(&cache).unwrap();
    private_file::create_private_dir(&dir).unwrap();
    let schedule = Mutex::new(Schedule::new());
    let t0 = Instant::now();
    let held = PrivateLock::take(&dir, "pass.lock", Duration::ZERO, "busy").unwrap();
    assert!(
        !run(
            &schedule,
            t0,
            true,
            || claim(&dir, 1_790_000_000),
            || panic!("busy")
        )
        .unwrap()
    );
    drop(held);
    assert!(
        !run::<()>(
            &schedule,
            t0 + Duration::from_secs(1),
            true,
            || panic!("backoff"),
            || panic!("backoff")
        )
        .unwrap()
    );
    aterm_uds::atomic_file::write(&dir.join(STAMP), b"damaged", 0o600).unwrap();
    assert!(
        run(
            &schedule,
            t0 + RETRY,
            true,
            || claim(&dir, 1_790_000_600),
            || panic!("damaged stamp")
        )
        .is_err()
    );
    assert_eq!(std::fs::read(dir.join(STAMP)).unwrap(), b"damaged");
    assert!(
        !run::<()>(
            &schedule,
            t0 + RETRY + Duration::from_secs(1),
            true,
            || panic!("failed stamp backoff"),
            || panic!("failed stamp")
        )
        .unwrap()
    );
}

#[test]
fn an_owned_searchable_cache_parent_keeps_its_mode_and_gets_a_private_pass_leaf() {
    use std::os::unix::fs::PermissionsExt as _;

    let root = aterm_tempfile::tempdir().unwrap();
    let cache = root.path().join("aterm");
    private_file::create_private_dir(&cache).unwrap();
    std::fs::set_permissions(&cache, std::fs::Permissions::from_mode(0o755)).unwrap();
    let dir = cache.join("disk-reserve.noindex");
    assert!(!dir.exists());
    let held = claim(&dir, 1_790_000_000).unwrap().unwrap();
    private_file::check_private_dir(&dir).unwrap();
    assert_eq!(
        std::fs::metadata(&cache).unwrap().permissions().mode() & 0o777,
        0o755
    );
    assert_eq!(
        private_file::read_private(&dir, STAMP, 12).unwrap().len(),
        12
    );
    assert!(claim(&dir, 1_790_000_001).unwrap().is_none());
    drop(held);
    assert!(claim(&dir, 1_790_000_001).unwrap().is_none());
}

#[test]
fn existing_nonprivate_or_linked_cache_directories_are_not_changed() {
    use std::os::unix::fs::{PermissionsExt as _, symlink};

    for leaf in [false, true] {
        let root = aterm_tempfile::tempdir().unwrap();
        let cache = root.path().join("aterm");
        let dir = cache.join("disk-reserve.noindex");
        private_file::create_private_dir(&cache).unwrap();
        private_file::create_private_dir(&dir).unwrap();
        let unsafe_dir = if leaf { &dir } else { &cache };
        let mode = if leaf { 0o755 } else { 0o775 };
        std::fs::set_permissions(unsafe_dir, std::fs::Permissions::from_mode(mode)).unwrap();
        assert!(claim(&dir, 1_790_000_000).is_err());
        assert_eq!(
            std::fs::metadata(unsafe_dir).unwrap().permissions().mode() & 0o777,
            mode
        );
        assert!(!dir.join(STAMP).exists());
    }
    let root = aterm_tempfile::tempdir().unwrap();
    let target = root.path().join("other");
    private_file::create_private_dir(&target).unwrap();
    let cache = root.path().join("aterm");
    symlink(&target, &cache).unwrap();
    assert!(claim(&cache.join("disk-reserve.noindex"), 1_790_000_000).is_err());
    assert_eq!(std::fs::read_dir(&target).unwrap().count(), 0);
}

#[test]
fn cache_upkeep_leaves_opt_outs_private_unknown_and_pressure_looks_alone() {
    let config = Config::default();
    assert!(eligible(&config, Level::Ok, true, true, false));
    for off in [
        Config {
            auto_free_gib: 0,
            ..config
        },
        Config {
            dead_build_dirs: false,
            ..config
        },
    ] {
        assert!(!eligible(&off, Level::Ok, true, true, false));
    }
    for (level, owner, placed, requested) in [
        (Level::Unknown, true, true, false),
        (Level::Low, true, true, false),
        (Level::Short, true, true, false),
        (Level::Emergency, true, true, false),
        (Level::Ok, false, true, false),
        (Level::Ok, true, false, false),
        (Level::Ok, true, true, true),
    ] {
        let allowed = eligible(&config, level, owner, placed, requested);
        assert!(!allowed);
        assert!(
            !run::<()>(
                &Mutex::new(Schedule::new()),
                Instant::now(),
                allowed,
                || panic!("no claim"),
                || panic!("no sweep")
            )
            .unwrap()
        );
    }
}
