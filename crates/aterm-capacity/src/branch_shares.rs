// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Isolated qualify branches under one Release allocation.

use crate::Jobserver;
use std::io;
use std::path::Path;
use std::sync::Mutex;

/// Two isolated GNU jobservers and an in-process export reservation. A branch must be fully
/// drained before `finish` is called: tokens in its FIFO then cease to represent live work.
#[derive(Debug)]
pub struct BranchShares {
    pools: [Jobserver; 2],
    initial: [u32; 2],
    active: [bool; 2],
    qualify_allocations: Mutex<State>,
}

#[derive(Debug)]
struct State {
    allocated: [u32; 3],
    ended: [bool; 3],
    initial_reserved: bool,
    serial_build: bool,
}

/// Every token initially seeded into the build and tree FIFOs. Dropping this reservation
/// restores each token to its original FIFO; [`InitialReservation::release_to_build`] instead
/// transfers the complete non-export allocation to build without ever minting an extra token.
#[derive(Debug)]
pub struct InitialReservation<'a> {
    shares: &'a BranchShares,
    tokens: Vec<crate::Token<'a>>,
}

impl BranchShares {
    /// Partition `slots` between the active compiler branches (build 0, tree 1) and the
    /// caller-sized export (2). When both compiler branches run, tree keeps one token and build
    /// gets every other non-export token; on a one-slot host tree keeps priority so its compile
    /// can produce the index the build consumes. A sole compiler branch gets every non-export
    /// slot from the start.
    /// # Errors
    /// The allocation is invalid, no branch is active, or either FIFO cannot be created.
    pub fn new(path: &Path, slots: u32, export: u32, active: [bool; 2]) -> io::Result<Self> {
        if slots == 0 || export > slots || (!active[0] && !active[1] && export != slots) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid branch shares",
            ));
        }
        let available = slots - export;
        let initial = match active {
            [true, true] if available > 0 => [available - 1, 1],
            [true, true] => [0, 0],
            [true, false] => [available, 0],
            [false, true] => [0, available],
            [false, false] => [0, 0],
        };
        let pools = [
            Jobserver::seeded(path, slots, initial[0])?,
            Jobserver::seeded(&path.with_extension("tree.jobs"), slots, initial[1])?,
        ];
        let shares = Self {
            pools,
            initial,
            active,
            qualify_allocations: Mutex::new(State {
                allocated: [initial[0], initial[1], export],
                ended: [!active[0], !active[1], export == 0],
                initial_reserved: false,
                serial_build: false,
            }),
        };
        shares.describe(
            &shares
                .qualify_allocations
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        Ok(shares)
    }

    /// A branch's private FIFO. After qualify the tree FIFO also serves bundle checks.
    #[must_use]
    pub fn pool(&self, tree: bool) -> &Jobserver {
        &self.pools[usize::from(tree)]
    }

    /// Hold every build and tree token before either branch is handed its FIFO. This is the
    /// qualify admission barrier: memory reservations can stand before a Cargo client consumes
    /// its implicit token. The returned tokens remember their source FIFO and restore the same
    /// isolated shares when dropped.
    /// # Errors
    /// A branch already started or its newly seeded FIFO does not contain its complete share.
    pub fn reserve_initial(&self) -> io::Result<InitialReservation<'_>> {
        let mut state = self
            .qualify_allocations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.ended[0] != !self.active[0]
            || state.ended[1] != !self.active[1]
            || state.allocated[..2] != self.initial
            || state.initial_reserved
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "qualify branch shares have already changed",
            ));
        }
        let expected = usize::try_from(self.initial.iter().copied().sum::<u32>())
            .expect("qualify core grant fits usize");
        let mut held = Vec::with_capacity(expected);
        for pool in &self.pools {
            while let Some(token) = pool.try_acquire()? {
                held.push(token);
            }
        }
        if held.len() != expected {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                format!(
                    "new qualify branch FIFOs held {} of their {expected} tokens",
                    held.len()
                ),
            ));
        }
        state.initial_reserved = true;
        Ok(InitialReservation {
            shares: self,
            tokens: held,
        })
    }

    /// Hand a branch its initial dispatch width and private FIFO. Cargo's GNU clients can
    /// borrow returned allocations; multi-token dispatch never waits for another live share.
    /// # Errors
    /// The share cannot be represented.
    pub fn hand_to(&self, command: &mut aterm_uds::spawnfd::Command, tree: bool) -> io::Result<()> {
        let i = usize::from(tree);
        let state = self
            .qualify_allocations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.pools[i].hand_share_to(command, state.allocated[i].max(self.initial[i]).max(1))
    }

    /// Recover the whole allocation of a drained branch, even when its clients lost tokens.
    /// Repeated completion is inert. The last allocation goes to the post-qualify tree FIFO.
    /// # Panics
    /// `branch` is outside 0 (build), 1 (tree), 2 (export).
    pub fn finish(&self, branch: usize) {
        let mut state = self
            .qualify_allocations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert!(
            !state.initial_reserved,
            "return the qualify admission reservation before finishing a branch"
        );
        if state.ended[branch] {
            return;
        }
        state.ended[branch] = true;
        if branch == 0 {
            state.serial_build = false;
        }
        let recipient =
            if branch == 2 && state.allocated[1] == 0 && !state.ended[1] && !state.serial_build {
                1
            } else {
                (0..2).find(|&i| !state.ended[i]).unwrap_or(1)
            };
        let tokens = self.recover(&mut state, branch, recipient);
        eprintln!(
            "qualify-pool ended={branch} reminted={tokens} recipient={recipient} shares={:?} ended_shares={:?} fifos=[{},{}]",
            state.allocated,
            state.ended,
            self.pools[0].path().display(),
            self.pools[1].path().display()
        );
    }

    /// Give a waiting tree its compiler share when the serial bundle's clients have drained.
    /// The build branch remains live while it waits for the executable index tree produces.
    pub fn serial_bundle_drained(&self) {
        let mut state = self
            .qualify_allocations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.serial_build && !state.ended[1] {
            state.serial_build = false;
            let tokens = self.recover(&mut state, 0, 1);
            eprintln!(
                "qualify-pool serial_bundle_drained reminted={tokens} recipient=1 shares={:?}",
                state.allocated
            );
        }
    }

    fn recover(&self, state: &mut State, branch: usize, recipient: usize) -> u32 {
        let tokens = std::mem::take(&mut state.allocated[branch]);
        // If the last branch is tree, its surviving bytes already back the bundle allocation.
        // Drain those bytes first: its clients may have returned any subset of the allocation.
        while let Ok(Some(token)) = self
            .pools
            .get(branch)
            .map_or(Ok(None), Jobserver::try_acquire)
        {
            std::mem::forget(token);
        }
        state.allocated[recipient] += tokens;
        // Publish the wider dispatch share before a reminted byte can wake a child. The child
        // replans from this record immediately after acquiring that byte.
        self.describe(state);
        self.pools[recipient].remint(tokens);
        tokens
    }

    fn describe(&self, state: &State) {
        let record = format!(
            "coordinator_pid={} assigned_holders=[build,tree,export] shares={:?} ended={:?} initial_reserved={} serial_build={} fifos=[{},{}] unreturned_token_holders=unobservable-in-GNU-protocol",
            std::process::id(),
            state.allocated,
            state.ended,
            state.initial_reserved,
            state.serial_build,
            self.pools[0].path().display(),
            self.pools[1].path().display()
        );
        for (i, pool) in self.pools.iter().enumerate() {
            if let Err(error) = crate::book::write_staged(
                &pool.path().with_extension("slots"),
                format!("{}:{}", self.pools[0].slots(), state.allocated[i]).as_bytes(),
                "qualify-allocation-",
                4096,
            ) {
                eprintln!("qualify-pool dispatch allocation unavailable: {error}");
            }
            if let Err(error) = crate::book::write_staged(
                &pool.path().with_extension("allocation"),
                record.as_bytes(),
                "qualify-description-",
                16384,
            ) {
                eprintln!("qualify-pool allocation record unavailable: {error}; {record}");
            }
        }
        eprintln!("qualify-pool {record}");
    }
}

impl InitialReservation<'_> {
    /// Number of non-export tokens held behind the admission barrier.
    #[must_use]
    pub fn len(&self) -> usize {
        self.tokens.len()
    }

    /// Whether this barrier holds no compiler token.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tokens.is_empty()
    }

    /// Give every non-export token to build. This is the bounded serial escape when the tree's
    /// early memory claim queued behind the slice parent: no tree compiler can run until build
    /// drains and [`BranchShares::serial_bundle_drained`] or [`BranchShares::finish`] transfers
    /// the same allocation back to tree.
    /// # Errors
    /// A branch allocation changed while the caller held the admission reservation.
    pub fn release_to_build(mut self) -> io::Result<()> {
        let mut state = self
            .shares
            .qualify_allocations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !state.initial_reserved
            || self.shares.active != [true, true]
            || state.ended[..2] != [false, false]
            || state.allocated[..2] != self.shares.initial
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "qualify branch shares changed during their admission reservation",
            ));
        }
        let expected = self.shares.initial.iter().copied().sum::<u32>();
        if self.tokens.len() != usize::try_from(expected).expect("qualify grant fits usize") {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "qualify admission reservation lost a token",
            ));
        }
        for token in self.tokens.drain(..) {
            std::mem::forget(token);
        }
        state.allocated[0] = expected;
        state.allocated[1] = 0;
        state.initial_reserved = false;
        state.serial_build = true;
        // A blocked build child reads its dispatch width as soon as the first byte wakes it.
        // Publish the transferred width before making any byte runnable.
        self.shares.describe(&state);
        self.shares.pools[0].remint(expected);
        eprintln!(
            "qualify-pool serial_escape=build reminted={expected} shares={:?} fifos=[{},{}]",
            state.allocated,
            self.shares.pools[0].path().display(),
            self.shares.pools[1].path().display()
        );
        Ok(())
    }
}

impl Drop for InitialReservation<'_> {
    fn drop(&mut self) {
        let mut state = self
            .shares
            .qualify_allocations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.initial_reserved {
            // Return the bytes while the state lock still excludes `finish`; publishing the
            // barrier as gone first would let completion remint the same allocation.
            self.tokens.clear();
            state.initial_reserved = false;
            self.shares.describe(&state);
        }
    }
}

impl Drop for BranchShares {
    fn drop(&mut self) {
        for pool in &self.pools {
            let _ = std::fs::remove_file(pool.path().with_extension("allocation"));
            let _ = std::fs::remove_file(pool.path().with_extension("slots"));
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn take(pool: &Jobserver) -> Vec<crate::Token<'_>> {
        let mut held = Vec::new();
        while let Some(token) = pool.try_acquire().unwrap() {
            held.push(token);
        }
        held
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
        let shares =
            BranchShares::new(&dir.path().join("isolated.jobs"), 4, 0, [true, true]).unwrap();
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
        assert!(shares.pool(false).try_acquire().unwrap().is_none());
        assert!(shares.pool(true).try_acquire().unwrap().is_none());
        drop(reservation);
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

        let serial =
            BranchShares::new(&dir.path().join("serial.jobs"), 18, 4, [true, true]).unwrap();
        serial
            .reserve_initial()
            .unwrap()
            .release_to_build()
            .unwrap();
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
        let shares =
            BranchShares::new(&dir.path().join("misuse.jobs"), 4, 0, [true, true]).unwrap();
        let reservation = shares.reserve_initial().unwrap();
        assert!(
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| shares.finish(0))).is_err()
        );
        drop(reservation);
        assert_eq!(
            take(shares.pool(false)).len() + take(shares.pool(true)).len(),
            4
        );

        let shares =
            BranchShares::new(&dir.path().join("partial.jobs"), 4, 0, [true, true]).unwrap();
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
}
