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

/// Short-lived requests for another root-authorized account preparation.
pub(crate) const RECOVERIES: Ring = Ring {
    application: "accounts",
    role: "recoveries",
    period_s: 86_400,
};

/// The lab journal's ring (docs/DESIGN-alab-lab-model-2026-11-01.md §4.17): one table a week
/// of what each Mac sees while it uses an account anyway (sessions, sign-ins, quota readings),
/// each writer in its fixed share ([`JOURNAL_SHARES`]). A Mac's window is the previous, current
/// and next week; older weeks are deleted whole on each Mac's own clock.
pub(crate) const JOURNAL: Ring = Ring {
    application: "alab",
    role: "journal",
    period_s: 604_800,
};

/// Every ring the replica subscribes to and the catalog diagnostics enumerate.
pub(super) const RINGS: [&Ring; 3] = [&CLAIMS, &RECOVERIES, &JOURNAL];

/// What one period table holds, in all and from one writer: records and wire bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Used {
    pub records: usize,
    pub bytes: usize,
    pub writer_records: usize,
    pub writer_bytes: usize,
}

/// How much of one period table a write may fill: the table's caps, and one writer's share of
/// them. A write that would pass either spills into the next period (`place`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Shares {
    pub records: usize,
    pub bytes: usize,
    pub writer_records: usize,
    pub writer_bytes: usize,
}

/// The writer count a fixed share divides by (design §3.9): a constant of the ring, never the
/// current membership, so writers that join mid-period cannot together pass the table.
pub(crate) const SHARE_WRITERS: usize = 128;

/// The journal ring's shares (design §3.9): at most 7,680 records and 115 MiB a period table,
/// below the store's 8,192 records and 128 MiB so both sides of a split fit after a heal, and
/// each writer a fixed 1/128 of a 6,912-record pool (54) and of a 108 MiB pool (864 KiB). 128
/// writers fill at most the pools; even 151 fit the store, in records and in bytes.
///
/// The asserts below hold that at compile time. NEGATIVE CONTROL: with the byte pool the
/// table's 115 MiB (920 KiB a writer), 151 writers hold 135.7 MiB and the build fails at the
/// byte assert (E0080).
pub(crate) const JOURNAL_SHARES: Shares = Shares::fixed(
    (7_680, 115 * 1024 * 1024),
    (6_912, 108 * 1024 * 1024),
    SHARE_WRITERS,
);
// 128 writers' shares fill at most the pools, within the table's caps, and even 151 writers'
// shares fit the store's records and bytes.
const _: () = assert!(SHARE_WRITERS * JOURNAL_SHARES.writer_records <= JOURNAL_SHARES.records);
const _: () = assert!(SHARE_WRITERS * JOURNAL_SHARES.writer_bytes <= JOURNAL_SHARES.bytes);
const _: () = assert!(151 * JOURNAL_SHARES.writer_records <= crate::table::store::MAX_RECORDS);
const _: () = assert!(151 * JOURNAL_SHARES.writer_bytes <= crate::table::store::MAX_BYTES);

impl Shares {
    /// A table of `table` records and bytes, whose writers each hold a fixed `1 / writers` of
    /// `pool`'s records and of its bytes.
    pub(crate) const fn fixed(
        (table_records, table_bytes): (usize, usize),
        (pool_records, pool_bytes): (usize, usize),
        writers: usize,
    ) -> Self {
        Self {
            records: table_records,
            bytes: table_bytes,
            writer_records: pool_records / writers,
            writer_bytes: pool_bytes / writers,
        }
    }

    /// Whether an append of `records` records and `bytes` wire bytes no longer fits beside
    /// `used`, in the table or in this writer's share.
    pub(crate) const fn full(&self, used: &Used, records: usize, bytes: usize) -> bool {
        records > self.records.saturating_sub(used.records)
            || bytes > self.bytes.saturating_sub(used.bytes)
            || records > self.writer_records.saturating_sub(used.writer_records)
            || bytes > self.writer_bytes.saturating_sub(used.writer_bytes)
    }
}

/// Why an own-chain append was kept for later rather than written: no window table had room for
/// it, or this Mac has no storage slot left for the one it goes into. The journal's spool keeps
/// its events then (design §3.9, §3.10), so a cap or a full disk of tables costs latency, never
/// events.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kept {
    Unplaced(Unplaced),
    /// The table has no storage here and every slot is taken: an older build's catalog tables
    /// past the shared slots, until the storage release frees them.
    NoSlot,
}

impl Kept {
    /// Why, in the words a Mac reports (the journal flusher keeps it for `sum` and `check`).
    pub(crate) const fn reason(self) -> &'static str {
        match self {
            Self::Unplaced(unplaced) => unplaced.reason(),
            Self::NoSlot => "journal: no storage slot",
        }
    }
}

/// Why a write found no period table in its ring's window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Unplaced {
    /// The clock is behind this Mac's floor.
    Behind,
    /// The row lapses beyond the window's last period.
    Beyond,
    /// Every table from the first the row may go into to the window's last is full.
    Full,
}

impl Unplaced {
    const fn reason(self) -> &'static str {
        match self {
            Self::Behind => {
                "this Mac's clock is behind the ring tables it took up; writes resume when it reaches them"
            }
            Self::Beyond => "the row lapses beyond the ring tables' window",
            Self::Full => "this writer's share of the ring tables is full; it resumes next period",
        }
    }
}

/// The period table a row that lapses at `until` goes into, by the clock `now` and this Mac's
/// ring `state`: the current period's, this writer's own latest period's (`own`) where later,
/// never below the floor, so never a table this Mac deleted (`RingState::outdated`), and never
/// earlier than the period before the one `until` falls in, so `until < (epoch + 2) · period`
/// and the row has lapsed on any clock that deletes its table. A `full` table spills into the
/// next period's while that lies in the window.
///
/// No row is placed while the clock is behind the floor. Such a clock either ran ahead and came
/// back or is set back, and no Mac can tell which: set back, its window (the clock's,
/// `RingState::current`) leaves out the period after the floor, its peers' current one, where its
/// own latest row and its peers' current rows can sit, so a row written at the floor would be
/// read on a view without them and outranked by its own later one. Once the clock reaches the
/// floor, the window reaches every table that holds a row of this Mac's: each write raises the
/// floor to the period before the current one first and goes at most a period past the current
/// one, so no row lies more than two periods past the floor.
pub(crate) fn place(
    ring: &Ring,
    state: &RingState,
    now: u64,
    own: Option<u64>,
    until: u64,
    full: impl Fn(u64) -> bool,
) -> std::result::Result<u64, Unplaced> {
    let last = *state.window(ring, now).end();
    let epoch = state
        .current(ring, now)
        .max(state.floor)
        .max(own.unwrap_or(0))
        .max(ring.epoch_of(until).saturating_sub(1));
    if ring.epoch_of(now) < state.floor {
        return Err(Unplaced::Behind);
    }
    if epoch > last {
        return Err(Unplaced::Beyond);
    }
    (epoch..=last)
        .find(|epoch| !full(*epoch))
        .ok_or(Unplaced::Full)
}

/// The wire bytes a sealed record of a `payload`-byte payload takes at most: the payload and
/// one envelope (one parent, no merge).
pub(crate) const fn record_bytes(payload: usize) -> usize {
    payload.saturating_add(crate::crypto::MAX_OPERATION - crate::crypto::MAX_BODY)
}

/// The period table an own-chain record of a `payload`-byte payload that lapses at `until` goes
/// into ([`place`]), by this writer's retained `used` of each window table it holds: its own
/// latest period is the highest that holds a record of its, and a table whose share the record
/// would pass is full. The journal flusher paces by the period this names, so a share that
/// spilled into the next period is paced as that period's (`alab::journal::flush`).
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AlabJournal",
        action = "FlushCurrent",
        project = "aterm_db::alab::journal::model_tests::journal_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AlabJournal",
        action = "FlushNext",
        project = "aterm_db::alab::journal::model_tests::journal_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AlabJournal",
        action = "FlushKept",
        project = "aterm_db::alab::journal::model_tests::journal_transition"
    )
)]
pub(crate) fn landing(
    ring: &Ring,
    shares: Shares,
    state: &RingState,
    now: u64,
    used: &BTreeMap<u64, Used>,
    until: u64,
    payload: usize,
) -> std::result::Result<u64, Unplaced> {
    let own = used
        .iter()
        .filter(|(_, used)| used.writer_records > 0)
        .map(|(epoch, _)| *epoch)
        .max();
    let bytes = record_bytes(payload);
    place(ring, state, now, own, until, |epoch| {
        shares.full(&used.get(&epoch).copied().unwrap_or_default(), 1, bytes)
    })
}

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
        let mut sha = aterm_digest::Sha256::new();
        sha.update(DOMAIN);
        sha.update(trust.network);
        sha.update(trust.authority);
        sha.update(trust.genesis);
        for word in [self.application, self.role] {
            sha.update((word.len() as u64).to_le_bytes());
            sha.update(word.as_bytes());
        }
        sha.update(epoch.to_le_bytes());
        let hash = sha.finalize();
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
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AlabJournal",
            action = "Tick",
            project = "aterm_db::alab::journal::model_tests::journal_transition"
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
        let mut bytes = Vec::with_capacity(STATE_MAGIC.len() + 8 + 1 + self.held.len() * 8);
        bytes.extend_from_slice(STATE_MAGIC);
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
        // One spelling per state: sorted, distinct periods.
        let mut held = BTreeSet::new();
        let mut prev: Option<u64> = None;
        for chunk in chunks {
            let epoch = u64::from_le_bytes(*chunk);
            if prev.is_some_and(|p| p >= epoch) {
                return Err(damaged());
            }
            prev = Some(epoch);
            held.insert(epoch);
        }
        Ok(Self {
            floor: u64::from_le_bytes(*floor),
            held,
        })
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
        // Every table digest this Mac holds storage (`<hex>`) or a claim (`<hex>.claim`,
        // `claim_name`) under, decoded once: matching each period's digest against them needs
        // neither disk I/O nor string allocation per period.
        let mut digests = BTreeSet::new();
        for (dir, suffix) in [(&self.root, ""), (&claims, ".claim")] {
            match std::fs::read_dir(dir) {
                Ok(entries) => {
                    for entry in entries {
                        let name = entry?.file_name();
                        if let Some(hex) = name.to_str().and_then(|s| s.strip_suffix(suffix))
                            && let Ok(digest) = crate::database::decode_hex::<32>(hex)
                        {
                            digests.insert(digest);
                        }
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        let mut held = BTreeSet::new();
        if !digests.is_empty() {
            for epoch in 0..=ring.epoch_of(now).saturating_add(REBUILD_SPAN) {
                if held.len() >= MAX_HELD {
                    break;
                }
                let scope = ring.scope(self.trust, epoch)?;
                if digests.contains(&scope.digest()) {
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
        let mut floor = [0u8; FLOOR_LEN];
        floor[..FLOOR_MAGIC.len()].copy_from_slice(FLOOR_MAGIC);
        floor[FLOOR_MAGIC.len()..].copy_from_slice(&state.floor.to_le_bytes());
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
        for epoch in &doomed {
            state.held.remove(epoch);
        }
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

    /// Append `payload` to `ring` on this writer's own chain (`Store::commit_own`), in the first
    /// window table from `place`'s start where it fits this writer's fixed `shares`, by the
    /// records and wire bytes each table retains. A full share spills into the next period while
    /// that lies in the window, so the cap costs latency, never rows. Returns the period and the
    /// receipt.
    ///
    /// # Errors
    /// No control writer, a clock behind the floor, a row beyond the window, no table left with
    /// room for this writer, no storage slot, or damaged storage.
    #[cfg(test)]
    pub(crate) fn append_own(
        &self,
        session: &Session<'_>,
        ring: &Ring,
        shares: Shares,
        now: u64,
        until: u64,
        payload: &[u8],
    ) -> Result<(u64, crate::table::store::Receipt)> {
        self.append_within(session, ring, shares, now, until, payload)?
            .map_err(|kept| Error::new(kept.reason()))
    }

    /// Append `payload` to the lab journal (`JOURNAL`) in this writer's fixed share
    /// ([`JOURNAL_SHARES`]), as [`Self::append_own`] does, but keep it for later where it
    /// cannot go now: every window table is full for this writer, the clock is behind the floor,
    /// the row lapses beyond the window, or this Mac has no storage slot left for the table
    /// (`journal: no storage slot`). The caller's spool keeps the events then.
    ///
    /// # Errors
    /// No control writer, an unreadable ring state, or damaged storage.
    pub(crate) fn append_journal(
        &self,
        session: &Session<'_>,
        now: u64,
        until: u64,
        payload: &[u8],
    ) -> Result<std::result::Result<(u64, crate::table::store::Receipt), Kept>> {
        self.append_within(session, &JOURNAL, JOURNAL_SHARES, now, until, payload)
    }

    /// The own-chain append both of the above share: the placement by fixed shares, then the
    /// storage room, then the enrollment and the commit.
    fn append_within(
        &self,
        session: &Session<'_>,
        ring: &Ring,
        shares: Shares,
        now: u64,
        until: u64,
        payload: &[u8],
    ) -> Result<std::result::Result<(u64, crate::table::store::Receipt), Kept>> {
        let state = self.retire(session, ring, now)?;
        let actor = session.actor();
        let mut used = BTreeMap::new();
        for epoch in state.window(ring, now) {
            let scope = ring.scope(self.trust, epoch)?;
            if let Some(store) = Store::observe_if_exists(session, &self.path(scope)?, scope)? {
                used.insert(epoch, store.retained_use(session, actor)?);
            }
        }
        let epoch = match landing(ring, shares, &state, now, &used, until, payload.len()) {
            Ok(epoch) => epoch,
            Err(unplaced) => return Ok(Err(Kept::Unplaced(unplaced))),
        };
        if !self.has_room(ring.scope(self.trust, epoch)?)? {
            return Ok(Err(Kept::NoSlot));
        }
        let mut store = self.enroll_ring(session, ring, epoch, now)?;
        let receipt = store.commit_own(session, payload)?;
        Ok(Ok((epoch, receipt)))
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
                let digest = scope.digest();
                if policy.excluded.contains(&digest) {
                    continue;
                }
                let path = self.path(scope)?;
                if !path.try_exists()? {
                    continue;
                }
                changed |= state.hold(ring, epoch, now)?;
                tables.insert(digest, (scope, path));
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
                let digest = scope.digest();
                if offered.contains(&digest) && !policy.excluded.contains(&digest) {
                    let path = self.path(scope)?;
                    if !path.try_exists()? {
                        wanted.push((epoch, digest, scope, path));
                    }
                }
            }
            if wanted.is_empty() {
                continue;
            }
            self.ensure_catalog(session)?;
            let (_, mut count) = self.retained()?;
            let mut held = state.held.len();
            wanted.retain(|(epoch, _, _, _)| {
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
            for (epoch, _, _, _) in &wanted {
                state.hold(ring, *epoch, now)?;
            }
            self.save_ring_state(session, ring, &state)?;
            for (_, digest, scope, path) in wanted {
                private_file::create_private_dir(&path)?;
                tables.insert(digest, (scope, path));
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
                let digest = scope.digest();
                if policy.excluded.contains(&digest) {
                    continue;
                }
                let path = self.path(scope)?;
                if !path.try_exists()? {
                    continue;
                }
                if !state.held.contains(&epoch) {
                    return Ok(None);
                }
                tables.insert(digest, (scope, path));
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
        Ok(self.ring_of(session, id, now)?.is_some())
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
