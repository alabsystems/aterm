// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The writer of records format 2 (`docs/DESIGN-vault-log-2026-11-14.md` step 4, tracker
//! FL-148F): a `Put` or `Delete` a log drafts for it states its register ([`Statement`]) and
//! signs as record version 2 ([`Draft::sign`]).
//!
//! A log drafts for such a writer only once one is attached to it ([`Log::write_two`]); a log
//! that reads format 1 and has none drafts as version 1, stating nothing, byte for byte as
//! before. A log that reads format 2 and has none refuses to draft a `Put` or `Delete`: no
//! version-1 op is signed over version-2 history, whose shed and pruned ops a version-1 op would
//! read by their values ([`super::pass`]). The client attaches one to every store it loads at
//! format 2 (`client::load_records`), and to the store an author drafts from on a Mac whose first
//! format-2 write is due (`client::format_two`), so no running build attaches one while
//! [`crate::commons::hearing::READS_FORMAT`] names no release.
//!
//! What a statement names, as [`super::checkpoint`] reads it:
//!
//! - **`carries`**: a `Put` whose entry (value, `if_absent` and anchor) is what the register's
//!   winner writes carries that value, and names the value's semantic representative, the
//!   earliest op of its carry chain, which the resolve reads as the winner's rep. A version-1
//!   reseal states nothing, so a chain through one is read from the values alone; the writer
//!   names the rep itself so the chain does not end there. Any other `Put`, and every `Delete`,
//!   carries nothing.
//! - **`supersedes`**: the ops of its register, of every signer, in the draft's causal past that
//!   it leaves replaced: every one, but, for a carry, the siblings of another write that the
//!   carry leaves standing. A `Delete` is read by the newest of its names
//!   ([`Log::frame_reps`]), and a name prunes only an op of its own signer
//!   ([`super::checkpoint`]). Of those ops, the ones an earlier version-2 op of the same signer
//!   in the same register already names are left out: that name stands in every log that holds
//!   the op or its stub ([`super::rooted::Stub`]), and a named op lies in its namer's past, so
//!   the register's newest ops are never among them. So a signer's first version-2 write of an
//!   entry restates the whole history it read there, version 1 included, and each later one
//!   names what came since.
//! - A statement holds at most [`MAX_SUPERSEDES`] names and its record at most [`MAX_RECORD`]
//!   bytes. A draft whose names do not all fit names the newest that do, by record order; the
//!   older ones stay unnamed, and so kept by a prune, until a later write of the same signer
//!   names them.
//! - The Commons control register's `Put`s name every control `Put` in their past, as the newest
//!   replaces each, and carry nothing.
//!
//! The registers are read from one resolve of the log under the writer's identity and kept
//! ([`Index`]): a draft's own records, held into the log as its batch goes on, are taken into it
//! by their signer and sequence, and any other record the log holds since is read by a resolve
//! again. A draft that does not follow every head of its log (more heads than a record names)
//! states what a resolve of its own causal past reads, which the kept index does not take in.

use super::{
    Body, Draft, Kind, Log, MAX_RECORD, MAX_SUPERSEDES, MOST_FRAME_TWO, MemberId, RecordId,
    Statement,
};
use crate::age::Identity;
use crate::text::{Audience, Entry};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, PoisonError};

/// What [`writes_of`] hashes first.
const WRITES_DOMAIN: &[u8] = b"aterm/members/writer-writes/1\0";

/// A writer of format 2: the identity it resolves its log's registers under, and what it read
/// of them ([`Index`]). A log's clones share it.
pub(crate) struct Writing {
    identity: Identity,
    index: Mutex<Option<Index>>,
}

impl std::fmt::Debug for Writing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Writing { .. }")
    }
}

/// What one resolve read of a log's registers, for a writer ([`Log::register_facts`]).
#[derive(Clone, Debug, Default)]
pub(super) struct Facts {
    /// Each `(Audience, name)` register the resolve opened an op of.
    pub(super) registers: BTreeMap<(Audience, String), Register>,
    /// Every counted Commons control `Put`, in record order.
    pub(super) commons: Vec<RecordId>,
}

/// One register, as a resolve read it.
#[derive(Clone, Debug, Default)]
pub(super) struct Register {
    /// Every op of it the resolve read, in record order.
    pub(super) ops: Vec<RecordId>,
    /// The siblings that write otherwise than the winner: what a carry of the winner's value
    /// leaves standing.
    pub(super) standing: Vec<RecordId>,
    /// What its winner writes.
    pub(super) winner: Option<Winner>,
}

/// What a register's winner writes.
#[derive(Clone, Debug)]
pub(super) enum Winner {
    /// A value: the op it stands for (its semantic representative) and [`writes_of`] its entry.
    Put {
        /// The rep.
        rep: RecordId,
        /// [`writes_of`] the winner's entry.
        writes: [u8; 32],
    },
    /// A `Delete`.
    Delete,
}

/// The register a draft writes, and what.
pub(super) enum Target<'a> {
    /// An entry: `Some(writes)` a `Put` of an entry [`writes_of`] gives, `None` a `Delete`.
    Entry(&'a (Audience, String), Option<[u8; 32]>),
    /// The Commons control register.
    Commons,
}

/// What a drafted op will be once its record is held, as [`Index`] takes it in.
#[derive(Clone)]
enum Pending {
    /// An entry's op.
    Entry {
        key: (Audience, String),
        /// [`writes_of`] its entry, `None` for a `Delete`.
        writes: Option<[u8; 32]>,
        /// The rep it carries, for a carry.
        carries: Option<RecordId>,
    },
    /// A Commons control `Put`.
    Commons,
}

/// What a writer read of its log's registers.
struct Index {
    /// Every held `Put` and `Delete` it took in, read or not.
    seen: BTreeSet<RecordId>,
    facts: Facts,
    /// The ops drafted from it, by signer and sequence, until their records are held.
    pending: BTreeMap<(MemberId, u64), Pending>,
}

/// SHA-256 of what `entry` writes, as two ops of one register write the same thing exactly
/// where the resolve classes them alike: its `if_absent`, its anchor and its value.
#[must_use]
pub(super) fn writes_of(entry: &Entry) -> [u8; 32] {
    let mut hash = ring::digest::Context::new(&ring::digest::SHA256);
    hash.update(WRITES_DOMAIN);
    hash.update(&[u8::from(entry.if_absent)]);
    match &entry.anchor {
        Some(anchor) => {
            hash.update(&[1]);
            hash.update(&(anchor.len() as u64).to_be_bytes());
            hash.update(anchor.as_bytes());
        }
        None => hash.update(&[0]),
    }
    hash.update(entry.value.as_slice());
    let mut out = [0; 32];
    out.copy_from_slice(hash.finish().as_ref());
    out
}

impl Index {
    /// The index of `log` as `identity` resolves it; `None` where it opens nothing (removed).
    fn build(log: &Log, identity: &Identity) -> Option<Self> {
        let facts = log.register_facts(identity).ok()??;
        Some(Self {
            seen: ops_of(log).map(|(id, _, _)| id).collect(),
            facts,
            pending: BTreeMap::new(),
        })
    }

    /// Take in the ops `log` holds that this index has not read: each a drafted one
    /// ([`Index::pending`]). `false` where `log` holds another, or lacks one this index read,
    /// and the index must be built again.
    fn follow(&mut self, log: &Log) -> bool {
        let mut held = 0;
        for (id, actor, sequence) in ops_of(log) {
            held += 1;
            if self.seen.contains(&id) {
                continue;
            }
            let Some(pending) = self.pending.remove(&(actor, sequence)) else {
                return false;
            };
            if !self.take(id, pending) {
                return false;
            }
            self.seen.insert(id);
        }
        held == self.seen.len()
    }

    /// Take in the held op `id`, drafted as `pending`. `false` where what the register reads
    /// next is not known without a resolve: a carry beside siblings it leaves standing.
    fn take(&mut self, id: RecordId, pending: Pending) -> bool {
        match pending {
            Pending::Commons => self.facts.commons.push(id),
            Pending::Entry {
                key,
                writes,
                carries,
            } => {
                let register = self.facts.registers.entry(key).or_default();
                register.ops.push(id);
                match (writes, carries) {
                    (None, _) => register.winner = Some(Winner::Delete),
                    (Some(_), Some(_)) if !register.standing.is_empty() => return false,
                    (Some(writes), carries) => {
                        register.winner = Some(Winner::Put {
                            rep: carries.unwrap_or(id),
                            writes,
                        });
                    }
                }
                register.standing.clear();
            }
        }
        true
    }
}

/// Every held `Put` and `Delete` of `log`: its id, signer and sequence.
fn ops_of(log: &Log) -> impl Iterator<Item = (RecordId, MemberId, u64)> + '_ {
    log.records()
        .filter(|record| matches!(record.kind(), Kind::Put | Kind::Delete))
        .map(|record| (*record.id(), *record.actor(), record.sequence()))
}

impl Log {
    /// Draft from now on for a writer of format 2 that resolves its registers as `identity`
    /// ([`writer`](self)): the `Put`s and `Delete`s this log and its clones draft state their
    /// register and sign as record version 2. A log that drafts for one already keeps it.
    pub(crate) fn write_two(&mut self, identity: &Identity) {
        if self.writing.is_some() {
            return;
        }
        self.writing = Some(Arc::new(Writing {
            identity: Identity::from_secret_bytes(*identity.secret_bytes()),
            index: Mutex::new(None),
        }));
    }

    /// A writer of format 2 of the same identity as this log's, with nothing read yet: what a
    /// projection of this log drafts for ([`Log::project_heads`]).
    pub(super) fn writing_afresh(&self) -> Option<Arc<Writing>> {
        self.writing.as_ref().map(|writing| {
            Arc::new(Writing {
                identity: Identity::from_secret_bytes(*writing.identity.secret_bytes()),
                index: Mutex::new(None),
            })
        })
    }

    /// Whether this log drafts for a writer of format 2 ([`Log::write_two`]).
    #[cfg(test)]
    #[must_use]
    pub(crate) fn writes_two(&self) -> bool {
        self.writing.is_some()
    }

    /// What `draft`, which `actor` signs, states of `target`, where this log drafts for a writer
    /// of format 2 ([`writer`](self)); `None` where it drafts for none and reads no format 2, or
    /// where the writer's identity opens nothing here. A draft that does not follow every head
    /// states what its own causal past reads ([`Log::project_heads`]).
    ///
    /// # Errors
    /// This log reads format 2 and drafts for no writer of format 2: a version-1 `Put` or
    /// `Delete` has no place in it (the shedding pass and the keep set read every op over
    /// version-2 history by its statement, [`super::pass`]); or the draft's past cannot be
    /// projected.
    pub(super) fn statement_for(
        &self,
        actor: &MemberId,
        draft: &Draft,
        target: Target<'_>,
    ) -> crate::Result<Option<Statement>> {
        let Some(writing) = self.writing.as_ref() else {
            if self.stored_at.admits_two() {
                return Err(crate::Error::new(
                    "this vault log reads format 2 and signs its Puts and Deletes as record \
                     version 2, and this draft has no writer of format 2",
                ));
            }
            return Ok(None);
        };
        let past = self.past_of(&draft.parents);
        if !self.heads().iter().all(|head| past.holds(head)) {
            return self.statement_within(writing, actor, draft, target);
        }
        let mut guard = writing.index.lock().unwrap_or_else(PoisonError::into_inner);
        if !guard.as_mut().is_some_and(|index| index.follow(self)) {
            *guard = Index::build(self, &writing.identity);
        }
        let Some(index) = guard.as_mut() else {
            return Ok(None);
        };
        let (statement, pending) = state(
            self,
            |id| past.holds(id),
            &index.facts,
            actor,
            draft,
            &target,
        );
        index.pending.insert((*actor, draft.sequence), pending);
        Ok(Some(statement))
    }

    /// [`Log::statement_for`] of a draft that does not follow every head of this log (more heads
    /// than a record names): its registers read from its own causal past alone, by a resolve of
    /// that past's projection, which the writer's kept index does not take in.
    #[cfg(any(unix, windows))]
    fn statement_within(
        &self,
        writing: &Writing,
        actor: &MemberId,
        draft: &Draft,
        target: Target<'_>,
    ) -> crate::Result<Option<Statement>> {
        let at = self.project_heads(&draft.parents)?;
        let Some(facts) = at.register_facts(&writing.identity)? else {
            return Ok(None);
        };
        Ok(Some(state(&at, |_| true, &facts, actor, draft, &target).0))
    }

    #[cfg(not(any(unix, windows)))]
    fn statement_within(
        &self,
        _: &Writing,
        _: &MemberId,
        _: &Draft,
        _: Target<'_>,
    ) -> crate::Result<Option<Statement>> {
        Err(crate::Error::new(
            "a draft that follows fewer than every head is not stated on this platform",
        ))
    }
}

/// What `draft`, which `actor` signs, states of `target` by the registers `facts` read of `log`,
/// `in_past` saying which ops lie in the draft's causal past; and what its op will be once held.
/// Of the ops it supersedes, those an earlier version-2 op of `actor` in the register already
/// names are left out; where the rest do not fit its record ([`room_for`]), it names the newest
/// that do, and a later write of `actor`'s names the others ([`writer`](self)).
fn state(
    log: &Log,
    in_past: impl Fn(&RecordId) -> bool,
    facts: &Facts,
    actor: &MemberId,
    draft: &Draft,
    target: &Target<'_>,
) -> (Statement, Pending) {
    let empty = Register::default();
    let (ops, standing, carries, pending) = match target {
        Target::Commons => (&facts.commons, &[][..], None, Pending::Commons),
        Target::Entry(key, writes) => {
            let register = facts.registers.get(*key).unwrap_or(&empty);
            let carries = match (&register.winner, writes) {
                (Some(Winner::Put { rep, writes: held }), Some(writes)) if held == writes => {
                    Some(*rep)
                }
                _ => None,
            };
            let standing: &[RecordId] = if carries.is_some() {
                &register.standing
            } else {
                &[]
            };
            let pending = Pending::Entry {
                key: (*key).clone(),
                writes: *writes,
                carries,
            };
            (&register.ops, standing, carries, pending)
        }
    };
    // What an earlier version-2 op of this signer in the register already names.
    let stated: BTreeSet<RecordId> = ops
        .iter()
        .filter_map(|id| log.get(id))
        .filter(|record| record.actor() == actor)
        .filter_map(|record| record.statement())
        .flat_map(|statement| statement.supersedes.iter().copied())
        .collect();
    // In record order, so the newest come last.
    let mut named: BTreeSet<RecordId> = BTreeSet::new();
    let mut supersedes: Vec<RecordId> = ops
        .iter()
        .filter(|id| in_past(id) && !standing.contains(id) && !stated.contains(*id))
        .filter(|id| named.insert(**id))
        .copied()
        .collect();
    let room = room_for(&draft.body, carries);
    if supersedes.len() > room {
        supersedes.drain(..supersedes.len() - room);
    }
    supersedes.sort_unstable();
    (
        Statement {
            supersedes,
            carries,
        },
        pending,
    )
}

/// How many names a statement of `body`, carrying `carries`, holds: at most
/// [`MAX_SUPERSEDES`], and as many as keep its record within [`MAX_RECORD`] beside its
/// ciphertext and the most a version-2 header takes.
fn room_for(body: &Body, carries: Option<RecordId>) -> usize {
    let bare = Statement {
        supersedes: Vec::new(),
        carries,
    };
    let Ok((clear, tail)) = super::encode_clear(body, Some(&bare)) else {
        return 0;
    };
    let taken = clear.len() + tail.len() + MOST_FRAME_TWO;
    let room = (MAX_RECORD.saturating_sub(taken) / 32).min(MAX_SUPERSEDES);
    #[cfg(test)]
    let room = MOST_NAMES.get().map_or(room, |most| room.min(most));
    room
}

#[cfg(test)]
thread_local! {
    /// Not a fault: the most names a statement drafted on this thread holds, below what its
    /// record has room for, so a test fills it with a few ops.
    pub(super) static MOST_NAMES: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
#[path = "writer_tests.rs"]
mod tests;
