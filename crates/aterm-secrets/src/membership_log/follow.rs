// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Following a vault into records format 2 (`docs/DESIGN-vault-log-2026-11-14.md` steps 3 and
//! 4, tracker FL-148E): how a node that states format 1 comes to state format 2.
//!
//! A node's format is what its format mark names ([`crate::crossing::FormatMark`]), and a mark
//! is one-way: a node that stores one exchanges nothing with a format-1 node from then on
//! ([`crate::crossing::RecordsBound::can_exchange`]). So a mark is stored on **signed evidence**
//! alone: a record of version 2 (every `Checkpoint` is one) that the node's own log holds
//! complete, verified, and signed by a seat the log holds now ([`evidence`]). What a source
//! says of itself (the bound its header states, a count, an unsigned field, a record that only
//! waits on a parent, one a removed key signed) is no evidence, so a LAN host that claims
//! format 2 moves no Mac across the line, and neither does a key the vault removed. A node
//! that holds no genesis has no log to hold a seat: the copy it follows in is a copy of its
//! own store, which takes a genesis only of its vault's key line
//! (`client::records_store_selected`), so a host's own log, whose seat is its own, moves it
//! nowhere.
//!
//! **Take first, then judge.** "A seat the log holds now" is only as current as the log. A
//! records pass therefore takes what its sources sent at the format the Mac states, and writes
//! it, before it asks any source again stating format 2 (`client::records_pass`): the evidence
//! is judged on a copy of the store as the state lock then finds it, which holds a `Remove`
//! the pass's own source just sent. A pass that failed holding a batch it verified and did not
//! take follows nothing. What is left is the stale view any member has: a Mac none of whose
//! sources that answered holds a seat's removal still reads that seat as one, takes its
//! format-1 records as it ever did, and follows on a version-2 record it signed; the mark is
//! never lowered, so that Mac states format 2 from then on, where its vault may not.
//!
//! **Mark first, then follow.** A node that meets a source that refused it beside a bound
//! stating a later format ([`crate::ErrorKind::LaterFormat`], and no other refusal) asks it
//! again stating format 2, with no mark stored; takes what it answers into a copy of its store
//! no reader sees, read at [`Reading::Follow`] ([`follow_records`]); and only where that copy
//! holds the evidence stores its mark durably and then lets the copy become its store
//! ([`Followed::take`], through [`crate::crossing::after_format_mark`]). A node that dies
//! between the two holds the mark and none of the records, and states format 2 beside a
//! format-1 store it takes the records into again (a pass at format 2, or the same join
//! bundle, read at the format its mark names); no file, slot or reply holds a format-2 record
//! beside a node that states format 1.
//!
//! **The gate.** Everything here runs only on a node whose build reads format 2
//! ([`crate::crossing::FormatMark::follows`]): none does while
//! [`crate::commons::hearing::READS_FORMAT`] names no release. [`follow_records`],
//! [`take_on_evidence`] and [`reading`] ask it themselves, so a caller that forgot to cannot
//! store a mark or read a records file at format 2.
//!
//! **A marked node on a build that reads no format 2** (the build a followed Mac is rolled
//! back to, which every build is while the gate is shut) reads its records at format 1
//! ([`reading`]): a followed file as the records before its first version-2 one. It writes
//! nothing over that file (`client::reads_marked_format`), states the mark's format, and so
//! takes and serves nothing across the line until it is updated.
//!
//! **The mesh.** The mesh's members lane states no records bound, so the HTTP fence's `409`
//! never sees it (ATERM-KI-1116-1471): a Mac whose format mark names a later format than 1
//! neither feeds it nor takes from it (`client::members_lane_open`), whatever its build reads,
//! and leaves the membership unit out of its repair rounds, so a format-1 neighbour offers it
//! nothing (`aterm_mesh::stores::Stores::members_lane`). A write asks before it hands its
//! records to the mesh's egress, and the egress asks again when it reads them, so records
//! handed over before the mark was stored are not published after it
//! (`aterm_mesh::egress::Feed::Members`).
//!
//! **What a person sees.** `aterm fleet` shows `needs update` for a member whose build does
//! not read the format this Mac's mark names, and for this Mac's own row where its own build
//! does not (`aterm_agent::fleet_view::FormatFloor`): the row of the seat that bound this
//! Mac's node key, since the command reads no node's view.
//!
//! **Its callers.** A Mac's records pass (`client::follow_later_format`, [`follow_records`]);
//! a new Mac taking a join bundle whose member states format 2 (`client::admit`,
//! [`take_on_evidence`]); and a vault server, below.
//!
//! **The server** (tracker FL-148E2). A vault server follows as a Mac does, by the same two
//! rules. A records POST that states format 2 to a server that states format 1 is read into a
//! follow copy (`server::records_admission`, `server::apply_records`); the server stores its
//! mark and then lets its file and its slot take the copy inside [`Followed::take`], and
//! answers the fence's `409` where the copy holds no evidence. Its peer pull takes what every
//! server of the pass answered at the format it states, and only then asks again, stating
//! format 2, each server that refused it for a later format (`server::follow_later_peers`).
//! The server reads its records file, a POST's body, a peer's frames and its slot's store at
//! [`reading`] of its mark (`server::Shared::records_reading`). A marked server on a build
//! that reads no format 2 takes no POSTed record (`server::records_admission`'s `500`): the
//! store it read of its file ends before the first version-2 record, and a write from it would
//! replace the file. Reading format 2, the server keeps a whole version-2 record over the
//! shed frame it holds, in its slot and on its file (`Store::lacks_whole`,
//! `server::complete_part`), plans an append by the file's bytes
//! (`server::persist_records`), and names a staged frame by its record's id
//! (`server/records_input.rs` `stage_with`).
//!
//! # Served roots
//!
//! A Mac behind a format-2 vault's final checkpoint holds the records after the cut only once
//! it took the root they follow ([`super::served`]), so the evidence may be a record it holds
//! complete only with that root. A Mac's pull that reads format 2 (its mark names it, or it
//! asks a source again to follow it) sends `X-Aterm-Records-Served` and reads the frame after
//! the records (`transport::Pull::takes_served`, `untaken::ReceivedFrames::read_at`); the
//! follow copy and the pass take records and roots together ([`follow_records`],
//! `Store::ingest_served`), and a store rooted that way is written as a rewritten records
//! file, mark first as any followed store (`client::persist_stored`,
//! `crate::records_storage::rewrite`), which the Mac's reader then selects and reads back
//! (`client::records_cache`). A member whose log is rooted hands the frame after its records
//! in its join bundle, and a new Mac that reads format 2 reads it there
//! (`client::admit::held_records`, `bundle_records`). A pull that reads format 1 sends no
//! such header, and a store that reads format 1 is written and read as it always was.
//!
//! # The join's asking side
//!
//! A member whose mark is stored refuses a run that states format 1 before it touches the
//! phrase, naming its format on the `NO` line (`members::wire::later_refusal`). A new Mac
//! whose build follows runs the same phrase with it once more, stating format 2
//! (`members::join::run_phrase`, `client::later_records_bound`), and takes the bundle on the
//! evidence in its records, mark first (`client::admit`). Stating format 2 stores nothing. A
//! new Mac whose build does not follow asks once and fails with the member's reason.
//!
//! A proof with nobody's Allow asks the same way. The member refuses a proof that states
//! format 1 before its possession challenge, so before the Mac has sent anything a member
//! could sign on, and its `NO` line names its format (`members::desk`'s `later_refused`).
//! Where no member let the Mac in and none may have signed, a Mac whose build follows proves
//! itself to the members once more, stating format 2 (`members::join::prove_first`), and takes
//! the bundle as above. Every caller that proves a Mac goes through that function, so the
//! callers that build a `Joining` (aterm-agent's `members_cli` and `vault_join`, aterm-lab)
//! state the bound they read and restate nothing themselves.
//!
//! A seatless Mac asks nobody again (`client::later_records_bound`): it takes its seat's
//! records at the format its own mark names (`client::seat_ask`), so a member that handed it
//! format-2 records would have signed for a Mac that takes none of them.
//!
//! A Mac that took a bundle into a rewritten records entry and is handed one again (it left,
//! or its vault let it go) readies that entry as its writer selects it
//! (`client::ready_records_entry`), so the bundle's write is not refused by the format-1
//! selection before its configuration is written.
//!
//! # Windows
//!
//! A Windows member never follows, and this is decided by what its build compiles, not at run
//! time. The mark's writers ([`FormatMark::store`], `crossing::after_format_mark`) and
//! [`FormatMark::follows`] are Unix only, so a Windows build stores no mark, reads every
//! store at format 1 (`client::records_reading`), asks no source again
//! (`client::follow_later_format`, `client::later_records_bound`), sends no served header and
//! writes no rewritten file (`client::persist_stored` and `crate::records_storage::rewrite`
//! are not compiled there). A mark that reaches a Windows member's folder some other way (a
//! folder copied from a Mac) is still read ([`FormatMark::stored`]): its build then writes no
//! records file (`client::reads_marked_format`) and shuts its members lane
//! (`client::members_lane_open`), as a Unix build that reads no format 2 does, and a
//! rewritten layout beside it is refused by its format-1 selection
//! (`crate::records_storage::selected`), so it reads no frame as records. Nothing there is
//! misread or replaced; the member is refused until it runs a build that follows.
//!
//! # Not yet
//!
//! Each is fenced as before: the node and the source refuse each other, and no mark is stored.
//!
//! - **A served root at a vault server** (tracker FL-148E; [`super::rooted`]'s "Not yet").
//!   The server's peer pull asks for no served frame (`transport::fetch_record_frames_with_known`)
//!   and its follow copy takes records alone, so a server behind a format-2 vault's cut holds
//!   the records after the cut waiting. A server's records file is one flat file whose slot
//!   knows it by a count of the log's first records (`server::RecordSlot`'s `on_file`), which
//!   a rewritten file's stubs and root are not in: that row gives the file and the slot a
//!   rooted shape, and then the pull takes the frame. The sites: `server::persist_records`
//!   writes a rooted store through `crate::records_storage::rewrite`, as
//!   `client::persist_stored` does, under the server's writer lock and stamp check;
//!   `server::look_at_records` selects a rewritten entry and reads its frame
//!   ([`Store::recovering_stored`]) only where the server's format mark is stored; an append
//!   is planned past the frame's bytes; `server::apply_records` hands the pulled frame to
//!   [`Store::ingest_served`] and, where the server is following, to [`follow_records`];
//!   and the pull states that it takes one (`transport::fetch_record_frames_with_known`).
//! - **A seatless Mac's seat from a format-2 member.** `client::seat_ask` reads the seat's
//!   records at the Mac's own format, roots included (`client::ingest_as_read`); a seatless
//!   Mac at format 1 is refused by a format-2 member, asks it nothing again
//!   (`client::later_records_bound`) and does not follow by a seat bundle: its records pass
//!   follows its vault first.
//! - **A cut's version-1 stubs in a vault that grew** (tracker FL-148F). A Mac behind a cut
//!   that reads a version-1 record's stub takes the root only on the shares of more than half
//!   of the seats its log then holds ([`super::served`]), the seats admitted since the cut
//!   among them. So a vault of three seats, two of which shared, hands a fourth Mac a bundle
//!   it refuses (`a served root is not final by its shares`) until a third seat shares: the
//!   pass that prunes decides whose shares it waits for.
//! - **A rewritten file's writers.** `client::persist_stored` writes a rewritten entry under
//!   the generation it selected and leaves the pin's capacity as the rewrite set it; a store
//!   opened through the vault's head appends nothing and is written whole. The other readers
//!   of the records entry select as format 1 does and refuse a rewritten layout:
//!   `Paths::records_data_file`, `client::vault_memo`'s and `client::seat_ask`'s standing,
//!   `client::genesis`'s private-realm read, `client::remove`'s look, aterm-db's moved-home
//!   check and aterm-agent's fleet log and census ([`super::rooted`]'s "Not yet").
//! - **A marked server's reads on a build that reads no format 2** (tracker FL-148E). Such a
//!   server takes no POST; its records GET still serves, to a puller that states its mark's
//!   format, the records before the file's first version-2 one, which that puller holds or
//!   takes as any records. Its peer pull states the mark's format and, reading format 1,
//!   refuses a version-2 frame.
//! - **The follow pull's notes** (tracker FL-148E). The server's second ask of a peer
//!   (`server::follow_later_peers`) proves no digest to the pull gate, and a peer whose
//!   refusal was its pull's only answer at several endpoints is asked again at each, until
//!   one answer moves the server into format 2.
//! - **The frontier database protocol** (tracker FL-148E). `atdb/5` to `atdb/8` state no
//!   format or byte capacity (`aterm-db` `control/frontier.rs`: `Metadata`, `put_bound`,
//!   `read_bound`), so a Mac past format 1 speaks none of them (`format_one`) and its Commons
//!   tables move by the mesh's table lanes alone. They state both, read and decode records at
//!   the store's format, and aterm-db's records exchange (`Store::missing`,
//!   `complete_missing`) reads a dropped record as dropped and carries the served frame. Its
//!   tests need version-2 records and a rooted store, which only this crate's tests build
//!   (`Draft::sign_two`, `Faults::follows`): that row decides the seam.
//! - **A Mac below the fence that is told so** (tracker FL-148E). A Mac whose build does not
//!   follow is told nothing when a source refuses it for a later format
//!   ([`crate::ErrorKind::LaterFormat`]). Its own `aterm fleet` row shows no `needs update`:
//!   the refusal is a source's word, kept nowhere, and `aterm_agent::fleet_view` reads this
//!   Mac's mark alone. Its sync does not say so either: a sync's outcome is its envelope's,
//!   the records pass that was refused returns nothing to it (`client::subscriber_records`,
//!   and `client::bounded`'s reader pass, drop the pass's result), and
//!   `crate::sync_diagnostic` has no word for the kind (it reads `other`, and no path hands a
//!   sync's outcome that kind). That row decides what tells the person: the pass's refusal
//!   kept beside the sync's outcome and a word for it, which a build before the word reads as
//!   no diagnostic, or a state the row reads, which a LAN host's unsigned word could then
//!   raise.
//! - **Readers of a dropped record** (tracker FL-148E; [`super::rooted`]'s "Not yet"):
//!   `claude_token::plan` reads the draft of the write a register's op stands for
//!   (`OpenedAudienceEntries::stands_for`, then `Log::get` in its `draft_of`), and a carried
//!   rep may be one a prune dropped: the settle then holds, where it must read the stub's
//!   signer, time and epoch; `client::log_entries::legacy_applied_held`'s
//!   `Log::down_set_search` walks held records alone, so a log that dropped an applied head
//!   reads the applied log absent.
//! - **The writers** (tracker FL-148F): nothing signs a version-2 record or a `Checkpoint`,
//!   sheds or prunes, and the one rewrite is a marked Mac's taking of a served root, whose
//!   write hands its hook nothing (a marked Mac's members lane is shut), so the one generation
//!   a write hands its hook is that of an entry never rewritten (`client::WRITTEN_UNDER`). The authoring stops
//!   (`membership_log/budgets.rs`), the held drafts (`client/log_entries.rs` `HELD_LIMIT`) and
//!   the writes that wait for a key (`client/log_entries/waiting.rs` `WAITING_LIMIT`) name
//!   [`Reading::STORE_BYTES`], every format's capacity while each holds one reply.
//! - **The flip** (tracker FL-148G): [`crate::commons::hearing::READS_FORMAT`] names no
//!   release, and must not until every item above of FL-148E has landed.
//! - **A store past one reply** (tracker FL-249): [`crate::crossing::FORMAT_TWO_BYTES`] is
//!   one reply's bytes, so ATERM-KI-1116-1470's paging gates nothing here.

use super::{Reading, Record, RecordId, Store};
use crate::Result;
use crate::crossing::{FORMAT_ONE, FORMAT_TWO, FormatMark};

/// The format `mark`'s node reads its records at: [`FORMAT_TWO`] where its mark names it and
/// its build reads it ([`FormatMark::follows`]), format 1 everywhere else. A mark of a format
/// this build does not read leaves the node reading format 1 and stating the mark's: it takes
/// and serves nothing across the line, as a build before the reader does.
///
/// # Errors
/// The mark cannot be read ([`FormatMark::stored`]).
pub(crate) fn reading(mark: &FormatMark) -> Result<Reading> {
    if !mark.follows() {
        return Ok(Reading::ONE);
    }
    Ok(match mark.stored()? {
        FORMAT_TWO => Reading::Format(FORMAT_TWO),
        _ => Reading::ONE,
    })
}

/// The signed evidence `store` holds that its vault is in format 2: the first record of
/// version 2 its log holds complete whose signer holds a seat in that log. `None` where it
/// holds none: a version-2 record that waits on a parent, or one signed by a key that holds no
/// seat now, is none.
#[must_use]
pub(crate) fn evidence(store: &Store) -> Option<RecordId> {
    let log = store.log()?;
    let membership = log.membership_ref();
    log.records()
        .find(|record| record.version() == 2 && membership.get(record.actor()).is_some())
        .map(|record| *record.id())
}

/// A copy of a node's store that took a format-2 source's records and holds the evidence
/// ([`evidence`]): what the node's store becomes once its mark is stored ([`Self::take`]).
/// Nothing reads it before then.
pub(crate) struct Followed {
    store: Store,
    added: usize,
}

impl Followed {
    /// How many records the copy holds that the node's store did not.
    pub(crate) const fn added(&self) -> usize {
        self.added
    }

    /// Store `mark` at [`FORMAT_TWO`], durably, and only then run `write` on the copy, now read
    /// at format 2: `write` is what makes it the node's store (its records file, its slot).
    ///
    /// # Errors
    /// The mark cannot be stored (then `write` does not run and nothing reads the copy), or
    /// `write` fails (the node then holds the mark and its store as it was).
    pub(crate) fn take<T>(
        self,
        mark: &FormatMark,
        write: impl FnOnce(&Store) -> Result<T>,
    ) -> Result<T> {
        crate::crossing::after_format_mark(mark, FORMAT_TWO, || write(&self.store))
    }
}

/// Whether `mark`'s node takes for the evidence what is none: never, outside a test's mutants,
/// a source's statement of format 2 (`crate::crossing::Faults::mark_on_header`) and a
/// version-2 record `store` holds only waiting on a parent
/// (`crate::crossing::Faults::mark_on_waiting`).
fn claimed(mark: &FormatMark, store: &Store) -> bool {
    #[cfg(test)]
    {
        let faults = crate::crossing::faults(mark.path());
        faults.mark_on_header
            || (faults.mark_on_waiting
                && store.inventory().iter().any(|id| {
                    store
                        .uncomplete(id)
                        .is_some_and(|record| record.version() == 2)
                }))
    }
    #[cfg(not(test))]
    {
        let _ = (mark, store);
        false
    }
}

/// Store `mark` at [`FORMAT_TWO`], durably, and only then run `write`, which makes `store` the
/// store of `mark`'s node: for a node that holds no store to copy (a new Mac taking a join
/// bundle), whose `store` nothing reads before `write`. Refused, with no mark stored, where
/// `store` holds no evidence ([`evidence`]) or this node's build reads no format 2.
///
/// # Errors
/// As said; the mark cannot be stored (then `write` does not run); or `write` fails.
pub(crate) fn take_on_evidence<T>(
    mark: &FormatMark,
    store: &Store,
    write: impl FnOnce() -> Result<T>,
) -> Result<T> {
    if !mark.follows() {
        return Err(crate::Error::new(
            "this build reads no membership records of format 2",
        ));
    }
    if !holds_evidence(mark, store) {
        return Err(crate::Error::new(
            "the records are stated to be in format 2 and hold no record of it that a seat signed",
        ));
    }
    crate::crossing::after_format_mark(mark, FORMAT_TWO, write)
}

/// Whether `store`, which `mark`'s node would take from a source that states format 2, holds
/// the evidence its mark is stored on ([`evidence`]).
pub(crate) fn holds_evidence(mark: &FormatMark, store: &Store) -> bool {
    // MUTANTS (`crate::crossing::Faults::mark_on_header`, `mark_on_waiting`): the source's
    // statement, or a version-2 record that only waits, is the evidence.
    evidence(store).is_some() || claimed(mark, store)
}

/// Take `records`, which a source that states format 2 sent the node whose mark is `mark` and
/// whose store is `store`, into a copy of `store` read at [`Reading::Follow`], with the roots
/// the source served beside them (`served`, [`super::served`]): a node behind a format-2
/// vault's cut holds the records after the cut complete only once it took the root they follow,
/// and the evidence is a record its log holds complete. `Some` only where the copy then holds
/// the evidence ([`evidence`]); `None` where it does not (the copy is dropped: no mark is stored
/// and `store` is what it was), where this node's build reads no format 2
/// ([`FormatMark::follows`]), or where its mark already names a later format than 1.
///
/// # Errors
/// The mark cannot be read, or the copy refuses the records or a served root
/// ([`Store::ingest_served`]): nothing is stored either way.
pub(crate) fn follow_records(
    mark: &FormatMark,
    store: &Store,
    records: Vec<Record>,
    served: Option<&super::Served>,
) -> Result<Option<Followed>> {
    if !mark.follows() || mark.stored()? != FORMAT_ONE {
        return Ok(None);
    }
    let mut copy = store.clone().read_at(Reading::Follow);
    let added = copy.ingest_served(records, served)?;
    if !holds_evidence(mark, &copy) {
        return Ok(None);
    }
    Ok(Some(Followed {
        store: copy.read_at(Reading::Format(FORMAT_TWO)),
        added,
    }))
}

#[cfg(test)]
#[path = "follow_tests.rs"]
mod tests;
