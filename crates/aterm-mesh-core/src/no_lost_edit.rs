// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! P3, no lost edit (step PF3): what a Mac keeps of an edit it published until the edit is known
//! to be in the log, and what it signs of the `[everyone]` writes that waited for a removal's new
//! key (one-mesh §4.2 M4, W2 and W3; `docs/design/one-mesh-proofs/P3-no-lost-edit.md`).
//!
//! A publish holds its drafted records in a hold of its own version, and keeps its waiting
//! `[everyone]` writes in a pending set of its own version, both written before the envelope goes
//! out. A hold, or a pending set, goes only on proof: every server refused its version
//! ([`answer_refuses`]), its version landed, or a head this Mac settled proves its version on no
//! server for good. A head proves that by descent (its parent spine names the version: the head
//! was drafted from that version's text, which carries its edit) or by reach: the version is of
//! the head's sequence or below, the head was taken by a server ([`hold_waits`]), and each server
//! the version's publish may have reached names one server and took the head
//! ([`url_stays`]), so it refuses the version as not newer for good. The ALab vault's compiled-in
//! names move between the Macs that serve them, so they never name one server; a reach this Mac
//! could not write down is unknown, and no answer settles it.
//!
//! A pending set lands once a server is seen to hold its version: its writes wait in
//! `everyone.waiting`, a later set's over an earlier one's, and each name it writes leaves every
//! earlier pending set ([`pending_keeps`]). A vault pass signs only where no rotation is due, it
//! opens the newest epoch, and every pending set reads ([`pass_signs`]); it leaves alone a write
//! whose name a pending set writes (this Mac's own later edit of it) or a held draft of this Mac
//! changes (an edit of it in flight), and every write while a held draft does not read; it signs
//! a write only where the log does not hold its value yet and holds one of its bases, and never a
//! take-back ([`write_signs`]): so it never reverts a value the edit never saw. A write the log
//! holds as none of its bases was overtaken, and is dropped and recorded ([`write_overtaken`]).
//!
//! The reach rule rests on each server's compare-and-set, server.rs `takes_over`, and a path
//! vault's, transport.rs `write_path_held`, whose answer is [`Takeover::of`]: a newer version that
//! carries the held chain on and names the held head among its parents ([`takes_head`]), or a
//! version whose pin was replaced, with no sequence check, which the island names as the
//! environment premise it is (a server that took a version of its build's own line gives way to
//! nothing until its build changes). The one unversioned hold a build before per-version holds
//! kept goes only once the records store holds every record in it complete, at an adoption
//! ([`hold_adopts`]) and at a settled head alike ([`unversioned_goes`]).
//!
//! A publish keeps its records only while its hold is there ([`drafted_keeps`]); a version whose
//! hold went by descent before it landed settles the other holds by reach alone
//! ([`spine_settles`]). A publish drafts in its turn, one at a time, and a write it makes wait is
//! drafted over the values this Mac's held drafts leave; an edit is refused where this Mac's
//! waiting writes or held drafts do not read ([`draft_refuses`]).
//!
//! The decisions are the scalar `contract!` kernels below; each states `ensures result ==
//! spec_<name>(..) by <name>_is_exact`, where `spec_<name>` is a definition of
//! `no_lost_edit_island.rs`. That island (every Trust build) proves, for every number of
//! versions, servers, URLs, holds, pending sets, writes and names, for the Worlds that ask their
//! kernels as parameters: a hold goes only on proof; no edit of a version a server may hold is
//! lost (the descent rule's text inheritance stated as the obligation it is), a server's replaced
//! pin included where it judges only at the start of its line; a waiting write is signed only
//! after a server took its version, and only over one of its bases; one is dropped as overtaken
//! only by a value another Mac wrote, or by a value this Mac drafted after it (never by one of
//! this Mac's own earlier publishes); a stored version's write is never lost, with no
//! bound on the pending sets and a set that does not open never read as none; and each rule W2
//! and W3 replaced loses an edit, as a run the kernel evaluates. `no_lost_edit_binding_island.rs`
//! proves each compiled kernel equal to its spec and closes the laws for the Worlds that ask the
//! compiled kernels. The folds below are the island's, step for step and with no early exit; until loops
//! reach Trust's E6 import, the answer sheets `no_lost_edit::tests` recomputes hold them to the
//! island (TG12 removes them).

contract! {
    /// A server a publish may have reached stays in its hold's reach unless its URL names one
    /// server and that server took the head: it then holds the head or a newer version for good,
    /// and refuses the hold's version as not newer.
    pub fn url_stays(one_server: bool, took_it: bool) -> bool
    clauses [ensures result == spec_url_stays(one_server, took_it) by url_stays_is_exact]
    { !one_server || !took_it }
}
contract! {
    /// A settled head proves nothing by reach of a hold of a higher sequence (a server that holds
    /// the head may still take it), nor where no server is known to have taken the head.
    pub fn hold_waits(above_head: bool, took_any: bool) -> bool
    clauses [ensures result == spec_hold_waits(above_head, took_any) by hold_waits_is_exact]
    { above_head || !took_any }
}
contract! {
    /// A server's answer proves the envelope on no server: it stored nothing, and it refused
    /// (`NotStored` or `Refused`), never a reply lost after a store.
    pub fn answer_refuses(stored: bool, refusal: bool) -> bool
    clauses [ensures result == spec_answer_refuses(stored, refusal) by answer_refuses_is_exact]
    { !stored && refusal }
}
contract! {
    /// A pending write stays in its set while its set has not landed and no later set that lands
    /// writes its name (that later write replaces it).
    pub fn pending_keeps(lands: bool, later_lands_name: bool) -> bool
    clauses [ensures result == spec_pending_keeps(lands, later_lands_name) by pending_keeps_is_exact]
    { !lands && !later_lands_name }
}
contract! {
    /// A vault pass signs waiting writes only where no newest epoch wraps a removed Mac, it opens
    /// the newest epoch, and every pending set of this Mac reads (one that does not may write any
    /// name, and may be a stored version's edit).
    pub fn pass_signs(rotation_due: bool, opens_newest: bool, pending_read: bool) -> bool
    clauses [ensures result == spec_pass_signs(rotation_due, opens_newest, pending_read) by pass_signs_is_exact]
    { !rotation_due && (opens_newest && pending_read) }
}
contract! {
    /// A waiting write is signed: the log does not hold its value yet, holds one of its bases (what
    /// the edit read there), and the write puts a value (a take-back is never signed as a
    /// `Delete`, F15).
    pub fn write_signs(done: bool, stands_over: bool, take_back: bool) -> bool
    clauses [ensures result == spec_write_signs(done, stands_over, take_back) by write_signs_is_exact]
    { !done && (stands_over && !take_back) }
}
contract! {
    /// A waiting write is overtaken: the log does not hold its value, and holds a value none of
    /// its bases is (another write changed the name since the edit read it), or the write is a
    /// take-back that finds the name held.
    pub fn write_overtaken(done: bool, stands_over: bool, take_back: bool) -> bool
    clauses [ensures result == spec_write_overtaken(done, stands_over, take_back) by write_overtaken_is_exact]
    { !done && (!stands_over || take_back) }
}
contract! {
    /// A server takes an envelope as its head (server.rs `takes_over`): its pin holds, and the pin
    /// was replaced (the held line gave way to the key the server's build names: no sequence and
    /// no parents check), or the envelope is newer than the version held, carries the held handoff
    /// chain on, and names the held head among its parents wherever it or the held vault names
    /// parents.
    pub fn takes_head(pin_ok: bool, replaced: bool, not_newer: bool, chain_ok: bool, names_parents: bool, held_parented: bool, among_parents: bool) -> bool
    clauses [ensures result == spec_takes_head(pin_ok, replaced, not_newer, chain_ok, names_parents, held_parented, among_parents) by takes_head_is_exact]
    { pin_ok && (replaced || (!not_newer && (chain_ok && ((!names_parents && !held_parented) || among_parents)))) }
}
contract! {
    /// The envelope is the version the server holds (its sequence and its bytes), under a pin
    /// that holds and was not replaced: the server keeps it, and nothing changes.
    pub fn holds_version(pin_ok: bool, replaced: bool, not_newer: bool, same_version: bool) -> bool
    clauses [ensures result == spec_holds_version(pin_ok, replaced, not_newer, same_version) by holds_version_is_exact]
    { pin_ok && (!replaced && (not_newer && same_version)) }
}
contract! {
    /// 409: the envelope is not newer than the version held, and is not that version.
    pub fn refuses_not_newer(pin_ok: bool, replaced: bool, not_newer: bool, same_version: bool) -> bool
    clauses [ensures result == spec_refuses_not_newer(pin_ok, replaced, not_newer, same_version) by refuses_not_newer_is_exact]
    { pin_ok && (!replaced && (not_newer && !same_version)) }
}
contract! {
    /// 403: the envelope is newer, but drops or changes a link of the held handoff chain.
    pub fn refuses_chain(pin_ok: bool, replaced: bool, not_newer: bool, chain_ok: bool) -> bool
    clauses [ensures result == spec_refuses_chain(pin_ok, replaced, not_newer, chain_ok) by refuses_chain_is_exact]
    { pin_ok && (!replaced && (!not_newer && !chain_ok)) }
}
contract! {
    /// 409: the envelope is newer and carries the chain on, but it or the held vault names
    /// parents and its parents leave the held head out.
    pub fn refuses_parents(pin_ok: bool, replaced: bool, not_newer: bool, chain_ok: bool, names_parents: bool, held_parented: bool, among_parents: bool) -> bool
    clauses [ensures result == spec_refuses_parents(pin_ok, replaced, not_newer, chain_ok, names_parents, held_parented, among_parents) by refuses_parents_is_exact]
    { pin_ok && (!replaced && (!not_newer && (chain_ok && ((names_parents || held_parented) && !among_parents)))) }
}
contract! {
    /// A head takes a hold in (`adopt_held_draft`): the records store took the hold's records
    /// with no error, and the head's stamp is complete with them.
    pub fn hold_adopts(ingested: bool, completes: bool) -> bool
    clauses [ensures result == spec_hold_adopts(ingested, completes) by hold_adopts_is_exact]
    { ingested && completes }
}
contract! {
    /// The unversioned hold a build before per-version holds kept (`records.drafted`) goes: it
    /// reads, and the records store holds every record in it. Nothing else removes it: no answer
    /// says whether its unnamed version was stored.
    pub fn unversioned_goes(reads: bool, all_stored: bool) -> bool
    clauses [ensures result == spec_unversioned_goes(reads, all_stored) by unversioned_goes_is_exact]
    { reads && all_stored }
}
contract! {
    /// A publish keeps its drafted records, once a server took its version, only while the
    /// version's hold is there (`keep_drafted_until`): a hold gone by then went by adoption, whose
    /// records the store holds already, or by descent, and kept after a descent the records would
    /// put this version's values back over every edit drafted from the descending head's text.
    pub fn drafted_keeps(held: bool) -> bool
    clauses [ensures result == spec_drafted_keeps(held) by drafted_keeps_is_exact]
    { held }
}
contract! {
    /// A landed version settles the other holds by its parent spine (descent) only where it
    /// drafted no records or kept them (`forget_landed_draft`): a version whose records stayed
    /// where a descending head left them carries no edit of its own past that head, and a spine
    /// read through it would drop a hold that head's spine may not name. Its servers settle the
    /// holds by reach either way.
    pub fn spine_settles(drafted: bool, kept: bool) -> bool
    clauses [ensures result == spec_spine_settles(drafted, kept) by spine_settles_is_exact]
    { !drafted || kept }
}
contract! {
    /// An `[everyone]` edit whose writes wait (`writes`) is refused at its draft
    /// (`draft_document_diff_with`) where this Mac's waiting writes do not read, or its held
    /// drafts do not read (they are read where writes wait): what this Mac cannot read is never
    /// read as none (W3).
    pub fn draft_refuses(writes: bool, waiting_unread: bool, held_unread: bool) -> bool
    clauses [ensures result == spec_draft_refuses(writes, waiting_unread, held_unread) by draft_refuses_is_exact]
    { writes && (waiting_unread || held_unread) }
}

#[cfg(clean_islands)]
#[rustfmt::skip]
#[path = "no_lost_edit_island.rs"]
mod island;

/// What a head this Mac settled proves of one hold or pending set before its reach is read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gate {
    /// The head's parent spine names the version: the head descends from it, and goes.
    Descends,
    /// Nothing by reach ([`hold_waits`]): it stays.
    Waits,
    /// Its reach decides ([`settle_reach`]).
    Reach,
}

impl Gate {
    /// The gate of a hold of sequence `held_seq` against a head of sequence `head_seq` whose
    /// parent spine names the hold's version where `named_by_spine`, and which some server is
    /// known to have taken where `took_any` (the island's `gateOf`).
    #[must_use]
    pub fn of(named_by_spine: bool, held_seq: u64, head_seq: u64, took_any: bool) -> Self {
        if named_by_spine {
            Self::Descends
        } else if hold_waits(held_seq > head_seq, took_any) {
            Self::Waits
        } else {
            Self::Reach
        }
    }
}

/// What a settled head leaves of a hold's reach ([`settle_reach`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Settle {
    /// No server it may have reached may still take it: the hold goes.
    Goes,
    /// Every server stays: the hold stays as it is.
    Stays,
    /// Some servers stay: the hold stays with these, by their index in its reach.
    Narrows(Vec<usize>),
}

/// What a head that passed [`Gate::Reach`] leaves of a hold's reach: `reach` is each server its
/// publish may have reached, as whether its URL names one server and whether that server took the
/// head, or `None` where the reach is unknown, which no answer settles (the island's
/// `settleReach`). Every server is asked, with no early exit.
#[must_use]
pub fn settle_reach(reach: Option<&[(bool, bool)]>) -> Settle {
    let Some(reach) = reach else {
        return Settle::Stays;
    };
    let mut left = Vec::new();
    for (at, &(one_server, took_it)) in reach.iter().enumerate() {
        if url_stays(one_server, took_it) {
            left.push(at);
        }
    }
    if left.is_empty() {
        Settle::Goes
    } else if left.len() < reach.len() {
        Settle::Narrows(left)
    } else {
        Settle::Stays
    }
}

/// Whether a publish's answers prove its envelope on no server, so its hold and pending set go:
/// each answer, as whether that server stored it and whether it refused, refuses
/// ([`answer_refuses`]). Every answer is asked, with no early exit (the island's `forgets`).
#[must_use]
pub fn publish_forgets(answers: impl IntoIterator<Item = (bool, bool)>) -> bool {
    let mut forgets = true;
    for (stored, refusal) in answers {
        forgets &= answer_refuses(stored, refusal);
    }
    forgets
}

/// What a vault pass does with one waiting write (the island's `verdictOf`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// A pending set of this Mac writes its name: it is left as it is, for that set.
    Leaves,
    /// The log holds the name as the write leaves it: it goes, signed by nothing more.
    Done,
    /// It is dropped and recorded ([`write_overtaken`]).
    Overtaken,
    /// It is signed ([`write_signs`]).
    Signs,
}

impl Verdict {
    /// The verdict on a waiting write whose name a pending set writes where `deferred`, that the
    /// log holds as it leaves it where `done`, whose bases hold the log's value where
    /// `stands_over`, and that takes the name back to absent where `take_back`.
    #[must_use]
    pub fn of(deferred: bool, done: bool, stands_over: bool, take_back: bool) -> Self {
        if deferred {
            Self::Leaves
        } else if write_signs(done, stands_over, take_back) {
            Self::Signs
        } else if write_overtaken(done, stands_over, take_back) {
            Self::Overtaken
        } else {
            Self::Done
        }
    }
}

/// What landing does to this Mac's pending sets ([`land`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Landing {
    /// Per set, whether it lands: its writes wait now.
    pub lands: Vec<bool>,
    /// Per set, per name it writes (in the order given), whether the set keeps its write of that
    /// name ([`pending_keeps`]); every entry of a set that lands is `false`.
    pub keeps: Vec<Vec<bool>>,
    /// Per name, the set whose write of it waits now: the latest set that lands and writes it;
    /// `None` where none does, and what waited there before stays.
    pub waits: Vec<Option<usize>>,
}

/// Land this Mac's pending sets, oldest first: `sets[i]` is whether a server holds set `i`'s
/// version, and the names it writes, as dense ids below `names` (the island's `landing`). Each set
/// whose version a server holds lands, and its writes wait, a later set's over an earlier one's;
/// each name a set that lands writes leaves every earlier set. Every set and name is asked, with
/// no early exit.
#[must_use]
pub fn land(sets: &[(bool, &[usize])], names: usize) -> Landing {
    let later_lands = |i: usize, name: usize| {
        let mut any = false;
        for (j, &(stored, writes)) in sets.iter().enumerate() {
            let mut writes_it = false;
            for &other in writes {
                writes_it |= other == name;
            }
            any |= j > i && stored && writes_it;
        }
        any
    };
    let lands = sets.iter().map(|&(stored, _)| stored).collect();
    let keeps = sets
        .iter()
        .enumerate()
        .map(|(i, &(stored, writes))| {
            writes
                .iter()
                .map(|&name| pending_keeps(stored, later_lands(i, name)))
                .collect()
        })
        .collect();
    let mut waits = vec![None; names];
    for (i, &(stored, writes)) in sets.iter().enumerate() {
        for &name in writes {
            if stored && name < names {
                waits[name] = Some(i);
            }
        }
    }
    Landing {
        lands,
        keeps,
        waits,
    }
}

/// What a server makes of an envelope against the version it holds (server.rs `takes_over`, the
/// rule a PUT and a version at the vault's store file keep alike).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Takeover {
    /// It becomes the head, under its pin ([`takes_head`]).
    Takes,
    /// It is the version held ([`holds_version`]).
    Holds,
    /// 403: the pin check refused it.
    PinRefused,
    /// 409: not newer than the version held ([`refuses_not_newer`]).
    NotNewer,
    /// 403: it drops or changes a link of the held handoff chain ([`refuses_chain`]).
    BreaksChain,
    /// 409: its parents leave the held head out ([`refuses_parents`]).
    OffParents,
}

/// The scalar facts `takes_over` reads of an envelope and the version a server holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TakeoverFacts {
    /// The pin check passed (`check_pin_with` against the held signer and its chain).
    pub pin_ok: bool,
    /// The pin it passed under is `Pin::Replaced`: the held line gave way to the build's key.
    pub replaced: bool,
    /// Its sequence is the held one or below.
    pub not_newer: bool,
    /// It is the version held: the held sequence and the held bytes.
    pub same_version: bool,
    /// It carries the held handoff chain on (`check_extends`).
    pub chain_ok: bool,
    /// It names parents.
    pub names_parents: bool,
    /// The held version, or an ancestor of it, named parents.
    pub held_parented: bool,
    /// Its parents name the held head.
    pub among_parents: bool,
}

impl Takeover {
    /// The answer to `facts`. The island proves that exactly one of the five kernels or a refused
    /// pin holds for every argument (`takeover_partitions`), so the order asked is the code's
    /// refusal order and changes nothing.
    #[must_use]
    pub fn of(facts: TakeoverFacts) -> Self {
        let TakeoverFacts {
            pin_ok,
            replaced,
            not_newer,
            same_version,
            chain_ok,
            names_parents,
            held_parented,
            among_parents,
        } = facts;
        if takes_head(
            pin_ok,
            replaced,
            not_newer,
            chain_ok,
            names_parents,
            held_parented,
            among_parents,
        ) {
            Self::Takes
        } else if holds_version(pin_ok, replaced, not_newer, same_version) {
            Self::Holds
        } else if refuses_not_newer(pin_ok, replaced, not_newer, same_version) {
            Self::NotNewer
        } else if refuses_chain(pin_ok, replaced, not_newer, chain_ok) {
            Self::BreaksChain
        } else if refuses_parents(
            pin_ok,
            replaced,
            not_newer,
            chain_ok,
            names_parents,
            held_parented,
            among_parents,
        ) {
            Self::OffParents
        } else {
            Self::PinRefused
        }
    }
}

#[cfg(test)]
mod tests;
