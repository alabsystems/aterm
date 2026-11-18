// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::TokenCount;
use std::time::{Duration, Instant};

/// The largest grant a sweep covers whole. No branch of `branch_shares.rs` or `jobserver.rs`
/// turns on a slot count above it: the first split reads only whether any slot is free, the lend
/// floor is half rounded up (every lend of 0 to 3 at both parities occurs by 6 free slots), a
/// resume needs more than one build slot and a fixture loan a tree of two, reached at 3.
const SWEPT: u32 = 6;

/// The production grant, kept as each sweep's witness of a two-digit share and a large pool.
/// NOT covered: real-FIFO conservation at 7 to 17 slots, and at 18 the exports [`exports`]
/// leaves out. Each world costs the same files and renames whatever its slots, so the whole
/// range to 18 was 3,404 worlds for the arithmetic of the first 548.
const LARGE: u32 = 18;

/// Every grant from `from` to [`SWEPT`], then [`LARGE`].
fn grants(from: u32) -> impl Iterator<Item = u32> {
    (from..=SWEPT).chain([LARGE])
}

/// The export reservations judged at `slots`, none above `most`: all of them to [`SWEPT`], and
/// at [`LARGE`] the shapes a release runs (none, four) with the most the sweep allows.
fn exports(slots: u32, most: u32) -> Vec<u32> {
    if slots <= SWEPT {
        (0..=most).collect()
    } else {
        vec![0, 4, most]
    }
}

fn take(pool: &Jobserver) -> Vec<crate::Token<'_>> {
    let mut held = Vec::new();
    while let Some(token) = pool.try_acquire().unwrap() {
        held.push(token);
    }
    held
}

/// Each branch's sole adopter counts its FIFO whole after `step`: nothing minted, nothing read
/// out.
fn counted_whole(clients: &[Jobserver; 2], step: &str) {
    for (branch, client) in clients.iter().enumerate() {
        let count = client.count_alone(&|| true).unwrap();
        assert!(
            matches!(count, TokenCount::Whole { .. }),
            "after {step}, branch {branch} counted {count:?}"
        );
    }
}

fn fixture_loan(shares: &BranchShares) -> FixtureShare<'_> {
    let started = Instant::now();
    shares
        .fixture_share(&|| started.elapsed() >= Duration::from_secs(60))
        .unwrap()
}

#[test]
fn shared_tree_lending_preserves_live_tokens_and_half_the_build_allocation() {
    for slots in grants(2) {
        for export in exports(slots, slots) {
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
    for slots in grants(2) {
        for export in exports(slots, slots - 2) {
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
                // Each branch's one adopter holds that branch's live work, and counts its FIFO
                // after every step: bytes and its live tokens are always the allocation.
                let clients = [false, true]
                    .map(|tree| Jobserver::adopt(&shares.pool(tree).handed()).unwrap());
                let barrier = shares.reserve_initial().unwrap();
                for client in &clients {
                    assert!(matches!(
                        client.count_alone(&|| true).unwrap(),
                        TokenCount::Unsettled(_)
                    ));
                }
                assert!(!shares.serial_tree_waiting());
                assert!(!shares.resume_admitted_tree().unwrap());
                barrier.release_to_build().unwrap();
                counted_whole(&clients, "serial escape");
                let mut live_build = take(&clients[0]);
                assert!(shares.serial_tree_waiting());
                assert!(!shares.resume_admitted_tree().unwrap());
                // One compiler returns its byte; every other client remains live. Recovering
                // the whole branch here would duplicate those clients' outstanding tokens.
                drop(live_build.pop().unwrap());
                counted_whole(&clients, "a compiler returned its byte");
                assert!(shares.resume_admitted_tree().unwrap());
                counted_whole(&clients, "resumed");
                let tree = take(&clients[1]);
                assert_eq!(tree.len(), 1);
                assert!(take(&clients[0]).is_empty());
                assert_eq!(
                    live_build.len() + tree.len() + export as usize,
                    slots as usize
                );
                assert!(!shares.serial_tree_waiting());
                assert!(!shares.resume_admitted_tree().unwrap());
                drop(live_build);
                drop(tree);
                shares.build_compilers_drained();
                counted_whole(&clients, "compilers drained");
                assert_eq!(take(&clients[1]).len(), (slots - export) as usize);
                let fixture = fixture_loan(&shares);
                counted_whole(&clients, "fixture loan");
                // The producer takes the loan's byte and gives it back before it is reaped.
                assert_eq!(take(&clients[0]).len(), 1);
                drop(fixture);
                counted_whole(&clients, "fixture returned");
                for branch in order {
                    shares.finish(branch);
                    counted_whole(&clients, "a branch ended");
                    let state = shares.qualify_allocations.lock().unwrap();
                    assert_eq!(
                        take(&clients[0]).len()
                            + take(&clients[1]).len()
                            + state.allocated[2] as usize,
                        slots as usize
                    );
                }
                assert_eq!(take(&clients[1]).len(), slots as usize);
                let state = shares.qualify_allocations.lock().unwrap();
                assert_eq!(
                    (state.shortfalls, state.surpluses),
                    (0, 0),
                    "no reap count of conserved work is short or over"
                );
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
    for slots in grants(3) {
        for export in exports(slots, slots - 2) {
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

/// Marks the run of [`token_client`] its parent test started, and says what it does once it
/// holds its token: `hold` dies by SIGKILL holding it; `return` gives it back and exits;
/// `wait-hold` and `wait-return` first wait for its standard input to close.
const TOKEN_CLIENT: &str = "ATERM_BRANCH_TOKEN_CLIENT";

/// A jobserver client of the pool its parent handed it: it adopts the pool, takes one token,
/// says so on standard output, and then does what [`TOKEN_CLIENT`] says.
#[test]
fn token_client() {
    let Some(how) = std::env::var_os(TOKEN_CLIENT) else {
        return;
    };
    use std::io::{Read as _, Write as _};
    let pool = Jobserver::inherited().unwrap().unwrap();
    let token = pool.try_acquire().unwrap().expect("a free token");
    println!("token-client pid={} held=1", std::process::id());
    std::io::stdout().flush().unwrap();
    let how = how.to_str().unwrap();
    if how.starts_with("wait-") {
        let mut rest = Vec::new();
        let _ = std::io::stdin().read_to_end(&mut rest);
    }
    if how.ends_with("hold") {
        let _held = token;
        aterm_uds::process::kill_self_now();
    }
    drop(token);
    println!("token-client returned=1");
}

/// [`token_client`] doing `how`, handed `pool` by `hand`, with its standard input and output
/// piped.
fn token_client_command(
    how: &str,
    hand: impl FnOnce(&mut aterm_uds::spawnfd::Command),
) -> aterm_uds::spawnfd::Command {
    let mut child = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
    child
        .args([
            "--exact",
            "branch_shares::tests::token_client",
            "--nocapture",
        ])
        .env(TOKEN_CLIENT, how)
        .stdin(aterm_uds::spawnfd::Stdio::piped())
        .stdout(aterm_uds::spawnfd::Stdio::piped());
    hand(&mut child);
    child
}

/// Run `command` to its end: its pid, how it ended, and what it printed.
fn reap_token_client(
    command: &mut aterm_uds::spawnfd::Command,
) -> (u32, std::process::ExitStatus, String) {
    let child = command.spawn().unwrap();
    let pid = child.id();
    let output = child.wait_with_output().unwrap();
    (
        pid,
        output.status,
        String::from_utf8(output.stdout).unwrap(),
    )
}

/// THE NAMED CASE. A real jobserver client, handed the build FIFO by `hand_to`, takes a token
/// and dies by SIGKILL holding it. Once it is reaped and noted, build's reap point
/// (`build_compilers_drained`) counts two of build's three tokens: the record names the branch,
/// both counts and that client, the `.allocation` record carries it, and the re-mint leaves the
/// tree FIFO holding all four. A second client, handed the tree FIFO, dies the same way, and
/// tree's end counts three of four: the second shortfall's record names that client on the
/// tree FIFO alone, the FIFO is made whole again, and the first shortfall still names the
/// first. A command handed neither FIFO is noted on neither. CONTROL: clients that return their
/// token before exiting leave no record and no extra token.
#[test]
fn a_client_killed_holding_a_token_is_recorded_and_made_whole_at_its_reap() {
    use std::os::unix::process::ExitStatusExt as _;
    let program = std::env::current_exe()
        .unwrap()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    for hold in [true, false] {
        let dir = aterm_tempfile::tempdir().unwrap();
        let shares = BranchShares::new(&dir.path().join("jobs"), 4, 0, [true, true]).unwrap();
        let how = if hold { "hold" } else { "return" };
        let ended = if hold { "signal=9" } else { "exit=0" };
        let killed = |tree: bool| {
            let mut client = token_client_command(how, |cmd| {
                shares.hand_to(cmd, tree).unwrap();
            });
            let (pid, status, said) = reap_token_client(&mut client);
            assert!(said.contains("held=1"), "the client held its token: {said}");
            assert_eq!(status.signal(), hold.then_some(9));
            shares.reaped(&client, pid, Some(status));
            format!("pid={pid} program={program} ended={ended}")
        };
        let first = killed(false);
        // Handed no FIFO: noted on neither branch.
        let unhanded = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
        shares.reaped(&unhanded, 1, None);
        shares.build_compilers_drained();
        let first_shortfall = hold.then_some(Recovery {
            branch: 0,
            recipient: 1,
            assigned: 3,
            returned: Some(2),
            read_error: None,
        });
        let first_reaped = if hold { vec![first.clone()] } else { vec![] };
        {
            let state = shares.qualify_allocations.lock().unwrap();
            assert_eq!(
                (&state.first_shortfall, &state.first_shortfall_reaped),
                (&first_shortfall, &first_reaped),
                "the first shortfall names the branch, both counts and the client reaped"
            );
            assert_eq!((state.shortfalls, state.surpluses), (u32::from(hold), 0));
        }
        let allocation =
            std::fs::read_to_string(shares.pool(true).path().with_extension("allocation")).unwrap();
        assert!(
            allocation.contains(&format!("shortfalls={}", u32::from(hold))),
            "{allocation}"
        );
        let tree = take(shares.pool(true));
        assert_eq!(tree.len(), 4, "the FIFO holds its whole allocation again");
        drop(tree);

        let second = killed(true);
        shares.finish(1);
        assert_eq!(
            shares.pool(true).last_count().map(|record| record.reaped),
            hold.then(|| vec![second]),
            "the second shortfall names the client handed the tree FIFO, and only it"
        );
        let state = shares.qualify_allocations.lock().unwrap();
        assert_eq!(
            (state.shortfalls, state.surpluses),
            (2 * u32::from(hold), 0),
            "each loss is one shortfall; a whole count records nothing"
        );
        assert_eq!(
            (&state.first_shortfall, &state.first_shortfall_reaped),
            (&first_shortfall, &first_reaped),
            "a later shortfall never replaces the first"
        );
        drop(state);
        assert_eq!(take(shares.pool(true)).len(), 4, "made whole again");
    }
}

/// A token returned after its branch was counted and re-minted is a surplus: a live build
/// client holds one of build's three tokens when build's compilers are reported drained, so
/// that count is short (its adopter alive) and re-mints the whole allocation to tree; the
/// client then gives its token back to the build FIFO, and build's end reads it out as a
/// surplus, not a token to re-mint. The tree FIFO ends at its allocation, no more.
#[test]
fn a_token_returned_after_its_branch_was_reminted_is_read_out_as_a_surplus() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let shares = BranchShares::new(&dir.path().join("jobs"), 4, 0, [true, true]).unwrap();
    let client = Jobserver::adopt(&shares.pool(false).handed()).unwrap();
    let late = client.try_acquire().unwrap().expect("a build token");
    shares.build_compilers_drained();
    assert_eq!(
        shares
            .pool(false)
            .last_count()
            .map(|record| record.evidence),
        Some("the coordinator's reap point; adopters=alive".to_owned()),
        "the count says an adopter still lives"
    );
    drop(late);
    shares.finish(0);
    {
        let state = shares.qualify_allocations.lock().unwrap();
        assert_eq!((state.shortfalls, state.surpluses), (1, 1));
    }
    assert_eq!(
        shares
            .pool(false)
            .last_count()
            .map(|record| (record.outcome, record.changed)),
        Some(("over", 1))
    );
    assert!(take(shares.pool(false)).is_empty(), "the surplus is gone");
    assert_eq!(
        take(shares.pool(true)).len(),
        4,
        "tree holds its allocation"
    );
}

/// A sole adopter's own count ([`Jobserver::count_alone`]). The tree FIFO's one adopter (the
/// verifier) hands it to a child, which takes a token. While that child lives, its hold of the
/// clients lock keeps the count unjudged, recorded as unjudged, and nothing is minted; it is
/// reaped holding its token, and a count the caller says a child may still be alive for is
/// unjudged too, not recorded again, and the notes are kept. The count once it is alone finds
/// three of four, mints one, and records it as the first shortfall with every
/// client noted since; the next is whole, with the adopter's own four tokens counted as held.
/// A second loss is recorded too, but not as the first. A byte minted twice is read out as a
/// surplus. CONTROL: a child that gives its token back before it exits leaves the FIFO whole,
/// never over-full. CONTROL: while the creator holds the transfer lock with a byte it took out
/// of the FIFO to move (as `recover` drains before it re-mints), the count is unjudged and
/// mints nothing, so the FIFO ends at the allocation once the creator puts it back.
#[test]
fn a_sole_adopter_counts_its_fifo_whole_only_when_no_other_holder_lives() {
    use std::io::{BufRead as _, BufReader};
    use std::os::unix::process::ExitStatusExt as _;
    for hold in [true, false] {
        let dir = aterm_tempfile::tempdir().unwrap();
        let shares = BranchShares::new(&dir.path().join("jobs"), 4, 0, [false, true]).unwrap();
        let creator = shares.pool(true);
        let verifier = Jobserver::adopt(&creator.handed()).unwrap();
        assert_eq!(
            verifier.count_alone(&|| true).unwrap(),
            TokenCount::Whole { allocated: 4 }
        );
        verifier.note_reaped("earlier".to_owned());
        let mut command =
            token_client_command(if hold { "wait-hold" } else { "wait-return" }, |cmd| {
                verifier.hand_to(cmd);
            });
        let mut child = command.spawn().unwrap();
        let mut said = BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        while !line.contains("held=1") {
            line.clear();
            assert_ne!(
                said.read_line(&mut line).unwrap(),
                0,
                "the client never held"
            );
        }
        assert_eq!(
            verifier.count_alone(&|| true).unwrap(),
            TokenCount::Unsettled("another handle has adopted this pool"),
            "a live client's token is not a loss"
        );
        let unjudged = verifier.last_count();
        assert_eq!(
            unjudged.as_ref().map(|record| (
                record.outcome,
                record.first,
                record.evidence.as_str(),
                record.reaped.as_slice()
            )),
            Some((
                "short-unjudged",
                true,
                "another handle has adopted this pool",
                &["earlier".to_owned()][..]
            )),
            "a shortfall it cannot judge is recorded, as unjudged"
        );
        drop(child.stdin.take());
        let status = child.wait().unwrap();
        assert_eq!(status.signal(), hold.then_some(9));
        let client = format!("pid={} ended={status}", child.id());
        verifier.note_reaped(client.clone());
        assert!(
            matches!(
                verifier.count_alone(&|| false).unwrap(),
                TokenCount::Unsettled(_)
            ),
            "a count with a child perhaps alive is never judged"
        );
        assert_eq!(
            verifier.last_count(),
            unjudged,
            "an unjudged shortfall is recorded once"
        );
        verifier.note_reaped("later".to_owned());
        let count = verifier.count_alone(&|| true).unwrap();
        assert_eq!(
            count,
            if hold {
                TokenCount::Short {
                    allocated: 4,
                    found: 3,
                    reminted: 1,
                }
            } else {
                TokenCount::Whole { allocated: 4 }
            }
        );
        assert_eq!(
            verifier.last_count(),
            if hold {
                Some(crate::jobserver::CountRecord {
                    outcome: "short",
                    first: true,
                    allocated: 4,
                    in_fifo: 3,
                    held: 0,
                    changed: 1,
                    evidence: "sole client".to_owned(),
                    reaped: vec!["earlier".to_owned(), client.clone(), "later".to_owned()],
                    unlisted: 0,
                })
            } else {
                unjudged
            },
            "the first shortfall names every client noted since the last whole count; a whole \
             count records nothing"
        );
        let held = take(&verifier);
        assert_eq!(held.len(), 4, "the FIFO holds its allocation, no more");
        assert_eq!(
            verifier.count_alone(&|| true).unwrap(),
            TokenCount::Whole { allocated: 4 },
            "the counter's own tokens are held, not lost"
        );
        drop(held);

        // Another loss: a dead client's byte, read out of the FIFO and never written back.
        std::mem::forget(creator.try_acquire().unwrap().expect("a token"));
        assert_eq!(
            verifier.count_alone(&|| true).unwrap(),
            TokenCount::Short {
                allocated: 4,
                found: 3,
                reminted: 1,
            }
        );
        assert_eq!(
            verifier
                .last_count()
                .map(|record| (record.outcome, record.first)),
            Some(("short", !hold)),
            "a shortfall after the first is not the first"
        );

        // A byte minted twice.
        let moving = creator.moving();
        creator.remint(1, &moving);
        drop(moving);
        assert_eq!(
            verifier.count_alone(&|| true).unwrap(),
            TokenCount::Over {
                allocated: 4,
                found: 5,
                discarded: 1,
            },
            "a surplus is read out"
        );
        assert_eq!(
            take(&verifier).len(),
            4,
            "and the FIFO holds its allocation"
        );

        let moving = creator.moving();
        std::mem::forget(
            creator
                .try_acquire_moving(&moving)
                .unwrap()
                .expect("a token to move"),
        );
        assert_eq!(
            verifier.count_alone(&|| true).unwrap(),
            TokenCount::Unsettled(
                "the creator is moving an allocation or holds tokens outside the FIFO"
            ),
            "a transfer in flight is never judged"
        );
        creator.remint(1, &moving);
        drop(moving);
        assert_eq!(
            take(&verifier).len(),
            4,
            "the transfer's own byte, and no minted one"
        );
    }
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
        shares.qualify_allocations.lock().unwrap().last_recovery,
        Some(Recovery {
            branch: 0,
            recipient: 1,
            assigned: 3,
            returned: Some(0),
            read_error: None,
        })
    );
    let lost = shares
        .qualify_allocations
        .lock()
        .unwrap()
        .first_shortfall
        .unwrap();
    assert_eq!(lost.shortfall(), Some(3));
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
    shares.finish(1);
    {
        let state = shares.qualify_allocations.lock().unwrap();
        assert_eq!(state.last_recovery.unwrap().shortfall(), Some(0));
        assert_eq!(state.first_shortfall, Some(lost));
    }
    assert_eq!(take(tree).len(), 4);

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

#[test]
fn recovery_distinguishes_returned_tokens_from_export_reservations() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let shares = BranchShares::new(&dir.path().join("jobs"), 5, 1, [true, true]).unwrap();
    shares.finish(2);
    let export = shares
        .qualify_allocations
        .lock()
        .unwrap()
        .last_recovery
        .unwrap();
    assert_eq!(
        (export.assigned, export.returned, export.shortfall()),
        (1, None, None)
    );
    shares.finish(0);
    let build = shares
        .qualify_allocations
        .lock()
        .unwrap()
        .last_recovery
        .unwrap();
    assert_eq!(
        (build.assigned, build.returned, build.shortfall()),
        (4, Some(4), Some(0))
    );
    shares.finish(1);
    let tree = shares
        .qualify_allocations
        .lock()
        .unwrap()
        .last_recovery
        .unwrap();
    assert_eq!(
        (tree.assigned, tree.returned, tree.shortfall()),
        (5, Some(5), Some(0))
    );
    assert_eq!(take(shares.pool(true)).len(), 5);
    assert_eq!(
        shares.qualify_allocations.lock().unwrap().first_shortfall,
        None
    );
    let unread = Recovery {
        returned: Some(1),
        read_error: Some(io::ErrorKind::Other),
        ..tree
    };
    assert_eq!(unread.shortfall(), None);
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
        ["--config", "build.jobs=14"],
        "NEGATIVE CONTROL: a live Cargo must not snapshot the initial one-token allocation"
    );
    assert_eq!(
        cargo
            .get_envs()
            .find(|(name, _)| *name == "CARGO_BUILD_JOBS")
            .and_then(|(_, value)| value),
        Some(std::ffi::OsStr::new("14")),
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
    for slots in grants(1) {
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
