// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The publisher (docs/DESIGN-shared-files-2026-11-11.md §4): one pass drains each shared file's
//! outbox through this Mac's writer session, an intent a record. It runs in three places: the
//! process that owns the Commons runtime, in its one-minute pass (`crate::alab::upkeep`); a
//! write verb of `aterm file`, bounded to two seconds; and nowhere else.
//!
//! - **Observed only.** A cell is written only while its register's candidates are still the
//!   ones the intent's author saw ([`grid::settle`], whose comparison is the kernel
//!   `aterm_db_core::file_merge::settle`). An intent is one record or none: where any register
//!   it writes moved, the whole intent is HELD, durable in the held list, shown `⚠ mine ‖
//!   theirs` by a read, never applied and never dropped. A person resolves it.
//! - **Once.** The intent's id is its record's request id. The engine scopes a request to the
//!   actor that asks ([`Log::retry`]), so before it signs, a pass looks through every retained
//!   record of the table for one that carries the id ([`Log::carried`]), whichever actor of this
//!   Mac's lineage signed it. The done marker is durable before the intent leaves the outbox, so
//!   a pass killed between the record and the marker, or between the marker and the dequeue,
//!   signs nothing again.
//! - **A person's drop wins or is told.** A pass signs an intent only under its claim
//!   ([`aterm_file::Claim`]): the outbox lock, taken once the intent is seen still queued and
//!   kept until its marker is written. A drop before the claim leaves nothing to sign; one after
//!   it waits, finds the marker and answers that the intent is published.
//! - **What a follow-up was written against.** An edit that follows an earlier intent of this
//!   Mac (`intent:<id>`) is compared with the candidates that intent left in the register: its
//!   operation where its record wrote the register, else the register as it stood when the
//!   intent settled (another writer had already written the same value). The marker keeps them
//!   ([`aterm_file::Done::left`]), so a later pass compares the same way.
//! - **A fenced writer never signs.** The writer session is the one every table write opens
//!   (`application::with_store_until`): it selects this Mac's admitted writer generation and
//!   refuses while a succession fences the home. The outbox is kept once per Mac, under its
//!   original home, so the intents a fenced Mac accepted are the ones its admitted successor
//!   signs, with its own actor.
//! - **An outbox takes its file's class's intents.** Through the outbox, an owner file
//!   ([`super::Class::Owner`]) takes no Mac's statement of its own row, and a Mac file
//!   ([`super::Class::Mac`]) no owner's write: the intent is held with the rule
//!   ([`super::Class::refuses`]), never signed. This is the publisher's rule alone. Code that
//!   writes the registry itself (a Mac's self-registration, the importer) passes no outbox; what
//!   it writes is judged by every read instead, under the authority rules of a collection that
//!   opts into them ([`crate::typed::authority`]), and projects as written in one that does not.
//! - **The table's rules hold every write and revert** as they hold a grid save or an owner verb
//!   ([`Compiled::rules`], as the owner's word): a fixed column written only at its row's
//!   birth, the table's own guard (an account's label naming another account's slug), a
//!   reference to a row the table holds, no unique set two live rows come to share and no
//!   retired row a live row names. A refused intent is held with the rule.
//! - **What it published is noted.** Each intent a record carries is noted in this Mac's
//!   journal spool between its marker and its dequeue, as a value-free `decided` event: the
//!   operation's first 8 bytes, the intent's verb and its `by` word ([`decided`]). The journal's
//!   flusher publishes it in `ai/events` (docs/DESIGN-shared-files-2026-11-11.md §5,
//!   "Provenance"). The word is a claim, never an admission condition.
//! - **Nothing here reads.** A pass opens no reader of a shared file and runs no materializer
//!   pass; the verb that asked runs one afterwards.

use super::Type;
use crate::said::Said;
use crate::{
    Error, Result,
    alab::journal::{Event, Kind, Source, Spool, Via, Word as EventWord, event::DECIDED_CLI},
    att::{self, Author, Compiled, View},
    catalog::{Directory, policy::Policy},
    database::{Change, Command},
    documents::{application, log::Log},
    generation,
    table::Session,
    typed::{
        Collection, LIFECYCLE,
        cell::{self, CellKey, CellValue},
        grid::{self, Plan},
        keys,
    },
};
use astream_wire::sync::Digest;
use aterm_file::{Claim, File, Intent, Mark, Name, outbox};
use aterm_json::Value;
use aterm_secrets::client::Paths;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    time::{Duration, Instant},
};

/// How long a write verb's own attempt may take before it answers `queued`.
pub const INLINE: Duration = Duration::from_secs(2);

/// How a hold's reason begins where the register moved under the write: another writer's value
/// reached it after the snapshot the write was read against.
pub const MOVED: &str = "changed since it was written";

/// The cells a hold for [`MOVED`] names, each `<row>.<column>` with its row's key made safe
/// (`keys::safe_key`), as the publisher lists them; `None` for a hold of any other reason.
#[must_use]
pub fn moved(why: &str) -> Option<Vec<&str>> {
    let cells = why.strip_prefix(MOVED)?.strip_prefix(": ")?;
    Some(cells.split(", ").collect())
}

/// What bounds one pass, and where it notes what it published.
#[derive(Clone, Copy, Debug)]
pub struct Pass<'a> {
    /// When the pass stops opening custody and signing.
    pub by: Instant,
    /// This Mac's clock, Unix milliseconds, for display.
    pub now_ms: u64,
    /// The journal spool each intent a record carries is noted in, as a `decided` event
    /// ([`decided`]), which this Mac's flusher publishes in `ai/events`: this user's
    /// ([`crate::alab::upkeep::journal_spool`]); `None` notes nothing.
    pub journal: Option<&'a Spool>,
}

/// The verb a `decided` event names (`via`): what `intent` does, read off its own shape, since an
/// intent keeps no verb. A revert and a person's resolve of a held write say so; a row to be born
/// is an `add`; a Mac's statement of its own row ([`Intent::mine`]) a `state`; a write that ends
/// a row's lifecycle a `rm`; any other a `set`. A face's commit writes intents of these shapes,
/// and each is named by what it does.
#[must_use]
pub fn via(intent: &Intent) -> Via {
    if intent.revert.is_some() {
        Via::Revert
    } else if intent.resolves.is_some() {
        Via::Resolve
    } else if intent.born {
        Via::Add
    } else if intent.mine() {
        Via::State
    } else if intent.edits.iter().any(|edit| {
        edit.column == LIFECYCLE
            && edit.elem.is_none()
            && edit.value.as_str().is_some_and(|word| word != "live")
    }) {
        Via::Rm
    } else {
        Via::Set
    }
}

/// The `decided` event (docs/DESIGN-shared-files-2026-11-11.md §5, "Provenance"; §10 file 5)
/// that says this Mac published `intent` as the operation `op` (64 lower-case hex) at `at_ms`, its
/// marker's time: the operation's first 8 bytes, the intent's verb ([`via`]) and its `by` word.
/// It names no account, value, address, path or session: every field is a word of the closed
/// vocabulary. `None` where `op` is no digest or `by` no word of the vocabulary's bound (an
/// older build's line), so nothing is noted.
#[must_use]
pub fn decided(intent: &Intent, op: &str, at_ms: u64) -> Option<Event> {
    outbox::digest(op)?;
    Some(Event {
        t: at_ms / 1000,
        kind: Kind::Decided,
        account: None,
        cli: EventWord::new(DECIDED_CLI)?,
        sub: None,
        window: None,
        model: None,
        pct_x10: None,
        reset: None,
        res: None,
        until: None,
        plan: None,
        src: Source::File,
        why: None,
        to: None,
        tab: None,
        harness: None,
        op: Some(EventWord::new(op.get(..16)?)?),
        via: Some(via(intent)),
        program: Some(EventWord::new(&intent.by).filter(|_| aterm_file::name::part(&intent.by))?),
    })
}

/// What a pass did with one intent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Answer {
    /// A record carries it: the operation, lower-case hex, or `None` where the table already
    /// held every value it wrote and nothing was signed.
    Published { op: Option<String> },
    /// It stays in the outbox; why, value-free.
    Queued { why: String },
    /// A register it writes moved, or this build refuses it: held; why, value-free.
    Held { why: String },
}

/// What one pass did for one file.
#[derive(Clone, Debug)]
pub struct Outcome {
    pub name: Name,
    /// Each intent the pass looked at, by id, oldest first.
    pub intents: Vec<(String, Answer)>,
    /// A local file could not be written.
    pub problem: Option<String>,
}

impl Outcome {
    /// What the pass did with the intent `id`.
    #[must_use]
    pub fn answer(&self, id: &str) -> Option<&Answer> {
        self.intents
            .iter()
            .find(|(intent, _)| intent == id)
            .map(|(_, answer)| answer)
    }
}

/// One file's part of a pass.
struct Work {
    file: &'static Type,
    held: File,
    outcome: Outcome,
}

impl Work {
    fn answer(&mut self, id: &str, answer: Answer) {
        self.outcome.intents.retain(|(intent, _)| intent != id);
        self.outcome.intents.push((id.to_owned(), answer));
    }

    fn problem(&mut self, error: &std::io::Error) {
        self.outcome.problem = Some(error.to_string());
    }

    /// The marker (under the pass's claim, where it holds one), the `decided` note of an intent
    /// a record carries, then the dequeue: the order a kill between any two is safe in. A pass
    /// killed before its note leaves the intent queued beside its marker, and the next pass
    /// notes it as it dequeues it; one killed after the note notes the same event again (its
    /// time is the marker's, `marked_ms`), which the flusher publishes once: its outbox keeps
    /// each event once and drops one this Mac's batches in the window already hold. An intent
    /// another pass dequeued first (the marker finds it no longer queued) was noted by that
    /// pass, and is not noted again.
    fn done(
        &mut self,
        claim: Option<Claim>,
        intent: &Intent,
        mark: &Mark,
        marked_ms: u64,
        journal: Option<&Spool>,
    ) {
        let id = intent.id.as_str();
        let marked = match claim {
            Some(claim) => claim.mark_done(mark, marked_ms),
            None => {
                #[expect(
                    clippy::disallowed_methods,
                    reason = "the publisher marks an intent a record carries"
                )]
                let marked = self.held.mark_done(id, mark, marked_ms);
                marked
            }
        };
        let queued = match marked {
            Ok(queued) => queued,
            Err(error) => {
                // The record is signed and no marker says so: the next pass finds the record by
                // the intent's id and writes the marker then.
                self.problem(&error);
                self.answer(
                    id,
                    Answer::Queued {
                        why: "its marker could not be written".to_owned(),
                    },
                );
                return;
            }
        };
        #[cfg(test)]
        if seam::stops(seam::Stop::AfterMarker) {
            self.answer(
                id,
                Answer::Queued {
                    why: "the pass was killed after its marker".to_owned(),
                },
            );
            return;
        }
        if queued && let (Some(journal), Some(op)) = (journal, &mark.op) {
            self.note(journal, intent, op, marked_ms);
        }
        #[cfg(test)]
        if seam::stops(seam::Stop::AfterNote) {
            self.answer(
                id,
                Answer::Queued {
                    why: "the pass was killed after its note".to_owned(),
                },
            );
            return;
        }
        #[expect(
            clippy::disallowed_methods,
            reason = "the publisher takes a marked intent out of the outbox"
        )]
        let dequeued = self.held.dequeue(id);
        if let Err(error) = dequeued {
            self.problem(&error);
        }
        self.answer(
            id,
            Answer::Published {
                op: mark.op.clone(),
            },
        );
    }

    /// Note in `journal` that the operation `op` published `intent` ([`decided`]). The note costs
    /// the publication nothing: the record is signed and marked whatever the spool does, a full
    /// spool drops the event as it drops every one, and one that cannot be written is this
    /// outcome's problem.
    fn note(&mut self, journal: &Spool, intent: &Intent, op: &str, marked_ms: u64) {
        let Some(event) = decided(intent, op, marked_ms) else {
            return;
        };
        if let Err(error) = journal.append(&event) {
            self.problem(&error);
        }
    }

    fn hold(&mut self, claim: Claim, id: &str, why: String, now_ms: u64) {
        match claim.hold(&why, now_ms) {
            Ok(_) => self.answer(id, Answer::Held { why }),
            Err(error) => {
                self.problem(&error);
                self.answer(
                    id,
                    Answer::Queued {
                        why: "the held list could not be written".to_owned(),
                    },
                );
            }
        }
    }

    /// Every intent still waiting stays queued for `why`.
    fn queue(&mut self, why: &str) {
        let waiting: Vec<String> = self
            .held
            .outbox()
            .waiting()
            .map(|intent| intent.id.clone())
            .collect();
        for id in waiting {
            if self.outcome.answer(&id).is_none() {
                self.answer(
                    &id,
                    Answer::Queued {
                        why: why.to_owned(),
                    },
                );
            }
        }
    }
}

/// Why a pass could not open this Mac's writer, in one word and the refusing layer's own.
fn why(home: &Paths, error: &Error) -> String {
    let word = if error.is_busy() {
        "busy"
    } else if error.is_awaiting_membership() || error.is_enrollment_pending() {
        "membership"
    } else if generation::fenced(home).unwrap_or(false) {
        "succession"
    } else {
        "custody"
    };
    format!("{word}: {error}")
}

/// `key` with a `@self` suffix filled: the Commons machine uuid of the Mac that signs.
#[must_use]
pub fn filled(key: &str, signer: &str) -> String {
    match key.strip_suffix(outbox::SELF) {
        Some(stem) => format!("{stem}{signer}"),
        None => key.to_owned(),
    }
}

/// The Commons machine uuid of the session's own seat, as a key names it.
fn signer(session: &Session<'_>) -> Result<String> {
    let actor = session.actor();
    session
        .members()?
        .iter()
        .find(|binding| binding.actor() == actor)
        .map(|binding| keys::uuid32(binding.machine()))
        .ok_or_else(|| Error::new("this Mac's writer holds no seat in its lab"))
}

fn request(id: &str) -> Option<[u8; 16]> {
    let mut out = [0; 16];
    (outbox::is_id(id) && aterm_codec::hex::decode_into(id.as_bytes(), &mut out)).then_some(out)
}

/// What became of the earlier intents a pass has settled, for an edit that says it follows one
/// (`intent:<id>` among its expected candidates).
#[derive(Default)]
struct Earlier {
    /// Published: the operation, or `None` where nothing was signed.
    published: BTreeMap<String, Option<Digest>>,
    /// What each published intent left in each register it writes (by intent id, then stored
    /// key): what an edit that follows it was written against.
    left: BTreeMap<(String, String), Vec<Digest>>,
    /// Still in the outbox, not settled by this pass yet.
    waiting: Vec<String>,
}

impl Earlier {
    fn of(file: &File) -> Self {
        let mut earlier = Self::default();
        for done in file.outbox().done {
            let op = done.op.as_deref().and_then(outbox::digest);
            earlier.settled(&done.intent.id, op, &done.left);
        }
        earlier
    }

    /// `id` settled: published as `op`, leaving `left` in its registers.
    fn settled(&mut self, id: &str, op: Option<Digest>, left: &BTreeMap<String, Vec<String>>) {
        self.published.insert(id.to_owned(), op);
        for (key, ops) in left {
            let ops = ops.iter().filter_map(|op| outbox::digest(op)).collect();
            self.left.insert((id.to_owned(), key.clone()), ops);
        }
    }
}

/// The candidates of the register `key` of `collection` in `view`, sorted.
fn candidates(view: &View, collection: &Collection, key: &str) -> Vec<Digest> {
    let mut now: Vec<Digest> = view
        .cells
        .get(&(collection.name.to_owned(), key.to_owned()))
        .map(|cell| cell.candidates.iter().map(|c| c.operation).collect())
        .unwrap_or_default();
    now.sort_unstable();
    now
}

/// What `intent` left in each register it writes once it settled: `op` where its record wrote
/// the register (`wrote` names those), else the register as `view` holds it. A row to be born
/// and a revert have no follower ([`super::write`] follows only an edit of a published row), so
/// they leave nothing to compare with.
fn left(
    collection: &'static Collection,
    intent: &Intent,
    signer: &str,
    view: &View,
    op: Option<Digest>,
    wrote: &BTreeSet<String>,
) -> BTreeMap<String, Vec<String>> {
    let mut left = BTreeMap::new();
    if intent.born || intent.revert.is_some() {
        return left;
    }
    for edit in &intent.edits {
        let pk = filled(&edit.key, signer);
        let Ok(change) = cell::change(
            collection,
            CellKey {
                pk: &pk,
                col: &edit.column,
                elem: edit.elem.as_deref(),
            },
            &CellValue::owner(collection.version, edit.value.clone()),
        ) else {
            continue;
        };
        let ops = match op {
            Some(op) if wrote.contains(&change.key) => vec![op],
            _ => candidates(view, collection, &change.key),
        };
        let ops = ops
            .iter()
            .map(|op| aterm_codec::hex::encode_lower(op))
            .collect();
        left.insert(change.key, ops);
    }
    left
}

/// Why an intent cannot be planned.
enum Stop {
    /// Held: this build, or the table as it is, refuses it.
    Hold(String),
    /// It follows an intent that is still queued.
    Wait(String),
}

/// The save `intent` asks for: its keys filled, its values checked as any owner's write, and
/// each edit carrying the candidates its author saw.
fn plan(
    compiled: &Compiled,
    collection: &'static Collection,
    intent: &Intent,
    signer: &str,
    view: &View,
    earlier: &Earlier,
) -> std::result::Result<Plan, Stop> {
    let mut plan = Plan::default();
    let born = if intent.born {
        let values: BTreeMap<String, Value> = intent
            .edits
            .iter()
            .filter(|edit| edit.elem.is_none() && !edit.value.is_null())
            .map(|edit| (edit.column.clone(), edit.value.clone()))
            .collect();
        let pk = (compiled.birth)(collection, &values, &view.cells)
            .map_err(|error| Stop::Hold(error.to_string()))?;
        if !crate::typed::pk_grammar(&pk) {
            return Err(Stop::Hold(
                "the derived key breaks the key grammar".to_owned(),
            ));
        }
        plan.born.push(pk.clone());
        Some(pk)
    } else {
        None
    };
    let retire = if compiled.table.ends.contains(&collection.name) {
        "ended"
    } else {
        "retired"
    };
    for edit in &intent.edits {
        let pk = born.clone().unwrap_or_else(|| filled(&edit.key, signer));
        let change = cell::change(
            collection,
            CellKey {
                pk: &pk,
                col: &edit.column,
                elem: edit.elem.as_deref(),
            },
            &CellValue::owner(collection.version, edit.value.clone()),
        )
        .map_err(|error| Stop::Hold(error.to_string()))?;
        let mut expected = Vec::with_capacity(edit.expected.len());
        for entry in &edit.expected {
            match entry.strip_prefix(outbox::INTENT) {
                None => expected.push(outbox::digest(entry).ok_or_else(|| {
                    Stop::Hold("an expected candidate is no operation digest".to_owned())
                })?),
                Some(id) => match earlier.published.get(id) {
                    // What the earlier intent left in this register is what this one's author
                    // saw there: its own operation, or the candidates that already said its
                    // value.
                    Some(_)
                        if earlier
                            .left
                            .contains_key(&(id.to_owned(), change.key.clone())) =>
                    {
                        expected.extend(&earlier.left[&(id.to_owned(), change.key.clone())]);
                    }
                    // A marker that does not say what it left: its operation, if it signed one.
                    Some(Some(op)) => expected.push(*op),
                    Some(None) => {}
                    None if earlier.waiting.iter().any(|waiting| waiting == id) => {
                        return Err(Stop::Wait(format!(
                            "it follows {id}, which is not published yet"
                        )));
                    }
                    None => {
                        return Err(Stop::Hold(format!(
                            "it follows {id}, which was held or dropped"
                        )));
                    }
                },
            }
        }
        expected.sort_unstable();
        expected.dedup();
        if edit.column == LIFECYCLE
            && edit.value.as_str() == Some(retire)
            && !plan.retired.contains(&pk)
        {
            plan.retired.push(pk.clone());
        }
        let edit = grid::Edit {
            pk: pk.clone(),
            column: edit.column.clone(),
            elem: edit.elem.clone(),
            key: change.key,
            x: edit.value.clone(),
            value: change.value.unwrap_or_default(),
            expected,
            mine: edit.text.clone(),
            at: 0,
            record: 0,
            line: 0,
        };
        match plan.rows.iter_mut().find(|(row, _)| *row == pk) {
            Some((_, edits)) => edits.push(edit),
            None => plan.rows.push((pk, vec![edit])),
        }
    }
    Ok(plan)
}

/// What one intent came to under the writer.
enum Step {
    /// A record carries it, and wrote these registers (stored keys) of the file's collection.
    Signed(Digest, BTreeSet<String>),
    /// The table already held every value it wrote.
    Holds,
    Hold(String),
    Wait(String),
}

/// The open writer session one family's intents are settled in.
struct Writer<'a, 'b> {
    session: &'a Session<'b>,
    compiled: &'static Compiled,
    signer: String,
}

impl Writer<'_, '_> {
    /// Settle `intent` against the table as it is: sign it, or say why not.
    fn settle(
        &self,
        store: &mut crate::table::store::Store,
        log: &Log,
        view: &View,
        collection: &'static Collection,
        intent: &Intent,
        earlier: &Earlier,
    ) -> Result<Step> {
        let Some(request) = request(&intent.id) else {
            return Ok(Step::Hold("its id is no request id".to_owned()));
        };
        let wrote = |changes: Option<&[Change]>| -> BTreeSet<String> {
            changes
                .unwrap_or_default()
                .iter()
                .filter(|change| change.collection == collection.name)
                .map(|change| change.key.clone())
                .collect()
        };
        // Whichever actor of this Mac's lineage signed it, it is signed.
        if let Some(op) = log.carried(request) {
            return Ok(Step::Signed(op, wrote(log.record_changes(&op))));
        }
        // Who made it is what its author's process stated when it was accepted, signed as it is
        // wherever the generation takes it: never this pass's own process, which may be another
        // agent's verb or the owner's minute pass. An intent that states none is signed stating
        // none, and one whose statement breaks its rules is held. A generation that takes no
        // statement (version 1 records) signs none, whatever the intent states, so it never
        // holds a write over a statement it would not carry: a later build's statement this one
        // cannot judge is no reason to stop.
        let carried = intent.said.as_ref().filter(|_| self.compiled.records >= 2);
        let said = match carried.map(Said::from_value).transpose() {
            Ok(said) => self.compiled.carries(said.as_ref()),
            Err(why) => {
                return Ok(Step::Hold(format!(
                    "its statement of who made it is refused: {why}"
                )));
            }
        };
        let command = if let Some(target) = &intent.revert {
            let Some(target) = outbox::digest(target) else {
                return Ok(Step::Hold("it names no operation to revert".to_owned()));
            };
            let mut command =
                match log.compensate(request, target, &view.quota.over, &view.inert.changes) {
                    Ok(command) => command,
                    Err(error) => return Ok(Step::Hold(error.to_string())),
                };
            if let Command::Revert { said: carried, .. } = &mut command {
                carried.clone_from(&said);
            }
            // A file reverts only what wrote it: an operation that changed another collection
            // is undone through that one's file, under its own history.
            if command
                .changes()
                .unwrap_or_default()
                .iter()
                .any(|change| change.collection != collection.name)
            {
                return Ok(Step::Hold(format!(
                    "that operation changes more than {}; revert it where each of its cells is \
                     shared",
                    collection.name
                )));
            }
            if let Some(changes) = command.changes()
                && let Some(refused) =
                    self.compiled
                        .rules(&mut [changes.to_vec()], view, Author::Owner)?
            {
                return Ok(Step::Hold(Error::from(refused).to_string()));
            }
            command
        } else {
            let plan = match plan(
                self.compiled,
                collection,
                intent,
                &self.signer,
                view,
                earlier,
            ) {
                Ok(plan) => plan,
                Err(Stop::Hold(why)) => return Ok(Step::Hold(why)),
                Err(Stop::Wait(why)) => return Ok(Step::Wait(why)),
            };
            #[cfg(test)]
            let blind = seam::ignores_expected();
            #[cfg(not(test))]
            let blind = false;
            let mut rows: Vec<Vec<Change>> = if blind {
                // The negative control of `model_tests`: every edit written whatever the
                // register holds now.
                plan.rows
                    .iter()
                    .map(|(_, edits)| {
                        edits
                            .iter()
                            .map(|edit| Change {
                                collection: collection.name.to_owned(),
                                key: edit.key.clone(),
                                value: Some(edit.value.clone()),
                            })
                            .collect()
                    })
                    .collect()
            } else {
                let settled = grid::settle(collection, &plan, &view.cells);
                if !settled.stale.is_empty() {
                    let moved: Vec<String> = settled
                        .stale
                        .iter()
                        .map(|stale| format!("{}.{}", keys::safe_key(&stale.pk), stale.column))
                        .collect();
                    return Ok(Step::Hold(format!("{MOVED}: {}", moved.join(", "))));
                }
                settled.rows
            };
            if rows.is_empty() {
                return Ok(Step::Holds);
            }
            if let Some(refused) = self.compiled.rules(&mut rows, view, Author::Owner)? {
                return Ok(Step::Hold(Error::from(refused).to_string()));
            }
            // One intent, one record: its rows are never split across two. Where the family
            // gives each row's change a record of its own (`Compiled::rows_apart`), an intent
            // that writes two rows would give them one record and one reason, so it is held.
            if self.compiled.rows_apart && rows.iter().filter(|row| !row.is_empty()).count() > 1 {
                return Ok(Step::Hold(
                    "it writes more than one row, and this table keeps each row's change in a \
                     record of its own; write one row at a time"
                        .to_owned(),
                ));
            }
            let changes: Vec<Change> = rows.into_iter().flatten().collect();
            let mut packed = match att::pack_saying(vec![changes], said.as_deref()) {
                Ok(packed) => packed,
                Err(error) => return Ok(Step::Hold(error.to_string())),
            };
            let (Some(changes), true) = (packed.pop(), packed.is_empty()) else {
                return Ok(Step::Hold("it passes one record's limits".to_owned()));
            };
            Command::Write {
                request,
                changes,
                said,
            }
        };
        self.compiled.admits(&command)?;
        // One record's body bounds what it carries: a revert's restored values and its
        // statement included (a write's packing counted them already).
        let payload = command.encode()?;
        if payload.len() > crate::crypto::MAX_BODY {
            return Ok(Step::Hold(
                if command.said().is_some() {
                    "it and its statement of who made it pass one record's limits"
                } else {
                    "it passes one record's limits"
                }
                .to_owned(),
            ));
        }
        // A record still waiting on a causal parent may write a row this one writes: its
        // candidates are not in the cells compared above, so this intent waits for it. A write
        // of any other row goes now.
        if let Some(why) = view.holds_back(&[command.changes().unwrap_or_default().to_vec()]) {
            return Ok(Step::Wait(why));
        }
        let wrote = wrote(command.changes());
        Ok(Step::Signed(
            application::append(self.session, store, &payload)?.id,
            wrote,
        ))
    }
}

/// Publish the intents of `works`, the files of one table family, under this Mac's writer.
fn family(
    home: &Paths,
    policy: &Policy,
    first: &'static Compiled,
    works: &mut [&mut Work],
    pass: &Pass<'_>,
) -> Result<()> {
    let compiled = att::current_within(home, first, pass.by)?;
    let table = compiled.table;
    application::with_store_until(
        home,
        false,
        move |bootstrap| compiled.descriptor(bootstrap.trust()),
        pass.by,
        |session, store, scope| {
            session.writer()?;
            let catalog = Directory::new(session.paths(), session.trust()).catalog_scope();
            if !policy.includes(catalog.digest(), scope.digest()) {
                for work in works.iter_mut() {
                    work.queue("off: [commons] excludes this table on this Mac");
                }
                return Ok(());
            }
            let writer = Writer {
                session,
                compiled,
                signer: signer(session)?,
            };
            // Read again after each record: the next intent is compared with the table as
            // the last one left it.
            let mut read: Option<(Log, View)> = None;
            for at in 0..works.len() {
                let Some((_, bound)) = works[at].file.edited() else {
                    continue;
                };
                let collection = compiled.collection(bound.name)?;
                let mut earlier = Earlier::of(&works[at].held);
                let intents: Vec<Intent> = works[at].held.outbox().waiting().cloned().collect();
                earlier.waiting = intents.iter().map(|intent| intent.id.clone()).collect();
                for intent in intents {
                    if Instant::now() >= pass.by {
                        for work in works.iter_mut() {
                            work.queue("busy: the pass ran out of time");
                        }
                        return Ok(());
                    }
                    let work = &mut *works[at];
                    let (log, view) = match read.take() {
                        Some(read) => read,
                        None => {
                            let log = Log::read_with(session, store, scope, compiled.decoder())?;
                            log.writable(session.actor())?;
                            let view = View::read(table, session, &log)?;
                            (log, view)
                        }
                    };
                    let carried = request(&intent.id).and_then(|request| log.carried(request));
                    #[cfg(test)]
                    seam::before_claim();
                    // The claim holds the outbox from here until the marker: a person's drop
                    // is either before it, and nothing is signed, or after the marker.
                    #[expect(
                        clippy::disallowed_methods,
                        reason = "the publisher claims a queued intent to sign or hold it"
                    )]
                    let claimed = work.held.claim(&intent.id);
                    let claim = match claimed {
                        Ok(Some(claim)) => claim,
                        Ok(None) => {
                            earlier.waiting.retain(|id| *id != intent.id);
                            read = Some((log, view));
                            continue;
                        }
                        Err(error) => {
                            work.problem(&error);
                            work.answer(
                                &intent.id,
                                Answer::Queued {
                                    why: "busy: its outbox is being changed".to_owned(),
                                },
                            );
                            read = Some((log, view));
                            continue;
                        }
                    };
                    // A file takes only its class's writes: an owner file no Mac's statement,
                    // a Mac file no owner write. What a record already carries is published,
                    // and settling finds it.
                    let step = match work.file.class.refuses(&intent) {
                        Some(why) if carried.is_none() => Step::Hold(why.to_owned()),
                        _ => writer.settle(store, &log, &view, collection, &intent, &earlier)?,
                    };
                    earlier.waiting.retain(|id| *id != intent.id);
                    match step {
                        Step::Signed(op, wrote) => {
                            #[cfg(test)]
                            if seam::stops(seam::Stop::AfterSign) {
                                work.answer(
                                    &intent.id,
                                    Answer::Queued {
                                        why: "the pass was killed after it signed".to_owned(),
                                    },
                                );
                                return Ok(());
                            }
                            // The store its record was appended to, and that store's witness
                            // after the append (or after the earlier pass's append a carried
                            // record was found by): a snapshot read at that count or later
                            // holds it.
                            let at = store.witnesses().first().map(|own| aterm_file::At {
                                store: own.digest,
                                count: own.count,
                            });
                            let mark = Mark {
                                op: Some(aterm_codec::hex::encode_lower(&op)),
                                generation: compiled.generation,
                                at,
                                left: left(
                                    collection,
                                    &intent,
                                    &writer.signer,
                                    &view,
                                    Some(op),
                                    &wrote,
                                ),
                            };
                            earlier.settled(&intent.id, Some(op), &mark.left);
                            work.done(Some(claim), &intent, &mark, pass.now_ms, pass.journal);
                            if carried.is_some() {
                                read = Some((log, view));
                            }
                        }
                        Step::Holds => {
                            let mark = Mark {
                                op: None,
                                generation: compiled.generation,
                                at: None,
                                left: left(
                                    collection,
                                    &intent,
                                    &writer.signer,
                                    &view,
                                    None,
                                    &BTreeSet::new(),
                                ),
                            };
                            earlier.settled(&intent.id, None, &mark.left);
                            work.done(Some(claim), &intent, &mark, pass.now_ms, pass.journal);
                            read = Some((log, view));
                        }
                        Step::Hold(why) => {
                            work.hold(claim, &intent.id, why, pass.now_ms);
                            read = Some((log, view));
                        }
                        Step::Wait(why) => {
                            drop(claim);
                            earlier.waiting.push(intent.id.clone());
                            work.answer(&intent.id, Answer::Queued { why });
                            read = Some((log, view));
                        }
                    }
                }
            }
            Ok(())
        },
    )
}

/// Run one pass over `only`, or over every file this build compiles: `paths` is any home of this
/// Mac's writer lineage, `config` its `aterm.toml`. Each file gets its outcome; nothing is an
/// error, and a pass with nothing queued opens no custody.
#[must_use]
pub fn pass(paths: &Paths, config: &Path, only: Option<&Name>, pass: &Pass<'_>) -> Vec<Outcome> {
    publish_intents(paths, config, super::FILES, only, pass)
}

/// [`pass`] over `files`, which a test names in place of this build's [`super::FILES`].
#[cfg(test)]
pub(crate) fn pass_over(
    paths: &Paths,
    config: &Path,
    files: &'static [super::Type],
    pass: &Pass<'_>,
) -> Vec<Outcome> {
    publish_intents(paths, config, files, None, pass)
}

/// [`pass`], whole. The `SharedFile` anchors sit here rather than on a function named `pass`:
/// the step audit tells anchored functions apart by their bare names, and the materializer's
/// pass and the catalog's release pass are anchored too.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "SharedFile",
        action = "Sign",
        project = "aterm_db::file::model_tests::write_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "SharedFile",
        action = "Done",
        project = "aterm_db::file::model_tests::write_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "SharedFile",
        action = "Hold",
        project = "aterm_db::file::model_tests::write_transition"
    )
)]
fn publish_intents(
    paths: &Paths,
    config: &Path,
    files: &'static [super::Type],
    only: Option<&Name>,
    pass: &Pass<'_>,
) -> Vec<Outcome> {
    let home = generation::original(paths);
    let root = super::root(&home);
    let mut works: Vec<Work> = files
        .iter()
        .filter(|file| only.is_none_or(|only| *only == file.file()))
        .map(|file| Work {
            file,
            held: File::at(&root, &file.file()),
            outcome: Outcome {
                name: file.file(),
                intents: Vec::new(),
                problem: None,
            },
        })
        .collect();
    // What a killed pass left: an intent whose marker is written is noted at the marker's time
    // and leaves the outbox; nothing is signed again.
    for work in &mut works {
        let outbox = work.held.outbox();
        for intent in &outbox.queued {
            if let Some(done) = outbox.done.iter().find(|done| done.intent.id == intent.id) {
                let mark = Mark {
                    op: done.op.clone(),
                    generation: done.generation.unwrap_or_default(),
                    at: done.at,
                    left: done.left.clone(),
                };
                #[cfg(test)]
                seam::before_claim();
                work.done(None, intent, &mark, done.ms, pass.journal);
            }
        }
    }
    let waits = |works: &[Work]| {
        works
            .iter()
            .any(|work| work.held.outbox().waiting().next().is_some())
    };
    if !waits(&works) {
        return works.into_iter().map(|work| work.outcome).collect();
    }
    let policy = match Policy::read(config) {
        Ok(policy) if policy.enabled => Some(policy),
        Ok(_) => {
            for work in &mut works {
                work.queue("off: [commons] turns sharing off on this Mac");
            }
            None
        }
        Err(error) => {
            for work in &mut works {
                work.queue(&format!("policy: {error}"));
            }
            None
        }
    };
    if let Some(policy) = policy {
        // A log, a file of the journal ring and a view take no intent: each Mac's program
        // appends its own log entries, its rows go out with its journal flush, and nobody
        // writes a view. What waits in such a file's outbox stays there, said why, and is
        // never signed.
        for work in &mut works {
            if work.file.edited().is_none() && work.held.outbox().waiting().next().is_some() {
                work.queue(if work.file.table().is_some() {
                    "this file is a log each machine's program appends to, not its outbox"
                } else {
                    "this file is written by each Mac's journal flush, not by its outbox"
                });
            }
        }
        let mut families: Vec<&'static Compiled> = Vec::new();
        for (family, _) in works.iter().filter_map(|work| work.file.edited()) {
            if !families
                .iter()
                .any(|known| known.table.role == family.table.role)
            {
                families.push(family);
            }
        }
        for first in families {
            let mut of: Vec<&mut Work> = works
                .iter_mut()
                .filter(|work| {
                    work.file
                        .edited()
                        .is_some_and(|(family, _)| family.table.role == first.table.role)
                        && work.held.outbox().waiting().next().is_some()
                })
                .collect();
            if of.is_empty() {
                continue;
            }
            if let Err(error) = family(&home, &policy, first, &mut of, pass) {
                let why = why(&home, &error);
                for work in of {
                    work.queue(&why);
                }
            }
        }
    }
    works.into_iter().map(|work| work.outcome).collect()
}

/// The `SharedFile` steps no code here takes: which law a walk checks, what custody, a peer or
/// the writer succession does, an input that moves with no record, and the areas later slices
/// build.
#[cfg(any(test, debug_assertions))]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SharedFile",
        action = "ChooseRead",
        reason = "The model's choice of the law one walk checks: no code chooses it. shared_file_conforms takes the reading area's steps on a fresh stand-in lab."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SharedFile",
        action = "ChooseWrite",
        reason = "The model's choice of the law one walk checks: no code chooses it. shared_file_conforms takes the writing area's steps on a fresh stand-in lab."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SharedFile",
        action = "ChooseAuthority",
        reason = "The model's choice of the law one walk checks: no code chooses it. shared_file_conforms takes the authority area's steps on a collection opted into the authority rules (typed::authority)."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SharedFile",
        action = "ChooseFace",
        reason = "The model's choice of the law one walk checks: no code chooses it, and the face area's code is SF-06's."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SharedFile",
        action = "ChooseRotation",
        reason = "The model's choice of the law one walk checks: no code chooses it. shared_file_conforms takes the rotation area's fold on a fresh stand-in lab."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SharedFile",
        action = "Fence",
        reason = "Writer succession fences a home (crate::generation, model WriterSuccession): no shared-file code does. shared_file_conforms fences its stand-in home through generation's seam and holds the publisher to it."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SharedFile",
        action = "Admit",
        reason = "Writer succession admits and imports a successor (crate::generation::advance_while): no shared-file code does. file::tests runs the real recovery pass and has the successor publish the fenced home's intent once."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SharedFile",
        action = "AdmitUnwitnessed",
        reason = "A succession that completes without the fenced home's unwitnessed record is crate::generation's import of the witnessed prefix: no shared-file code chooses what is witnessed. The publisher's scan for the intent's request id then finds no record and signs it."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SharedFile",
        action = "Invalidate",
        reason = "A later source receipt invalidates a completion in crate::generation: no shared-file code does. file::tests holds the materializer to it through generation's seam."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SharedFile",
        action = "Refuse",
        reason = "Custody refusing (a busy vault, a lost membership, policy) is the environment: file::tests makes each refusal on a stand-in lab and holds reads to answering."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SharedFile",
        action = "Recover",
        reason = "Custody answering again is the environment: no shared-file code chooses it."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SharedFile",
        action = "Grow",
        reason = "A record arriving in this Mac's replica is Commons repair or another writer here: no shared-file code chooses it. shared_file_conforms writes one through the registry."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SharedFile",
        action = "Unrecorded",
        reason = "An input of the projection that moves with no record of the table (a later build's rules, a handoff this Mac imports, a Remove that makes a cutoff count) is the environment: no shared-file code chooses it. The reading walk of file::model_read_tests takes it as a later build (file::seam::new_build), and file::tests walks the parts of the projection key such a move changes."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SharedFile",
        action = "PeerSecret",
        reason = "A peer's record holding a value the guards refuse is the environment: file::tests writes one and holds the materializer to leaving it out."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SharedFile",
        action = "PeerWrite",
        reason = "Another writer's record on a register an intent names is the environment: shared_file_conforms writes one through the registry before the publisher runs."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SharedFile",
        action = "PeerReplaces",
        reason = "Another writer replacing this Mac's signed record is the environment: no shared-file code chooses it. The intent is done once its record is retained, whatever a later record says."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SharedFile",
        action = "RefreshLook",
        reason = "The look a pass takes before it opens custody is inside materialize::pass, one function with the record that follows it: the Materialize anchor covers the pass, and file::tests (a_pass_another_overtook_records_nothing) runs a second pass between the two."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SharedFile",
        action = "RefreshWrite",
        reason = "The record of a pass whose look another pass overtook is inside materialize::pass (aterm_file::File::record): the Materialize anchor covers the pass, and file::tests (a_pass_another_overtook_records_nothing) holds it to recording nothing."
    )
)]
#[doc(hidden)]
pub fn shared_file_environment() {}

/// The `SharedFile` steps no code of this build takes as its own: the face and its commit
/// (SF-06), the rotation pool (SF-09), a Mac's reading of a window, and the Mac rows and entries
/// the journal's typed readers merge (`typed::owned`). The steps of authority on merge are the
/// authority rules' (`typed::authority`).
#[cfg(any(test, debug_assertions))]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SharedFile",
        action = "OwnRow",
        reason = "A Mac's own row of the journal ring is merged by typed::owned::read_owned (its AlabCells anchors), and the materializer projects ai/logins and ai/usage from that read: file::tests' SF-08 exit test holds a row at another Mac's key inert in the file and counted in its status."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SharedFile",
        action = "ForeignRow",
        reason = "A Mac's own row of the journal ring is merged by typed::owned::read_owned (its AlabCells anchors), and the materializer projects ai/logins and ai/usage from that read: file::tests' SF-08 exit test holds a row at another Mac's key inert in the file and counted in its status."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SharedFile",
        action = "OwnEntry",
        reason = "A Mac's own entry of the journal ring's event log is merged by typed::owned::read_log, which keeps an entry only at its signer's own key by the rule read_owned's AlabCells anchors hold, and the materializer projects ai/events from that read: file::tests' SF-08 exit test holds every Mac's events to the file and an older build's reading of the same record."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SharedFile",
        action = "ForeignEntry",
        reason = "A Mac's own entry of the journal ring's event log is merged by typed::owned::read_log, which keeps an entry only at its signer's own key by the rule read_owned's AlabCells anchors hold, and the materializer projects ai/events from that read: file::tests' SF-08 exit test holds every Mac's events to the file and an older build's reading of the same record."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SharedFile",
        action = "EditCell",
        reason = "The face a person edits and its explicit commit are SF-06's."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SharedFile",
        action = "LoseRow",
        reason = "The face a person edits and its explicit commit are SF-06's."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SharedFile",
        action = "Watch",
        reason = "The face a person edits and its explicit commit are SF-06's."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SharedFile",
        action = "Commit",
        reason = "The face a person edits and its explicit commit are SF-06's."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SharedFile",
        action = "Restore",
        reason = "The face a person edits and its explicit commit are SF-06's."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SharedFile",
        action = "Remove",
        reason = "Retiring a row from the face is SF-06's commit; `aterm file rm` here writes the same lifecycle intent and file::tests holds it to the publisher."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SharedFile",
        action = "SignIn",
        reason = "A login this Mac holds is a candidate of its rotation pool whatever ai/links says: the pool is the harness's accounts::candidates over this Mac's roster, which a link only orders (Rank::link) and never limits, and harness::accounts::tests' SF-09 exit test holds an unranked row in it beside ranked ones."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SharedFile",
        action = "Link",
        reason = "This Mac's live ai/links rows rank its accounts (file::rotation, whose file::tests SF-09 test holds the rank to this Mac's own live links), and the harness orders its pool by that rank within a class and limits nothing by it: harness::accounts::tests' SF-09 exit test."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SharedFile",
        action = "EndLink",
        reason = "An ended or other Mac's ai/links row gives no rank (file::rotation, whose file::tests SF-09 test holds a dead link and another Mac's out), so its account stays in the harness's pool, unranked: harness::accounts::tests' SF-09 exit test holds an unranked row in it."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SharedFile",
        action = "Pool",
        reason = "The rotation pool is the harness's accounts::candidates over this Mac's roster, ordered by this Mac's ai/links rank within a class (file::rotation) and never limited by it, a peer's ai/limits wall demoting a row and excluding none: harness::accounts::tests' SF-09 exit test."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SharedFile",
        action = "HereOldWeek",
        reason = "A Mac's reading of a limit window is the environment: the harness reads it, and the journal flusher states it in the Mac's ai/usage row (alab::journal::book::Facts::usage_row). shared_file_conforms states each reading as that row says it and folds them."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SharedFile",
        action = "HereNewWeek",
        reason = "A Mac's reading of a limit window is the environment: the harness reads it, and the journal flusher states it in the Mac's ai/usage row (alab::journal::book::Facts::usage_row). shared_file_conforms states each reading as that row says it and folds them."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SharedFile",
        action = "PeerOldWeek",
        reason = "A Mac's reading of a limit window is the environment: the harness reads it, and the journal flusher states it in the Mac's ai/usage row (alab::journal::book::Facts::usage_row). shared_file_conforms states each reading as that row says it and folds them."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SharedFile",
        action = "PeerNewWeek",
        reason = "A Mac's reading of a limit window is the environment: the harness reads it, and the journal flusher states it in the Mac's ai/usage row (alab::journal::book::Facts::usage_row). shared_file_conforms states each reading as that row says it and folds them."
    )
)]
#[doc(hidden)]
pub fn shared_file_later_slices() {}

/// Seams of this crate's tests: a pass killed at one of the two points a kill may not make it
/// sign again, a publisher that ignores what an intent's author saw (the conformance test's
/// negative control), and an act run where a pass is about to claim an intent.
#[cfg(test)]
#[path = "publish_seam_tests.rs"]
pub(crate) mod seam;
