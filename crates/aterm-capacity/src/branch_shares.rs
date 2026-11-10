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
    qualify_allocations: Mutex<State>,
}

#[derive(Debug)]
struct State {
    allocated: [u32; 3],
    ended: [bool; 3],
}

impl BranchShares {
    /// Partition `slots` between build (0), tree (1) and export (2). Tree has priority on a
    /// one-slot host so its compile can produce the index the build consumes.
    /// # Errors
    /// The allocation is invalid or either FIFO cannot be created.
    pub fn new(path: &Path, slots: u32, export: u32) -> io::Result<Self> {
        if slots == 0 || export > slots {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid branch shares",
            ));
        }
        let available = slots - export;
        let initial = [available / 2, available - available / 2];
        let pools = [
            Jobserver::seeded(path, slots, initial[0])?,
            Jobserver::seeded(&path.with_extension("tree.jobs"), slots, initial[1])?,
        ];
        let shares = Self {
            pools,
            initial,
            qualify_allocations: Mutex::new(State {
                allocated: [initial[0], initial[1], export],
                ended: [false, false, export == 0],
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
        if state.ended[branch] {
            return;
        }
        state.ended[branch] = true;
        let tokens = std::mem::take(&mut state.allocated[branch]);
        let recipient = if branch == 2 && state.allocated[1] == 0 && !state.ended[1] {
            1
        } else {
            (0..2).find(|&i| !state.ended[i]).unwrap_or(1)
        };
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
        self.pools[recipient].remint(tokens);
        self.describe(&state);
        eprintln!(
            "qualify-pool ended={branch} reminted={tokens} recipient={recipient} shares={:?} ended_shares={:?} fifos=[{},{}]",
            state.allocated,
            state.ended,
            self.pools[0].path().display(),
            self.pools[1].path().display()
        );
    }

    fn describe(&self, state: &State) {
        let record = format!(
            "coordinator_pid={} assigned_holders=[build,tree,export] shares={:?} ended={:?} fifos=[{},{}] unreturned_token_holders=unobservable-in-GNU-protocol",
            std::process::id(),
            state.allocated,
            state.ended,
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
        std::mem::forget(held);
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
        assert!(output.status.success());
        let output = String::from_utf8(output.stdout).unwrap();
        print!("{output}");
        assert!(output.contains(&format!(
            "reads={expected} writes=0 held_on_exit={expected}"
        )));
    }

    #[test]
    fn leaked_branch_allocation_returns_and_shared_fifo_control_hangs() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let shares = BranchShares::new(&dir.path().join("isolated.jobs"), 4, 0).unwrap();
        let tree = shares.pool(true);
        let inherited_tree = Jobserver::adopt(&tree.handed_share(2).unwrap()).unwrap();
        assert_eq!(inherited_tree.slots(), 2);
        let tree_held = take(tree);
        assert_eq!(tree_held.len(), 2);
        leak(shares.pool(false), 2);
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
            .acquire_many(2, &|| started.elapsed() >= Duration::from_secs(60))
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
        let started = Instant::now();
        let missing = shared.acquire(&|| started.elapsed() >= Duration::from_secs(60));
        assert_eq!(missing.unwrap_err().kind(), io::ErrorKind::Interrupted);
        println!(
            "shared recovered=0 held_by_live_branch={} lost=2 detector_s={:.6}",
            other.len(),
            started.elapsed().as_secs_f64()
        );
    }

    #[test]
    fn every_completion_order_conserves_the_grant_including_one_slot() {
        for slots in 1..=18 {
            for export in [0, 1] {
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
                        BranchShares::new(&dir.path().join("jobs"), slots, export).unwrap();
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
    fn one_slot_runs_export_then_tree_then_build() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let shares = BranchShares::new(&dir.path().join("jobs"), 1, 1).unwrap();
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
