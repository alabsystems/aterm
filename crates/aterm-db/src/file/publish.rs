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
//! - **Nothing here reads.** A pass opens no reader of a shared file and runs no materializer
//!   pass; the verb that asked runs one afterwards.

use super::Type;
use crate::{
    Error, Result,
    att::{self, Compiled, View},
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

/// What bounds one pass.
#[derive(Clone, Copy, Debug)]
pub struct Pass {
    /// When the pass stops opening custody and signing.
    pub by: Instant,
    /// This Mac's clock, Unix milliseconds, for display.
    pub now_ms: u64,
}

impl Pass {
    /// A pass that ends `within` from now.
    #[must_use]
    pub fn within(within: Duration) -> Self {
        Self {
            by: Instant::now() + within,
            now_ms: aterm_time::unix_millis().unwrap_or(0),
        }
    }
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

    /// The marker (under the pass's claim, where it holds one), then the dequeue: the order a
    /// kill between the two is safe in.
    fn done(&mut self, claim: Option<Claim>, id: &str, mark: &Mark, now_ms: u64) {
        let marked = match claim {
            Some(claim) => claim.mark_done(mark, now_ms),
            None => self.held.mark_done(id, mark, now_ms),
        };
        match marked {
            Ok(_) => {}
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
        }
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
        if let Err(error) = self.held.dequeue(id) {
            self.problem(&error);
        }
        self.answer(
            id,
            Answer::Published {
                op: mark.op.clone(),
            },
        );
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
            secret: false,
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
        let command = if let Some(target) = &intent.revert {
            let Some(target) = outbox::digest(target) else {
                return Ok(Step::Hold("it names no operation to revert".to_owned()));
            };
            let command = match log.compensate(request, target, &view.quota.over) {
                Ok(command) => command,
                Err(error) => return Ok(Step::Hold(error.to_string())),
            };
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
                && let Some(refused) = (self.compiled.guard)(&mut [changes.to_vec()], &view.cells)?
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
                    return Ok(Step::Hold(format!(
                        "changed since it was written: {}",
                        moved.join(", ")
                    )));
                }
                settled.rows
            };
            if rows.is_empty() {
                return Ok(Step::Holds);
            }
            if let Some(refused) = (self.compiled.guard)(&mut rows, &view.cells)? {
                return Ok(Step::Hold(Error::from(refused).to_string()));
            }
            // One intent, one record: its rows are never split across two.
            let changes: Vec<Change> = rows.into_iter().flatten().collect();
            let mut packed = match att::pack(vec![changes]) {
                Ok(packed) => packed,
                Err(error) => return Ok(Step::Hold(error.to_string())),
            };
            let (Some(changes), true) = (packed.pop(), packed.is_empty()) else {
                return Ok(Step::Hold("it passes one record's limits".to_owned()));
            };
            Command::Write { request, changes }
        };
        let wrote = wrote(command.changes());
        Ok(Step::Signed(
            application::append(self.session, store, &command.encode()?)?.id,
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
    pass: &Pass,
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
                let collection = compiled.collection(works[at].file.collection.name)?;
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
                            let log = Log::read_with(session, store, scope, att::decode)?;
                            log.writable(session.actor())?;
                            let view = View::read(table, session, &log);
                            if !view.complete {
                                return Err(Error::new(
                                    "this table's history here is incomplete; a write waits for \
                                     a complete scan",
                                ));
                            }
                            (log, view)
                        }
                    };
                    let carried = request(&intent.id).and_then(|request| log.carried(request));
                    #[cfg(test)]
                    seam::before_claim();
                    // The claim holds the outbox from here until the marker: a person's drop
                    // is either before it, and nothing is signed, or after the marker.
                    let claim = match work.held.claim(&intent.id) {
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
                    let step = writer.settle(store, &log, &view, collection, &intent, &earlier)?;
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
                            let mark = Mark {
                                op: Some(aterm_codec::hex::encode_lower(&op)),
                                generation: compiled.generation,
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
                            work.done(Some(claim), &intent.id, &mark, pass.now_ms);
                            if carried.is_some() {
                                read = Some((log, view));
                            }
                        }
                        Step::Holds => {
                            let mark = Mark {
                                op: None,
                                generation: compiled.generation,
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
                            work.done(Some(claim), &intent.id, &mark, pass.now_ms);
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
pub fn pass(paths: &Paths, config: &Path, only: Option<&Name>, pass: &Pass) -> Vec<Outcome> {
    publish_intents(paths, config, only, pass)
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
fn publish_intents(paths: &Paths, config: &Path, only: Option<&Name>, pass: &Pass) -> Vec<Outcome> {
    let home = generation::original(paths);
    let root = super::root(&home);
    let mut works: Vec<Work> = super::FILES
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
    // What a killed pass left: an intent whose marker is written only leaves the outbox.
    for work in &mut works {
        let outbox = work.held.outbox();
        for intent in &outbox.queued {
            if let Some(done) = outbox.done.iter().find(|done| done.intent.id == intent.id) {
                let mark = Mark {
                    op: done.op.clone(),
                    generation: done.generation.unwrap_or_default(),
                    left: done.left.clone(),
                };
                work.done(None, &intent.id, &mark, pass.now_ms);
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
        let mut families: Vec<&'static Compiled> = Vec::new();
        for work in &works {
            if !families
                .iter()
                .any(|family| family.table.role == work.file.family.table.role)
            {
                families.push(work.file.family);
            }
        }
        for first in families {
            let mut of: Vec<&mut Work> = works
                .iter_mut()
                .filter(|work| {
                    work.file.family.table.role == first.table.role
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

/// The `SharedFile` steps no code here takes: which law a walk checks, what a peer or the
/// writer succession does, and the areas later slices build.
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
        reason = "The model's choice of the law one walk checks: no code chooses it, and the authority area's code is SF-07's."
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
        reason = "The model's choice of the law one walk checks: no code chooses it, and the rotation area's code is SF-08's."
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

/// The `SharedFile` steps of the areas later slices build: authority on merge (SF-07), the face
/// and its commit (SF-06), and account rotation (SF-08).
#[cfg(any(test, debug_assertions))]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SharedFile",
        action = "OwnRow",
        reason = "No shared file of this build holds a Mac's own row yet: SF-07 binds one and anchors its merge."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SharedFile",
        action = "ForeignRow",
        reason = "No shared file of this build holds a Mac's own row yet: SF-07 binds one and anchors its merge."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SharedFile",
        action = "OwnEntry",
        reason = "No shared file of this build is a log yet: SF-07 binds one and anchors its merge."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SharedFile",
        action = "ForeignEntry",
        reason = "No shared file of this build is a log yet: SF-07 binds one and anchors its merge."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SharedFile",
        action = "OwnerWrites",
        reason = "Column authority (Owner over Automatic) is SF-07's projection: this build's files hold owner columns only."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SharedFile",
        action = "AutomaticWrites",
        reason = "Column authority (Owner over Automatic) is SF-07's projection: this build's files hold owner columns only."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SharedFile",
        action = "Birth",
        reason = "Birth cells under an owner's candidate are SF-07's projection."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SharedFile",
        action = "OwnerSets",
        reason = "Birth cells under an owner's candidate are SF-07's projection."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SharedFile",
        action = "Rewrite",
        reason = "A fixed column's identity after birth is SF-07's projection; the typed grid already refuses the edit (grid::plan)."
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
        reason = "Account rotation over shared files is SF-08's."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SharedFile",
        action = "Link",
        reason = "Account rotation over shared files is SF-08's."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SharedFile",
        action = "EndLink",
        reason = "Account rotation over shared files is SF-08's."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SharedFile",
        action = "Pool",
        reason = "Account rotation over shared files is SF-08's."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SharedFile",
        action = "HereOldWeek",
        reason = "Folding two Macs' limit observations is SF-08's."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SharedFile",
        action = "HereNewWeek",
        reason = "Folding two Macs' limit observations is SF-08's."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SharedFile",
        action = "PeerOldWeek",
        reason = "Folding two Macs' limit observations is SF-08's."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SharedFile",
        action = "PeerNewWeek",
        reason = "Folding two Macs' limit observations is SF-08's."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SharedFile",
        action = "Fold",
        reason = "Folding two Macs' limit observations is SF-08's."
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
