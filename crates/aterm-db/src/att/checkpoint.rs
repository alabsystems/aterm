// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A typed table's checkpoints (docs/DESIGN-table-checkpoint-2026-11-15.md, tracker LT-B10B):
//! what state a checkpoint of a typed table certifies, and the pass that signs one.
//!
//! The state is what [`View::read`] makes of the records at the cut: its cells (claims
//! interpreted), its owned collections' rows and the records past the table's quota. Each part
//! is a function of the cut and of the control history every member holds, so two writers at
//! one cut digest the same state; what a view says of this Mac alone (what it awaits, what it
//! refuses) is not part of it.
//!
//! The pass ([`Handle::checkpoint`]) runs from the node service's hearing tick, once per table,
//! and signs at most one checkpoint each time. While any current member holds the version fence
//! ([`hearing::CHECKPOINTS`]) it signs nothing. Otherwise it seconds the newest share another
//! writer signed that this Mac computes identically, or proposes its own once the cut has grown
//! by [`STRIDE`] records past every one it shared, so a table gains one checkpoint record per
//! writer per [`STRIDE`] of its own.
//!
//! Who holds the fence, and why, is one function ([`fence`], tracker LT-C1), which a read asks on
//! demand ([`Handle::usage_fenced`], which `aterm db table check` and the Tables page's Check
//! view print). Nothing sends, journals or counts it. It names the seats that signed a record in
//! the last 30 days too, which only the table's log says. The pass judges the roster before it
//! opens anything ([`held`], the same question of the same roster), so a fenced pass opens no
//! table, takes no writer and writes nothing: it answers [`Signed::Fenced`] and no more.
//!
//! Each pass then records the table's floor (tracker LT-B10C1): the newest checkpoint this Mac
//! signed that is final by the rule the vault log shares ([`finality`]), judged as every read
//! judges its settled floor ([`checkpoint::finals`], tracker LT-B10D1): more than half of the
//! members whose own frontier tip in the cut was signed in the 30 days before the cut have signed
//! its body, the cut's time being its newest frontier tip's. The store counts its quotas above
//! that floor.

use super::{Handle, HistoryView, Table, Usage, View};
use crate::{
    Result,
    checkpoint::{self, Body, History},
    documents::{application, log::Log},
};
use astream_wire::sync::{Actor, Digest};
use aterm_secrets::commons::{
    binding::Binding,
    bootstrap::Bootstrap,
    finality,
    hearing::{self, Ledger},
    roster::Access,
};
use std::collections::{BTreeMap, BTreeSet};

/// How much older than the proposing Mac's clock a record must be to go below a cut: a
/// checkpoint never races what is still arriving (design §3).
pub(crate) const AGE_MS: u64 = 14 * 24 * 60 * 60 * 1000;

/// How many records a cut must hold beyond every one this Mac shared before it proposes another.
pub(crate) const STRIDE: u64 = 1024;

/// Domain separation for a typed table's state digest.
const STATE_DOMAIN: &[u8] = b"aterm/att/checkpoint-state/1\0";

/// SHA-256 of the typed table's state at `cut` (a causally closed set of `log`'s records).
///
/// # Errors
/// The view's resolution refuses (as [`super::Resolved::read`]), a cell candidate has no header
/// here, or a value does not encode.
pub(crate) fn state(
    table: &'static Table,
    session: &crate::table::Session<'_>,
    log: &Log,
    cut: &BTreeSet<Digest>,
) -> Result<Digest> {
    // The cut judged as a final checkpoint of it will be: what a read from it holds.
    let view = View::read(table, session, &log.settled_at(cut).as_of_cut(cut))?;
    let index = log.causal_index();
    let cells = checkpoint::cells_digest(&view.cells, |id| {
        index.get(id).map(|header| (header.actor, header.sequence))
    })?;
    let mut hash = aterm_digest::Sha256::new();
    hash.update(STATE_DOMAIN);
    hash.update(cells);
    hash.update((view.owned.len() as u64).to_le_bytes());
    for (collection, read) in &view.owned {
        put_text(&mut hash, collection);
        hash.update((read.inert as u64).to_le_bytes());
        hash.update((read.rows.len() as u64).to_le_bytes());
        for (key, row) in &read.rows {
            put_text(&mut hash, key);
            hash.update(row.machine);
            hash.update(row.actor);
            let value = aterm_json::to_vec(&row.value)
                .map_err(|_| crate::Error::new("an owned row's value does not encode"))?;
            hash.update((value.len() as u64).to_le_bytes());
            hash.update(value);
            for part in [row.rank.0, row.rank.1, row.rank.2] {
                hash.update(part.to_le_bytes());
            }
            hash.update(row.operation);
        }
    }
    hash.update((view.quota.over.len() as u64).to_le_bytes());
    for id in &view.quota.over {
        hash.update(id);
    }
    Ok(hash.finalize())
}

fn put_text(hash: &mut aterm_digest::Sha256, text: &str) {
    hash.update((text.len() as u64).to_le_bytes());
    hash.update(text.as_bytes());
}

/// Whether `later`'s cut holds `earlier`'s: entrywise at or past it.
fn contains(later: &Body, earlier: &Body) -> bool {
    earlier.frontier.iter().all(|(actor, tip)| {
        later
            .frontier
            .get(actor)
            .is_some_and(|at| at.sequence >= tip.sequence)
    })
}

/// One set of a table's seats, and those of them that hold its checkpoint fence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Seats {
    /// The seats, each by its Commons writer.
    pub seats: BTreeSet<Actor>,
    /// Each of them that holds the fence, and why as [`hearing::Hold`] names it under
    /// [`hearing::CHECKPOINTS`], in the seats' order: heard in the last 7 days on an older build
    /// ([`hearing::Hold::Older`] with the release it reported) or with no report that still
    /// counts (`Older(None)`), or not heard in that time ([`hearing::Hold::Unheard`]).
    pub holders: Vec<(Actor, hearing::Hold)>,
}

/// Who holds a table's checkpoint fence at one time, by one ledger ([`fence`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fence {
    /// Every seat on the roster. The pass signs nothing while any of them holds the fence: a
    /// Mac that comes back on an older build could not read the table from a checkpoint.
    pub roster: Seats,
    /// The roster's seats that signed a record of the table in the 30 days before that time
    /// ([`finality::HEARD_WITHIN_MS`]): the seats a cut is taken over and finality counts. Its
    /// holders are the roster's holders among them.
    pub recent: Seats,
}

impl Fence {
    /// Whether the pass may sign: no seat on the roster holds the fence.
    #[must_use]
    pub fn open(&self) -> bool {
        self.roster.holders.is_empty()
    }
}

/// Whether a seat of `roster` holds the checkpoint fence at `now_ms` by `ledger`: what the pass
/// asks before it opens a table, and what [`Fence::open`] denies of the same roster, ledger and
/// time.
fn held<'a>(roster: impl IntoIterator<Item = &'a Binding>, ledger: &Ledger, now_ms: u64) -> bool {
    !ledger
        .holding_below(roster, now_ms, hearing::CHECKPOINTS)
        .is_empty()
}

/// Who holds the checkpoint fence of the table `log` reads, whose seats are `roster`, at
/// `now_ms` by `ledger`: the seats on the roster and those that signed a record in the 30 days
/// before `now_ms`, and for each set its holders with why each holds. It reads only what it is
/// given, so the pass that signs ([`Handle::checkpoint`]) and the read that reports
/// ([`Handle::usage_fenced`]) cannot disagree on the same table, ledger and time.
pub(crate) fn fence<'a>(
    roster: impl IntoIterator<Item = &'a Binding>,
    log: &Log,
    ledger: &Ledger,
    now_ms: u64,
) -> Fence {
    let roster: BTreeMap<Actor, &Binding> = roster
        .into_iter()
        .map(|seat| (seat.actor(), seat))
        .collect();
    let holders = ledger.holding_below(roster.values().copied(), now_ms, hearing::CHECKPOINTS);
    let seats: BTreeSet<Actor> = roster.keys().copied().collect();
    // Each seat's newest record here, by its signed authoring time.
    let mut newest: BTreeMap<Actor, u64> = BTreeMap::new();
    for id in &log.projection().ordered {
        let (Some(header), Some(at)) = (log.causal_index().get(id), log.authored_ms(id)) else {
            continue;
        };
        if seats.contains(&header.actor) {
            let entry = newest.entry(header.actor).or_insert(at);
            *entry = (*entry).max(at);
        }
    }
    let recent = finality::recent(&seats, now_ms, |actor| newest.get(actor).copied());
    let recent_holders = holders
        .iter()
        .filter(|(actor, _)| recent.contains(actor))
        .copied()
        .collect();
    Fence {
        roster: Seats { seats, holders },
        recent: Seats {
            seats: recent,
            holders: recent_holders,
        },
    }
}

/// This Mac's validated seat on the roster, as the pass reads it without waiting; `None` where
/// it has no Commons or no seat, or a vault writer holds its state at this moment, so the pass
/// signs nothing this time. Only the pass asks it: a read waits a writer out.
fn seated(paths: &aterm_secrets::client::Paths) -> Option<Access> {
    Bootstrap::cached(paths)
        .ok()
        .flatten()?
        .access()
        .ok()
        .flatten()
}

/// What a read of the fence rests on beside the table's own records: the ledger this Mac keeps
/// beside its Commons journal ([`Ledger::read`], which [`Handle::usage_fenced`] judges by), as
/// its file stands now, by the SHA-256 of its bytes. The node service keeps that file at each
/// change and every ten minutes, and nothing of it moves the table. A reader that shows a fence
/// it read (the Tables page's Check view) asks this to learn the ledger moved while the table
/// did not. `None` where no Commons is read here at this moment or no ledger is kept yet.
#[must_use]
pub fn ledger_mark(paths: &aterm_secrets::client::Paths) -> Option<Digest> {
    let bootstrap = Bootstrap::cached(paths).ok().flatten()?;
    let kept = std::fs::read(bootstrap.heard_file()).ok()?;
    let mut hash = aterm_digest::Sha256::new();
    hash.update(kept);
    Some(hash.finalize())
}

/// What the pass did in one table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Signed {
    /// A seat on the roster holds the version fence, so nothing is signed. Who holds it, and
    /// why, is a read's to say ([`Handle::usage_fenced`]).
    Fenced,
    /// Nothing is due.
    Nothing,
    /// This Mac signed another writer's body, which it computes identically.
    Seconded,
    /// This Mac proposed a body of its own.
    Proposed,
}

impl Handle {
    /// Sign at most one checkpoint of this table (the module's pass): none while a seat on the
    /// roster holds the fence ([`held`]), and then it opens nothing; else the
    /// newest share of another writer this Mac computes identically and has not signed, whose
    /// cut holds every one this Mac shared; else, at `now_ms`, its own, once the cut holds
    /// [`STRIDE`] records more than every one it shared. Then record the table's floor: the
    /// newest final checkpoint this Mac signed.
    ///
    /// # Errors
    /// No Commons, the table's storage cannot be read or written, or the record cannot be signed.
    pub fn checkpoint(&self, ledger: &Ledger, now_ms: u64) -> Result<Signed> {
        self.checkpoint_every(ledger, now_ms, STRIDE)
    }

    /// What the table holds here ([`Self::usage`]) and, from the same read of it, who holds its
    /// checkpoint fence at `now_ms` ([`fence`]) by the ledger this Mac keeps beside its Commons
    /// journal ([`Ledger::read`]): what the node service's pass last kept, so what its next
    /// pass judges by, give or take the contacts since. The seats are the members of the
    /// membership that read validated, so a Mac with no seat reads no table and judges no
    /// fence, and a vault writer that holds this Mac's state is waited out as [`Self::usage`]
    /// waits it out, never read as no seat. Read on demand: it writes and sends nothing.
    ///
    /// # Errors
    /// As [`Self::usage`].
    pub fn usage_fenced(&self, now_ms: u64) -> Result<(View, Usage, Fence)> {
        self.usage_with(|session, log| {
            fence(
                session.member_bindings(),
                log,
                &Ledger::read(session.bootstrap()),
                now_ms,
            )
        })
    }

    /// [`Self::usage_fenced`] with the table's eligible edits in causal order, all from one
    /// observation ([`Self::view_with_history`]): what the Tables page's Check view reads.
    ///
    /// # Errors
    /// As [`Self::view_with_history`].
    pub fn view_with_history_fenced(&self, now_ms: u64) -> Result<(HistoryView, Fence)> {
        self.view_with_history_and(|session, log| {
            fence(
                session.member_bindings(),
                log,
                &Ledger::read(session.bootstrap()),
                now_ms,
            )
        })
    }

    /// [`Self::usage_fenced`] by `ledger` in place of the one this Mac keeps: what a pass
    /// handed the same ledger and time judges.
    ///
    /// # Errors
    /// As [`Self::usage`].
    pub fn usage_fenced_by(&self, ledger: &Ledger, now_ms: u64) -> Result<(View, Usage, Fence)> {
        self.usage_with(|session, log| fence(session.member_bindings(), log, ledger, now_ms))
    }

    /// [`Self::checkpoint`] with `stride` in place of [`STRIDE`].
    ///
    /// # Errors
    /// As [`Self::checkpoint`].
    pub(crate) fn checkpoint_every(
        &self,
        ledger: &Ledger,
        now_ms: u64,
        stride: u64,
    ) -> Result<Signed> {
        let Some(access) = seated(&self.paths) else {
            return Ok(Signed::Nothing);
        };
        let roster = || access.members().iter().map(|member| member.binding());
        // Judged before the table is opened: a fenced pass reads no log, takes no writer of the
        // control journal or of the table, seals no held write and repairs no tail.
        if held(roster(), ledger, now_ms) {
            return Ok(Signed::Fenced);
        }
        let table = self.compiled.table;
        application::with_store(
            &self.paths,
            false,
            self.descriptor(),
            |session, store, scope| {
                let log = Log::read_with(session, store, scope, self.compiled.decoder())?;
                // Open, by `held` above: its recent seats are the cut's writers, and the store
                // refuses to sign over any holder it names.
                let fence = fence(roster(), &log, ledger, now_ms);
                let pass = Pass {
                    table,
                    session,
                    log: &log,
                    scope,
                    fence: &fence,
                    now_ms,
                };
                let signed = pass.sign(store, stride)?;
                if let Some(body) = pass.final_floor(store)
                    && store.floor() != Some(&body)
                {
                    store.set_floor(session, &body)?;
                }
                Ok(signed)
            },
        )
    }
}

/// One table's pass, over the log it read.
struct Pass<'a> {
    table: &'static Table,
    session: &'a crate::table::Session<'a>,
    log: &'a Log,
    scope: crate::table::Scope,
    /// Who holds the version fence ([`fence`]): none on the roster, or the pass signs nothing,
    /// and the store refuses to sign over any
    /// ([`crate::table::store::Store::sign_checkpoint`]). Its recent seats are the table's
    /// current writers (design §3), whose vectors a cut is taken over.
    fence: &'a Fence,
    now_ms: u64,
}

impl Pass<'_> {
    /// Second the newest share this Mac computes identically, or propose its own once due.
    fn sign(&self, store: &mut crate::table::store::Store, stride: u64) -> Result<Signed> {
        let (session, log) = (self.session, self.log);
        let me = session.actor();
        let authored = |id: &Digest| log.authored_ms(id);
        let state = |cut: &BTreeSet<Digest>| state(self.table, session, log, cut);
        let history = History {
            scope: self.scope.digest(),
            generation: self.scope.generation(),
            index: log.causal_index(),
            projection: log.projection(),
            authored: &authored,
            state: &state,
        };
        let shares: Vec<(Actor, Body)> = store
            .checkpoints()
            .map(|(signer, body)| (signer, body.clone()))
            .filter(|(_, body)| body.scope == history.scope)
            .collect();
        let mine: Vec<&Body> = shares
            .iter()
            .filter(|(signer, _)| *signer == me)
            .map(|(_, body)| body)
            .collect();
        let nests = |body: &Body| mine.iter().all(|earlier| contains(body, earlier));
        let mut theirs: Vec<&Body> = shares
            .iter()
            .filter(|(signer, body)| *signer != me && !mine.contains(&body) && nests(body))
            .map(|(_, body)| body)
            .collect();
        theirs.sort_by_key(|body| std::cmp::Reverse(body.below));
        for body in theirs {
            if let Ok(computed) = checkpoint::second(&history, body) {
                store.sign_checkpoint(session, &computed, &self.fence.roster.holders)?;
                return Ok(Signed::Seconded);
            }
        }
        let Some(computed) = checkpoint::compute(
            &history,
            &self.fence.recent.seats,
            self.now_ms.saturating_sub(AGE_MS),
        ) else {
            return Ok(Signed::Nothing);
        };
        let shared = mine.iter().map(|body| body.below).max().unwrap_or(0);
        if !nests(computed.body()) || computed.body().below < shared.saturating_add(stride) {
            return Ok(Signed::Nothing);
        }
        store.sign_checkpoint(session, &computed, &self.fence.roster.holders)?;
        Ok(Signed::Proposed)
    }

    /// The newest checkpoint of this table this Mac signed that is final, whose cut holds the
    /// store's present floor; `None` where there is none. Final is judged by the function every
    /// read names its settled floor by ([`checkpoint::finals`]), over the store's checkpoints now,
    /// this pass's own share included, so the floor this Mac records is the settled floor it
    /// signed.
    fn final_floor(&self, store: &crate::table::store::Store) -> Option<Body> {
        let me = self.session.actor();
        let floor = store.floor();
        checkpoint::finals(
            store.checkpoints(),
            self.scope.digest(),
            self.scope.generation(),
            &self.session.member_actors(),
            |actor, tip| self.log.tip_ms(actor, tip),
        )
        .into_iter()
        .rfind(|(body, signed)| {
            signed.contains(&me) && floor.is_none_or(|floor| contains(body, floor))
        })
        .map(|(body, _)| body)
    }
}

/// [`Handle::checkpoint`] for every typed table this build compiles, at the generation this
/// Mac's catalog uses: how many it signed. A table it cannot read or write is passed over; the
/// next tick tries it again.
pub fn sign_due(paths: &aterm_secrets::client::Paths, ledger: &Ledger, now_ms: u64) -> usize {
    let mut handles: Vec<Handle> = Vec::new();
    for compiled in super::generations() {
        let handle = Handle::at(paths.clone(), compiled);
        if !handles
            .iter()
            .any(|held| std::ptr::eq(held.compiled, handle.compiled))
        {
            handles.push(handle);
        }
    }
    handles
        .into_iter()
        .filter(|handle| {
            matches!(
                handle.checkpoint(ledger, now_ms),
                Ok(Signed::Seconded | Signed::Proposed)
            )
        })
        .count()
}
