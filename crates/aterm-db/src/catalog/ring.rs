// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! RINGS OF PERIOD TABLES (docs/DESIGN-fluid-swarm-2026-10-28.md F19). A table whose rows all
//! lapse within one period rides a ring of tables, one per period (a UTC day for account claims),
//! each named by its ring and its period's number, outside the catalog: nothing is declared, so no
//! catalog record or creator slot is spent per period. Each Mac's window is the period before the
//! current one, the current one and the next; it takes up a window table when it writes one or a
//! peer offers it, so a ring nobody writes costs no repair turn, and deletes older tables itself,
//! under the control writer. It raises its floor first and never enrolls a period below it, so
//! neither a clock set back nor a peer's repair brings a deleted table back. A writer puts each
//! row in a table at least as late as the period before the one its row lapses in, so every row
//! of a deleted table has lapsed on the clock that deleted it.
//!
//! A damaged or unreadable state stops nothing: a reader places the window by the clock alone,
//! and the next writer pass rebuilds the state from the storage this Mac holds.

use super::*;
use crate::table::store::{claim_dir, claim_name};
use aterm_uds::private_file::{self, PrivateDir, PrivateFileError};
use std::ops::RangeInclusive;

/// One ring of period tables: its name, which no catalog descriptor can produce, and its period.
pub(crate) struct Ring {
    pub application: &'static str,
    pub role: &'static str,
    pub period_s: u64,
}

/// The account claims' ring (`crate::accounts::claims`): one table per UTC day.
pub(crate) const CLAIMS: Ring = Ring {
    application: "accounts",
    role: "claims",
    period_s: crate::accounts::claims::EPOCH_S,
};

/// Every ring the replica subscribes to.
const RINGS: [&Ring; 1] = [&CLAIMS];

const DOMAIN: &[u8] = b"aterm/commons/ring-table/1\0";
const STATE_MAGIC: &[u8; 5] = b"ATRS1";
/// The most periods one Mac holds of a ring: its window and the one it is about to delete need
/// four; the rest is slack for a clock that moved while a pass was interrupted.
const MAX_HELD: usize = 8;
/// How many periods either side of the clock's a rebuilt state looks through for this Mac's
/// storage; a Mac whose state is lost after a longer absence leaves its older tables' storage.
const REBUILD_SPAN: u64 = 400;
const STATE_MAX: usize = STATE_MAGIC.len() + 8 + 1 + MAX_HELD * 8;

impl Ring {
    /// The table of period `epoch`: a deterministic identity in its own domain, at generation 1.
    pub(crate) fn scope(&self, trust: Trust, epoch: u64) -> Result<Scope> {
        let mut bytes = DOMAIN.to_vec();
        bytes.extend_from_slice(&trust.network);
        bytes.extend_from_slice(&trust.authority);
        bytes.extend_from_slice(&trust.genesis);
        for word in [self.application, self.role] {
            bytes.extend_from_slice(&(word.len() as u64).to_le_bytes());
            bytes.extend_from_slice(word.as_bytes());
        }
        bytes.extend_from_slice(&epoch.to_le_bytes());
        let hash = aterm_digest::Sha256::digest(&bytes);
        let mut table: [u8; 16] = hash[..16].try_into().expect("fixed digest width");
        table[0] |= 0x80;
        Scope::new(trust, table, 1)
    }

    /// The period `secs` (seconds since the Unix epoch) falls in.
    pub(crate) const fn epoch_of(&self, secs: u64) -> u64 {
        secs / self.period_s
    }

    pub(crate) fn state_name(&self) -> String {
        format!("ring-{}-{}.state", self.application, self.role)
    }
}

/// What one Mac keeps of a ring: the lowest period it may still enroll (`floor`) and every period
/// whose storage or enrollment claim it may hold (`held`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct RingState {
    pub floor: u64,
    pub held: BTreeSet<u64>,
}

impl RingState {
    /// The current period: the clock's, but never below the one after the floor, so a clock set
    /// back keeps the window at or above the floor.
    pub(crate) fn current(&self, ring: &Ring, now: u64) -> u64 {
        ring.epoch_of(now).max(self.floor.saturating_add(1))
    }

    /// The periods this Mac subscribes to and may enroll: the one before the current, the current
    /// and the next.
    pub(crate) fn window(&self, ring: &Ring, now: u64) -> RangeInclusive<u64> {
        let current = self.current(ring, now);
        current.saturating_sub(1)..=current.saturating_add(1)
    }

    /// Raise the floor to the window's first period and name the held periods below it, which
    /// stay held until their storage is gone.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbClaimRing",
            action = "Retire",
            project = "aterm_db::accounts::claims::tests::ring_transition"
        )
    )]
    pub(crate) fn ratchet(&mut self, ring: &Ring, now: u64) -> Vec<u64> {
        self.floor = *self.window(ring, now).start();
        self.held.range(..self.floor).copied().collect()
    }

    /// Hold `epoch`, which must lie in the window. Whether it was newly held.
    ///
    /// # Errors
    /// `epoch` lies outside the window, or this Mac would hold more than [`MAX_HELD`] periods.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbClaimRing",
            action = "Enroll",
            project = "aterm_db::accounts::claims::tests::ring_transition"
        )
    )]
    pub(crate) fn hold(&mut self, ring: &Ring, epoch: u64, now: u64) -> Result<bool> {
        if !self.window(ring, now).contains(&epoch) {
            return Err(Error::new("ring table period is outside this Mac's window"));
        }
        if self.held.contains(&epoch) {
            return Ok(false);
        }
        if self.held.len() >= MAX_HELD {
            return Err(Error::new("ring table holds too many periods"));
        }
        Ok(self.held.insert(epoch))
    }

    fn encode(&self) -> Vec<u8> {
        let mut bytes = STATE_MAGIC.to_vec();
        bytes.extend_from_slice(&self.floor.to_le_bytes());
        bytes.push(u8::try_from(self.held.len()).expect("held periods are bounded"));
        for epoch in &self.held {
            bytes.extend_from_slice(&epoch.to_le_bytes());
        }
        bytes
    }

    fn decode(bytes: &[u8]) -> Result<Self> {
        let damaged = || Error::new("ring table state is damaged");
        let rest = bytes.strip_prefix(STATE_MAGIC).ok_or_else(damaged)?;
        let (floor, rest) = rest.split_first_chunk::<8>().ok_or_else(damaged)?;
        let (&[count], rest) = rest.split_first_chunk::<1>().ok_or_else(damaged)?;
        if usize::from(count) > MAX_HELD || rest.len() != usize::from(count) * 8 {
            return Err(damaged());
        }
        let (chunks, _) = rest.as_chunks::<8>();
        let held: BTreeSet<u64> = chunks
            .iter()
            .map(|chunk| u64::from_le_bytes(*chunk))
            .collect();
        let state = Self {
            floor: u64::from_le_bytes(*floor),
            held,
        };
        // One spelling per state: sorted, distinct periods.
        if state.encode() != bytes {
            return Err(damaged());
        }
        Ok(state)
    }
}

#[cfg(test)]
thread_local! {
    /// Fail the next retirement once its tables are deleted, before it forgets them.
    pub(crate) static FAIL_AFTER_DELETE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

impl Directory {
    /// What this Mac keeps of `ring`; none yet reads as an empty state with no floor.
    ///
    /// # Errors
    /// Its state is unreadable or damaged.
    pub(crate) fn ring_state(&self, session: &Session<'_>, ring: &Ring) -> Result<RingState> {
        match private_file::read_private(&claim_dir(session)?, &ring.state_name(), STATE_MAX) {
            Ok(bytes) => RingState::decode(&bytes),
            Err(PrivateFileError::Missing) => Ok(RingState::default()),
            Err(_) => Err(Error::new("ring table state is unreadable")),
        }
    }

    /// What this Mac keeps of `ring`, for a reader: a damaged or unreadable state reads as none
    /// yet, so the window is the clock's alone, until the next writer pass rebuilds it
    /// (`retire`). A reader enrolls and deletes nothing, and every writer pass places the window
    /// by the state it holds or rebuilds.
    pub(crate) fn ring_view(&self, session: &Session<'_>, ring: &Ring) -> RingState {
        self.ring_state(session, ring).unwrap_or_default()
    }

    /// A state for `ring` in place of a damaged or unreadable one, from this Mac's storage: no
    /// floor yet (the retirement that follows raises it to the clock's window), holding every
    /// period within [`REBUILD_SPAN`] of the clock's whose table storage or enrollment claim
    /// exists, the earliest [`MAX_HELD`] where more do. The floor a clock set back relied on is
    /// lost with the state; the rebuilt one is the clock's, which a Mac new to the ring starts
    /// from, and every row of a table below it has lapsed on this clock.
    ///
    /// # Errors
    /// No Commons custody, or storage that cannot be examined.
    fn rebuild_ring_state(
        &self,
        session: &Session<'_>,
        ring: &Ring,
        now: u64,
    ) -> Result<RingState> {
        let claims = claim_dir(session)?;
        let clock = ring.epoch_of(now);
        let mut held = BTreeSet::new();
        for epoch in clock.saturating_sub(REBUILD_SPAN)..=clock.saturating_add(REBUILD_SPAN) {
            if held.len() >= MAX_HELD {
                break;
            }
            let scope = ring.scope(self.trust, epoch)?;
            if self.path(scope)?.try_exists()? || claims.join(claim_name(scope)).try_exists()? {
                held.insert(epoch);
            }
        }
        Ok(RingState { floor: 0, held })
    }

    fn save_ring_state(&self, session: &Session<'_>, ring: &Ring, state: &RingState) -> Result<()> {
        let dir = claim_dir(session)?;
        aterm_journal::create_private_dir_durable(&dir)?;
        aterm_journal::write_private_durable(&dir, &ring.state_name(), &state.encode())?;
        Ok(())
    }

    /// The catalog's storage comes before any ring table's: a ring directory beside no catalog
    /// would read as damaged custody (`observe_with_config`).
    fn ensure_catalog(&self, session: &Session<'_>) -> Result<()> {
        let scope = self.catalog_scope();
        if !claim_dir(session)?.join(claim_name(scope)).try_exists()? {
            self.catalog(session)?;
        }
        Ok(())
    }

    /// Delete every table of `ring` older than this Mac's window, under the control writer: raise
    /// the floor and write it durably, delete each held period below it (its storage directory and
    /// its enrollment claim), then forget them. A pass cut short anywhere is finished by the next.
    /// A damaged or unreadable state is rebuilt first (`rebuild_ring_state`) and written.
    ///
    /// # Errors
    /// No control writer, a state that could be neither read nor rebuilt and written, or storage
    /// that could not be deleted.
    pub(crate) fn retire(&self, session: &Session<'_>, ring: &Ring, now: u64) -> Result<RingState> {
        if !session.has_control_writer() {
            return Err(Error::new(
                "ring table retirement requires a locked control observation",
            ));
        }
        let (mut state, rebuilt) = match self.ring_state(session, ring) {
            Ok(state) => (state, false),
            Err(_) => (self.rebuild_ring_state(session, ring, now)?, true),
        };
        let floor = state.floor;
        let doomed = state.ratchet(ring, now);
        if rebuilt || state.floor != floor {
            self.save_ring_state(session, ring, &state)?;
        }
        if doomed.is_empty() {
            return Ok(state);
        }
        let claims = claim_dir(session)?;
        for epoch in &doomed {
            let scope = ring.scope(self.trust, *epoch)?;
            if scope == self.catalog_scope() {
                return Err(Error::new("a ring table cannot name the catalog"));
            }
            let path = self.path(scope)?;
            match private_file::judge_private_dir(&path)? {
                PrivateDir::Missing => {}
                PrivateDir::Private => std::fs::remove_dir_all(&path)?,
                PrivateDir::Open(_) => {
                    return Err(Error::new("ring table storage is not private"));
                }
            }
            match std::fs::remove_file(claims.join(claim_name(scope))) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        for dir in [&self.root, &claims] {
            if dir.try_exists()? {
                aterm_durable::durable_dir(dir)?;
            }
        }
        #[cfg(test)]
        if FAIL_AFTER_DELETE.with(|fail| fail.replace(false)) {
            return Err(Error::new(
                "injected failure after a ring retirement deleted",
            ));
        }
        let floor = state.floor;
        state.held.retain(|epoch| *epoch >= floor);
        self.save_ring_state(session, ring, &state)?;
        Ok(state)
    }

    /// Enroll `ring`'s table of period `epoch` for a write, under the control writer: retire
    /// first (the caller's job), then hold the period durably before its storage exists.
    ///
    /// # Errors
    /// `epoch` lies outside this Mac's window, no control writer or storage slot, or damaged
    /// storage.
    pub(crate) fn enroll_ring(
        &self,
        session: &Session<'_>,
        ring: &Ring,
        epoch: u64,
        now: u64,
    ) -> Result<Store> {
        session.writer()?;
        self.ensure_catalog(session)?;
        let mut state = self.ring_state(session, ring)?;
        if state.hold(ring, epoch, now)? {
            self.save_ring_state(session, ring, &state)?;
        }
        self.enroll(session, ring.scope(self.trust, epoch)?)
    }

    /// Retire old ring tables and name the window's tables this Mac holds storage for, under the
    /// control writer, honouring local policy. Nothing is reserved here: a Mac takes up a ring
    /// table when it writes one (`enroll_ring`) or a peer offers it (`adopt_ring_tables`), so a
    /// ring nobody writes costs no repair turn.
    ///
    /// # Errors
    /// No control writer, an unreadable ring state, or storage that could not be deleted.
    pub(crate) fn ring_subscriptions(
        &self,
        session: &Session<'_>,
        policy: &Policy,
        now: u64,
    ) -> Result<BTreeMap<Digest, (Scope, PathBuf)>> {
        let mut tables = BTreeMap::new();
        if !policy.enabled || policy.excluded.contains(&self.catalog_scope().digest()) {
            return Ok(tables);
        }
        for ring in RINGS {
            let mut state = self.retire(session, ring, now)?;
            let mut changed = false;
            for epoch in state.window(ring, now) {
                let scope = ring.scope(self.trust, epoch)?;
                let path = self.path(scope)?;
                if policy.excluded.contains(&scope.digest()) || !path.try_exists()? {
                    continue;
                }
                changed |= state.hold(ring, epoch, now)?;
                tables.insert(scope.digest(), (scope, path));
            }
            if changed {
                self.save_ring_state(session, ring, &state)?;
            }
        }
        Ok(tables)
    }

    /// Reserve each table of some ring's window that a peer offers (`offered`) and this Mac has no
    /// storage for, under the control writer, honouring local policy: hold it durably, then
    /// reserve its storage, so the next repair round can carry its rows. A table with no storage
    /// slot left is skipped, not an error.
    ///
    /// # Errors
    /// No control writer, an unreadable ring state, or storage that could not be created or
    /// deleted.
    pub(crate) fn adopt_ring_tables(
        &self,
        session: &Session<'_>,
        policy: &Policy,
        now: u64,
        offered: &BTreeSet<Digest>,
    ) -> Result<BTreeMap<Digest, (Scope, PathBuf)>> {
        let mut tables = BTreeMap::new();
        if !policy.enabled || policy.excluded.contains(&self.catalog_scope().digest()) {
            return Ok(tables);
        }
        for ring in RINGS {
            let mut state = self.retire(session, ring, now)?;
            let mut wanted = Vec::new();
            for epoch in state.window(ring, now) {
                let scope = ring.scope(self.trust, epoch)?;
                let path = self.path(scope)?;
                if offered.contains(&scope.digest())
                    && !policy.excluded.contains(&scope.digest())
                    && !path.try_exists()?
                {
                    wanted.push((epoch, scope, path));
                }
            }
            if wanted.is_empty() {
                continue;
            }
            self.ensure_catalog(session)?;
            let mut count = std::fs::read_dir(&self.root)?.count();
            wanted.retain(|_| {
                count += 1;
                count <= MAX_TABLES + 1
            });
            for (epoch, _, _) in &wanted {
                state.hold(ring, *epoch, now)?;
            }
            self.save_ring_state(session, ring, &state)?;
            for (_, scope, path) in wanted {
                private_file::create_private_dir(&path)?;
                tables.insert(scope.digest(), (scope, path));
            }
            aterm_durable::durable_dir(&self.root)?;
        }
        Ok(tables)
    }

    /// The window's ring tables this Mac holds storage for, without reserving or deleting
    /// anything. None asks the caller for the writer: a held period below the window awaits its
    /// deletion, or a window table's storage is not held yet.
    ///
    /// # Errors
    /// Unreadable ring state or table storage.
    pub(crate) fn existing_ring_subscriptions(
        &self,
        session: &Session<'_>,
        policy: &Policy,
        now: u64,
    ) -> Result<Option<BTreeMap<Digest, (Scope, PathBuf)>>> {
        let mut tables = BTreeMap::new();
        if !policy.enabled || policy.excluded.contains(&self.catalog_scope().digest()) {
            return Ok(Some(tables));
        }
        for ring in RINGS {
            let state = self.ring_state(session, ring)?;
            let window = state.window(ring, now);
            if state.held.range(..*window.start()).next().is_some() {
                return Ok(None);
            }
            for epoch in window {
                let scope = ring.scope(self.trust, epoch)?;
                let path = self.path(scope)?;
                if policy.excluded.contains(&scope.digest()) || !path.try_exists()? {
                    continue;
                }
                if !state.held.contains(&epoch) {
                    return Ok(None);
                }
                tables.insert(scope.digest(), (scope, path));
            }
        }
        Ok(Some(tables))
    }

    /// Whether `id` names a table of some ring's current window, which never reaches below the
    /// floor: a retired period stays refused however a repair round reaches it. A damaged ring
    /// state permits the clock's window (`ring_view`).
    ///
    /// # Errors
    /// A ring table's scope cannot be formed.
    pub(crate) fn ring_permits(&self, session: &Session<'_>, id: Digest, now: u64) -> Result<bool> {
        Ok(self.ring_window(session, now)?.contains(&id))
    }

    /// Every table of every ring's current window, which never reaches below the floor; a
    /// damaged ring state reads as none yet (`ring_view`), so it stops no repair round.
    ///
    /// # Errors
    /// A ring table's scope cannot be formed.
    pub(crate) fn ring_window(&self, session: &Session<'_>, now: u64) -> Result<BTreeSet<Digest>> {
        let mut ids = BTreeSet::new();
        for ring in RINGS {
            let state = self.ring_view(session, ring);
            for epoch in state.window(ring, now) {
                ids.insert(ring.scope(self.trust, epoch)?.digest());
            }
        }
        Ok(ids)
    }

    /// `id`'s ring and period, where it names a table of a ring's current window (`ring_view`'s).
    ///
    /// # Errors
    /// A ring table's scope cannot be formed.
    pub(crate) fn ring_of(
        &self,
        session: &Session<'_>,
        id: Digest,
        now: u64,
    ) -> Result<Option<(&'static Ring, u64, Scope)>> {
        for ring in RINGS {
            let state = self.ring_view(session, ring);
            for epoch in state.window(ring, now) {
                let scope = ring.scope(self.trust, epoch)?;
                if scope.digest() == id {
                    return Ok(Some((ring, epoch, scope)));
                }
            }
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests;
