// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::time::{Duration, Instant};

fn take(pool: &Jobserver) -> Vec<crate::Token<'_>> {
    let mut held = Vec::new();
    while let Some(token) = pool.try_acquire().unwrap() {
        held.push(token);
    }
    held
}

fn fixture_loan(shares: &BranchShares) -> FixtureShare<'_> {
    let started = Instant::now();
    shares
        .fixture_share(&|| started.elapsed() >= Duration::from_secs(60))
        .unwrap()
}

#[test]
fn shared_tree_lending_preserves_live_tokens_and_half_the_build_allocation() {
    for slots in 2..=18 {
        for export in 0..=slots {
            for serial in [false, true] {
                for live_limit in [0, 1, u32::MAX] {
                    let dir = aterm_tempfile::tempdir().unwrap();
                    let shares =
                        BranchShares::new(&dir.path().join("jobs"), slots, export, [true, true])
                            .unwrap();
                    let barrier = shares.reserve_initial().unwrap();
                    assert!(!shares.compiler_branches_active());
                    assert_eq!(shares.lend_free_build_to_admitted_tree().unwrap(), 0);
                    if serial {
                        barrier.release_to_build().unwrap();
                    } else {
                        drop(barrier);
                    }
                    let before = shares.qualify_allocations.lock().unwrap().allocated;
                    let live_count = before[0].min(live_limit);
                    let live: Vec<_> = (0..live_count)
                        .map(|_| shares.pool(false).try_acquire().unwrap().unwrap())
                        .collect();
                    let floor = (slots - export).div_ceil(2).max(1);
                    let expected = before[0].saturating_sub(floor).min(before[0] - live_count);
                    assert_eq!(shares.lend_free_build_to_admitted_tree().unwrap(), expected);
                    let after = shares.qualify_allocations.lock().unwrap().allocated;
                    assert_eq!(after, [before[0] - expected, before[1] + expected, export]);
                    assert_eq!(
                        take(shares.pool(false)).len()
                            + take(shares.pool(true)).len()
                            + live.len()
                            + export as usize,
                        slots as usize
                    );
                    assert_eq!(shares.lend_free_build_to_admitted_tree().unwrap(), 0);
                    drop(live);
                    shares.build_compilers_drained();
                    assert!(!shares.compiler_branches_active());
                    assert_eq!(shares.lend_free_build_to_admitted_tree().unwrap(), 0);
                    for branch in [0, 1, 2] {
                        shares.finish(branch);
                    }
                    assert_eq!(take(shares.pool(true)).len(), slots as usize);
                }
            }
        }
    }
}

#[test]
fn export_returns_can_expand_a_shared_tree_loan_without_stealing_live_build_work() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let shares = BranchShares::new(&dir.path().join("jobs"), 18, 4, [true, true]).unwrap();
    let live_build = [
        shares.pool(false).try_acquire().unwrap().unwrap(),
        shares.pool(false).try_acquire().unwrap().unwrap(),
    ];
    let mut live_tree = take(shares.pool(true));
    assert_eq!(shares.lend_free_build_to_admitted_tree().unwrap(), 6);
    live_tree.extend(take(shares.pool(true)));
    assert_eq!(live_tree.len(), 7);
    shares.finish(2);
    assert_eq!(shares.lend_free_build_to_admitted_tree().unwrap(), 2);
    live_tree.extend(take(shares.pool(true)));
    assert_eq!(live_tree.len(), 9);
    assert_eq!(take(shares.pool(false)).len() + live_build.len(), 9);
    drop(live_tree);
    shares.finish(1);
    assert_eq!(take(shares.pool(false)).len() + live_build.len(), 18);
    assert_eq!(shares.lend_free_build_to_admitted_tree().unwrap(), 0);
    drop(live_build);
    shares.build_compilers_drained();
    shares.finish(0);
    assert_eq!(take(shares.pool(true)).len(), 18);
}

#[test]
fn resuming_an_admitted_tree_moves_a_free_token_without_reminting_live_build_work() {
    for slots in 2..=18 {
        for export in 0..=slots - 2 {
            for order in [
                [0, 1, 2],
                [0, 2, 1],
                [1, 0, 2],
                [1, 2, 0],
                [2, 0, 1],
                [2, 1, 0],
            ] {
                let dir = aterm_tempfile::tempdir().unwrap();
                let shares =
                    BranchShares::new(&dir.path().join("jobs"), slots, export, [true, true])
                        .unwrap();
                let barrier = shares.reserve_initial().unwrap();
                assert!(!shares.serial_tree_waiting());
                assert!(!shares.resume_admitted_tree().unwrap());
                barrier.release_to_build().unwrap();
                let mut live_build = take(shares.pool(false));
                assert!(shares.serial_tree_waiting());
                assert!(!shares.resume_admitted_tree().unwrap());
                // One compiler returns its byte; every other client remains live. Recovering
                // the whole branch here would duplicate those clients' outstanding tokens.
                drop(live_build.pop().unwrap());
                assert!(shares.resume_admitted_tree().unwrap());
                let tree = take(shares.pool(true));
                assert_eq!(tree.len(), 1);
                assert!(take(shares.pool(false)).is_empty());
                assert_eq!(
                    live_build.len() + tree.len() + export as usize,
                    slots as usize
                );
                assert!(!shares.serial_tree_waiting());
                assert!(!shares.resume_admitted_tree().unwrap());
                drop(live_build);
                drop(tree);
                shares.build_compilers_drained();
                assert_eq!(take(shares.pool(true)).len(), (slots - export) as usize);
                for branch in order {
                    shares.finish(branch);
                    let state = shares.qualify_allocations.lock().unwrap();
                    assert_eq!(
                        take(shares.pool(false)).len()
                            + take(shares.pool(true)).len()
                            + state.allocated[2] as usize,
                        slots as usize
                    );
                }
                assert_eq!(take(shares.pool(true)).len(), slots as usize);
            }
        }
    }
}

#[test]
fn a_resumed_tree_preserves_the_last_build_slot_and_normal_shares() {
    for slots in 1..=2 {
        let dir = aterm_tempfile::tempdir().unwrap();
        let export = slots - 1;
        let shares =
            BranchShares::new(&dir.path().join("jobs"), slots, export, [true, true]).unwrap();
        shares
            .reserve_initial()
            .unwrap()
            .release_to_build()
            .unwrap();
        assert_eq!(take(shares.pool(false)).len(), 1);
        assert!(!shares.resume_admitted_tree().unwrap());
        assert!(take(shares.pool(true)).is_empty());
        shares.finish(2);
        assert_eq!(shares.resume_admitted_tree().unwrap(), slots > 1);
        assert_eq!(take(shares.pool(false)).len(), 1);
        assert_eq!(take(shares.pool(true)).len(), (slots - 1) as usize);
        shares.build_compilers_drained();
        assert!(!shares.serial_tree_waiting());
        assert!(!shares.resume_admitted_tree().unwrap());
        assert_eq!(take(shares.pool(true)).len(), slots as usize);
    }

    let dir = aterm_tempfile::tempdir().unwrap();
    let shares = BranchShares::new(&dir.path().join("jobs"), 18, 4, [true, true]).unwrap();
    assert!(!shares.resume_admitted_tree().unwrap());
    assert_eq!(take(shares.pool(false)).len(), 13);
    assert_eq!(take(shares.pool(true)).len(), 1);
}

#[test]
fn a_resumed_tree_can_finish_while_build_clients_still_hold_their_tokens() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let shares = BranchShares::new(&dir.path().join("jobs"), 18, 4, [true, true]).unwrap();
    shares
        .reserve_initial()
        .unwrap()
        .release_to_build()
        .unwrap();
    let live = [
        shares.pool(false).try_acquire().unwrap().unwrap(),
        shares.pool(false).try_acquire().unwrap().unwrap(),
    ];
    assert!(shares.resume_admitted_tree().unwrap());
    shares.finish(1);
    assert_eq!(take(shares.pool(false)).len() + live.len() + 4, 18);
    assert!(!shares.serial_tree_waiting());
    shares.finish(2);
    assert_eq!(take(shares.pool(false)).len() + live.len(), 18);
    drop(live);
    shares.build_compilers_drained();
    shares.finish(0);
    assert_eq!(take(shares.pool(true)).len(), 18);
}

#[test]
fn drained_compilers_return_idle_tokens_and_fixture_loans_survive_every_end_order() {
    for slots in 3..=18 {
        for export in 0..=slots - 2 {
            for serial in [false, true] {
                for order in [[1, 2], [2, 1]] {
                    let dir = aterm_tempfile::tempdir().unwrap();
                    let shares =
                        BranchShares::new(&dir.path().join("jobs"), slots, export, [true, true])
                            .unwrap();
                    if serial {
                        shares
                            .reserve_initial()
                            .unwrap()
                            .release_to_build()
                            .unwrap();
                    }
                    // A compiler killed after consuming its tokens must not lose the grant.
                    for token in take(shares.pool(false)) {
                        std::mem::forget(token);
                    }
                    shares.build_compilers_drained();
                    shares.build_compilers_drained();
                    assert!(take(shares.pool(false)).is_empty());
                    assert_eq!(
                        take(shares.pool(true)).len(),
                        (slots - export) as usize,
                        "NEGATIVE CONTROL: retained build leaves the tree at one token while waiting for its index"
                    );
                    let fixture = fixture_loan(&shares);
                    assert!(
                        shares.reserve_initial().is_err(),
                        "a fixture loan cannot be taken back behind an admission barrier"
                    );
                    assert_eq!(take(shares.pool(true)).len(), (slots - export - 1) as usize);
                    assert_eq!(shares.qualify_allocations.lock().unwrap().allocated[0], 1);
                    // The fixture's fallback compiler may also lose its implicit token.
                    let mut fixture_tokens = take(shares.pool(false));
                    assert_eq!(fixture_tokens.len(), 1);
                    std::mem::forget(fixture_tokens.pop().unwrap());
                    for branch in order {
                        shares.finish(branch);
                        let state = shares.qualify_allocations.lock().unwrap();
                        assert_eq!(state.allocated.iter().sum::<u32>(), slots);
                        assert_eq!(
                            state.allocated[0], 1,
                            "completion cannot remint a live fixture loan"
                        );
                        assert!(take(shares.pool(false)).is_empty());
                        assert_eq!(
                            take(shares.pool(true)).len() as u32 + 1 + state.allocated[2],
                            slots
                        );
                    }
                    drop(fixture);
                    shares.finish(0);
                    assert!(take(shares.pool(false)).is_empty());
                    assert_eq!(take(shares.pool(true)).len(), slots as usize);
                }
            }
        }
    }
}

#[test]
fn a_fixture_wait_can_cancel_without_taking_a_live_tree_token() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let shares = BranchShares::new(&dir.path().join("jobs"), 4, 1, [true, true]).unwrap();
    let started = Instant::now();
    assert_eq!(
        shares
            .fixture_share(&|| started.elapsed() >= Duration::from_secs(60))
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidInput
    );
    shares.build_compilers_drained();
    let tree = take(shares.pool(true));
    assert_eq!(tree.len(), 3);
    let polls = std::cell::Cell::new(0);
    assert_eq!(
        shares
            .fixture_share(&|| {
                polls.set(polls.get() + 1);
                polls.get() > 1
            })
            .unwrap_err()
            .kind(),
        io::ErrorKind::Interrupted
    );
    assert!(take(shares.pool(false)).is_empty());
    drop(tree);
    let fixture = fixture_loan(&shares);
    assert_eq!(take(shares.pool(false)).len(), 1);
    assert_eq!(take(shares.pool(true)).len(), 2);
    drop(fixture);
    assert_eq!(take(shares.pool(true)).len(), 3);
}

#[test]
fn fixture_loans_on_tiny_grants_wait_for_export_without_minting() {
    for slots in 1..=2 {
        for export in 0..=slots {
            let dir = aterm_tempfile::tempdir().unwrap();
            let shares =
                BranchShares::new(&dir.path().join("jobs"), slots, export, [true, true]).unwrap();
            let barrier = shares.reserve_initial().unwrap();
            shares.build_compilers_drained();
            drop(barrier);
            assert!(shares.reserve_initial().is_err());
            let mut uncharged = aterm_uds::spawnfd::command("unused");
            assert!(shares.hand_to(&mut uncharged, false).is_err());
            if export == slots {
                assert!(take(shares.pool(true)).is_empty());
                shares.finish(2);
            }
            let fixture = fixture_loan(&shares);
            assert_eq!(take(shares.pool(false)).len(), 1);
            shares.finish(1);
            shares.finish(2);
            assert_eq!(take(shares.pool(true)).len(), (slots - 1) as usize);
            drop(fixture);
            shares.finish(0);
            assert_eq!(take(shares.pool(true)).len(), slots as usize);
        }
    }
}

#[test]
fn a_drained_compiler_phase_waits_for_the_admission_barrier_to_open() {
    for serial in [false, true] {
        let dir = aterm_tempfile::tempdir().unwrap();
        let shares = BranchShares::new(&dir.path().join("jobs"), 6, 1, [true, true]).unwrap();
        let barrier = shares.reserve_initial().unwrap();
        shares.build_compilers_drained();
        assert!(take(shares.pool(false)).is_empty());
        assert!(take(shares.pool(true)).is_empty());
        if serial {
            barrier.release_to_build().unwrap();
        } else {
            drop(barrier);
        }
        assert!(take(shares.pool(false)).is_empty());
        assert_eq!(take(shares.pool(true)).len(), 5);
        let fixture = fixture_loan(&shares);
        let mut cmd = aterm_uds::spawnfd::command("unused");
        shares.hand_to(&mut cmd, false).unwrap();
        let handed = cmd
            .get_envs()
            .find(|(key, _)| *key == crate::POOL_ENV)
            .unwrap()
            .1
            .unwrap();
        let client = Jobserver::adopt(handed).unwrap();
        assert_eq!(
            cmd.get_envs()
                .find(|(key, _)| *key == "CARGO_BUILD_JOBS")
                .unwrap()
                .1,
            Some(std::ffi::OsStr::new("1")),
            "direct Cargo fallback clients also spend only the fixture loan"
        );
        for name in ["MAKEFLAGS", "CARGO_MAKEFLAGS"] {
            assert_eq!(
                cmd.get_envs().find(|(key, _)| *key == name).unwrap().1,
                None,
                "a direct fixture client cannot inherit another GNU allocation"
            );
        }
        assert_eq!(
            client.slots(),
            1,
            "the fixture cannot inherit the old compiler width"
        );
        drop(fixture);
        shares.finish(0);
        shares.finish(1);
        shares.finish(2);
        assert_eq!(take(shares.pool(true)).len(), 6);
    }
}

#[test]
fn leaking_child() {
    if std::env::var_os("ATERM_LEAK_FIXTURE").is_none() {
        return;
    }
    let pool = Jobserver::inherited().unwrap().unwrap();
    let held = take(&pool);
    println!(
        "class=deliberate-leak pid={} reads={} writes=0 held_on_exit={}",
        std::process::id(),
        held.len(),
        held.len()
    );
    use std::io::Write as _;
    std::io::stdout().flush().unwrap();
    let _held = held;
    aterm_uds::process::kill_self_now();
}

fn leak(pool: &Jobserver, expected: usize) {
    let mut child = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
    child
        .args([
            "--exact",
            "branch_shares::tests::leaking_child",
            "--nocapture",
        ])
        .env("ATERM_LEAK_FIXTURE", "1");
    pool.hand_to(&mut child);
    let output = child.output().unwrap();
    use std::os::unix::process::ExitStatusExt as _;
    assert_eq!(output.status.signal(), Some(9));
    let output = String::from_utf8(output.stdout).unwrap();
    print!("{output}");
    assert!(output.contains(&format!(
        "reads={expected} writes=0 held_on_exit={expected}"
    )));
}

#[test]
fn leaked_branch_allocation_returns_but_shared_fifo_tokens_stay_lost() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let shares = BranchShares::new(&dir.path().join("isolated.jobs"), 4, 0, [true, true]).unwrap();
    let tree = shares.pool(true);
    let inherited_tree = Jobserver::adopt(&tree.handed_share(1).unwrap()).unwrap();
    assert_eq!(inherited_tree.slots(), 1);
    assert_eq!(
        inherited_tree.grant(),
        4,
        "the adopted branch retains its eventual dispatch ceiling"
    );
    let tree_held = take(tree);
    assert_eq!(tree_held.len(), 1);
    leak(shares.pool(false), 3);
    assert!(tree.try_acquire().unwrap().is_none());
    let started = Instant::now();
    shares.finish(0);
    assert_eq!(
        inherited_tree.slots(),
        4,
        "a child replans with the recovered share"
    );
    std::fs::write(tree.path().with_extension("slots"), "4:").unwrap();
    assert_eq!(
        inherited_tree.slots(),
        4,
        "a broken read cannot shrink a planned charge"
    );
    std::fs::write(tree.path().with_extension("slots"), "4:2").unwrap();
    assert_eq!(
        inherited_tree.slots(),
        4,
        "a stale read cannot shrink a planned charge"
    );
    std::fs::write(tree.path().with_extension("slots"), "8:8").unwrap();
    assert_eq!(
        tree.slots(),
        4,
        "the creator keeps its grant even beside a stale record"
    );
    let recovered = tree
        .acquire_many(3, &|| started.elapsed() >= Duration::from_secs(60))
        .unwrap();
    println!(
        "isolated recovered={} live={} wall_s={:.6}",
        recovered.len(),
        recovered.len() + tree_held.len(),
        started.elapsed().as_secs_f64()
    );
    assert!(tree.try_acquire().unwrap().is_none(), "no fifth token");
    shares.finish(0);
    assert!(
        tree.try_acquire().unwrap().is_none(),
        "duplicate completion cannot mint"
    );
    drop((recovered, tree_held));

    let shared = Jobserver::new(&dir.path().join("shared.jobs"), 4).unwrap();
    let other = shared.acquire_many(2, &|| false).unwrap();
    leak(&shared, 2);
    assert!(
        shared.try_acquire().unwrap().is_none(),
        "a shared FIFO has no token after its other branch leaked them"
    );
    let missing = shared.acquire(&|| true);
    assert_eq!(missing.unwrap_err().kind(), io::ErrorKind::Interrupted);
    println!(
        "shared recovered=0 held_by_live_branch={} lost=2 cancelled=true",
        other.len(),
    );
}

/// One live Cargo-like tree client starts with one implicit token but retains the release's
/// full dispatch ceiling. When build ends, the same client—not only a later child—can consume
/// every reminted token. NEGATIVE CONTROL: snapshotting `slots()` as the dispatch ceiling
/// before the transfer yields one and can never dispatch the thirteen newly available jobs.
#[test]
fn a_live_tree_client_keeps_the_eventual_ceiling_while_its_fifo_widens() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let shares = BranchShares::new(&dir.path().join("live.jobs"), 14, 0, [true, true]).unwrap();
    let tree = shares.pool(true);
    let client = Jobserver::adopt(&tree.handed_share(1).unwrap()).unwrap();
    assert_eq!(
        client.slots(),
        1,
        "only one tree token is initially assigned"
    );
    assert_eq!(
        client.grant(),
        14,
        "the live client's dispatch ceiling is not the one-token snapshot"
    );
    let started = Instant::now();
    let timed_out = || started.elapsed() >= Duration::from_secs(60);
    let mut cargo = aterm_uds::spawnfd::command("targo");
    let implicit = client.attach_cargo(&mut cargo, &timed_out).unwrap();
    assert_eq!(
        cargo.get_args().collect::<Vec<_>>(),
        ["-j", "14"],
        "NEGATIVE CONTROL: a live Cargo must not snapshot the initial one-token allocation"
    );
    std::thread::scope(|scope| {
        let workers: Vec<_> = (0..13)
            .map(|_| scope.spawn(|| client.acquire(&timed_out).unwrap()))
            .collect();
        shares.finish(0);
        let held: Vec<_> = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect();
        assert_eq!(client.slots(), 14, "the published allocation widened live");
        assert_eq!(client.grant(), 14, "the ceiling stayed fixed at the grant");
        assert_eq!(held.len() + 1, 14);
        assert!(
            client.try_acquire().unwrap().is_none(),
            "the live client cannot exceed the release grant"
        );
        drop(held);
    });
    drop(implicit);
}

#[test]
fn every_completion_order_conserves_the_build_first_grant_for_every_plan() {
    for slots in 1..=18 {
        for (active, export) in [
            ([true, true], 0),
            ([true, true], 1),
            ([true, false], 0),
            ([true, false], 1),
            ([false, true], 0),
            ([false, true], 1),
            ([false, false], slots),
        ] {
            for order in [
                [0, 1, 2],
                [0, 2, 1],
                [1, 0, 2],
                [1, 2, 0],
                [2, 0, 1],
                [2, 1, 0],
            ] {
                let dir = aterm_tempfile::tempdir().unwrap();
                let shares =
                    BranchShares::new(&dir.path().join("jobs"), slots, export, active).unwrap();
                let available = slots - export;
                assert_eq!(
                    shares.initial,
                    match active {
                        [true, true] if available > 0 => [available - 1, 1],
                        [true, true] => [0, 0],
                        [true, false] => [available, 0],
                        [false, true] => [0, available],
                        [false, false] => [0, 0],
                    },
                    "NEGATIVE CONTROL: every active tree keeps exactly one non-export token, \
                     build gets the remainder, and no inactive branch gets a share"
                );
                for branch in order {
                    shares.finish(branch);
                    let state = shares.qualify_allocations.lock().unwrap();
                    assert_eq!(state.allocated.iter().sum::<u32>(), slots);
                    let free = take(shares.pool(false));
                    let tree = take(shares.pool(true));
                    assert_eq!(
                        free.len() + tree.len() + state.allocated[2] as usize,
                        slots as usize
                    );
                }
                assert_eq!(take(shares.pool(true)).len(), slots as usize);
            }
        }
    }
}

#[test]
fn admission_reservation_restores_or_serializes_without_minting() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let shares = BranchShares::new(&dir.path().join("drop.jobs"), 18, 4, [true, true]).unwrap();
    let reservation = shares.reserve_initial().unwrap();
    assert_eq!(reservation.len(), 14);
    assert!(
        std::fs::read_to_string(shares.pool(false).path().with_extension("allocation"))
            .unwrap()
            .contains("initial_reserved=true"),
        "NEGATIVE CONTROL: resource-host records name the live token holder"
    );
    assert!(shares.pool(false).try_acquire().unwrap().is_none());
    assert!(shares.pool(true).try_acquire().unwrap().is_none());
    drop(reservation);
    assert!(
        std::fs::read_to_string(shares.pool(false).path().with_extension("allocation"))
            .unwrap()
            .contains("initial_reserved=false")
    );
    let build = take(shares.pool(false));
    let tree = take(shares.pool(true));
    assert_eq!(
        (build.len(), tree.len()),
        (13, 1),
        "NEGATIVE CONTROL: ordinary release preserves build's critical-path width and \
         tree's one-token concurrent progress"
    );
    assert!(shares.pool(false).try_acquire().unwrap().is_none());
    assert!(shares.pool(true).try_acquire().unwrap().is_none());
    drop((build, tree));

    let serial = BranchShares::new(&dir.path().join("serial.jobs"), 18, 4, [true, true]).unwrap();
    serial
        .reserve_initial()
        .unwrap()
        .release_to_build()
        .unwrap();
    let allocation =
        std::fs::read_to_string(serial.pool(false).path().with_extension("allocation")).unwrap();
    assert!(allocation.contains("initial_reserved=false"));
    assert!(allocation.contains("serial_build=true"));
    assert_eq!(
        std::fs::read_to_string(serial.pool(false).path().with_extension("slots")).unwrap(),
        "18:14"
    );
    assert_eq!(
        std::fs::read_to_string(serial.pool(true).path().with_extension("slots")).unwrap(),
        "18:0"
    );
    let build = take(serial.pool(false));
    assert_eq!(build.len(), 14);
    assert!(serial.pool(true).try_acquire().unwrap().is_none());
    drop(build);

    // Export completion cannot wake the tree while its memory claim is queued.
    serial.finish(2);
    assert_eq!(take(serial.pool(false)).len(), 18);
    assert!(serial.pool(true).try_acquire().unwrap().is_none());
    serial.finish(0);
    assert_eq!(take(serial.pool(true)).len(), 18);
    assert!(serial.pool(false).try_acquire().unwrap().is_none());
}

#[test]
fn reservation_misuse_and_partial_acquisition_do_not_duplicate_tokens() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let shares = BranchShares::new(&dir.path().join("misuse.jobs"), 4, 0, [true, true]).unwrap();
    let reservation = shares.reserve_initial().unwrap();
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| shares.finish(0))).is_err());
    drop(reservation);
    assert_eq!(
        take(shares.pool(false)).len() + take(shares.pool(true)).len(),
        4
    );

    let shares = BranchShares::new(&dir.path().join("partial.jobs"), 4, 0, [true, true]).unwrap();
    let stolen = shares.pool(false).try_acquire().unwrap().unwrap();
    assert_eq!(
        shares.reserve_initial().unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
    drop(stolen);
    assert_eq!(
        take(shares.pool(false)).len() + take(shares.pool(true)).len(),
        4
    );
}

#[test]
fn one_slot_runs_export_then_tree_then_build() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let shares = BranchShares::new(&dir.path().join("jobs"), 1, 1, [true, true]).unwrap();
    assert!(shares.pool(false).try_acquire().unwrap().is_none());
    assert!(shares.pool(true).try_acquire().unwrap().is_none());
    shares.finish(2);
    let tree_work = shares.pool(true).try_acquire().unwrap().unwrap();
    assert!(shares.pool(false).try_acquire().unwrap().is_none());
    drop(tree_work);
    shares.finish(1);
    let build_work = shares.pool(false).try_acquire().unwrap().unwrap();
    assert!(shares.pool(true).try_acquire().unwrap().is_none());
    drop(build_work);
    shares.finish(0);
    assert_eq!(take(shares.pool(true)).len(), 1);
}

/// Diagnostic replica, kept out of the ordinary suite because it starts real compilers.
#[test]
#[ignore = "manual two-Cargo token-balance experiment"]
fn two_cargos_and_a_tree_reservation_report_exit_balances() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let tree = Jobserver::new(&dir.path().join("tree.jobs"), 1).unwrap();
    let _tree_work = tree.acquire(&|| false).unwrap();
    let pools = [
        Jobserver::new(&dir.path().join("native.jobs"), 3).unwrap(),
        Jobserver::new(&dir.path().join("compat.jobs"), 3).unwrap(),
    ];
    let mut commands = Vec::new();
    let mut implicit = Vec::new();
    for (i, pool) in pools.iter().enumerate() {
        let root = dir.path().join(format!("slice{i}.noindex"));
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("Cargo.toml"), format!("[workspace]\n[package]\nname = \"slice{i}\"\nversion = \"0.0.0\"\nedition = \"2024\"\n[profile.dev]\ncodegen-units = 16\n")).unwrap();
        std::fs::write(root.join("src/lib.rs"), "#[inline(never)] pub fn work(mut x: u64) -> u64 { for i in 0..10000 { x = x.wrapping_mul(31).wrapping_add(i); } x }").unwrap();
        let mut command = aterm_uds::spawnfd::command("targo");
        command
            .args(["--unverified", "build", "--offline"])
            .current_dir(&root);
        implicit.push(pool.attach_cargo(&mut command, &|| false).unwrap());
        commands.push(command);
    }
    let mut refs: Vec<_> = commands.iter_mut().collect();
    let outputs = aterm_uds::deadline::outputs_until(
        &mut refs,
        Instant::now() + Duration::from_secs(60),
        Duration::from_secs(60),
    );
    drop(implicit);
    for (i, (pool, output)) in pools.iter().zip(outputs).enumerate() {
        let output = output.unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let returned = take(pool);
        println!(
            "class=Cargo-plus-rustc slice={i} implicit_reads=1 implicit_writes=1 free_on_exit={} net_lost={} internal_reads_writes=not-traced",
            returned.len(),
            3 - returned.len()
        );
        assert_eq!(returned.len(), 3);
    }
}
