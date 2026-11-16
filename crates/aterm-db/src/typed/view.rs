// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Portable typed table snapshots; storage acquisition lives in the platform's ATT module.

use super::{
    self as typed, Collection, Table,
    cell::{self, Cells, Projection},
    grid,
    owned::{self, Op, OwnedRead, Quota, SchemaState},
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
    /// The projected cells without the past-quota records.
    pub cells: Cells,
    /// The owned collections' rows, past-quota records left out, each at its signer's own key.
    pub owned: BTreeMap<&'static str, OwnedRead>,
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
            owned: BTreeMap::new(),
            complete: true,
            awaiting: Awaiting::NONE,
            refused: Vec::new(),
        }
    }

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
