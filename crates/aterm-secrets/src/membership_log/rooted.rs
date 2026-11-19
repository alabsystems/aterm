// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A vault log rooted at a final checkpoint (`docs/DESIGN-vault-log-2026-11-14.md` step 4,
//! trackers FL-148c and FL-148C2): the records [`checkpoint::plan_at`]'s prune keeps, every record outside the
//! cut, and in place of each record it drops a [`Stub`]. Nothing here signs, writes a records
//! file or chooses when to prune (FL-148d rewrites the file, FL-148f runs the pass), and nothing
//! outside the tests prunes: this lands dark.
//!
//! - **What is kept of a dropped record.** Fields of its record alone, so a stub always takes
//!   fewer bytes than its record ([`Stub::bytes`]): its id, its signer, sequence and previous
//!   record (a record that follows it is checked against them, and a draft and the next proposal
//!   follow a Mac's newest record dropped or not), its authored time (the next cut's day), its
//!   kind, epoch, the op it stands for and what its statement names as replaced (what the
//!   evaluators and a recomputed checkpoint read), the checkpoint a dropped share signs, and its
//!   parents; and a version-2 record's shed frame past its network ([`Stub::frame`]), whose
//!   SHA-256 is its id, which stands for those fields on the wire and against the byte bound.
//!   Nothing a cut says of it: whether it counts there is read at each cut, as of a record
//!   (below, "What a settled cut dropped is gone"). Its place, the held records its past ends
//!   at, is walked from the parents when the evaluators first need it ([`Virtual`]), never kept: a place is as wide as the vault's concurrency and would make a
//!   stub of a small record larger than the record. A record whose parent was dropped follows
//!   that parent's place, so every held record's causal past is its past in the whole log, less
//!   the dropped records, and the counts against each budget fall by what was dropped. A stub is
//!   made here, from a record this log held, or read from a peer with a root whose shares sign
//!   it and that this log computes from it ([`super::served`]): a checkpoint's `dropped` digest
//!   binds every field of each stub ([`Stub::encode`]), and a version-2 record's stub also checks
//!   itself against its id ([`Stub::checked`]).
//! - **Pruned only at what this log computes.** [`Log::rooted_at`] prunes only at a body this
//!   log computes itself at its heads, and [`Log::adopt`] only at one [`checkpoint::judge`] finds
//!   final. A rooted log computes and proposes the next checkpoint as the whole log does: the
//!   cut day, each seat's newest record and its last signed share read the stubs as records
//!   ([`checkpoint::propose`]), and a plan reads a stub of an op as its record, by the epoch and
//!   the names its stub keeps, so the stubs in a cut are dropped records of its prune where the
//!   whole log's drops their records, and stay stubs where it keeps them ([`Log::stubs_in_past`]):
//!   an op a kept op carries, a kept `Remove`'s cutoff, an op under an owed epoch, and an op an
//!   earlier cut keeps that a later prune dropped (a winner there that an op past that cut
//!   superseded). So a log pruned at a later root computes an earlier checkpoint as the whole log
//!   does, and holds it settled where the whole log does, its shares following the later cut or
//!   not. A prune keeps only the roots its own does not nest, so the roots are an antichain and a
//!   log pruned again and again keeps one root per side of a split; beside them it keeps each
//!   nested root whose dropped set holds a stub no root dropped (an op the later root's whole log
//!   keeps), the one digest that binds that stub, which it serves with its roots
//!   ([`Rooting::earlier`], [`super::served`]). A checkpoint whose every share a prune dropped
//!   (one an earlier checkpoint nests, outside the newest final one's chain, as where its shares
//!   follow that earlier cut) is known by its shares' stubs as by its shares: each is a version-2
//!   record's and keeps its frame, so its signer and the body it signs ([`Stub::share_body`]),
//!   and a log judges it, holds it settled and reads what it dropped as the whole log does. And a
//!   plan reads whether a checkpoint is nested in the newest final one through the stubs, a head
//!   a prune dropped included, as the whole log reads it through the records.
//! - **Never back.** [`Log::insert`] answers a dropped record [`super::Inserted::Known`], and
//!   [`Log::knows`] (what a store holds and contains) covers held and dropped alike, so a peer
//!   that still holds the whole log is never asked for, and never gives back, a dropped record.
//! - **Insert.** A parent in the dropped set is placed by its stub. A `Remove` whose cutoff was
//!   dropped is checked by the stubs: the cutoff must be the removed Mac's and lie in its causal
//!   past, which the stubs' parents and the held records' parents tell. A prune drops no `Epoch`
//!   ([`checkpoint`]'s keep set), so a `Wrap`, `Put` or `Delete` names a held one.
//! - **A record concurrent with the cut is taken** (a departure from M7, `DESIGN-alab-one-mesh`,
//!   which refuses it). M7 refused it because a log that kept only the dropped ids could not
//!   place it; the stubs place it exactly. Refusing it would refuse the other side of a split,
//!   which no rule a side applies alone tells from a Mac that slept across the cut
//!   ([`crate::commons::finality`]): two sides that each pruned at their own final cut would then
//!   never merge, and a prune of a union would not be the union of the prunes (a log that held
//!   the record before its prune keeps it).
//! - **Two sides of a split.** A log takes a root only by [`Log::rooted_at`], at a body it
//!   computes itself, from its own records or from stubs a peer served with that root
//!   ([`Log::take_served`]); a merge takes no root and no stub from the other log. So a log rooted at
//!   one side's final cut takes the other side's records as a whole log does, placing those that
//!   follow its own dropped records by their stubs, and takes back none of them; it computes the
//!   other side's checkpoint as the whole log does, reading at their stubs the records its own
//!   prune dropped and the other side's keeps, adopts it, and then has no newest root
//!   ([`Log::newest_root`]) until it adopts a checkpoint whose cut nests both.
//!
//! - **What the evaluators read of a stub.** A `Remove` whose cutoff a prune dropped (a
//!   sleeper's, or a share of an undo at a [`Log::undo_cutoff`] behind the cut) counts as in the
//!   whole log: the membership evaluators place the cutoff at the stub's own index past every
//!   held one's ([`VIRTUAL`], [`Virtual`]), a held record lying in its past where it lies in the
//!   past of the stub's place, and the stub's signer and sequence order the quorums. An undo's
//!   cutoff may be a dropped record of its member's, read off the stubs as the whole log reads
//!   it off the records ([`super::eval`]). What a removed Mac reads past its cutoff places a
//!   dropped cutoff and a dropped winner's rep the same way, with the rep's signer and epoch
//!   from its stub. A keyed epoch's dependents (F21) include its dropped ops, whose signer and
//!   place the stubs give, so its holders and the coverage of a newer keyed epoch are the whole
//!   log's.
//! - **Statements, in every log.** Every log, whole or rooted, reads a version-2 `Put` or
//!   `Delete` by its statement ([`Log::stated`]), and a version-1 op by the frontier the held ops
//!   give it, so which ops a prune dropped never decides how an op that names them reads. A name
//!   counts only where the frames show it could be true, which the frames and stubs tell alike in
//!   every log: an op of the namer's audience in its past, for a carry a `Put`. That is
//!   [`checkpoint`]'s naming rule less two of its conditions, that the namer counts and holds its
//!   own epoch: both change as the log grows (a later `Remove` voids a namer or the `Wrap` that
//!   gave it its key), while a reading is kept in a dropped `Delete`'s stub ([`Stub::carries`])
//!   and so must be one no later record changes. A voided namer's reading matters only where it
//!   counts, which the evaluators decide, and a name that supersedes ([`checkpoint`]'s keep set)
//!   asks both. A `Put` that
//!   states a carry stands for what the op it names stands for, through a dropped `Put`'s stub to
//!   the op its value came from ([`Stub::carries`], each step's name checked against its own
//!   namer); a `Delete` reads the newest ops it names, sets aside each carry another of them
//!   replaced (its rep before a named op of another class), and is a carry of the winner among
//!   what is left where that is `Delete`s alone, which a dropped `Delete`'s stub keeps
//!   ([`Log::frame_reps`]). The register takes the stated rep as it is, a dropped op's or one
//!   this reader did not open. A statement that names nothing it could reads as stating nothing
//!   (an edit, or a `Delete` that writes), and one that misstates what its writer read (claims a
//!   carry of a value it does not seal, or no carry of one it does) reads as it says, in every
//!   log alike: no log refuses a record another log holds, and a false statement reaches only an
//!   entry of its writer's audience, which its writer could write anyway.
//! - **A version-1 op beside a cut reads by its counting frontier, in every log.** It states
//!   nothing, so it reads by the counting ops newest in its past ([`super::entries`]'s
//!   register), which in a rooted log lack every op a prune dropped. In the log's own
//!   evaluation they lack them in the whole log too: a log prunes only at a final checkpoint
//!   ([`Log::adopt`]) or, for an earlier root a peer served, a settled one ([`super::served`]),
//!   so every root is settled in each log that holds its shares, and what it dropped counts in
//!   none (below). So a version-1 op beside the cut reads alike in the rooted log and in the
//!   whole log of its records, whether it came before the prune or after, and a stub reads as no
//!   op of a register in either. Not so at an earlier cut that does not hold the root settled
//!   (`E`'s, its shares past it, in a log rooted at `L`): there the dropped op counts, the whole
//!   log's frontier of a version-1 op in that cut holds it and the rooted log's does not. That
//!   reading reaches no checkpoint: a plan reads no register's frontier ([`checkpoint`]'s keep
//!   set reads what each version-2 statement names and carries, a `Remove`'s cutoff, a signer's
//!   newer op of the same register, an owed epoch, which records count and the Commons' newest
//!   counting `Put`s, and its body's register frontier is the ops no counting version-2 op
//!   names), so the earlier checkpoint computes, judges and
//!   settles alike in both logs, as `rooted_tests.rs`'s chain law asserts over the version-1 ops
//!   of that kind its random vaults write. A plan that read a version-1 frontier would split
//!   them, and must first read it through the stubs. Decided over the two other ways out.
//!   Fixing its reading at the prune cannot serve a record taken later, which no prune saw.
//!   Refusing, or parking, a version-1 op beside a cut would refuse a record the whole log
//!   takes, so a prune of a union would no longer be the union of the prunes, and would lose
//!   honest history: a Mac of a build before FL-148E's fence that slept across the cut signs
//!   version 1, and FL-148F's writers' version 2 does not reach it. The cost, alike in every
//!   log: such an op over an op a settled cut dropped reads without it, so a reseal of the
//!   dropped op's value is an edit beside the op that superseded it (`a.key` in conflict) where,
//!   without the prune's rule, it would be a carry that loses; a version-2 op states its carry
//!   and is not exposed (`rooted_tests.rs`'s
//!   `vault_checkpoint_a_version_one_op_beside_a_cut_reads_as_the_whole_log`).
//! - **What a settled cut dropped is gone.** A checkpoint is settled in a log that recomputes it
//!   and holds shares of it, or their stubs, signed by more than half of its cut's recent seats,
//!   whether or not they still count ([`checkpoint::settled`]): unlike finality, which a later
//!   `Remove` undoes by un-counting a share, it never stops holding, as a prune needs. A record a
//!   settled checkpoint drops counts in no log ([`checkpoint::gone`]); a log pruned there holds
//!   only its stub, and a later prune drops it again and keeps nothing for its sake. No stub
//!   says it is gone: each cut reads it so where a checkpoint settled in that cut drops it, by
//!   the shares that cut holds or dropped, as the whole log reads its record. A flag a prune set
//!   (FL-148C's `Stub::gone`) read a stub a later cut held gone as gone in an earlier cut too,
//!   where the checkpoint that made it gone is not settled (its shares lie between the two
//!   cuts): there the earlier checkpoint's plan keeps it, for a carry or a cutoff, and a log
//!   pruned later computed that checkpoint otherwise than the whole log. Naming that checkpoint
//!   in the stub would not do either: several settled checkpoints may drop one stub, settled
//!   in different cuts, and a cut reads it gone where any of them is settled. So the log keeps
//!   what decides it, every share of every checkpoint, held or as a stub (a log forgets no
//!   stub), and each share's body ([`Stub::share_body`]); in the log rooted at a settled cut
//!   every stub is gone, its root (and each earlier root) settled there. So a `Remove`, or a quorum, at a cutoff
//!   behind a final cut revives nothing the prune dropped: it still takes its member out and
//!   stops counting its records past the cutoff, those in the cut too, and an op one of them
//!   superseded stays dropped in every log, its entry unset until set again, as the design
//!   accepted (its section 5). A name supersedes only an op of its own signer
//!   ([`checkpoint`]'s keep set), so what stays dropped is the removed Mac's own: a value a write
//!   of its key replaced, never another seat's. The rule chosen over a clamp of the cutoff to the
//!   cut, which would keep a removed Mac's records in the cut counting: it only ever stops records
//!   counting, never makes one count that a `Remove` took out, and only records the cut's own plan
//!   drops (an op its own signer's newer op superseded, older shares; never a winner, a sibling or
//!   a weighed record). Settledness is as weak as finality's recent seats and weaker in one way,
//!   which is what it costs to never stop holding: a seat alone at its cut settles it (a side of a
//!   split, a vault whose other seats are dead, or a key that signs its own head with a clock
//!   running ahead so it alone is recent), and a share a key signed past its seat's cutoff still
//!   settles. A key stolen and then removed can so make final, at once and after its removal, the
//!   loss of its own seat's values that its writes superseded: what the design accepts behind a
//!   final cut, and no more, since every Mac that holds the cut recomputes the plan. And a
//!   checkpoint stops being final once one that nests it settles, whose prune drops all but one
//!   of its shares ([`checkpoint::judge`] counts no gone share): a log adopts the newer one.
//! - **Heads.** A dropped record nothing held or dropped follows (a quiet seat's share of an
//!   older checkpoint) stays a head, and no record a stub names as a parent is one, so the heads
//!   and their digest are the whole log's; a draft names such a stub as a parent as the whole
//!   log's would. The heads APIs read a dropped head by its place ([`Log::reaches`],
//!   [`Log::project_heads`], which keeps the stubs in the heads' past and their heads).
//! - **Bounds.** A stub's bytes ([`Stub::bytes`], fewer than its record's) count against the
//!   byte bound and the memory footprint with the held records', so a rooted log holds no more
//!   than the whole log was allowed, and a log at its byte bound frees room by a prune. A
//!   version-2 record's stub counts its frame in place of its fields, still fewer bytes than its
//!   record, whole or shed, but more than its fields alone: what a receiver that does not hold
//!   the cut checks it by ([`super::served`]). So a prune frees less of a version-2 op than of a
//!   version-1 one (its ciphertext alone), and almost nothing of a share, whose frame is its
//!   checkpoint's body, every head of the cut named: the cost of a share's stub a receiver
//!   checks against its signer, chosen over a stub that names its checkpoint once for all its
//!   shares, whose fields would stand on the shares' word as a version-1 stub's do. The roots' bodies, the earlier roots kept beside them, the fields read
//!   off a version-2 stub's frame and what the evaluators read of the stubs ([`Virtual`]) count
//!   in the memory footprint alone: each root's body is a held share's body, and the evaluators' reading is a
//!   cache of the stubs, linear in them. No stub counts against a count budget: the counts bound
//!   what is weighed, opened and served per record, and a stub is none of these, and a log full
//!   by count frees room by a prune.
//!
//! Settled since (FL-148D), so no longer waited on: a store's bytes are bounded by its format's
//! capacity, the storage class, apart from one reply's, the wire class
//! ([`crate::crossing::capacity`], [`super::REPLY_CAP`]); the legacy fence counts the records a
//! root dropped ([`Log::known_count`]: aterm-db's legacy exchange is told of a rooted store's
//! stubs with its records, [`super::Store::complete_inventory`], so a store within the legacy
//! count of held records and past it with its stubs is fenced); and the mesh egress's members
//! cursor, a position in the held records that a prune shifts, is kept under the rewrite
//! generation (its number and the rewrite that made it) and placed again when it differs,
//! just past the last record it stepped past, which it knows by the record's id, the same
//! whole and shed (`crate::client::Paths::records_generation`, [`super::record_id`],
//! `aterm_mesh::egress::MembersCursor`).
//!
//! Not yet (each with the row that owns it, which FL-148F's pass waits on):
//!
//! - **Consumers that read records by id or position** (FL-148E). The set a reconciling peer
//!   compares ([`super::Store::complete_inventory`]) names a dropped record as known, and a
//!   cursor at one starts again at genesis ([`super::Store::complete_missing`]). Not yet read as
//!   dropped: [`super::entries`]'s winning ops and carried reps, which may name a dropped op (the
//!   evaluators place it; a consumer that opens its ciphertext does not);
//!   `legacy_applied_held`, whose search for the applied heads walks held
//!   records alone, so a log that dropped one reads it absent; and the mesh's
//!   members unit and aterm-db's records exchange (`Store::missing`, `complete_missing`), which
//!   send records alone, so a peer behind the cut is sent, on every round, records that wait, and
//!   its summary never matches. Their own crates' tests need a rooted store, which only this
//!   crate's tests build. (A member's join bundle hands its roots after its records,
//!   `crate::client::admit`'s `held_records`.) (The mesh's
//!   members unit carries no rooted store's records: a rewritten file is format 2's, and a Mac
//!   past format 1 leaves the unit out, `crate::client::members_lane_open`. A write's hook now
//!   says which generation the records it hands over are of, [`super::store::Written`], and
//!   the mesh egress takes them under it.)
//! - **The storage sites that read this build's format** (FL-148E, the row that first stores
//!   a format mark: a store past format 1 exists from then on, and each of these sites must
//!   read it by then). A store, its log and its file read their bytes' bound from the store's
//!   own format ([`super::Store::capacity`], [`Log::capacity`]), and a site that has a store
//!   at hand reads and decodes by it. A Mac that follows a format-2 vault (`super::follow`)
//!   reads at the format its mark names: its store, its records cache (a rewritten layout
//!   and its frame included), the frames its records pass pulls, a member's bundle and a
//!   seat's records; so do aterm-db's catalog limit (the inspected log's own capacity) and
//!   aterm-gui's bound of the file (the mark's format). The sites with no store at hand (the
//!   held drafts, the writes that wait for a key, the authoring stops, the most a store's
//!   footprint reaches) name [`super::Reading::STORE_BYTES`], which is every format's capacity
//!   while a store is one reply and stops the build once it is not (FL-249). A vault server
//!   reads its records file, a POST's body and the frames its peer pull reads at its mark's
//!   format too (`server::Shared::records_reading`). (The first transition's layout and
//!   aterm-db's legacy wires are format 1's by what they are, and name format 1.) Not yet:
//!   the readers that name the records entry's data file without a store select it as format
//!   1 does and refuse a rewritten layout (`crate::client::Paths::records_data_file`, the
//!   vault memo's and the seat ask's standing, `client::genesis`'s private-realm read,
//!   `client::remove`'s look, aterm-db's moved-home check, aterm-agent's fleet log and
//!   census): on a Mac whose entry a rewrite made they fail or read nothing, and change
//!   nothing.
//! - **A served reply past one reply's bytes** (FL-249). A root's reply holds every record the
//!   peer lacks and the frame, which a prefix cannot replace ([`super::Store::missing_served`]).
//!   It is the held records the peer lacks and the stubs, both counted in the store's bytes,
//!   and the frame's head, counted in none (42 bytes and 32 for each root, at most
//!   [`super::served::MAX_SERVED_HEAD`], 8,234). While a store's capacity is one reply's
//!   ([`crate::crossing::FORMAT_TWO_BYTES`]) it therefore passes [`super::REPLY_CAP`] only
//!   from a store within that head of full to a peer that lacks nearly all it holds: the
//!   store then serves records alone, capped, and the peer waits on another that holds the
//!   cut or on this store's next prune, which frees room; nothing is lost or taken in part.
//!   It is not closed here: reserving the head in a rooted log's byte bound would put a log
//!   whose prune freed less than the head over its bound, and the wire's bound does not move.
//!   Once a store's capacity is past a reply (FL-249) every such reply passes it, and closing
//!   it needs a pull that names the records it holds waiting, so a reply need not carry them
//!   again and a root is served with the prefix that settles it: that row owns it.
//! - **Taking a served root into a vault server's file** (FL-148E). A Mac's records pass and
//!   a new Mac's join take a served root into a rewritten records file (`super::follow`'s
//!   "Served roots": a pull that reads format 2 asks for the frame,
//!   [`crate::transport::RECORDS_SERVED_HEADER`], and a rooted store is written through
//!   `crate::records_storage::rewrite` and read back from [`super::stored`]'s file). A vault
//!   server's peer pull still asks for none and reads records alone: its records file is one
//!   flat file with no rewritten form, so a server behind a cut holds what follows the cut
//!   waiting (`super::follow`'s "Not yet" names each site left of that row).

use super::checkpoint::{self, Plan};
use super::{Body, EpochAudience, Kind, Log, MemberId, Record, RecordId};
use crate::{Error, Result};
use aterm_mesh_core::convergence::beats;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, OnceLock};

/// A broken rooted log, for the tests' negative controls.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Mutant {
    /// A stub resolves to no held record: a record that follows a dropped record resolves
    /// nowhere.
    NoPlace,
    /// Insert takes a dropped record that arrives again.
    TakeBack,
    /// M7 over a dropped-id set alone: insert, outside a prune, refuses a record with a dropped
    /// parent that does not follow every root's cut, while a prune keeps what the whole log held.
    RefuseConcurrent,
    /// A prune keeps no stub as a head: a dropped record nothing follows leaves its place's held
    /// records as heads, as slice 1 did.
    NoStubHeads,
    /// The evaluators read a `Remove` whose cutoff a prune dropped as slice 1 did: as no
    /// `Remove` at all, and an undo's cutoff from the held records alone.
    HeldCutoffs,
    /// Every log reads every op by its frontier, a version-2 one too, as before this slice: a
    /// rooted log by the held ops of a frontier that lost a dropped op.
    FrontierOnly,
    /// A rooted log reads a version-2 op by its statement and a whole log by its frontier, as
    /// slice 2 read them where a register lost an op to the prune.
    RootedStatementsOnly,
    /// The stubs count against no budget.
    FreeStubs,
    /// The legacy fence counts held records alone ([`Log::known_count`]), as before FL-148D:
    /// not the records a root dropped, which a set-reconciling exchange names all the same.
    FenceHeldAlone,
    /// The register reads a statement naming a dropped `Put` as its carry as standing for that
    /// `Put`: a stub's own `carries` is not read.
    StubCarriesUnread,
    /// A statement's names count whatever they name: an op of another audience or kind, or a
    /// record that is no op.
    AnyName,
    /// A dropped `Delete` stands for itself: its stub keeps no rep.
    DeleteStandsForItself,
    /// A `Delete`'s statement reads every op it names among the newest as live, a carry another
    /// of them replaced included.
    StatedCarriesLive,
    /// A dropped share signs nothing: its stub keeps no checkpoint.
    SharesUnkept,
    /// A record a settled checkpoint's prune dropped counts as its evaluation alone says: a
    /// `Remove` behind a final cut revives in the whole log an op the rooted log dropped.
    Revives,
    /// A stub counts the place slice 2 kept with it: the held records it resolves to.
    StoredPlace,
    /// A prune keeps every earlier root, nested in its cut or not.
    KeepNestedRoots,
    /// The keyed epochs read their dependents among the held records alone.
    KeyedHeldOnly,
    /// A next checkpoint is proposed from the held records alone, as a whole log of them would.
    HeldProposal,
    /// A rooted log's next prune drops every stub in its cut, those the whole log's keeps too.
    DropEveryStub,
    /// A log's heads APIs read the held records alone: a dropped head reaches nothing.
    HeldIndexes,
    /// A plan keeps a dropped op only for a kept op's carry, a kept `Remove`'s cutoff or an owed
    /// epoch, as slice 3 first did: a stub is superseded for good, so a log a later prune cut
    /// cannot compute an earlier checkpoint that kept what that prune dropped.
    StubsDroppedForGood,
    /// A prune keeps no root it nests, as FL-148c did: a log pruned twice forgets the earlier
    /// root whose digest binds a stub the later root's whole log keeps, and serves no root.
    NoEarlierRoots,
    /// A prune at a root a root of the log already nests keeps it among the roots, as this
    /// slice's first cut did: a Mac that takes a later root first holds two nested roots.
    NestedAsRoot,
    /// A stub reads gone wherever the cut of a prune that made it, or one after, held it gone,
    /// as FL-148C's `Stub::gone` flag kept it: in an earlier cut too, where the checkpoint that
    /// made it gone is not settled ([`LAST_CUT_GONE`]).
    GoneFromTheLastCut,
    /// No dropped share's frame is read: a checkpoint every share of which a prune dropped,
    /// and that is no root, has no body, as before this slice.
    ShareBodiesUnread,
    /// A plan reads whether a checkpoint's cut is nested in the newest final one's among the
    /// held records alone, as before this slice: a checkpoint whose head a prune dropped is
    /// never nested, and the plan keeps every share of it.
    HeldNesting,
}

#[cfg(test)]
thread_local! {
    /// The mutant this thread's rooted logs are built and read as, if any.
    pub(super) static MUTANT: std::cell::Cell<Option<Mutant>> = const { std::cell::Cell::new(None) };
    /// Whether this thread is inside [`Log::pruned`].
    static PRUNING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// Under [`Mutant::GoneFromTheLastCut`], every record the cut of a prune on this thread held
    /// gone ([`checkpoint::gone_in_cut`]): what FL-148C's stubs said of themselves.
    pub(super) static LAST_CUT_GONE: std::cell::RefCell<BTreeSet<RecordId>> =
        const { std::cell::RefCell::new(BTreeSet::new()) };
}

#[cfg(test)]
pub(super) fn mutant(which: Mutant) -> bool {
    MUTANT.get() == Some(which)
}

/// A record a prune dropped, as a rooted log keeps it: fields of its frame alone, so every Mac
/// that drops it makes the same stub, and each held in the record itself, so a stub never takes
/// more than its record's wire bytes ([`Stub::bytes`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stub {
    /// Its signer.
    pub actor: MemberId,
    /// Its signer's sequence.
    pub sequence: u64,
    /// Its signer's previous record, one of its parents: a seat's newest record, as a next
    /// checkpoint's proposal reads it, is one no record of the seat names so.
    pub previous: Option<RecordId>,
    /// Its signer's clock: whether its seat was heard within a later cut's window.
    pub authored_ms: u64,
    /// Its kind.
    pub kind: Kind,
    /// What its register's resolve reads it as standing for, from its version-2 statement: for
    /// a `Put`, the op it states it carries the value of (its semantic representative,
    /// [`super::checkpoint`]'s carries rule), as stated; for a `Delete` over `Delete`s alone, the
    /// op their winner stands for, read off the names of its statement when it was dropped
    /// ([`Log::frame_reps`]). `None` for an edit, a version-1 op and any other kind.
    pub carries: Option<RecordId>,
    /// The epoch a dropped `Put` or `Delete` was sealed under, from its frame; `None` for any
    /// other kind. A prune drops no `Epoch`, so the op's audience is the held epoch's.
    pub epoch: Option<RecordId>,
    /// What a dropped version-2 `Put`'s or `Delete`'s statement names as replaced; empty for any
    /// other. A checkpoint whose cut holds it reads its names as the whole log reads its record's
    /// ([`checkpoint`]'s `Ops`): a log a later prune cut computes an earlier checkpoint, whose
    /// prune kept ops that later one dropped, as the whole log does.
    pub supersedes: Vec<RecordId>,
    /// The checkpoint a dropped share signs ([`checkpoint::checkpoint_id`] of its body); `None`
    /// for any other kind. What a later cut's judgment counts of the share
    /// ([`checkpoint::judge`]): its signer, and whether it counts, read at the stub; and, read
    /// off its frame ([`Stub::share_body`]), the body it signs, so a checkpoint whose every
    /// share a prune dropped is known as where its shares are held.
    pub shares: Option<RecordId>,
    /// Its parents. Where a record that follows it sits, the newest held records in its past,
    /// is read off them ([`Log::placed`]), not kept.
    pub parents: Vec<RecordId>,
    /// A version-2 record's shed frame past its network ([`super::Record::shed_frame`] from its
    /// kind byte on): the record's id is the SHA-256 of the record magic, version 2, the
    /// network and these bytes, so a Mac that does not hold the record checks every other field
    /// but a `Delete`'s `carries` against its id and its signer's signature ([`Stub::checked`]),
    /// and that `carries` by reading the statement again ([`Log::stub_delete_rep`]). `None` for
    /// a version-1 record, whose id hashes its ciphertext too: its stub's fields stand on the
    /// word of the shares that sign the digest naming it.
    pub frame: Option<Arc<[u8]>>,
}

/// The tag [`Stub::encode`] writes ahead of a version-1 record's stub; a version-2 record's
/// stub begins with [`super::VERSION_TWO`].
const STUB_ONE: u8 = 1;

impl Stub {
    /// The stub a prune makes of the held `record`, carrying `carries`: each field read off the
    /// record, and a version-2 record's shed frame.
    pub(super) fn of(record: &Record, carries: Option<RecordId>) -> Self {
        Self {
            actor: *record.actor(),
            sequence: record.sequence(),
            previous: record.previous().copied(),
            authored_ms: record.authored_ms(),
            kind: record.kind(),
            carries,
            epoch: match record.body() {
                Body::Put { epoch, .. } | Body::Delete { epoch, .. } => Some(*epoch),
                _ => None,
            },
            supersedes: record
                .statement()
                .map(|statement| statement.supersedes.clone())
                .unwrap_or_default(),
            shares: checkpoint::checkpoint_id(record.body()),
            parents: record.parents().to_vec(),
            frame: record
                .shed_frame()
                .and_then(|frame| frame.get(super::KIND_AT..).map(Arc::from)),
        }
    }

    /// The record `frame` (a version-2 shed frame past its network, [`Stub::frame`]) is under
    /// `network`, read as `reading` reads one, and its signature verified where `signed` asks
    /// ([`Record::decode_in`]; else [`Record::unsigned`]), and its stub, carrying `carries` where
    /// it is a `Delete` (the one field no frame holds) and what its statement says it carries
    /// otherwise.
    ///
    /// # Errors
    /// The bytes are no version-2 record of `network` that `reading` reads, or, where `signed`
    /// asks, its signer did not sign it.
    fn from_frame(
        network: &RecordId,
        frame: &[u8],
        carries: Option<RecordId>,
        reading: super::Reading,
        signed: bool,
    ) -> Result<(RecordId, Self)> {
        let whole = Self::whole_frame(network, frame);
        let record = if signed {
            Record::decode_in(&whole, reading)?
        } else {
            Record::unsigned(&whole, reading)?
        };
        let carries = match record.kind() {
            Kind::Delete => carries,
            _ => record.statement().and_then(|statement| statement.carries),
        };
        Ok((*record.id(), Self::of(&record, carries)))
    }

    /// The shed frame of the version-2 record whose frame past its network is `frame`, under
    /// `network`: the record magic, version 2, the network and `frame`.
    fn whole_frame(network: &RecordId, frame: &[u8]) -> Vec<u8> {
        let mut whole = Vec::with_capacity(super::KIND_AT + frame.len());
        whole.extend_from_slice(super::MAGIC);
        whole.push(super::VERSION_TWO);
        whole.extend_from_slice(network);
        whole.extend_from_slice(frame);
        whole
    }

    /// Whether this stub of `id`, read from a peer of `network`, is its record's: a version-2
    /// record's stub is checked field by field against the record its frame is, signature and
    /// all, whose id must be `id` (but a `Delete`'s `carries`, [`Log::stub_delete_rep`]); a
    /// version-1 record's has no frame and is not checked here, but for its kind: no version-1
    /// record is a share. The frame was read under its reader's format where its bytes were
    /// ([`Stub::decode`]); held in memory it is read as [`Log::insert`] takes a record, of any
    /// layout.
    ///
    /// # Errors
    /// Its frame is no record of `network`, is unsigned, is another record's, or says another
    /// field; or it keeps no frame and is a share's.
    pub(super) fn checked(&self, id: &RecordId, network: &RecordId) -> Result<()> {
        let Some(frame) = &self.frame else {
            // A share is a version-2 record's: no version-1 record is a `Checkpoint`.
            if self.kind == Kind::Checkpoint {
                return Err(Error::new("a served share's stub keeps no frame"));
            }
            return Ok(());
        };
        let (of, stub) =
            Self::from_frame(network, frame, self.carries, super::Reading::Follow, true)?;
        if of != *id || stub != *self {
            return Err(Error::new("a served stub is not the record its frame is"));
        }
        Ok(())
    }

    /// The body a dropped share signs, read off its frame under `network` (every share is a
    /// version-2 record, whose stub keeps its frame, and a served one is checked against its
    /// id, [`Stub::checked`]): what a log knows a checkpoint by where a prune dropped every
    /// share of it ([`checkpoint::settled`]). `None` for any other stub.
    pub(super) fn share_body(&self, network: &RecordId) -> Option<Body> {
        self.shares?;
        #[cfg(test)]
        if mutant(Mutant::ShareBodiesUnread) {
            return None;
        }
        let whole = Self::whole_frame(network, self.frame.as_ref()?);
        let record = Record::unsigned(&whole, super::Reading::Follow).ok()?;
        (checkpoint::checkpoint_id(record.body()) == self.shares).then(|| record.body().clone())
    }

    /// Whether it is a version-2 `Delete`'s stub: the one whose encoding holds `carries`
    /// beside its frame, read off the frame's kind byte.
    fn frames_a_delete(&self) -> bool {
        self.frame
            .as_ref()
            .is_some_and(|frame| frame.first() == Some(&Kind::Delete.code()))
    }

    /// The bytes a stub's fields take: its id and every field, an absent one as its tag alone.
    /// What a version-1 record's stub counts after its tag ([`Stub::bytes`]), and what a
    /// version-2 record's keeps in memory beside its frame, read off it ([`Stub::read_bytes`]).
    fn fields_bytes(&self) -> usize {
        let id = size_of::<RecordId>();
        let some = |field: &Option<RecordId>| 1 + field.map_or(0, |_| id);
        id + size_of::<MemberId>()
            + size_of::<u64>()
            + some(&self.previous)
            + size_of::<u64>()
            + 1
            + some(&self.carries)
            + some(&self.epoch)
            + 2
            + self.supersedes.len() * id
            + some(&self.shares)
            + 2
            + self.parents.len() * id
    }

    /// The bytes it takes in place of its record, which count against the byte bound
    /// ([`Log::passed_by`]): its encoding ([`Stub::wire_len`]). A version-1 record's stub is its
    /// tag, id and fields: the record holds each field but the id, plus its network, its
    /// signature (longer than the id) and its body. A version-2 record's is its tag and frame
    /// past the network (36 bytes less than its shed frame) and a `Delete`'s `carries` (at most
    /// 33): at least 3 bytes fewer than its shed frame, which is the whole of a `Checkpoint`
    /// share. So a stub always takes fewer bytes than the record it stands for, whole or shed.
    #[must_use]
    pub fn bytes(&self) -> usize {
        self.wire_len()
    }

    /// The bytes a version-2 record's stub keeps in memory beside its frame: the fields read
    /// off it ([`Stub::fields_bytes`]), which count in the memory footprint alone, as what the
    /// evaluators read of the stubs does ([`Log::footprint`]). `0` for a version-1 record's.
    pub(super) fn read_bytes(&self) -> usize {
        if self.frame.is_some() {
            self.fields_bytes()
        } else {
            0
        }
    }

    /// The stub of `id` in its one encoding, [`Stub::wire_len`] long: what a checkpoint's
    /// `dropped` digest hashes ([`checkpoint::dropped_digest`]) and a rooted log serves
    /// ([`super::served`]). Every number is big-endian, and every field is bound: a stub keeps
    /// what its record says alone, never whether it counts in some cut, which each cut reads
    /// for itself ([`checkpoint::gone`]).
    ///
    /// A version-1 record's stub:
    ///
    /// | bytes | field |
    /// |---|---|
    /// | 1 | `1` |
    /// | 32 | its id |
    /// | 48 | its signer |
    /// | 8 | its sequence |
    /// | 1, then 32 | its previous record: `0`, or `1` and the id |
    /// | 8 | its authored clock |
    /// | 1 | its kind, as its record's header writes it |
    /// | 1, then 32 | what it carries, as `previous` |
    /// | 1, then 32 | its epoch, as `previous` |
    /// | 2, then 32 each | what its statement names as replaced |
    /// | 1, then 32 | the checkpoint a share signs, as `previous` |
    /// | 2, then 32 each | its parents |
    ///
    /// A version-2 record's stub: `2`, its frame ([`Stub::frame`]: every field but its id,
    /// which hashes the frame, and a `Delete`'s `carries`), and for a `Delete` what it carries,
    /// as `previous`.
    pub fn encode(&self, id: &RecordId, out: &mut Vec<u8>) {
        let some = |out: &mut Vec<u8>, field: &Option<RecordId>| match field {
            Some(id) => {
                out.push(1);
                out.extend_from_slice(id);
            }
            None => out.push(0),
        };
        if let Some(frame) = &self.frame {
            out.push(super::VERSION_TWO);
            out.extend_from_slice(frame);
            if self.frames_a_delete() {
                some(out, &self.carries);
            }
            return;
        }
        let list = |out: &mut Vec<u8>, ids: &[RecordId]| {
            out.extend_from_slice(&u16::try_from(ids.len()).unwrap_or(u16::MAX).to_be_bytes());
            for id in ids {
                out.extend_from_slice(id);
            }
        };
        out.push(STUB_ONE);
        out.extend_from_slice(id);
        out.extend_from_slice(&self.actor);
        out.extend_from_slice(&self.sequence.to_be_bytes());
        some(out, &self.previous);
        out.extend_from_slice(&self.authored_ms.to_be_bytes());
        out.push(self.kind.code());
        some(out, &self.carries);
        some(out, &self.epoch);
        list(out, &self.supersedes);
        some(out, &self.shares);
        list(out, &self.parents);
    }

    /// The bytes [`Stub::encode`] writes.
    #[must_use]
    pub fn wire_len(&self) -> usize {
        match &self.frame {
            Some(frame) => {
                let carries = if self.frames_a_delete() {
                    1 + self.carries.map_or(0, |_| size_of::<RecordId>())
                } else {
                    0
                };
                1 + frame.len() + carries
            }
            None => 1 + self.fields_bytes(),
        }
    }

    /// One stub [`Stub::encode`] wrote, read off `bytes` at `at`, which it moves past it. A
    /// version-2 record's is read as the record
    /// its frame is under `network`, as `reading` reads one, its id that record's: a reader of
    /// format 1 refuses it as it refuses that record whole ([`Record::decode_in`]). Its
    /// signature is not verified here: a receiver verifies it once, where it reads the stub
    /// ([`Stub::checked`]), and never for a stub it ignores (of a record it holds or dropped).
    ///
    /// # Errors
    /// The bytes end first, a tag or the kind is no value [`Stub::encode`] writes, a list is
    /// longer than a record's (`MAX_SUPERSEDES` names, `MAX_PARENTS` parents), or a frame is no
    /// shed version-2 record of `network` that `reading` reads.
    pub(super) fn decode(
        bytes: &[u8],
        at: &mut usize,
        network: &RecordId,
        reading: super::Reading,
    ) -> Result<(RecordId, Self)> {
        let mut read = Cursor { bytes, at };
        match read.take(1)?[0] {
            STUB_ONE => {}
            super::VERSION_TWO => {
                let start = *read.at;
                let kind = read.take(1 + 48 + 8 + 32)?[0];
                let count = usize::from(read.take(1)?[0]);
                if count > super::MAX_PARENTS {
                    return Err(Error::new("a served stub names more than a record can"));
                }
                read.take(count * 32 + 8)?;
                let clear = read.u32()?;
                if read.u32()? != 0 {
                    return Err(Error::new("a served stub's frame is a shed frame"));
                }
                read.take(usize::try_from(clear).unwrap_or(usize::MAX))?;
                read.take(64)?;
                let frame = &bytes[start..*read.at];
                let carries = if kind == Kind::Delete.code() {
                    read.some()?
                } else {
                    None
                };
                #[cfg(test)]
                let reading = if super::served::mutant(super::served::Mutant::StubsFollow) {
                    super::Reading::Follow
                } else {
                    reading
                };
                return Self::from_frame(network, frame, carries, reading, false);
            }
            _ => return Err(Error::new("a served stub is of no record version")),
        }
        let id = read.id()?;
        let actor: MemberId = read
            .take(48)?
            .try_into()
            .map_err(|_| Error::new("a served stub is truncated"))?;
        let sequence = read.u64()?;
        let previous = read.some()?;
        let authored_ms = read.u64()?;
        let kind = Kind::from_code(read.take(1)?[0])?;
        let carries = read.some()?;
        let epoch = read.some()?;
        let supersedes = read.list(super::MAX_SUPERSEDES)?;
        let shares = read.some()?;
        let parents = read.list(super::MAX_PARENTS)?;
        Ok((
            id,
            Self {
                actor,
                sequence,
                previous,
                authored_ms,
                kind,
                carries,
                epoch,
                supersedes,
                shares,
                parents,
                frame: None,
            },
        ))
    }
}

/// A reader of what [`Stub::encode`] and [`super::served`] write.
pub(super) struct Cursor<'a, 'b> {
    pub(super) bytes: &'a [u8],
    pub(super) at: &'b mut usize,
}

impl<'a> Cursor<'a, '_> {
    pub(super) fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self
            .at
            .checked_add(n)
            .filter(|end| *end <= self.bytes.len())
            .ok_or_else(|| Error::new("a served stub is truncated"))?;
        let taken = &self.bytes[*self.at..end];
        *self.at = end;
        Ok(taken)
    }

    pub(super) fn id(&mut self) -> Result<RecordId> {
        self.take(32)?
            .try_into()
            .map_err(|_| Error::new("a served stub is truncated"))
    }

    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_be_bytes(
            self.take(4)?
                .try_into()
                .map_err(|_| Error::new("a served stub is truncated"))?,
        ))
    }

    pub(super) fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_be_bytes(
            self.take(8)?
                .try_into()
                .map_err(|_| Error::new("a served stub is truncated"))?,
        ))
    }

    fn some(&mut self) -> Result<Option<RecordId>> {
        match self.take(1)?[0] {
            0 => Ok(None),
            1 => Ok(Some(self.id()?)),
            _ => Err(Error::new("a served stub's field has no such tag")),
        }
    }

    /// A count of `width` bytes, at most `most`, then that many ids.
    pub(super) fn ids(&mut self, width: usize, most: usize) -> Result<Vec<RecordId>> {
        let mut count = [0u8; 8];
        count[8 - width..].copy_from_slice(self.take(width)?);
        let count = usize::try_from(u64::from_be_bytes(count)).unwrap_or(usize::MAX);
        if count > most {
            return Err(Error::new("a served stub names more than a record can"));
        }
        (0..count).map(|_| self.id()).collect()
    }

    fn list(&mut self, most: usize) -> Result<Vec<RecordId>> {
        self.ids(2, most)
    }
}
/// What every log's register reads of a version-2 op from its statement ([`Log::stated`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Stated {
    /// An op that writes a value of its own, or a `Delete` that writes.
    Edit,
    /// A carry: an op that seals again what the op it names wrote (a `Put`'s stated carry, or
    /// the winner among the `Delete`s a `Delete` read as live), standing for that op's rep.
    Carry(RecordId),
}

/// Where the evaluators place the stub at index `k` of [`Virtual`]: past every held record's
/// index and below the `usize::MAX` the weighing marks no record with.
pub(super) const VIRTUAL: usize = 1 << (usize::BITS - 2);

/// The stubs of a rooted log as its evaluators read them ([`super::eval`]): the stub at `k` (in id
/// order) stands at index [`VIRTUAL`]` + k` beside the held records, with its signer, sequence,
/// the held records it resolves to, and the stubs among its parents and children. A log only
/// grows and a stub resolves to held records, so it is built once for the log's life; which held
/// records name each stub as a parent grows with the log ([`Log::hold`]).
#[derive(Debug, Default)]
pub(super) struct Virtual {
    pub(super) ids: Vec<RecordId>,
    pub(super) actor: Vec<MemberId>,
    pub(super) sequence: Vec<u64>,
    /// The epoch each dropped `Put` or `Delete` names; `None` for a stub of any other kind.
    pub(super) op_epoch: Vec<Option<RecordId>>,
    /// The newest held records each stub's past holds, ascending.
    pub(super) place: Vec<Vec<usize>>,
    /// The stubs among each one's parents.
    parents: Vec<Vec<usize>>,
    /// The stubs that name each one as a parent.
    children: Vec<Vec<usize>>,
}

impl Virtual {
    /// Each stub's place found once, in one walk that takes each stub after its parents: a chain
    /// of dropped records costs its length, not its square. A cycle (which no log holds) resolves
    /// to what its members reach outside it.
    fn of(log: &Log) -> Self {
        let Some(rooting) = log.rooting() else {
            return Self::default();
        };
        let ids: Vec<RecordId> = rooting.stubs.keys().copied().collect();
        let k_of = |id: &RecordId| ids.binary_search(id).ok();
        let stubs: Vec<&Stub> = rooting.stubs.values().collect();
        let mut out = Self {
            ids: ids.clone(),
            actor: stubs.iter().map(|stub| stub.actor).collect(),
            sequence: stubs.iter().map(|stub| stub.sequence).collect(),
            op_epoch: stubs
                .iter()
                .map(|stub| {
                    stub.epoch
                        .filter(|_| matches!(stub.kind, Kind::Put | Kind::Delete))
                })
                .collect(),
            parents: stubs
                .iter()
                .map(|stub| stub.parents.iter().filter_map(k_of).collect())
                .collect(),
            ..Self::default()
        };
        let mut place: Vec<Option<Vec<usize>>> = vec![None; ids.len()];
        let mut open = vec![false; ids.len()];
        for root in 0..ids.len() {
            let mut stack = vec![(root, false)];
            while let Some((k, expanded)) = stack.pop() {
                if place[k].is_some() || (!expanded && open[k]) {
                    continue;
                }
                if !expanded {
                    open[k] = true;
                    stack.push((k, true));
                    stack.extend(out.parents[k].iter().map(|&j| (j, false)));
                    continue;
                }
                let mut set: BTreeSet<usize> = BTreeSet::new();
                #[cfg(test)]
                let parents: &[RecordId] = if mutant(Mutant::NoPlace) {
                    &[]
                } else {
                    &stubs[k].parents
                };
                #[cfg(not(test))]
                let parents: &[RecordId] = &stubs[k].parents;
                for parent in parents {
                    match log.held.get(parent) {
                        Some(&at) => {
                            set.insert(at);
                        }
                        None => {
                            if let Some(j) = k_of(parent) {
                                set.extend(place[j].iter().flatten().copied());
                            }
                        }
                    }
                }
                let set: Vec<usize> = set.into_iter().collect();
                place[k] = Some(log.reach.maximal(&set));
            }
        }
        out.place = place.into_iter().map(Option::unwrap_or_default).collect();
        out.children = vec![Vec::new(); ids.len()];
        for (k, parents) in out.parents.iter().enumerate() {
            for &p in parents {
                out.children[p].push(k);
            }
        }
        out
    }

    /// The index of the stub `id`, past every held record's; `None` where `id` is no stub.
    pub(super) fn index(&self, id: &RecordId) -> Option<usize> {
        self.ids.binary_search(id).ok().map(|k| VIRTUAL + k)
    }

    /// The stub at index `i`; `None` for a held record's.
    pub(super) fn at(&self, i: usize) -> Option<usize> {
        i.checked_sub(VIRTUAL).filter(|&k| k < self.ids.len())
    }

    /// The bytes it keeps in memory: each stub's id, signer and sequence, and its place,
    /// parents and children, counted in [`Log::footprint`].
    fn bytes(&self) -> usize {
        let list = |lists: &[Vec<usize>]| {
            lists
                .iter()
                .map(|list| size_of::<Vec<usize>>() + list.len() * size_of::<usize>())
                .sum::<usize>()
        };
        self.ids.len()
            * (size_of::<RecordId>()
                + size_of::<MemberId>()
                + size_of::<u64>()
                + size_of::<Option<RecordId>>())
            + list(&self.place)
            + list(&self.parents)
            + list(&self.children)
    }
}

/// What the evaluators read of a rooted log's stubs ([`Virtual`]), with the held records that
/// name each stub as a parent: whether a stub and a held record, or two stubs, lie in one
/// another's causal past.
#[derive(Clone, Copy)]
pub(super) struct StubView<'a> {
    pub(super) virt: &'a Virtual,
    followers: &'a BTreeMap<RecordId, Vec<usize>>,
    reach: &'a super::reach::Reach,
}

impl StubView<'_> {
    /// Whether position `a` lies in position `b`'s strict causal past, either a held record's
    /// index or a stub's ([`VIRTUAL`]).
    pub(super) fn before(&self, a: usize, b: usize) -> bool {
        match (self.virt.at(a), self.virt.at(b)) {
            (None, None) => self.reach.before(a, b),
            // A held record lies in a stub's past where it lies in the place's: the place is the
            // newest held records there.
            (None, Some(k)) => self.virt.place[k].iter().any(|&p| self.reach.upto(a, p)),
            (Some(j), None) => self.stub_before_held(j, b),
            (Some(j), Some(k)) => j != k && self.stub_below(j, k),
        }
    }

    /// Whether the stub `j` lies in the past of the held record `b`: a held record that names it,
    /// or a stub after it, as a parent is `b` or lies in `b`'s past.
    fn stub_before_held(&self, j: usize, b: usize) -> bool {
        let mut seen: BTreeSet<usize> = BTreeSet::new();
        let mut stack = vec![j];
        while let Some(k) = stack.pop() {
            if !seen.insert(k) {
                continue;
            }
            if self
                .followers
                .get(&self.virt.ids[k])
                .is_some_and(|after| after.iter().any(|&f| self.reach.upto(f, b)))
            {
                return true;
            }
            stack.extend(self.virt.children[k].iter().copied());
        }
        false
    }

    /// Whether the stub `j` lies in the past of the stub `k`: through stubs alone, or through a
    /// held record, which then lies no later than a record of `k`'s place.
    fn stub_below(&self, j: usize, k: usize) -> bool {
        if self.virt.place[k]
            .iter()
            .any(|&p| self.stub_before_held(j, p))
        {
            return true;
        }
        let mut seen: BTreeSet<usize> = BTreeSet::new();
        let mut stack: Vec<usize> = self.virt.parents[k].clone();
        while let Some(p) = stack.pop() {
            if p == j {
                return true;
            }
            if seen.insert(p) {
                stack.extend(self.virt.parents[p].iter().copied());
            }
        }
        false
    }
}

/// A rooted log's [`Virtual`], built on first read, and the held records that name each stub
/// as a parent, which [`Log::hold`] extends.
#[derive(Clone, Debug, Default)]
pub(super) struct Stubbed {
    virt: Arc<OnceLock<Virtual>>,
    followers: Arc<BTreeMap<RecordId, Vec<usize>>>,
}

impl Stubbed {
    /// The bytes it keeps in memory: what the evaluators read of the stubs, once built, and
    /// each held record that names a stub as a parent.
    pub(super) fn bytes(&self) -> usize {
        self.virt.get().map_or(0, Virtual::bytes)
            + self
                .followers
                .values()
                .map(|after| {
                    size_of::<RecordId>()
                        + size_of::<Vec<usize>>()
                        + after.len() * size_of::<usize>()
                })
                .sum::<usize>()
    }
}

/// A final checkpoint a log was pruned at.
#[derive(Clone, Debug)]
pub struct Root {
    /// [`checkpoint::checkpoint_id`] of its body.
    pub id: RecordId,
    /// Its body: [`Body::Checkpoint`].
    pub body: Body,
    /// What its prune dropped, sorted ([`checkpoint::Plan::dropped`]): the stubs its `dropped`
    /// digest names, which this log serves with it ([`super::served`]).
    pub dropped: Vec<RecordId>,
}

impl Root {
    /// Its cut's day and heads.
    pub(super) fn cut(&self) -> (u64, &[RecordId]) {
        match &self.body {
            Body::Checkpoint { cut_ms, heads, .. } => (*cut_ms, heads),
            _ => (0, &[]),
        }
    }

    /// The bytes its body keeps in memory, counted in [`Log::footprint`].
    pub(super) fn bytes(&self) -> usize {
        size_of::<RecordId>()
            + super::encode_body(&self.body).len()
            + size_of::<Vec<RecordId>>()
            + self.dropped.len() * size_of::<RecordId>()
    }
}

/// What a rooted log keeps of the checkpoints it was pruned at.
#[derive(Clone, Debug, Default)]
pub struct Rooting {
    /// The roots, by id: none's cut holds another's heads, so a prune whose cut nests an earlier
    /// root's takes its place, a prune at a root one nests is nested beside them (`earlier`,
    /// where needed), and the roots are as many as the sides of a split a log holds.
    pub roots: Vec<Root>,
    /// Each record a root dropped, by id: a rooted log holds none of them.
    pub stubs: BTreeMap<RecordId, Stub>,
    /// The roots a later prune nested, by id, each kept while its dropped set holds a stub
    /// that no root dropped (an op the later root's whole log keeps): the root whose digest
    /// binds that stub's fields, which this log serves beside its roots ([`Log::served`]).
    pub earlier: Vec<Root>,
}

impl Log {
    /// The final checkpoints this log was pruned at, and the stubs of what they dropped; `None`
    /// for a whole log.
    #[must_use]
    pub fn rooting(&self) -> Option<&Rooting> {
        self.rooting.as_deref()
    }

    /// Whether `id` is a record a root of this log dropped and it does not hold.
    #[must_use]
    pub fn is_dropped(&self, id: &RecordId) -> bool {
        self.stub(id).is_some()
    }

    /// Whether this log holds `id` or dropped it: what a store holds, and never takes again.
    #[must_use]
    pub fn knows(&self, id: &RecordId) -> bool {
        self.held.contains_key(id) || self.is_dropped(id)
    }

    /// How many records it knows ([`Log::knows`]): those it holds and those a root dropped,
    /// which it holds a stub of. It is the size of what a set-reconciling peer is told this log
    /// has ([`super::Store::complete_inventory`]), so it is the count a fence on that exchange
    /// judges ([`crate::crossing::crossed`], aterm-db's legacy exchange, whose inventory a
    /// build before the fence holds to [`super::LEGACY_RECORDS`] ids): a rooted log within the
    /// legacy count of held records names more ids than such a peer reads once its stubs are
    /// counted. [`Log::held_count`] for a whole log.
    #[must_use]
    pub fn known_count(&self) -> usize {
        #[cfg(test)]
        if mutant(Mutant::FenceHeldAlone) {
            return self.order.len();
        }
        self.order
            .len()
            .saturating_add(self.rooting().map_or(0, |r| r.stubs.len()))
    }

    pub(super) fn stub(&self, id: &RecordId) -> Option<&Stub> {
        self.rooting.as_deref().and_then(|r| r.stubs.get(id))
    }

    /// The stubs as the evaluators read them; `None` for a whole log, whose evaluators read
    /// held records alone.
    pub(super) fn stub_view(&self) -> Option<StubView<'_>> {
        self.rooting.as_ref()?;
        #[cfg(test)]
        if mutant(Mutant::HeldCutoffs) {
            return None;
        }
        let stubbed = self.stubbed.as_ref()?;
        Some(StubView {
            virt: stubbed.virt.get_or_init(|| Virtual::of(self)),
            followers: &stubbed.followers,
            reach: &self.reach,
        })
    }

    /// Where the evaluators place `id`: a held record's index, or a stub's past every held
    /// one's ([`VIRTUAL`]); `None` where it is neither.
    pub(super) fn position(&self, id: &RecordId) -> Option<usize> {
        match self.held.get(id) {
            Some(&at) => Some(at),
            None => self.stub_view()?.virt.index(id),
        }
    }

    /// Whether position `a` ([`Log::position`]) lies in position `b`'s strict causal past.
    pub(super) fn position_before(&self, a: usize, b: usize) -> bool {
        match self.stub_view() {
            Some(view) => view.before(a, b),
            None => self.reach.before(a, b),
        }
    }

    /// The id of the record at position `i` ([`Log::position`]), held or a stub's.
    pub(super) fn id_at(&self, i: usize) -> Option<RecordId> {
        match self.order.get(i) {
            Some(record) => Some(*record.id()),
            None => {
                let view = self.stub_view()?;
                view.virt.at(i).map(|k| view.virt.ids[k])
            }
        }
    }

    /// The signer of the record at position `i` ([`Log::position`]), held or a stub's.
    pub(super) fn actor_at(&self, i: usize) -> Option<MemberId> {
        match self.order.get(i) {
            Some(record) => Some(*record.actor()),
            None => {
                let view = self.stub_view()?;
                view.virt.at(i).map(|k| view.virt.actor[k])
            }
        }
    }

    /// Whether the record `a`, held or dropped, lies in the strict causal past of `b`, held or
    /// dropped; `false` where either is neither.
    pub(super) fn lies_before(&self, a: &RecordId, b: &RecordId) -> bool {
        match (self.position(a), self.position(b)) {
            (Some(a), Some(b)) => self.position_before(a, b),
            _ => false,
        }
    }

    /// A rooted log's [`Stubbed`] state and the bytes its stubs take start afresh: its stubs were
    /// just set. Nothing for a whole log.
    pub(super) fn restub(&mut self) {
        self.stubbed = self.rooting.is_some().then(Stubbed::default);
        self.stub_bytes = self.rooting.as_deref().map_or(0, |r| {
            r.stubs
                .values()
                .fold(0, |sum: usize, stub| sum.saturating_add(stub.bytes()))
        });
        #[cfg(test)]
        if mutant(Mutant::FreeStubs) {
            self.stub_bytes = 0;
        }
    }

    /// The bytes a rooted log keeps in memory beside its records and index: what its roots and
    /// the evaluators' reading of its stubs keep ([`Log::footprint`]). `0` for a whole log.
    pub(super) fn rooted_footprint(&self) -> usize {
        let Some(rooting) = self.rooting() else {
            return 0;
        };
        let roots = rooting
            .roots
            .iter()
            .chain(&rooting.earlier)
            .map(Root::bytes)
            .sum::<usize>();
        let fields = rooting.stubs.values().map(Stub::read_bytes).sum::<usize>();
        let read = self.stubbed.as_ref().map_or(0, Stubbed::bytes);
        roots.saturating_add(fields).saturating_add(read)
    }

    /// Note that the held record at `at` names each of its parents a root dropped: what
    /// [`StubView`] reads to tell that a stub lies in a held record's past. Nothing in a whole log.
    pub(super) fn note_followers(&mut self, record: &Record, at: usize) {
        if self.rooting.is_none() {
            return;
        }
        let dropped: Vec<RecordId> = record
            .parents()
            .iter()
            .filter(|p| self.is_dropped(p))
            .copied()
            .collect();
        if dropped.is_empty() {
            return;
        }
        let stubbed = self.stubbed.get_or_insert_with(Stubbed::default);
        let followers = Arc::make_mut(&mut stubbed.followers);
        for parent in dropped {
            followers.entry(parent).or_default().push(at);
        }
    }

    /// The audience and kind of the op `id`, held or dropped: a held `Put`'s or `Delete`'s body
    /// says them, a stub its kind and the held epoch it names. `None` for any other record.
    fn op_of(&self, id: &RecordId) -> Option<(&EpochAudience, Kind)> {
        if let Some(record) = self.get(id) {
            return match record.body() {
                Body::Put { audience, .. } | Body::Delete { audience, .. } => {
                    Some((audience, record.kind()))
                }
                _ => None,
            };
        }
        let stub = self.stub(id)?;
        match self.get(stub.epoch.as_ref()?)?.body() {
            Body::Epoch { audience, .. } => Some((audience, stub.kind)),
            _ => None,
        }
    }

    /// Whether a statement of an op of `audience` may name `id` (a name counts only where the
    /// frames show it could be true): an op of that audience, held or dropped, of kind `kind`
    /// where one is asked, and in `at`'s causal past. Not, as [`super::checkpoint`]'s keep set
    /// also asks, that the namer counts and holds its own epoch: a later record can change both,
    /// and a reading is kept in a stub ([`Stub::carries`]) no later record changes.
    fn may_name(
        &self,
        id: &RecordId,
        audience: &EpochAudience,
        kind: Option<Kind>,
        at: usize,
    ) -> bool {
        #[cfg(test)]
        if mutant(Mutant::AnyName) {
            return self
                .position(id)
                .is_some_and(|p| self.position_before(p, at));
        }
        self.op_of(id)
            .is_some_and(|(of, k)| of == audience && kind.is_none_or(|want| want == k))
            && self
                .position(id)
                .is_some_and(|p| self.position_before(p, at))
    }

    /// What the register reads of the held op at `i` from its own statement, in every log:
    /// `Some(Stated::Carry(rep))` where its statement makes it a carry of `rep` ([`Log::frame_reps`],
    /// `reps`), `Some(Stated::Edit)` where it makes it none. `None` for a version-1 op, which
    /// states nothing and reads by its frontier.
    ///
    /// A whole log reads a version-2 op so too, not by the frontier its ciphertexts give it, so
    /// that the reading never turns on which ops a prune dropped: the frontier of an op beside a
    /// cut holds ops the prune dropped, whose values no frame keeps, and a log rooted at another
    /// cut drops others. A statement is read only as far as [`super::checkpoint`]'s rule lets its
    /// names count, which the frames and stubs tell alike in every log; one that names nothing it
    /// could is read as stating nothing (an edit, or a `Delete` that writes), never refused,
    /// since a log refuses no record another log holds.
    pub(super) fn stated(&self, i: usize, reps: &BTreeMap<RecordId, RecordId>) -> Option<Stated> {
        #[cfg(test)]
        if mutant(Mutant::FrontierOnly)
            || (mutant(Mutant::RootedStatementsOnly) && self.rooting.is_none())
        {
            return None;
        }
        let record = &self.order[i];
        record.statement()?;
        if !matches!(record.kind(), Kind::Put | Kind::Delete) {
            return None;
        }
        Some(match reps.get(record.id()) {
            Some(rep) => Stated::Carry(*rep),
            None => Stated::Edit,
        })
    }

    /// The op `id` stands for in its register as the frames say, held or dropped: what
    /// `reps` (this log's [`Log::frame_reps`] so far) says of a held op, a dropped `Delete`'s
    /// kept rep ([`Stub::carries`]), and for a dropped `Put` that states a carry it could name,
    /// the op that carry stands for; `id` itself for every other op. `seen` guards the walk
    /// through dropped `Put`s.
    fn frame_rep(&self, id: &RecordId, reps: &BTreeMap<RecordId, RecordId>) -> RecordId {
        let mut at = *id;
        let mut seen: BTreeSet<RecordId> = BTreeSet::new();
        loop {
            if let Some(rep) = reps.get(&at) {
                return *rep;
            }
            let Some(stub) = self.stub(&at) else {
                return at;
            };
            match stub.kind {
                Kind::Delete => return stub.carries.unwrap_or(at),
                Kind::Put => {
                    #[cfg(test)]
                    if mutant(Mutant::StubCarriesUnread) {
                        return at;
                    }
                    let Some(next) = stub.carries else {
                        return at;
                    };
                    let Some((audience, _)) = self.op_of(&at) else {
                        return at;
                    };
                    let audience = audience.clone();
                    let Some(from) = self.position(&at) else {
                        return at;
                    };
                    if !seen.insert(at) || !self.may_name(&next, &audience, Some(Kind::Put), from) {
                        return at;
                    }
                    at = next;
                }
                _ => return at,
            }
        }
    }

    /// The op each held version-2 op that is a carry stands for, by id, read off the frames
    /// alone in one pass in log order (a name lies before its namer): a `Put` whose statement
    /// names a `Put` of its audience in its past as the op it carries stands for what that op
    /// stands for ([`Log::frame_rep`]); a `Delete` whose statement's newest names it could name
    /// ([`Log::may_name`]) are, once each carry another of them replaced is set aside (its rep in
    /// the past of a named op of another class: a `Delete` against a `Put`, or a `Put` of
    /// another rep), `Delete`s alone stands for what the winner among them stands for (the
    /// lowest rep, then the lowest id, as the register orders siblings). Every other op stands
    /// for itself and is left out. What a prune keeps in a dropped `Delete`'s stub
    /// ([`Stub::carries`]), and what every log's register reads a version-2 op as
    /// ([`Log::stated`]).
    pub(super) fn frame_reps(&self) -> BTreeMap<RecordId, RecordId> {
        let mut reps: BTreeMap<RecordId, RecordId> = BTreeMap::new();
        for (i, record) in self.order.iter().enumerate() {
            let Some(statement) = record.statement() else {
                continue;
            };
            let (Body::Put { audience, .. } | Body::Delete { audience, .. }) = record.body() else {
                continue;
            };
            let rep = match record.kind() {
                Kind::Put => statement
                    .carries
                    .filter(|carried| self.may_name(carried, audience, Some(Kind::Put), i))
                    .map(|carried| self.frame_rep(&carried, &reps)),
                Kind::Delete => self.deletes_rep(i, audience, &statement.supersedes, &reps),
                _ => None,
            };
            if let Some(rep) = rep {
                reps.insert(*record.id(), rep);
            }
        }
        reps
    }

    /// What the dropped version-2 `Delete` `id` stands for, read off its stub's statement at its
    /// stub's place as a prune read it off its record ([`Log::frame_reps`], `reps` this log's):
    /// a reading of the frames and stubs in its past alone, so a Mac that takes the stub from a
    /// peer reads it as the prune that made it did, and checks the `carries` the stub keeps,
    /// the one field its frame does not hold ([`Stub::frame`]). `None` where it writes, or `id`
    /// is no such stub.
    pub(super) fn stub_delete_rep(
        &self,
        id: &RecordId,
        reps: &BTreeMap<RecordId, RecordId>,
    ) -> Option<RecordId> {
        let stub = self.stub(id)?;
        if stub.kind != Kind::Delete || stub.frame.is_none() {
            return None;
        }
        let (audience, _) = self.op_of(id)?;
        let audience = audience.clone();
        let at = self.position(id)?;
        self.deletes_rep(at, &audience, &stub.supersedes, reps)
    }

    /// What the `Delete` at `i`, of `audience`, stands for by the ops its statement names as
    /// replaced (`named`): the rep of the winner among the newest it may name, once each carry
    /// another of them replaced is set aside, where each that stays is a `Delete`; `None` where
    /// none stays or a `Put` does, and the `Delete` writes.
    fn deletes_rep(
        &self,
        i: usize,
        audience: &EpochAudience,
        named: &[RecordId],
        reps: &BTreeMap<RecordId, RecordId>,
    ) -> Option<RecordId> {
        let past: Vec<(usize, RecordId)> = named
            .iter()
            .filter(|id| self.may_name(id, audience, None, i))
            .filter_map(|id| self.position(id).map(|at| (at, *id)))
            .collect();
        let newest: Vec<(usize, RecordId)> = past
            .iter()
            .copied()
            .filter(|&(a, _)| !past.iter().any(|&(b, _)| self.position_before(a, b)))
            .collect();
        // Each named op's kind and rep: a `Delete`, or a `Put` standing for its rep.
        let read: Vec<(usize, RecordId, bool, RecordId)> = newest
            .iter()
            .map(|&(at, id)| {
                let delete = self
                    .get(&id)
                    .map(Record::kind)
                    .or_else(|| self.stub(&id).map(|stub| stub.kind))
                    == Some(Kind::Delete);
                (at, id, delete, self.frame_rep(&id, reps))
            })
            .collect();
        #[cfg(test)]
        let replaced_carries = !mutant(Mutant::StatedCarriesLive);
        #[cfg(not(test))]
        let replaced_carries = true;
        let live: Vec<&(usize, RecordId, bool, RecordId)> = read
            .iter()
            .filter(|(_, id, delete, rep)| {
                !replaced_carries
                    || rep == id
                    || !read.iter().any(|(at, other, other_delete, other_rep)| {
                        other != id
                            && (delete != other_delete || (!delete && other_rep != rep))
                            && self
                                .position(rep)
                                .is_some_and(|r| self.position_before(r, *at))
                    })
            })
            .collect();
        if live.is_empty() || live.iter().any(|(_, _, delete, _)| !delete) {
            return None;
        }
        live.into_iter()
            .copied()
            .reduce(|w, n| {
                if beats(n.3 < w.3, n.3 == w.3, n.1 < w.1) {
                    n
                } else {
                    w
                }
            })
            .map(|(_, _, _, rep)| rep)
    }

    /// Where a record that follows `id` sits: the held record, or the held records in a stub's
    /// past that no other there follows less than its parents do, ascending. `None` where `id` is
    /// neither held nor resolves to a held record: a stub's parents name a record neither held
    /// nor dropped (the genesis lies in every record's past and is never dropped). One walk over
    /// the stubs, each taken once: a stub whose parent a later root dropped resolves through that
    /// stub's parents, and stubs that share parents cost no more than their number.
    pub(super) fn placed(&self, id: &RecordId) -> Option<Vec<usize>> {
        if let Some(&at) = self.held.get(id) {
            return Some(vec![at]);
        }
        let mut out: BTreeSet<usize> = BTreeSet::new();
        let mut seen: BTreeSet<RecordId> = BTreeSet::new();
        let mut stack: Vec<RecordId> = vec![*id];
        while let Some(next) = stack.pop() {
            if let Some(&at) = self.held.get(&next) {
                out.insert(at);
            } else if seen.insert(next) {
                let stub = self.stub(&next)?;
                #[cfg(test)]
                if mutant(Mutant::NoPlace) {
                    continue;
                }
                stack.extend(stub.parents.iter().copied());
            }
        }
        (!out.is_empty()).then(|| out.into_iter().collect())
    }

    /// The held records a record with `record`'s parents follows directly, ascending: each held
    /// parent, and the held records each dropped one resolves to. `None` where a parent is
    /// neither.
    pub(super) fn parent_indexes(&self, record: &Record) -> Option<Vec<usize>> {
        let mut out = Vec::with_capacity(record.parents().len());
        for parent in record.parents() {
            match self.held.get(parent) {
                Some(&at) => out.push(at),
                None if self.rooting.is_some() => out.extend(self.placed(parent)?),
                None => return None,
            }
        }
        out.sort_unstable();
        out.dedup();
        Some(out)
    }

    /// The held records the held record at `x` follows directly, ascending: every parent of a
    /// held record is placed ([`Log::parent_indexes`]). One path for every log, rooted or not,
    /// so the evaluators read a whole log's parents as they read a rooted one's.
    pub(super) fn placed_parents(&self, x: usize) -> impl Iterator<Item = usize> + use<> {
        self.parent_indexes(&self.order[x])
            .unwrap_or_default()
            .into_iter()
    }

    /// The held records a peer whose `have` names these ids holds: each held one, and the held
    /// records each dropped one resolves to.
    pub(super) fn have_indexes(&self, have: &[RecordId]) -> Vec<usize> {
        have.iter()
            .filter_map(|id| self.placed(id))
            .flatten()
            .collect()
    }

    /// The held records `ids` stand for in a rooted log's causal questions: each held one, and
    /// the held records each dropped one resolves to, whose pasts with them are its strict past.
    pub(super) fn rooted_indexes(&self, ids: &[RecordId]) -> Vec<usize> {
        #[cfg(test)]
        if mutant(Mutant::HeldIndexes) {
            return ids
                .iter()
                .filter_map(|id| self.held.get(id).copied())
                .collect();
        }
        self.have_indexes(ids)
    }

    /// Whether the dropped record `target` lies in the causal past of a record whose parents are
    /// `parents`: a walk back over the held records' parents and the stubs'.
    pub(super) fn stub_in_past(&self, parents: &[RecordId], target: &RecordId) -> bool {
        let mut seen: BTreeSet<RecordId> = BTreeSet::new();
        let mut stack: Vec<RecordId> = parents.to_vec();
        while let Some(id) = stack.pop() {
            if id == *target {
                return true;
            }
            if !seen.insert(id) {
                continue;
            }
            if let Some(record) = self.get(&id) {
                stack.extend(record.parents().iter().copied());
            } else if let Some(stub) = self.stub(&id) {
                stack.extend(stub.parents.iter().copied());
            }
        }
        false
    }

    /// The stubs in the causal past of `heads`, held records or stubs, and among them: a walk
    /// back over the held records' parents and the stubs'. A prune at a cut whose heads these
    /// are drops them again ([`checkpoint::plan_at`]), as the whole log's prune drops the records
    /// they stand for, but for those the whole log's keeps.
    pub(super) fn stubs_in_past(&self, heads: &[RecordId]) -> BTreeSet<RecordId> {
        let mut out: BTreeSet<RecordId> = BTreeSet::new();
        if self.rooting.is_none() {
            return out;
        }
        let mut seen: BTreeSet<RecordId> = BTreeSet::new();
        let mut stack: Vec<RecordId> = heads.to_vec();
        while let Some(id) = stack.pop() {
            if !seen.insert(id) {
                continue;
            }
            if let Some(record) = self.get(&id) {
                stack.extend(record.parents().iter().copied());
            } else if let Some(stub) = self.stub(&id) {
                out.insert(id);
                stack.extend(stub.parents.iter().copied());
            }
        }
        out
    }

    /// This log's roots with the stubs among `stubs` alone: what a log of the held records in the
    /// causal past of some records, whose stubs these are, keeps ([`Log::project_heads`],
    /// [`checkpoint`]'s projections). `None` for a whole log.
    pub(super) fn rooting_within(&self, stubs: &BTreeSet<RecordId>) -> Option<Arc<Rooting>> {
        let rooting = self.rooting()?;
        Some(Arc::new(Rooting {
            roots: rooting.roots.clone(),
            earlier: rooting.earlier.clone(),
            stubs: rooting
                .stubs
                .iter()
                .filter(|(id, _)| stubs.contains(*id))
                .map(|(id, stub)| (*id, stub.clone()))
                .collect(),
        }))
    }

    /// The final checkpoint whose cut contains every other root's, the latest by its day where
    /// several do and then the lowest id; `None` for a whole log, and for a log rooted at two
    /// sides of a split, until it adopts a checkpoint whose cut nests both.
    #[must_use]
    pub fn newest_root(&self) -> Option<&Root> {
        let roots = &self.rooting.as_deref()?.roots;
        roots
            .iter()
            .filter(|outer| {
                roots
                    .iter()
                    .all(|inner| inner.cut().1.iter().all(|head| self.in_cut(outer, head)))
            })
            .max_by_key(|root| (root.cut().0, std::cmp::Reverse(root.id)))
    }

    /// Whether `id` lies in `root`'s cut: it is a head or in a head's past, held or dropped.
    pub(super) fn in_cut(&self, root: &Root, id: &RecordId) -> bool {
        self.in_cut_of(root.cut().1, id)
    }

    /// Whether `id` lies in the cut whose heads are `heads`, as [`Log::in_cut`] reads a root's.
    pub(super) fn in_cut_of(&self, heads: &[RecordId], id: &RecordId) -> bool {
        heads.contains(id)
            || self.reaches(heads, id)
            || (self.is_dropped(id) && self.stub_in_past(heads, id))
    }

    /// This log pruned at the checkpoint `body`, which it computes itself at its heads
    /// ([`checkpoint::plan_at`]): it keeps the plan's records and every record outside the cut,
    /// and a [`Stub`] of each record the plan drops. Whether `body` is final is the caller's
    /// ([`Log::adopt`]). A log already rooted at `body`, or at a checkpoint whose cut holds
    /// `body`'s heads (a prune at `body` would drop nothing it has not), is itself.
    ///
    /// # Errors
    /// `body` is no checkpoint, its cut is not one this log holds ([`checkpoint::plan_at`]'s
    /// errors), or this log computes another body at its heads.
    pub(super) fn rooted_at(&self, body: &Body) -> Result<Self> {
        let Body::Checkpoint { cut_ms, heads, .. } = body else {
            return Err(Error::new("a log is rooted at a checkpoint"));
        };
        let id = checkpoint::checkpoint_id(body).ok_or_else(|| Error::new("not a checkpoint"))?;
        if self.rooting().is_some_and(|r| {
            r.roots
                .iter()
                .any(|root| root.id == id || heads.iter().all(|head| self.in_cut(root, head)))
        }) {
            return Ok(self.clone());
        }
        let plan = checkpoint::plan_at(self, *cut_ms, heads)?;
        if checkpoint::checkpoint_id(&plan.body) != Some(id) {
            return Err(Error::new(
                "this log computes another checkpoint at those heads",
            ));
        }
        self.pruned(&plan, body)
    }

    /// [`Log::rooted_at`] the checkpoint `body` where [`checkpoint::judge`] finds it final here.
    ///
    /// # Errors
    /// `body` is not final in this log, or [`Log::rooted_at`]'s errors.
    pub fn adopt(&self, body: &Body) -> Result<Self> {
        if !checkpoint::judge(self, body)?.is_final {
            return Err(Error::new("a log is pruned only at a final checkpoint"));
        }
        self.rooted_at(body)
    }

    /// The prune `plan` stands for, applied to this log at `body`, the plan's body.
    pub(super) fn pruned(&self, plan: &Plan, body: &Body) -> Result<Self> {
        let Body::Checkpoint { dropped, .. } = body else {
            return Err(Error::new("a log is rooted at a checkpoint"));
        };
        if plan.stubs.len() != plan.dropped.len()
            || checkpoint::dropped_digest(&plan.stubs) != *dropped
        {
            return Err(Error::new(
                "a prune drops what its checkpoint's digest names",
            ));
        }
        let id = checkpoint::checkpoint_id(body).ok_or_else(|| Error::new("not a checkpoint"))?;
        let drop: BTreeSet<RecordId> = plan.dropped.iter().copied().collect();
        #[cfg(test)]
        if mutant(Mutant::GoneFromTheLastCut) {
            let gone = checkpoint::gone_in_cut(self, body)?;
            LAST_CUT_GONE.with_borrow_mut(|last| last.extend(gone));
        }
        let mut rooting = self.rooting().cloned().unwrap_or_default();
        // Each dropped record's stub is the one the checkpoint's digest names: made from the
        // record where this log holds it, else this log's stub.
        for (dropped, stub) in &plan.stubs {
            rooting.stubs.insert(*dropped, stub.clone());
        }
        if drop.iter().any(|id| !self.knows(id)) {
            return Err(Error::new("a prune drops only records this log holds"));
        }
        let root = Root {
            id,
            body: body.clone(),
            dropped: plan.dropped.clone(),
        };
        // An earlier root whose cut this one's holds says nothing this one does not: the next
        // cut follows every one of its heads where it follows this one's.
        #[cfg(test)]
        let keep_nested = mutant(Mutant::KeepNestedRoots);
        #[cfg(not(test))]
        let keep_nested = false;
        let mut nested = std::mem::take(&mut rooting.earlier);
        if !keep_nested {
            let (inner, outer): (Vec<Root>, Vec<Root>) = std::mem::take(&mut rooting.roots)
                .into_iter()
                .partition(|old| old.cut().1.iter().all(|head| self.in_cut(&root, head)));
            rooting.roots = outer;
            nested.extend(inner);
        }
        // And this one, where a root this log holds nests it, is nested so: a Mac behind both
        // cuts that takes a later root first (a cut's day says nothing of what it nests) holds
        // the roots its peer holds, whatever the order ([`Log::take_served`]).
        #[cfg(test)]
        let file_nested = !keep_nested && !mutant(Mutant::NestedAsRoot);
        #[cfg(not(test))]
        let file_nested = !keep_nested;
        let (_, heads) = root.cut();
        if file_nested
            && rooting
                .roots
                .iter()
                .any(|old| heads.iter().all(|head| self.in_cut(old, head)))
        {
            nested.push(root);
        } else {
            rooting.roots.push(root);
            rooting.roots.sort_by_key(|root| root.id);
        }
        rooting.earlier = Self::earlier_needed(&rooting, nested);
        self.rebuilt(rooting, &drop)
    }

    /// Of the nested roots `nested`, those `rooting` keeps beside its roots ([`Rooting::earlier`]):
    /// each whose dropped set holds a stub `rooting` keeps that none of its roots dropped, the one
    /// digest that binds that stub's fields. Ascending by id, each once.
    fn earlier_needed(rooting: &Rooting, nested: Vec<Root>) -> Vec<Root> {
        #[cfg(test)]
        if mutant(Mutant::NoEarlierRoots) {
            return Vec::new();
        }
        let current: BTreeSet<&RecordId> = rooting
            .roots
            .iter()
            .flat_map(|root| root.dropped.iter())
            .collect();
        let mut out: BTreeMap<RecordId, Root> = BTreeMap::new();
        for root in nested {
            if root
                .dropped
                .iter()
                .any(|id| rooting.stubs.contains_key(id) && !current.contains(id))
            {
                out.entry(root.id).or_insert(root);
            }
        }
        out.into_values().collect()
    }

    /// A log of this one's records but `drop`, rooted as `rooting` says: each record placed
    /// afresh through its stubs, in this log's order, and each waiting record offered again.
    pub(super) fn rebuilt(&self, rooting: Rooting, drop: &BTreeSet<RecordId>) -> Result<Self> {
        self.rebuilt_with(rooting, drop, std::iter::empty())
    }

    /// [`Log::rebuilt`], with `more` offered after this log's own records, each in its turn and
    /// a refused one passed over: what a records file holds after its roots and stubs
    /// ([`super::stored`]), read into the log of its genesis.
    pub(super) fn rebuilt_with(
        &self,
        rooting: Rooting,
        drop: &BTreeSet<RecordId>,
        more: impl IntoIterator<Item = Record>,
    ) -> Result<Self> {
        let mut out = Self::new(self.order[0].clone())?;
        out.budgets = self.budgets;
        out.stored_at = self.stored_at;
        out.enforce_seats = self.enforce_seats;
        #[cfg(unix)]
        out.head.clone_from(&self.head);
        out.rooting = Some(Arc::new(rooting));
        out.restub();
        #[cfg(test)]
        PRUNING.set(true);
        let held = self
            .order
            .iter()
            .skip(1)
            .filter(|record| !drop.contains(record.id()))
            .try_for_each(|record| out.insert(record.clone()).map(|_| ()));
        for record in self.waiting.values() {
            let _ = out.insert(record.clone());
        }
        for record in more {
            let _ = out.insert(record);
        }
        #[cfg(test)]
        PRUNING.set(false);
        held?;
        // What the evaluators read of the stubs is built on first read, once every record a stub
        // resolves to is held.
        if let Some(stubbed) = out.stubbed.as_mut() {
            stubbed.virt = Arc::default();
        }
        // Each stub also keeps its place, once every record it resolves to is held.
        #[cfg(test)]
        if mutant(Mutant::StoredPlace)
            && let Some(rooting) = out.rooting.clone()
        {
            for id in rooting.stubs.keys() {
                let place = out.placed(id).map_or(0, |place| place.len());
                out.stub_bytes += 2 + place * size_of::<RecordId>();
            }
        }
        out.stub_heads();
        Ok(out)
    }

    /// The stub a prune makes of the dropped record `id` ([`Stub`]): from its record where this
    /// log holds it, else this log's own stub of it. `reps` is this log's [`Log::frame_reps`]
    /// where `id` is a held `Delete` with a statement (its kept rep), and may be empty
    /// otherwise. `None` where this log neither holds nor dropped `id`. What a checkpoint's
    /// `dropped` digest hashes ([`checkpoint::dropped_digest`]), so every Mac that drops `id` at
    /// one cut makes it alike.
    pub(super) fn stub_for(
        &self,
        id: &RecordId,
        reps: &BTreeMap<RecordId, RecordId>,
    ) -> Option<Stub> {
        let Some(record) = self.get(id) else {
            return self.stub(id).cloned();
        };
        let carries = match record.kind() {
            Kind::Delete => {
                #[cfg(test)]
                if mutant(Mutant::DeleteStandsForItself) {
                    return Some(Self::record_stub(record, None));
                }
                reps.get(record.id()).copied()
            }
            _ => record.statement().and_then(|statement| statement.carries),
        };
        Some(Self::record_stub(record, carries))
    }

    /// The stub of the held `record`, carrying `carries`.
    fn record_stub(record: &Record, carries: Option<RecordId>) -> Stub {
        #[cfg(test)]
        if mutant(Mutant::SharesUnkept) {
            return Stub {
                shares: None,
                ..Stub::of(record, carries)
            };
        }
        Stub::of(record, carries)
    }

    /// The heads of the whole log this rooted log stands for: no record a stub names as a parent
    /// is one, and a stub nothing held or dropped names as a parent is one (a quiet seat's share
    /// of an older checkpoint), so [`Log::heads_digest`] is a whole Mac's ([`Log::hold`] ends a
    /// stub's place there when a record that names it is held).
    pub(super) fn stub_heads(&mut self) {
        #[cfg(test)]
        if mutant(Mutant::NoStubHeads) {
            return;
        }
        let Some(rooting) = self.rooting.clone() else {
            return;
        };
        let followed: BTreeSet<&RecordId> = rooting
            .stubs
            .values()
            .flat_map(|stub| stub.parents.iter())
            .collect();
        for parent in &followed {
            self.heads.remove(*parent);
        }
        let held_after = self
            .stubbed
            .as_ref()
            .map(|stubbed| Arc::clone(&stubbed.followers))
            .unwrap_or_default();
        for id in rooting.stubs.keys() {
            if !followed.contains(id) && !held_after.contains_key(id) {
                self.heads.insert(*id);
            }
        }
    }

    /// Whether the tests' `RefuseConcurrent` refuses `record`, whose parents are the held records
    /// at `parents`: outside a prune, a record with a dropped parent that does not follow every
    /// root's cut.
    #[cfg(test)]
    pub(super) fn refuses_concurrent(&self, record: &Record, parents: &[usize]) -> bool {
        let Some(rooting) = self.rooting() else {
            return false;
        };
        if !mutant(Mutant::RefuseConcurrent)
            || PRUNING.get()
            || !record.parents().iter().any(|p| self.is_dropped(p))
        {
            return false;
        }
        !rooting.roots.iter().all(|root| {
            root.cut().1.iter().all(|head| match self.held.get(head) {
                Some(&at) => self.reach.in_past_of(at, parents),
                None => self.stub_in_past(record.parents(), head),
            })
        })
    }
}

#[cfg(test)]
#[path = "rooted_tests.rs"]
pub(super) mod tests;
