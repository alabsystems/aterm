// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::sync::mpsc;

#[test]
fn slots_for_cores_scales_with_budget_and_clamps_bounds() {
    assert_eq!(slots_for_cores(0), 1);
    assert_eq!(slots_for_cores(1), 1);
    assert_eq!(slots_for_cores(2), 2);
    assert_eq!(slots_for_cores(5), 2);
    assert_eq!(slots_for_cores(6), 3);
    assert_eq!(slots_for_cores(9), 4);
    assert_eq!(slots_for_cores(14), 7);
    assert_eq!(slots_for_cores(64), 8);

    assert_eq!(boot_slots_for_pool(1), 1);
    assert_eq!(boot_slots_for_pool(3), 1);
    assert_eq!(boot_slots_for_pool(4), 2);
    assert_eq!(boot_slots_for_pool(7), 3);

    assert_eq!(
        default_cores_from(Some(OsStr::new("4")), Some(OsStr::new("10")), 14),
        4
    );
    assert_eq!(
        default_cores_from(Some(OsStr::new("12")), Some(OsStr::new("6")), 14),
        6
    );
    // Negative control: widened `RUST_TEST_THREADS` (e.g. `aterm_suite` setting 4x cores)
    // never inflates the live subprocess budget above `host_cores`.
    assert_eq!(default_cores_from(Some(OsStr::new("56")), None, 14), 14);
    assert_eq!(default_cores_from(None, Some(OsStr::new("0")), 14), 14);
}

/// Pool directory admits at most `slots` concurrent exclusive holders and frees on drop.
/// NEGATIVE CONTROL: a third thread contending on a two-slot pool cannot acquire an
/// exclusive slot before its timeout while both holders keep their leases.
#[cfg(any(unix, windows))]
#[test]
fn pool_caps_concurrent_holders_and_releases_on_drop() {
    let tmp = aterm_tempfile::tempdir().expect("scratch pool dir");
    let pool = tmp.path().join("slots");
    init_pool_dir(&pool, 2).expect("init 2-slot pool");

    let (release_first, wait_first) = mpsc::channel::<()>();
    let (release_second, wait_second) = mpsc::channel::<()>();
    let (held_tx, held_rx) = mpsc::channel::<()>();

    std::thread::scope(|scope| {
        let pool_a = &pool;
        let held_tx1 = held_tx.clone();
        let h1 = scope.spawn(move || {
            let _lease = acquire_with_pool(Some(pool_a), Duration::from_secs(60))
                .expect("first exclusive lease");
            held_tx1.send(()).expect("report h1");
            wait_first
                .recv_timeout(Duration::from_secs(60))
                .expect("release h1");
        });
        let held_tx2 = held_tx;
        let h2 = scope.spawn(move || {
            let _lease = acquire_with_pool(Some(pool_a), Duration::from_secs(60))
                .expect("second exclusive lease");
            held_tx2.send(()).expect("report h2");
            wait_second
                .recv_timeout(Duration::from_secs(60))
                .expect("release h2");
        });
        held_rx
            .recv_timeout(Duration::from_secs(60))
            .expect("h1 acquired");
        held_rx
            .recv_timeout(Duration::from_secs(60))
            .expect("h2 acquired");

        // Negative control: a third thread cannot receive any usable lease while 2/2 slots
        // remain held.
        let third = scope
            .spawn(move || {
                acquire_with_pool(Some(pool_a), Duration::ZERO)
                    .err()
                    .map(|error| error.kind())
            })
            .join()
            .expect("third joined");
        assert_eq!(
            third,
            Some(io::ErrorKind::TimedOut),
            "third caller must receive no lease while 2/2 are held"
        );

        // Dropping the first holder frees its slot immediately for the next waiter.
        release_first.send(()).expect("release first holder");
        h1.join().expect("h1 finished");

        let fourth = scope
            .spawn(move || acquire_with_pool(Some(pool_a), Duration::from_secs(60)).is_ok())
            .join()
            .expect("fourth joined");
        assert!(
            fourth,
            "fourth caller must acquire the released exclusive slot"
        );

        release_second.send(()).expect("release second holder");
        h2.join().expect("h2 finished");
    });
}

/// Re-entrant calls on the same thread reuse the live lease without blocking, and dropping
/// the last clone on that thread releases the slot before the thread exits.
/// NEGATIVE CONTROL: while at least one clone remains alive on the first thread, another
/// thread still observes the single slot as exclusively held.
#[cfg(any(unix, windows))]
#[test]
fn reentrant_acquire_reuses_thread_lease_and_releases_when_last_clone_drops() {
    let tmp = aterm_tempfile::tempdir().expect("scratch pool dir");
    let pool = tmp.path().join("one-slot");
    init_pool_dir(&pool, 1).expect("init 1-slot pool");

    let first =
        acquire_with_pool(Some(&pool), Duration::from_secs(60)).expect("first exclusive lease");
    let second = acquire_with_pool(Some(&pool), Duration::ZERO).expect("reentrant exclusive lease");
    assert!(
        Arc::ptr_eq(&first, &second),
        "second boot on the same thread must share the first lease without re-locking"
    );

    drop(first);
    // `second` is still alive, so the single slot must remain locked.
    let blocked = std::thread::scope(|scope| {
        let pool_ref = &pool;
        scope
            .spawn(move || {
                acquire_with_pool(Some(pool_ref), Duration::ZERO)
                    .err()
                    .map(|error| error.kind())
            })
            .join()
            .expect("contender joined")
    });
    assert_eq!(blocked, Some(io::ErrorKind::TimedOut));

    // Dropping `second` on this still-running thread must unlock the slot immediately.
    drop(second);
    let freed = std::thread::scope(|scope| {
        let pool_ref = &pool;
        scope
            .spawn(move || acquire_with_pool(Some(pool_ref), Duration::from_secs(60)).is_ok())
            .join()
            .expect("post-drop contender joined")
    });
    assert!(
        freed,
        "slot must unlock as soon as the last lease on the thread drops"
    );
}

/// A configured shared pool is an admission authority, not a hint. Missing or malformed
/// structure refuses acquisition and never falls back to this process's private semaphore.
/// NEGATIVE CONTROL: with no shared pool configured, the local semaphore still admits a
/// caller normally.
#[cfg(any(unix, windows))]
#[test]
fn a_broken_configured_pool_refuses_instead_of_falling_back_locally() {
    let tmp = aterm_tempfile::tempdir().expect("scratch pools");
    let first_pool = tmp.path().join("first");
    let broken_pool = tmp.path().join("broken");
    init_pool_dir(&first_pool, 1).expect("first pool");
    init_pool_dir(&broken_pool, 1).expect("second pool");
    let duplicate = init_pool_dir(&broken_pool, 4).expect_err("pool init is exclusive");
    assert_eq!(duplicate.kind(), io::ErrorKind::AlreadyExists);
    assert_eq!(read_count(&broken_pool, COUNT_FILE).unwrap(), 1);

    let first =
        acquire_with_pool(Some(&first_pool), Duration::from_secs(60)).expect("first pool lease");
    let changed_pool = acquire_with_pool(Some(&broken_pool), Duration::ZERO)
        .err()
        .expect("nested admission cannot change pools");
    assert_eq!(changed_pool.kind(), io::ErrorKind::InvalidInput);
    drop(first);

    std::fs::remove_file(slot_path(&broken_pool, "slot", 0)).expect("break stack pool");
    let broken = acquire_with_pool(Some(&broken_pool), Duration::ZERO)
        .err()
        .expect("a broken configured pool must refuse");
    assert_eq!(broken.kind(), io::ErrorKind::NotFound);

    std::fs::write(broken_pool.join(COUNT_FILE), b"not-a-count\n").expect("malformed count");
    let malformed = acquire_with_pool(Some(&broken_pool), Duration::ZERO)
        .err()
        .expect("a malformed configured pool must refuse");
    assert_eq!(malformed.kind(), io::ErrorKind::InvalidData);

    std::fs::remove_file(slot_path(&broken_pool, "boot", 0)).expect("break boot pool");
    let broken_boot = acquire_boot_permit_with_pool(Some(&broken_pool), Duration::ZERO)
        .err()
        .expect("a broken configured boot pool must refuse");
    assert_eq!(broken_boot.kind(), io::ErrorKind::NotFound);

    let local = acquire_with_pool(None, Duration::from_secs(60))
        .expect("an absent shared configuration uses the local bound");
    drop(local);
    let local_boot = acquire_boot_permit_with_pool(None, Duration::from_secs(60))
        .expect("an absent shared configuration uses the local boot bound");
    drop(local_boot);
}

/// Every bounded decimal count is checked independently for stack and boot admission.
/// NEGATIVE CONTROL: a freshly initialized pool admits both kinds after the malformed
/// fixtures refuse without publishing replacement slots.
#[cfg(any(unix, windows))]
#[test]
fn configured_count_bounds_refuse_stack_and_boot_admission() {
    let tmp = aterm_tempfile::tempdir().expect("scratch pools");
    for (idx, raw) in [
        None,
        Some("0"),
        Some("65"),
        Some("bad"),
        Some("999999999999999999999999999999999999999999999999999999999999999999999"),
    ]
    .into_iter()
    .enumerate()
    {
        let pool = tmp.path().join(format!("broken-{idx}"));
        std::fs::create_dir(&pool).expect("broken pool dir");
        for name in [COUNT_FILE, BOOT_COUNT_FILE] {
            if let Some(raw) = raw {
                std::fs::write(pool.join(name), raw).expect("broken count");
            }
        }
        assert!(acquire_with_pool(Some(&pool), Duration::ZERO).is_err());
        assert!(acquire_boot_permit_with_pool(Some(&pool), Duration::ZERO).is_err());
    }

    let valid = tmp.path().join("valid");
    init_pool_dir(&valid, 1).expect("valid pool");
    assert!(acquire_with_pool(Some(&valid), Duration::from_secs(60)).is_ok());
    assert!(acquire_boot_permit_with_pool(Some(&valid), Duration::from_secs(60)).is_ok());
}

/// Stack and boot pools fail closed when saturated, and neither re-entrant lane can switch
/// from a configured shared pool to the local fallback while its lease remains live.
#[cfg(any(unix, windows))]
#[test]
fn boot_saturation_refuses_and_nested_admission_cannot_change_pools() {
    let tmp = aterm_tempfile::tempdir().expect("scratch pools");
    let pool = tmp.path().join("pool");
    init_pool_dir(&pool, 1).expect("pool");
    let held =
        acquire_boot_permit_with_pool(Some(&pool), Duration::from_secs(60)).expect("boot permit");
    let nested =
        acquire_boot_permit_with_pool(Some(&pool), Duration::ZERO).expect("same pool boot permit");
    assert!(Arc::ptr_eq(&held._inner, &nested._inner));
    assert!(acquire_boot_permit_with_pool(None, Duration::ZERO).is_err());
    let error = std::thread::scope(|scope| {
        scope
            .spawn(|| {
                acquire_boot_permit_with_pool(Some(&pool), Duration::ZERO)
                    .err()
                    .map(|error| error.kind())
            })
            .join()
            .expect("boot contender")
    });
    assert_eq!(error, Some(io::ErrorKind::TimedOut));

    let stack = acquire_with_pool(Some(&pool), Duration::from_secs(60)).expect("stack admission");
    assert!(acquire_with_pool(None, Duration::ZERO).is_err());
    assert!(acquire_with_pool(Some(&tmp.path().join("missing")), Duration::ZERO).is_err());
    drop((stack, nested, held));
    assert!(acquire_boot_permit_with_pool(Some(&pool), Duration::from_secs(60)).is_ok());
}

/// The standalone semaphore returns no lease when full, then admits immediately after drop.
#[test]
fn standalone_saturation_returns_no_lease() {
    fn pool() -> &'static LocalSemaphore {
        static POOL: OnceLock<LocalSemaphore> = OnceLock::new();
        POOL.get_or_init(|| LocalSemaphore::new(1))
    }
    let held = acquire_raw(
        PoolIdentity::Local,
        "slot",
        COUNT_FILE,
        pool(),
        Duration::ZERO,
    )
    .expect("first local lease");
    assert_eq!(
        acquire_raw(
            PoolIdentity::Local,
            "slot",
            COUNT_FILE,
            pool(),
            Duration::ZERO,
        )
        .err()
        .map(|error| error.kind()),
        Some(io::ErrorKind::TimedOut)
    );
    drop(held);
    assert!(
        acquire_raw(
            PoolIdentity::Local,
            "slot",
            COUNT_FILE,
            pool(),
            Duration::ZERO,
        )
        .is_ok()
    );
}

/// Re-entrant admission belongs to the opened directory object, not merely its pathname.
/// NEGATIVE CONTROL: replacing a pool directory at the same path refuses the old thread-local
/// lease; an alias of the unchanged directory still resolves to the same pool.
#[cfg(any(unix, windows))]
#[test]
fn reentrant_admission_tracks_the_pool_object_across_path_replacement() {
    let tmp = aterm_tempfile::tempdir().expect("scratch pools");
    let pool = tmp.path().join("pool");
    let displaced = tmp.path().join("displaced");
    init_pool_dir(&pool, 1).expect("first pool");
    let first = acquire_with_pool(Some(&pool), Duration::from_secs(60)).expect("first pool lease");
    std::fs::rename(&pool, &displaced).expect("displace first pool");
    init_pool_dir(&pool, 1).expect("replacement pool");
    let replacement = acquire_with_pool(Some(&pool), Duration::ZERO)
        .err()
        .expect("replacement cannot reuse old lease");
    assert_eq!(replacement.kind(), io::ErrorKind::InvalidInput);
    drop(first);

    let second = acquire_with_pool(Some(&pool), Duration::from_secs(60))
        .expect("replacement pool lease after old release");
    #[cfg(unix)]
    {
        let alias = tmp.path().join("alias");
        std::os::unix::fs::symlink(&pool, &alias).expect("pool alias");
        let through_alias = acquire_with_pool(Some(&alias), Duration::ZERO)
            .expect("unchanged pool alias reuses the lease");
        assert!(Arc::ptr_eq(&second, &through_alias));
    }
    drop(second);
}
