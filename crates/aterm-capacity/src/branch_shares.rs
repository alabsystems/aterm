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
    build_phase: BuildPhase,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BuildPhase {
    Compilers,
    Drained,
    Fixture,
}

impl BuildPhase {
    fn compilers_drained(self) -> bool {
        self != Self::Compilers
    }
}

/// One tree token lent to the fixture producer's build FIFO. Drop only after every producer
/// child has been reaped, including a fallback compiler. The returned token belongs to tree.
#[derive(Debug)]
#[must_use = "retain the fixture share until every producer child has been reaped"]
pub struct FixtureShare<'a> {
    shares: &'a BranchShares,
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
                build_phase: BuildPhase::Compilers,
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
            || state.build_phase != BuildPhase::Compilers
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
        self.describe(&state);
        Ok(InitialReservation {
            shares: self,
            tokens: held,
        })
    }

    /// Hand a branch its initial dispatch width and private FIFO. Cargo's GNU clients can
    /// borrow returned allocations; multi-token dispatch never waits for another live share.
    /// # Errors
    /// The share cannot be represented, or drained build compilers have no live fixture loan.
    pub fn hand_to(&self, command: &mut aterm_uds::spawnfd::Command, tree: bool) -> io::Result<()> {
        let i = usize::from(tree);
        let state = self
            .qualify_allocations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if i == 0 && state.build_phase == BuildPhase::Drained {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a fixture child requires its live CPU loan",
            ));
        }
        let width = if i == 0 && state.build_phase.compilers_drained() {
            state.allocated[i].max(1)
        } else {
            state.allocated[i].max(self.initial[i]).max(1)
        };
        if i == 0 && state.build_phase.compilers_drained() {
            // Fixture children are serial clients of the loan, including direct Cargo
            // fallbacks which do not adopt POOL_ENV. Their implicit job spends this loan;
            // it must not borrow more jobs through inherited GNU authentication.
            command
                .env("CARGO_BUILD_JOBS", width.to_string())
                .env_remove("MAKEFLAGS")
                .env_remove("CARGO_MAKEFLAGS");
        }
        self.pools[i].hand_share_to(command, width)
    }

    /// Recover the whole allocation of a drained branch, even when its clients lost tokens.
    /// Repeated completion is inert. The last allocation goes to the post-qualify tree FIFO.
    /// # Panics
    /// `branch` is outside 0 (build), 1 (tree), 2 (export), the admission reservation still
    /// stands, or build finishes before its fixture share has been returned.
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
        assert!(
            branch != 0 || state.build_phase != BuildPhase::Fixture,
            "reap the fixture producer before finishing build"
        );
        state.ended[branch] = true;
        if branch == 0 {
            state.serial_build = false;
        }
        let recipient = if state.build_phase.compilers_drained()
            || (branch == 2 && state.allocated[1] == 0 && !state.ended[1] && !state.serial_build)
        {
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

    /// Return the drained bundle compiler's entire allocation while its branch waits for the
    /// tree's executable index. A fixture producer borrows one token only when ready to run.
    /// Call only after every bundle compiler client has been reaped.
    /// Repeated notification is inert; an admission barrier defers the return until it opens.
    pub fn build_compilers_drained(&self) {
        let mut state = self
            .qualify_allocations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.build_phase.compilers_drained() || state.ended[0] {
            return;
        }
        state.build_phase = BuildPhase::Drained;
        if state.initial_reserved {
            self.describe(&state);
        } else {
            state.serial_build = false;
            self.recover(&mut state, 0, 1);
        }
    }

    /// Borrow exactly one currently free tree token for the fixture producer. No token is
    /// reserved while waiting for the executable index, and no live tree client loses a token.
    /// The caller must reap every fixture child before dropping the returned share.
    /// # Errors
    /// The compiler phase has not drained, the admission barrier still stands, build ended,
    /// the FIFO cannot be read, a free token has no recorded allocation, or `stop` fires.
    pub fn fixture_share(&self, stop: &dyn Fn() -> bool) -> io::Result<FixtureShare<'_>> {
        loop {
            if stop() {
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "fixture core wait stopped",
                ));
            }
            {
                let mut state = self
                    .qualify_allocations
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if !state.build_phase.compilers_drained()
                    || state.initial_reserved
                    || state.ended[0]
                {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "fixture share requires drained build compilers and an open barrier",
                    ));
                }
                if state.build_phase == BuildPhase::Drained
                    && let Some(token) = self.pools[1].try_acquire()?
                {
                    let Some(remaining) = state.allocated[1].checked_sub(1) else {
                        return Err(io::Error::other("free tree token has no allocation"));
                    };
                    std::mem::forget(token);
                    state.allocated[1] = remaining;
                    state.allocated[0] += 1;
                    state.build_phase = BuildPhase::Fixture;
                    self.describe(&state);
                    self.pools[0].remint(1);
                    return Ok(FixtureShare { shares: self });
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
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
            "coordinator_pid={} assigned_holders=[build,tree,export] shares={:?} ended={:?} initial_reserved={} serial_build={} build_phase={:?} fifos=[{},{}] unreturned_token_holders=unobservable-in-GNU-protocol",
            std::process::id(),
            state.allocated,
            state.ended,
            state.initial_reserved,
            state.serial_build,
            state.build_phase,
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
    /// the same allocation back to tree. If compiler completion was reported while the barrier
    /// stood, its allocation goes directly to tree when the barrier opens.
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
        let drained = state.build_phase.compilers_drained();
        state.allocated[0] = if drained { 0 } else { expected };
        state.allocated[1] = if drained { expected } else { 0 };
        state.initial_reserved = false;
        state.serial_build = !drained;
        // A blocked build child reads its dispatch width as soon as the first byte wakes it.
        // Publish the transferred width before making any byte runnable.
        self.shares.describe(&state);
        let recipient = usize::from(drained);
        self.shares.pools[recipient].remint(expected);
        eprintln!(
            "qualify-pool serial_escape recipient={recipient} reminted={expected} shares={:?} fifos=[{},{}]",
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
            if state.build_phase.compilers_drained() {
                state.serial_build = false;
                self.shares.recover(&mut state, 0, 1);
            } else {
                self.shares.describe(&state);
            }
        }
    }
}

impl Drop for FixtureShare<'_> {
    fn drop(&mut self) {
        let mut state = self
            .shares
            .qualify_allocations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(state.build_phase, BuildPhase::Fixture);
        state.build_phase = BuildPhase::Drained;
        self.shares.recover(&mut state, 0, 1);
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
#[path = "branch_shares_tests.rs"]
mod tests;
