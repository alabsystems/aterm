// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! RINGS OF PERIOD TABLES (docs/DESIGN-fluid-swarm-2026-10-28.md F19). A table whose rows all
//! lapse within one period rides a ring of tables, one per period (a UTC day for account claims),
//! each named by its ring and its period's number, outside the catalog: nothing is declared, so no
//! catalog record or creator slot is spent per period. Each Mac's window is the period before the
//! current one, the current one and the next; it takes up a window table when it writes one or a
//! peer offers it, so a ring nobody writes costs no repair turn, and deletes older tables itself,
//! under the control writer. It raises its floor first, never lowers it, never writes a period
//! below it and deletes only tables below it, so its own signed history in a table it deleted is
//! never forked by a fresh write there. A clock set back a period keeps the window at or above
//! the floor, so neither it nor a peer's repair brings a deleted table back; a clock further
//! behind the floor (one that ran ahead and came back, or one set back days) places the window
//! itself, so this Mac reads its peers' tables of those days again, below the floor, while it
//! writes nothing until the clock comes back to the floor (`accounts::claims::target`) and the
//! tables at or above the floor stay for it (`RingState::current`). A writer puts each row in a
//! table at least as late as the period before the one its row lapses in, so every row of a table
//! deleted below the window has lapsed on the clock that deleted it.
//!
//! A damaged or unreadable state stops nothing: a reader places the window by the clock alone,
//! and the next writer pass rebuilds the state from the storage this Mac holds and writes it,
//! moving aside a directory found where its file goes.

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
/// The most periods one Mac holds of a ring: its window, and the floor's three a clock set behind
/// them keeps (`RingState::ratchet`), need six; the rest is slack for a clock that moved while a
/// pass was interrupted.
const MAX_HELD: usize = 8;
/// The storage slots of `MAX_TABLES` kept for the rings: each holds at most [`MAX_HELD`] periods,
/// and a catalog table never takes one of these (`Directory::ring_storage`), so applications
/// that fill the catalog's slots leave every ring room to go on.
pub(crate) const RING_SLOTS: usize = RINGS.len() * MAX_HELD;
/// How many periods past the clock's a rebuilt state looks through for this Mac's storage, as
/// well as every period before it; a table a clock more than this far ahead took up is not found.
const REBUILD_SPAN: u64 = 400;
const STATE_MAX: usize = STATE_MAGIC.len() + 8 + 1 + MAX_HELD * 8;
const FLOOR_MAGIC: &[u8; 5] = b"ATRF1";
const FLOOR_LEN: usize = FLOOR_MAGIC.len() + 8;

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

    /// The file of the floor's second copy (`Directory::save_ring_state`).
    pub(crate) fn floor_name(&self) -> String {
        format!("ring-{}-{}.floor", self.application, self.role)
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
    /// The current period: the clock's, but never below the one after the floor while the clock
    /// is at most a period behind it, so a clock set back a period keeps the window at or above
    /// the floor. A clock further behind is one that ran ahead and came back, or one set back that
    /// far: the window is the clock's, so this Mac reads and takes up its peers' tables of those
    /// days again, while it writes nothing until the clock reaches the floor
    /// (`accounts::claims::target`), and never below the floor (`Directory::enroll_ring`), in a
    /// table it deleted, whose own signed history a fresh write would fork.
    pub(crate) fn current(&self, ring: &Ring, now: u64) -> u64 {
        let clock = ring.epoch_of(now);
        if clock < self.floor {
            clock
        } else {
            clock.max(self.floor.saturating_add(1))
        }
    }

    /// The periods this Mac subscribes to and may enroll: the one before the current, the current
    /// and the next.
    pub(crate) fn window(&self, ring: &Ring, now: u64) -> RangeInclusive<u64> {
        let current = self.current(ring, now);
        current.saturating_sub(1)..=current.saturating_add(1)
    }

    /// Raise the floor to the window's first period, never lowering it, and name the held periods
    /// to delete (`outdated`), which stay held until their storage is gone.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbClaimRing",
            action = "Retire",
            project = "aterm_db::accounts::claims::tests::ring_transition"
        )
    )]
    pub(crate) fn ratchet(&mut self, ring: &Ring, now: u64) -> Vec<u64> {
        self.floor = self.floor.max(*self.window(ring, now).start());
        self.outdated(ring, now)
    }

    /// The held periods outside the window and below the floor, which the ratchet raises to the
    /// window's first period: no write goes to them again. A held period at or above the floor
    /// stays while a clock set behind it places the window lower, since this Mac writes there
    /// again once its clock comes back, and a fresh table there would fork its own signed history
    /// that a peer's copy keeps.
    pub(crate) fn outdated(&self, ring: &Ring, now: u64) -> Vec<u64> {
        let window = self.window(ring, now);
        let floor = self.floor.max(*window.start());
        self.held
            .iter()
            .copied()
            .filter(|epoch| *epoch < floor && !window.contains(epoch))
            .collect()
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

/// A floor no higher than the one a lost state of `ring` kept, from what this Mac holds: the
/// floor's second copy in `claims` (`Directory::save_ring_state`), and the last held period `P`,
/// which lay in the window when it was taken up, whose first period the floor that pass left is
/// at or above, so the floor was `P - 2` or higher then. A floor never falls, and a copy a failed
/// write left behind is an older one, so each bound holds for the lost state's floor.
fn rebuilt_floor(claims: &Path, ring: &Ring, held: &BTreeSet<u64>) -> u64 {
    let copy = private_file::read_private(claims, &ring.floor_name(), FLOOR_LEN)
        .ok()
        .and_then(|bytes| {
            let floor = bytes.strip_prefix(FLOOR_MAGIC)?;
            Some(u64::from_le_bytes(floor.try_into().ok()?))
        });
    let last = held.last().map_or(0, |last| last.saturating_sub(2));
    last.max(copy.unwrap_or(0))
}

/// How many damaged ring states one claim directory keeps moved aside (`move_aside`).
const MAX_DAMAGED: u32 = 64;

/// Move `name` in `dir` (the claim directory) aside to the first free
/// `<name>.damaged-<n>`, kept for a person to look at, under the control writer, which no other
/// writer of `dir` holds at once. A name is free only while nothing is there: a rename onto an
/// empty directory would replace it.
///
/// # Errors
/// [`MAX_DAMAGED`] names are taken, or the rename fails.
fn move_aside(dir: &Path, name: &str) -> Result<()> {
    for n in 1..=MAX_DAMAGED {
        let aside = dir.join(format!("{name}.damaged-{n}"));
        match std::fs::symlink_metadata(&aside) {
            Ok(_) => continue,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                std::fs::rename(dir.join(name), &aside)?;
                return Ok(());
            }
            Err(error) => return Err(error.into()),
        }
    }
    Err(Error::new(
        "ring table state cannot be moved aside: too many damaged states are kept",
    ))
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
        Ok(self.written_ring_state(session, ring)?.unwrap_or_default())
    }

    /// What this Mac keeps of `ring`, or none where no state is written.
    ///
    /// # Errors
    /// Its state is unreadable or damaged.
    fn written_ring_state(&self, session: &Session<'_>, ring: &Ring) -> Result<Option<RingState>> {
        match private_file::read_private(&claim_dir(session)?, &ring.state_name(), STATE_MAX) {
            Ok(bytes) => RingState::decode(&bytes).map(Some),
            Err(PrivateFileError::Missing) => Ok(None),
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

    /// A state for `ring` in place of a missing, damaged or unreadable one, from this Mac's
    /// storage: holding every period up to [`REBUILD_SPAN`] past the clock's whose table storage
    /// or enrollment claim exists, the earliest [`MAX_HELD`] where more do, and a floor no higher
    /// than the lost one, which the retirement that follows raises to the clock's window where
    /// that is higher (`rebuilt_floor`).
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
        // Every name this Mac holds storage or a claim under, read once: matching each period's
        // names against them needs no look at the disk per period.
        let mut names = BTreeSet::new();
        for dir in [&self.root, &claims] {
            match std::fs::read_dir(dir) {
                Ok(entries) => {
                    for entry in entries {
                        names.insert(entry?.file_name());
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        let mut held = BTreeSet::new();
        if !names.is_empty() {
            for epoch in 0..=ring.epoch_of(now).saturating_add(REBUILD_SPAN) {
                if held.len() >= MAX_HELD {
                    break;
                }
                let scope = ring.scope(self.trust, epoch)?;
                let table = aterm_codec::hex::encode_lower(&scope.digest());
                if names.contains(std::ffi::OsStr::new(&table))
                    || names.contains(std::ffi::OsStr::new(&claim_name(scope)))
                {
                    held.insert(epoch);
                }
            }
        }
        Ok(RingState {
            floor: rebuilt_floor(&claims, ring, &held),
            held,
        })
    }

    /// Write `state` durably, then its floor's second copy. A directory where the state's file
    /// goes, which no rename can replace, is first moved aside (`move_aside`), never deleted; any
    /// other thing there is renamed over.
    fn save_ring_state(&self, session: &Session<'_>, ring: &Ring, state: &RingState) -> Result<()> {
        let dir = claim_dir(session)?;
        aterm_journal::create_private_dir_durable(&dir)?;
        let name = ring.state_name();
        match std::fs::symlink_metadata(dir.join(&name)) {
            Ok(meta) if meta.is_dir() => move_aside(&dir, &name)?,
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        aterm_journal::write_private_durable(&dir, &name, &state.encode())?;
        // The floor's second copy, which a rebuild reads when the state is lost
        // (`rebuilt_floor`): a bound only, so one that cannot be written fails nothing.
        let mut floor = FLOOR_MAGIC.to_vec();
        floor.extend_from_slice(&state.floor.to_le_bytes());
        let _ = aterm_journal::write_private_durable(&dir, &ring.floor_name(), &floor);
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

    /// Delete every table of `ring` this Mac writes no more, under the control writer: raise the
    /// floor and write it durably, delete each held period outside the window below the floor (its
    /// storage directory and its enrollment claim), then forget them. A pass cut short anywhere is
    /// finished by the next.
    /// A missing, damaged or unreadable state is rebuilt first (`rebuild_ring_state`) and
    /// written.
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
        let (mut state, rebuilt) = match self.written_ring_state(session, ring) {
            Ok(Some(state)) => (state, false),
            Ok(None) | Err(_) => (self.rebuild_ring_state(session, ring, now)?, true),
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
        state.held.retain(|epoch| !doomed.contains(epoch));
        self.save_ring_state(session, ring, &state)?;
        Ok(state)
    }

    /// Enroll `ring`'s table of period `epoch` for a write, under the control writer: retire
    /// first (the caller's job), then hold the period durably before its storage exists. A write
    /// never goes below the floor (`RingState::current`).
    ///
    /// # Errors
    /// `epoch` lies outside this Mac's window or below its floor, no control writer or storage
    /// slot, or damaged storage.
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
        if epoch < state.floor {
            return Err(Error::new("ring table period is below this Mac's floor"));
        }
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
            let (_, mut count) = self.retained()?;
            let mut held = state.held.len();
            wanted.retain(|(epoch, _, _)| {
                if count > MAX_TABLES {
                    return false;
                }
                if !state.held.contains(epoch) {
                    if held >= MAX_HELD {
                        return false;
                    }
                    held += 1;
                }
                count += 1;
                true
            });
            if wanted.is_empty() {
                continue;
            }
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

    /// The ids of the ring tables this Mac holds by its ring states, whose storage takes the slots
    /// kept for the rings ([`RING_SLOTS`]), never a catalog table's. A missing, damaged or
    /// unreadable state names none, so its storage counts against the catalog's slots until a
    /// writer pass rebuilds it.
    pub(crate) fn ring_storage(&self, session: &Session<'_>) -> BTreeSet<Digest> {
        let mut ids = BTreeSet::new();
        for ring in RINGS {
            let Ok(Some(state)) = self.written_ring_state(session, ring) else {
                continue;
            };
            for epoch in state.held {
                if let Ok(scope) = ring.scope(self.trust, epoch) {
                    ids.insert(scope.digest());
                }
            }
        }
        ids
    }

    /// The window's ring tables this Mac holds storage for, without reserving or deleting
    /// anything. None asks the caller for the writer: no state is written yet (the writer's
    /// retirement writes one, after looking for storage a lost state held), a held period awaits
    /// its deletion (`RingState::outdated`), or a window table's storage is not held yet.
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
            let Some(state) = self.written_ring_state(session, ring)? else {
                return Ok(None);
            };
            if !state.outdated(ring, now).is_empty() {
                return Ok(None);
            }
            for epoch in state.window(ring, now) {
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

    /// Whether `id` names a table of some ring's current window, which reaches below the floor
    /// only on a clock more than a period behind it (`RingState::current`): a period retired on a
    /// clock set back a period stays refused however a repair round reaches it. A damaged ring
    /// state permits the clock's window (`ring_view`).
    ///
    /// # Errors
    /// A ring table's scope cannot be formed.
    pub(crate) fn ring_permits(&self, session: &Session<'_>, id: Digest, now: u64) -> Result<bool> {
        Ok(self.ring_window(session, now)?.contains(&id))
    }

    /// Every table of every ring's current window (`RingState::current`); a damaged ring state
    /// reads as none yet (`ring_view`), so it stops no repair round.
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
