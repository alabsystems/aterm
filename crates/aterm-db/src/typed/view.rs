// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Portable typed table snapshots; storage acquisition lives in the platform's ATT module.

use super::{
    self as typed, Collection, Table,
    authority::Inert,
    cell::{self, Cells, Projection},
    grid,
    owned::{self, LogRead, Op, OwnedRead, Quota, SchemaState},
};
use crate::Result;
use astream_wire::sync::Digest;
use std::collections::{BTreeMap, BTreeSet};
#[cfg(unix)]
use std::{cell::RefCell, rc::Rc};

/// One read of a typed table, the quota applied.
#[derive(Clone, Debug)]
pub struct View {
    /// The table generation read, whose own declaration of each collection a projection takes
    /// ([`Self::collection`]); `None` for a fixture built without one.
    pub(crate) table: Option<&'static Table>,
    /// The authenticated ancestry used to distinguish an observed claim from a concurrent one.
    pub(crate) causal: astream_wire::sync::Index,
    /// Every eligible operation, past-quota ones included (the schema reader and history read
    /// them).
    pub ops: Vec<Op>,
    /// Each writer's automatic records, and the ones past the table's `self_quota`, which never
    /// project.
    pub quota: Quota,
    /// The projected cells without the past-quota records and without the writes the authority
    /// rules hold inert ([`typed::authority`]).
    pub cells: Cells,
    /// The writes the authority rules hold inert in the collections that opt into them: left out
    /// of `cells`, kept in `ops` and in history.
    pub inert: Inert,
    /// The owned collections' rows, past-quota records left out, each at its signer's own key.
    pub owned: BTreeMap<&'static str, OwnedRead>,
    /// The log collections' entries, past-quota records left out, each at its signer's own key
    /// (`<uuid32>.<n>`, [`owned::read_log`]): a log of a catalog table read as its typed rows.
    pub logs: BTreeMap<&'static str, LogRead>,
    /// No retained operation could still become visible here (`Log::awaited`: none waits on a
    /// causal parent that has not arrived, and this Mac's own writer has nothing withheld), so
    /// a comparison against these cells misses nothing a later projection would add.
    pub complete: bool,
    /// What the operations still waiting may yet add, and where: a write of rows none of them
    /// writes need not wait for them ([`Self::holds_back`]).
    pub awaiting: Awaiting,
    /// Retained operations this build never projects and that cannot become visible: refused
    /// payloads (a newer build's encoding, a reused request id), cut-off or forked writers'
    /// records and quarantined payloads. They never hold a grid save or an import back;
    /// `check` lists them.
    pub refused: Vec<Digest>,
    /// Who signed each of [`Self::refused`] and why it is not projected, in its order: what
    /// [`Self::refused_words`] says. `complete` does not count them, so no write waits on them.
    pub refusals: Vec<Unread>,
    /// Where [`Self::cells`] stand among the cells this process keeps of the table, where they
    /// are exactly those cells (no write held inert, no claim interpreted): a reader that made
    /// something of the cells at an earlier mark asks which changed since
    /// ([`crate::documents::materialized::Marked::changed_since`]). `None` for any other read.
    /// Only the unix cells cache marks cells, so the field is where that cache is.
    #[cfg(any(unix, test))]
    #[cfg_attr(
        not(unix),
        expect(
            dead_code,
            reason = "test literals name it on every host; only unix reads it"
        )
    )]
    pub(crate) cells_from: Option<crate::documents::materialized::Marked>,
}

/// One retained record a read holds and does not project ([`View::refused`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unread {
    /// The record.
    pub operation: Digest,
    /// Its writer.
    pub actor: astream_wire::sync::Actor,
    /// Its writer's Commons machine, from the binding in the roster the record names.
    pub machine: Option<[u8; 16]>,
    /// Why this build does not project it, in this build's words.
    pub why: String,
}

impl Unread {
    /// Its signer in a word: the start of its machine's uuid, else of its writer's key.
    #[must_use]
    pub fn signer(&self) -> String {
        let whole = match &self.machine {
            Some(machine) => typed::keys::uuid32(machine),
            None => aterm_codec::hex::encode_lower(&self.actor),
        };
        whole.chars().take(8).collect()
    }
}

/// What retained operations still waiting on a causal parent (`Log::awaited`) may add to a
/// read once their parents arrive.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Awaiting {
    /// The waiting records, each record that descends from one included.
    pub records: usize,
    /// The rows they write, as (collection, primary key); `None` where one of them cannot be
    /// read here (this Mac's own record with no payload), so any row may be its.
    pub rows: Option<BTreeSet<(String, String)>>,
    /// A later generation's restated history is all here: its roll cover row has arrived.
    pub restated: bool,
}

impl Awaiting {
    /// Nothing waits.
    pub const NONE: Self = Self {
        records: 0,
        rows: Some(BTreeSet::new()),
        restated: true,
    };
}

/// The primary key of a stored cell key `<pk>.<col>[.<elem>]`.
fn row_of(key: &str) -> &str {
    key.split_once('.').map_or(key, |(pk, _)| pk)
}

impl View {
    /// A complete empty read for test fixtures, without opening storage or enrollment.
    #[cfg(any(test, debug_assertions))]
    pub fn empty_for_test() -> Self {
        Self {
            table: None,
            causal: astream_wire::sync::Index::new(crate::database::MAX_RECORDS),
            ops: Vec::new(),
            quota: Quota::default(),
            cells: Cells::new(),
            inert: Inert::default(),
            owned: BTreeMap::new(),
            logs: BTreeMap::new(),
            complete: true,
            awaiting: Awaiting::NONE,
            refused: Vec::new(),
            refusals: Vec::new(),
            #[cfg(any(unix, test))]
            cells_from: None,
        }
    }

    /// What a read says whenever it holds records it does not project ([`Self::refused`]): how
    /// many, and each one's signer and why, those with the same signer and reason counted
    /// together in the order first held (`2 records held here are not read: 1 signed by
    /// c05f416f (unsupported database application schema); 1 signed by 9a0b1c2d (its writer was
    /// cut off or forked)`). `None` where it holds none. History, diff, the Tables page and a
    /// shared file's status print it; the read is complete all the same ([`Self::complete`]),
    /// so no write waits on these records.
    #[must_use]
    pub fn refused_words(&self) -> Option<String> {
        refused_words(&self.refusals)
    }
}

/// The sentence a read says of the records `refusals` it holds and does not project
/// ([`View::refused_words`], which a history page and a diff say of their own).
#[must_use]
pub fn refused_words(refusals: &[Unread]) -> Option<String> {
    {
        if refusals.is_empty() {
            return None;
        }
        let mut groups: Vec<(String, &str, usize)> = Vec::new();
        for refused in refusals {
            let signer = refused.signer();
            match groups
                .iter_mut()
                .find(|(s, why, _)| *s == signer && *why == refused.why)
            {
                Some((_, _, n)) => *n += 1,
                None => groups.push((signer, &refused.why, 1)),
            }
        }
        let n = refusals.len();
        let each: Vec<String> = groups
            .iter()
            .map(|(signer, why, k)| format!("{k} signed by {signer} ({why})"))
            .collect();
        Some(format!(
            "{n} {} held here {} not read: {}",
            if n == 1 { "record" } else { "records" },
            if n == 1 { "is" } else { "are" },
            each.join("; ")
        ))
    }
}

impl View {
    /// Why a write of `rows` must wait for history still arriving on this Mac, or `None` when
    /// nothing it writes can still change under it.
    ///
    /// A record that waits on a causal parent is not yet in the cells, so a comparison against
    /// them cannot see its candidates. Only the rows it writes are in doubt: a write elsewhere
    /// compares against cells no arriving record touches, and when the record lands it is
    /// concurrent with that write, never hidden by it. So a write waits only where it writes a
    /// row (collection and primary key) a waiting record writes, where a waiting record cannot
    /// be read here (its rows are unknown), or while a later generation's restated history is
    /// still arriving. The answer names how many records are withheld.
    #[must_use]
    pub fn holds_back(&self, rows: &[Vec<crate::database::Change>]) -> Option<String> {
        if self.complete {
            return None;
        }
        let withheld = format!("history incomplete ({} withheld)", self.awaiting.records);
        if !self.awaiting.restated {
            return Some(format!(
                "{withheld}: this generation's restated history is still arriving"
            ));
        }
        let Some(waiting) = &self.awaiting.rows else {
            return Some(format!(
                "{withheld}: a withheld record cannot be read here, so any row may be its"
            ));
        };
        rows.iter().flatten().find_map(|change| {
            let pk = row_of(&change.key);
            waiting
                .contains(&(change.collection.clone(), pk.to_owned()))
                .then(|| format!("{withheld}: one writes {} row {pk}", change.collection))
        })
    }

    /// `collection` as the generation this view read declares it: a generation keeps the shape
    /// the builds that read it know (a column a later generation retypes, such as a list it
    /// holds as a text set), while a build names each collection by its newest definition. A
    /// collection the generation does not hold, or a view with no table, is `collection` itself.
    pub fn collection<'c>(&self, collection: &'c Collection) -> &'c Collection {
        self.table
            .and_then(|table| table.collection(collection.name))
            .unwrap_or(collection)
    }

    /// One cells collection's rows, as the generation this view read declares it
    /// ([`Self::collection`]).
    pub fn project(&self, collection: &Collection) -> Projection {
        let collection = self.collection(collection);
        if collection
            .columns
            .iter()
            .any(|column| column.claim.is_some())
        {
            typed::claim::project(
                collection,
                &self.cells,
                self.ops
                    .iter()
                    .filter(|op| !self.quota.over.contains(&op.id)),
                &self.causal,
            )
        } else {
            cell::project(collection, &self.cells)
        }
    }

    /// [`Self::project`], with how each of the collection's claimed columns settled there
    /// ([`typed::claim::Settlement`]), which a page asks before a Write what a value staged in
    /// such a column will read as once written. A collection that claims nothing has none.
    pub fn project_settled(
        &self,
        collection: &Collection,
    ) -> (Projection, typed::claim::Settlements) {
        let collection = self.collection(collection);
        if collection
            .columns
            .iter()
            .any(|column| column.claim.is_some())
        {
            typed::claim::settled(
                collection,
                &self.cells,
                self.ops
                    .iter()
                    .filter(|op| !self.quota.over.contains(&op.id)),
                &self.causal,
            )
        } else {
            (
                cell::project(collection, &self.cells),
                typed::claim::Settlements::new(),
            )
        }
    }

    /// Export canonical values, preserving original cell identities for a later grid save.
    ///
    /// # Errors
    /// The collection is not editable or the grid layout cannot be encoded.
    pub fn export_grid(
        &self,
        table: &Table,
        collection: &'static Collection,
        filters: &[grid::Where],
    ) -> Result<grid::Export> {
        // Laid out as the generation this view read declares the collection, as it is projected.
        let collection = self.collection(collection);
        grid::export_projected(
            table,
            collection,
            &self.cells,
            &self.project(collection),
            filters,
        )
    }

    /// A roll preserves raw unknown contexts and refreshes the known columns' current claims.
    #[cfg(unix)]
    pub(crate) fn claim_provenance(&self, table: &Table) -> Result<typed::claim::CarriedClaims> {
        let mut out = typed::claim::carried_claims(&self.cells)?;
        for collection in table.collections {
            if collection
                .columns
                .iter()
                .any(|column| column.claim.is_some())
            {
                out.extend(typed::claim::snapshot(
                    collection,
                    &self.cells,
                    self.ops
                        .iter()
                        .filter(|operation| !self.quota.over.contains(&operation.id)),
                    &self.causal,
                )?);
            }
        }
        Ok(out)
    }

    /// Retirement needs the source cover and readable claim-bearing collections: opaque
    /// fields may carry identity rules whose ancestry this build cannot safely restate.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AlabIdentityClaims",
            action = "Judge",
            project = "aterm_db::typed::claim::model_tests::claim_transition"
        )
    )]
    #[cfg(any(unix, test))]
    pub(crate) fn require_roll_source(&self, table: &Table) -> Result<()> {
        if !self.complete {
            return Err(crate::Error::new(
                "the source registry has no complete generation cover; its history remains held",
            ));
        }
        for collection in table.collections {
            if collection
                .columns
                .iter()
                .any(|column| column.claim.is_some())
                && cell::project(collection, &self.cells)
                    .rows
                    .values()
                    .any(|row| !row.raw.is_empty())
            {
                return Err(crate::Error::new(
                    "an identity claim source needs a newer reader; its history remains held",
                ));
            }
        }
        Ok(())
    }

    /// The stored schema row of `collection`: the highest version any eligible operation wrote.
    pub fn schema(&self, collection: &str) -> SchemaState {
        owned::schema_state(&self.ops, collection)
    }
}

/// One [`View`]'s collections, each projected ([`View::project`]) at the first ask and handed
/// out again at every later one, so a reader that asks for one collection several times pays
/// for one projection of it: the shared-file materializer asks for MACHINE's rows for the
/// machines file itself and for cells each of its three files computes from them. It borrows
/// the view, which no one can change while it is borrowed, so what it hands out is that view's
/// projection. It knows a collection by its name, which no two collections of a table share;
/// a view is one table's, so a name names one collection of it.
#[cfg(unix)]
pub(crate) struct Projections<'v> {
    view: &'v View,
    memo: RefCell<BTreeMap<&'static str, Rc<Projection>>>,
}

#[cfg(unix)]
impl<'v> Projections<'v> {
    /// No collection of `view` projected yet.
    pub(crate) const fn new(view: &'v View) -> Self {
        Self {
            view,
            memo: RefCell::new(BTreeMap::new()),
        }
    }

    /// The view these are projections of.
    pub(crate) const fn view(&self) -> &'v View {
        self.view
    }

    /// `collection`'s rows ([`View::project`]), projected at the first ask of its name.
    pub(crate) fn project(&self, collection: &Collection) -> Rc<Projection> {
        if let Some(projection) = self.memo.borrow().get(collection.name) {
            return Rc::clone(projection);
        }
        let projection = Rc::new(self.view.project(collection));
        self.memo
            .borrow_mut()
            .insert(collection.name, Rc::clone(&projection));
        projection
    }
}
