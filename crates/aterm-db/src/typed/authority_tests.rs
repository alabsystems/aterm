// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The authority rules over hand-built causal histories of a scratch collection that opts in
//! (`RULED`, from version 2), each rule beside its control (`seam`), and one read of a real
//! store. The registry's account and preference collections opt in at version 2; their rules
//! end to end, over two Macs, are `alab::tests::one_address_is_three_accounts_slugged_claude_oai_and_google_and_an_automatic_owner_write_is_inert`.
//! Every value is synthetic.

use super::{
    Inert,
    seam::{self, Control},
};
use crate::{
    database::Change,
    documents::Unwritten,
    typed::{
        Collection, Column, Derive, Kind, Table, Ty, Writer, Writers,
        cell::{self, CellKey, CellValue, Cells, Field},
        grid,
        owned::Op,
    },
};
use astream_wire::sync::{Actor, Digest, Header, Index};
use aterm_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

const LIFE: &[&str] = &["live", "retired"];
const STATES: &[&str] = &["pending", "active", "paused"];

crate::att_collection!(
    /// A scratch collection under the authority rules from version 2: an identity column a birth
    /// writes and nobody rewrites, a state a birth writes and the owner may change, a name only
    /// an owner's verb writes, and an owner's note.
    pub(crate) RULED = Collection {
        name: "ruled",
        version: 2,
        kind: Kind::Cells,
        writers: Writers::Owner,
        prefix: "rl",
        derive: Derive::BirthSha256,
        columns: &[
            Column::new("realm", Ty::Text(32)).writer(Writer::Birth).fixed(),
            Column::new("status", Ty::Enum(STATES)).writer(Writer::Birth),
            Column::new("label", Ty::Text(32)).writer(Writer::Verb).null(),
            Column::new("note", Ty::Text(64)).null(),
            Column::new("row", Ty::Enum(LIFE)).writer(Writer::Birth),
        ],
        unique: &[],
        max_row: 0,
        authority: Some(2),
    }
);

crate::att_collection!(
    /// `RULED` as a collection that never opted in: its name an owner column.
    PLAIN = Collection {
        name: "ruled",
        version: 2,
        kind: Kind::Cells,
        writers: Writers::Owner,
        prefix: "rl",
        derive: Derive::BirthSha256,
        columns: &[
            Column::new("realm", Ty::Text(32))
                .writer(Writer::Birth)
                .fixed(),
            Column::new("status", Ty::Enum(STATES)).writer(Writer::Birth),
            Column::new("label", Ty::Text(32)).null(),
            Column::new("note", Ty::Text(64)).null(),
            Column::new("row", Ty::Enum(LIFE)).writer(Writer::Birth),
        ],
        unique: &[],
        max_row: 0,
        authority: None,
    }
);

/// A table holding only `RULED`.
pub(crate) const TABLE: Table = Table {
    application: "att-test",
    role: "authority",
    collections: &[&RULED],
    faces: &[],
    self_quota: 24,
    ends: &[],
};

const PLAIN_TABLE: Table = Table {
    collections: &[&PLAIN],
    ..TABLE
};

crate::att_collection!(
    /// A scratch collection under the rules that its owner births, with no Birth column: an
    /// identity column the owner writes once, and a note.
    pub(crate) OWNER_BORN = Collection {
        name: "ruled",
        version: 2,
        kind: Kind::Cells,
        writers: Writers::Owner,
        prefix: "rl",
        derive: Derive::BirthSha256,
        columns: &[
            Column::new("realm", Ty::Text(32)).fixed(),
            Column::new("note", Ty::Text(64)).null(),
            Column::new("row", Ty::Enum(LIFE)),
        ],
        unique: &[],
        max_row: 0,
        authority: Some(2),
    }
);

const OWNER_BORN_TABLE: Table = Table {
    collections: &[&OWNER_BORN],
    ..TABLE
};

/// The one row every history here writes.
pub(crate) const PK: &str = "rl-0000000000000001";

/// An import source's key.
const SOURCE: &str = "sr-00000000000000aa";

fn actor(n: u8) -> Actor {
    [n; 48]
}

/// A hand-built causal history: each write names the writes it observed, and a read replays it
/// in causal order exactly as `Log::cells_without` does.
pub(crate) struct History {
    index: Index,
    next: u64,
    heads: BTreeMap<u8, (u64, Digest)>,
    ops: Vec<Op>,
}

impl History {
    pub(crate) fn new() -> Self {
        Self {
            index: Index::new(1024),
            next: 0,
            heads: BTreeMap::new(),
            ops: Vec::new(),
        }
    }

    fn append(
        &mut self,
        writer: u8,
        seen: &[Digest],
        revert: Option<Digest>,
        changes: Vec<Change>,
    ) -> Digest {
        self.next += 1;
        let mut id = [0; 32];
        id[..8].copy_from_slice(&self.next.to_be_bytes());
        let previous = self.heads.get(&writer).map(|(_, id)| *id);
        let sequence = self.heads.get(&writer).map_or(1, |(s, _)| s + 1);
        let mut parents: BTreeSet<Digest> = seen.iter().copied().collect();
        parents.extend(previous);
        self.index
            .insert(Header {
                id,
                actor: actor(writer),
                sequence,
                previous,
                parents: parents.into_iter().collect(),
            })
            .unwrap();
        self.heads.insert(writer, (sequence, id));
        self.ops.push(Op {
            id,
            actor: actor(writer),
            sequence,
            roster: [7; 32],
            period: 0,
            revert,
            changes,
        });
        id
    }

    /// Append `changes` by `writer`, observing its own previous write and `seen`.
    pub(crate) fn write(&mut self, writer: u8, seen: &[Digest], changes: Vec<Change>) -> Digest {
        self.append(writer, seen, None, changes)
    }

    /// Append a revert of `target` restoring `changes`, by `writer`, observing everything.
    pub(crate) fn revert(&mut self, writer: u8, target: Digest, changes: Vec<Change>) -> Digest {
        let all = self.all();
        self.append(writer, &all, Some(target), changes)
    }

    /// Every write so far: what an append that observes everything names.
    pub(crate) fn all(&self) -> Vec<Digest> {
        self.ops.iter().map(|op| op.id).collect()
    }

    fn replay(&self, before: Option<Digest>, unwritten: &Unwritten) -> Cells {
        let mut cells = Cells::new();
        for id in self.index.project().ordered {
            if before.is_some_and(|target| target == id || !self.index.observes(&target, &id)) {
                continue;
            }
            let op = self.ops.iter().find(|op| op.id == id).unwrap();
            let kept: Vec<Change> = op
                .changes
                .iter()
                .filter(|c| !unwritten.contains(&(id, c.collection.clone(), c.key.clone())))
                .cloned()
                .collect();
            crate::documents::apply(
                &|later, earlier| self.index.observes(later, earlier),
                &mut cells,
                id,
                &kept,
                true,
            );
        }
        cells
    }

    /// The history as it was written, nothing judged.
    pub(crate) fn raw(&self) -> Cells {
        self.replay(None, &Unwritten::new())
    }

    /// A read of `table` under the authority rules.
    pub(crate) fn read(&self, table: &Table) -> (Cells, Inert) {
        super::read(table, &self.ops, &self.index, |before, unwritten| {
            self.replay(before, unwritten)
        })
    }
}

fn change(column: &str, value: &CellValue) -> Change {
    cell::change(
        &RULED,
        CellKey {
            pk: PK,
            col: column,
            elem: None,
        },
        value,
    )
    .unwrap()
}

/// The owner's write of `column` at version 2.
pub(crate) fn owner(column: &str, x: Value) -> Change {
    change(column, &CellValue::owner(2, x))
}

/// The owner's write of `column` at version `r`.
fn owner_at(r: u32, column: &str, x: Value) -> Change {
    change(column, &CellValue::owner(r, x))
}

/// An automatic path's write of `column` at version `r`.
pub(crate) fn automatic(r: u32, column: &str, x: Value) -> Change {
    change(column, &CellValue::automatic(r, x))
}

/// The importer's write of `column` at version 2.
pub(crate) fn imported(column: &str, x: Value) -> Change {
    change(column, &CellValue::imported(2, x, SOURCE))
}

/// A row's birth, every cell in one record, each made by `cell`: realm `claude`, status
/// `pending`, lifecycle `live`.
pub(crate) fn birth(cell: fn(&str, Value) -> Change) -> Vec<Change> {
    vec![
        cell("realm", json!("claude")),
        cell("status", json!("pending")),
        cell("row", json!("live")),
    ]
}

/// An automatic path's write at version 2, the shape `birth` takes.
pub(crate) fn automatic2(column: &str, x: Value) -> Change {
    automatic(2, column, x)
}

/// The row's field `column` in `cells`.
pub(crate) fn field(cells: &Cells, column: &str) -> Option<Field> {
    cell::project(&RULED, cells)
        .rows
        .get(PK)
        .and_then(|row| row.fields.get(column).cloned())
}

/// The row's shown value of `column` in `cells`.
pub(crate) fn shown(cells: &Cells, column: &str) -> Option<Value> {
    cell::project(&RULED, cells)
        .rows
        .get(PK)
        .and_then(|row| row.get(column).cloned())
}

/// A roll's carried value of `items`, each the candidate an operation (`[origin; 32]`) wrote.
fn carried(items: &[(u8, CellValue)]) -> Value {
    cell::carry(
        &items
            .iter()
            .map(|(origin, value)| ([*origin; 32], value.encode()))
            .collect::<BTreeMap<Digest, Value>>(),
    )
    .unwrap()
}

/// The change of `column` to `value` as stored, a carried value among them.
fn stored(column: &str, value: Value) -> Change {
    Change {
        collection: RULED.name.into(),
        key: format!("{PK}.{column}"),
        value: Some(value),
    }
}

fn at(operation: Digest, column: &str) -> (Digest, String, String) {
    (operation, RULED.name.to_owned(), format!("{PK}.{column}"))
}

fn records(n: usize) -> BTreeMap<&'static str, usize> {
    BTreeMap::from([(RULED.name, n)])
}

/// Rule 2: an automatic write on an Owner column is inert, as if never written, so the owner's
/// note it replaced is back; it stays in history, counted once. One from before the collection
/// opted in (version 1) is not judged.
/// NEGATIVE CONTROL: with rule 2 off (`Control::Owner`), the read shows the automatic note and
/// holds nothing inert; the test reads it so.
#[test]
fn rule_2_an_automatic_write_on_an_owner_column_never_projects() {
    let mut history = History::new();
    history.write(0, &[], birth(owner));
    history.write(0, &[], vec![owner("note", json!("the owner's"))]);
    let auto = history.write(
        1,
        &history.all(),
        vec![automatic(2, "note", json!("automatic"))],
    );
    let (cells, inert) = history.read(&TABLE);
    assert_eq!(shown(&cells, "note"), Some(json!("the owner's")));
    assert_eq!(inert.changes, BTreeSet::from([at(auto, "note")]));
    assert_eq!(inert.records, records(1));
    assert_eq!(inert.total(), 1);
    assert_eq!(inert.rows(RULED.name), BTreeSet::from([PK]));
    assert_eq!(shown(&history.raw(), "note"), Some(json!("automatic")));
    {
        let _off = seam::control(Control::Owner);
        let (cells, inert) = history.read(&TABLE);
        assert_eq!(shown(&cells, "note"), Some(json!("automatic")));
        assert_eq!(inert, Inert::default());
    }
    history.write(1, &history.all(), vec![automatic(1, "note", json!("old"))]);
    let (cells, inert) = history.read(&TABLE);
    assert_eq!(shown(&cells, "note"), Some(json!("old")));
    assert_eq!(inert.changes, BTreeSet::from([at(auto, "note")]));
}

/// Rule 3a: a birth writes its cells with its lifecycle in one record, so they stand; an
/// automatic write on a Birth column after it is inert, while the owner's write there stands.
/// NEGATIVE CONTROL: with rule 3a off (`Control::AfterBirth`), the automatic `active` shows; the
/// test reads it so.
#[test]
fn rule_3a_an_automatic_birth_write_after_the_birth_never_projects() {
    let mut history = History::new();
    history.write(1, &[], birth(automatic2));
    let later = history.write(1, &[], vec![automatic(2, "status", json!("active"))]);
    let (cells, inert) = history.read(&TABLE);
    assert_eq!(shown(&cells, "status"), Some(json!("pending")));
    assert_eq!(shown(&cells, "realm"), Some(json!("claude")));
    assert_eq!(shown(&cells, "row"), Some(json!("live")));
    assert_eq!(inert.changes, BTreeSet::from([at(later, "status")]));
    {
        let _off = seam::control(Control::AfterBirth);
        assert_eq!(
            shown(&history.read(&TABLE).0, "status"),
            Some(json!("active"))
        );
    }
    history.write(0, &history.all(), vec![owner("status", json!("active"))]);
    let (cells, inert) = history.read(&TABLE);
    assert_eq!(shown(&cells, "status"), Some(json!("active")));
    assert_eq!(inert.records, records(1));
}

/// Rule 3b: Mac 1, before it hears Mac 0, births the row with automatic cells beside Mac 0's
/// owner birth; once both are here, Mac 1's are inert, so status reads the owner's `pending` with
/// no conflict, and Mac 1's one record is counted once. The rule is judged again on what it
/// leaves: an automatic status a later automatic one replaced comes back once the later one is
/// inert, and is inert too.
/// NEGATIVE CONTROL: with rule 3b off (`Control::Contested`), status is a conflict of both
/// births; the test reads it so.
#[test]
fn rule_3b_an_automatic_birth_write_beside_an_owner_write_never_projects() {
    let mut history = History::new();
    history.write(0, &[], birth(owner));
    let theirs = history.write(
        1,
        &[],
        vec![
            automatic(2, "realm", json!("claude")),
            automatic(2, "status", json!("active")),
            automatic(2, "row", json!("live")),
        ],
    );
    let (cells, inert) = history.read(&TABLE);
    assert!(!field(&cells, "status").unwrap().conflicted());
    assert_eq!(shown(&cells, "status"), Some(json!("pending")));
    assert_eq!(
        inert.changes,
        BTreeSet::from([at(theirs, "realm"), at(theirs, "row"), at(theirs, "status")])
    );
    assert_eq!(inert.records, records(1));
    {
        let _off = seam::control(Control::Contested);
        let (cells, _) = history.read(&TABLE);
        assert!(field(&cells, "status").unwrap().conflicted());
    }

    // Two automatic writes of one unborn row's state, the later replacing the earlier, beside
    // the owner's: the first pass holds the later inert, the next the earlier it brought back.
    let mut history = History::new();
    let own = history.write(0, &[], vec![owner("status", json!("pending"))]);
    let first = history.write(1, &[], vec![automatic(2, "status", json!("active"))]);
    let second = history.write(1, &[], vec![automatic(2, "status", json!("pending"))]);
    let (cells, inert) = history.read(&TABLE);
    assert_eq!(
        field(&cells, "status").unwrap().candidates()[0].operation,
        own
    );
    assert_eq!(
        inert.changes,
        BTreeSet::from([at(first, "status"), at(second, "status")])
    );
}

/// Rule 4: a write to a fixed column after the birth is inert, by any writer.
/// NEGATIVE CONTROL: with rule 4 off (`Control::Fixed`), the owner's rewrite shows; the test
/// reads it so.
#[test]
fn rule_4_a_write_to_a_fixed_column_after_the_birth_never_projects() {
    let mut history = History::new();
    history.write(0, &[], birth(owner));
    let rewrite = history.write(0, &[], vec![owner("realm", json!("codex"))]);
    let (cells, inert) = history.read(&TABLE);
    assert_eq!(shown(&cells, "realm"), Some(json!("claude")));
    assert_eq!(inert.changes, BTreeSet::from([at(rewrite, "realm")]));
    {
        let _off = seam::control(Control::Fixed);
        assert_eq!(
            shown(&history.read(&TABLE).0, "realm"),
            Some(json!("codex"))
        );
    }
    // A revert restores what a read admitted: rule 4 does not judge it.
    let restored = history.revert(0, rewrite, vec![owner("realm", json!("claude"))]);
    let (cells, inert) = history.read(&TABLE);
    assert_eq!(shown(&cells, "realm"), Some(json!("claude")));
    assert!(!inert.changes.contains(&at(restored, "realm")));
}

/// The verb rule: an imported name and an automatic one an older peer writes at version 1 are
/// inert, at every version; the owner's name stands, and an automatic one after it is inert.
/// NEGATIVE CONTROL: with the verb rule judged only from version 2
/// (`Control::VerbFromAuthority`), the older peer's default name shows; the test reads it so.
#[test]
fn the_verb_rule_holds_an_imported_or_automatic_name_inert_at_every_version() {
    let mut history = History::new();
    history.write(0, &[], birth(owner));
    let import = history.write(
        2,
        &history.all(),
        vec![imported("label", json!("imported"))],
    );
    let peer = history.write(
        1,
        &history.all(),
        vec![automatic(1, "label", json!("claude-1a2b3c"))],
    );
    let (cells, inert) = history.read(&TABLE);
    assert_eq!(shown(&cells, "label"), None);
    assert_eq!(
        inert.changes,
        BTreeSet::from([at(import, "label"), at(peer, "label")])
    );
    {
        let _from = seam::control(Control::VerbFromAuthority);
        assert_eq!(
            shown(&history.read(&TABLE).0, "label"),
            Some(json!("claude-1a2b3c"))
        );
    }
    history.write(0, &history.all(), vec![owner("label", json!("work"))]);
    let stolen = history.write(
        1,
        &history.all(),
        vec![automatic(2, "label", json!("stolen"))],
    );
    let (cells, inert) = history.read(&TABLE);
    assert_eq!(shown(&cells, "label"), Some(json!("work")));
    assert!(inert.changes.contains(&at(stolen, "label")));
    assert_eq!(inert.records, records(3));
}

/// The same history read where no collection opts in projects every cell as it was written, and
/// nothing is judged or counted.
/// NEGATIVE CONTROL: the table that opts in, read over the same history, leaves the automatic
/// note out; the test reads it so.
#[test]
fn a_table_no_collection_of_which_opts_in_reads_every_cell_as_written() {
    let mut history = History::new();
    history.write(0, &[], birth(owner));
    history.write(0, &[], vec![owner("note", json!("the owner's"))]);
    history.write(
        1,
        &history.all(),
        vec![
            automatic(2, "note", json!("automatic")),
            automatic(2, "status", json!("active")),
            automatic(2, "realm", json!("codex")),
        ],
    );
    let (cells, inert) = history.read(&PLAIN_TABLE);
    assert_eq!(cells, history.raw());
    assert_eq!(inert, Inert::default());
    assert_eq!(shown(&cells, "note"), Some(json!("automatic")));
    assert_eq!(
        shown(&history.read(&TABLE).0, "note"),
        Some(json!("the owner's"))
    );
}

/// An owner's grid save over a register that holds an inert candidate settles: the export showed
/// the admitted cells, and the save compares against the same cells.
/// NEGATIVE CONTROL: the same save settled against the cells as written, as a reader that handed
/// the grid admitted cells and its settle raw ones would, finds the inert candidate and leaves
/// the note stale; the test settles it so.
#[test]
fn an_owner_save_over_a_register_holding_an_inert_candidate_settles() {
    fn no_birth(_: &Collection, _: &BTreeMap<String, Value>) -> crate::Result<String> {
        Err(crate::Error::new("no births here"))
    }
    let mut history = History::new();
    history.write(0, &[], birth(owner));
    history.write(0, &[], vec![owner("note", json!("first"))]);
    // Mac 1 had not heard the owner's note: its automatic one is concurrent, and inert.
    history.write(1, &[], vec![automatic(2, "note", json!("automatic"))]);
    let (cells, inert) = history.read(&TABLE);
    assert_eq!(inert.records, records(1));
    let note = (RULED.name.to_owned(), format!("{PK}.note"));
    assert_eq!(history.raw()[&note].candidates.len(), 2);
    let exported = grid::export(&TABLE, &RULED, &cells, &[]).unwrap();
    let row = grid::tests::record(&exported.text, PK);
    let saved = grid::tests::type_in(&exported.text, row, "note", "second");
    let plan = grid::plan(&TABLE, &RULED, &exported.base, &saved, &no_birth).unwrap();
    let settled = grid::settle(&RULED, &plan, &cells);
    assert!(settled.stale.is_empty(), "{:?}", settled.stale);
    assert_eq!(settled.rows.concat().len(), 1);
    let raw = grid::settle(&RULED, &plan, &history.raw());
    assert!(raw.rows.is_empty());
    assert_eq!(raw.stale.len(), 1);
    history.write(0, &history.all(), settled.rows.concat());
    let (cells, _) = history.read(&TABLE);
    assert_eq!(shown(&cells, "note"), Some(json!("second")));
    assert!(!field(&cells, "note").unwrap().conflicted());
}

/// A roll's carried value: rule 2 and the verb rule judge each item by its own flags, so an
/// automatic item on an Owner column is left out of the value and an automatic name leaves the
/// whole change inert; rules 3a and 4 cannot tell when a carried item was written, so a carried
/// automatic state after the carried lifecycle stands, where a fresh one is inert.
/// NEGATIVE CONTROL: with every write admitted (`Control::AdmitAll`), the carried note is the
/// conflict the earlier generation's reader never showed; the test reads it so.
#[test]
fn a_roll_restates_admitted_state_and_judges_each_carried_item_by_its_own_flags() {
    let raw_change = stored;
    let mut history = History::new();
    let lifecycle = history.write(
        9,
        &[],
        vec![
            raw_change(
                "row",
                carried(&[(0xa1, CellValue::automatic(2, json!("live")))]),
            ),
            raw_change(
                "realm",
                carried(&[(0xa1, CellValue::automatic(2, json!("claude")))]),
            ),
        ],
    );
    let roll = history.write(
        9,
        &[],
        vec![
            raw_change(
                "note",
                carried(&[
                    (0xb1, CellValue::owner(2, json!("first"))),
                    (0xb2, CellValue::automatic(2, json!("automatic"))),
                ]),
            ),
            raw_change(
                "status",
                carried(&[(0xc1, CellValue::automatic(2, json!("pending")))]),
            ),
            raw_change(
                "label",
                carried(&[(0xd1, CellValue::automatic(1, json!("claude-1a2b3c")))]),
            ),
        ],
    );
    let (cells, inert) = history.read(&TABLE);
    assert_eq!(shown(&cells, "note"), Some(json!("first")));
    assert!(!field(&cells, "note").unwrap().conflicted());
    assert_eq!(shown(&cells, "status"), Some(json!("pending")));
    assert_eq!(shown(&cells, "realm"), Some(json!("claude")));
    assert_eq!(shown(&cells, "label"), None);
    assert_eq!(inert.changes, BTreeSet::from([at(roll, "label")]));
    assert_eq!(
        inert.carried,
        BTreeMap::from([(at(roll, "note"), BTreeSet::from([[0xb2; 32]]))])
    );
    assert_eq!(inert.records, records(1));
    assert!(!inert.changes.contains(&at(lifecycle, "row")));
    // A fresh automatic state after the carried lifecycle is a write after the birth.
    let fresh = history.write(9, &[], vec![automatic(2, "status", json!("active"))]);
    let (cells, inert) = history.read(&TABLE);
    assert_eq!(shown(&cells, "status"), Some(json!("pending")));
    assert!(inert.changes.contains(&at(fresh, "status")));
    {
        let _all = seam::control(Control::AdmitAll);
        let (cells, inert) = history.read(&TABLE);
        assert!(field(&cells, "note").unwrap().conflicted());
        assert_eq!(inert.total(), 0);
    }
}

/// Rule 4 rests on births being whole: a birth's lifecycle and its fixed cells go in one record.
/// The observer writes its whole plan as one record, and `att::pack` never splits the row a birth
/// gives it, however full the records it packs into. A birth split in two would lose its later
/// fixed cells.
/// NEGATIVE CONTROL: the same birth written as two records, its lifecycle first, leaves its
/// realm inert; the test reads it so.
#[cfg(unix)]
#[test]
fn a_birth_writes_its_lifecycle_and_its_identity_in_one_record() {
    use crate::alab::{
        observe,
        registry::{ACCOUNT, ADDRESS},
    };
    use crate::typed::{LIFECYCLE, owned::OwnedRead};
    // The observer: one plan, one record, every cell of the account's birth in it.
    let plan = observe::plan(
        &observe::Input {
            uuid: "11111111111111111111111111111111",
            sightings: &[observe::Sighting {
                cli: "claude".into(),
                realm: "claude".into(),
                email: "ada@alpha.example.invalid".into(),
                state: "running".into(),
                tabs: 1,
                src: "profile".into(),
                at_ms: 1,
            }],
            now_ms: 1,
            room: true,
        },
        &Cells::new(),
        &OwnedRead::default(),
    )
    .unwrap();
    assert_eq!(plan.born, 1);
    // Every column of each born row is in the plan's one record, its lifecycle among them.
    for (collection, columns) in [
        (
            &ACCOUNT,
            &["provider", "realm", "login", "status", LIFECYCLE][..],
        ),
        (&ADDRESS, &["addr", "domain", "origin", LIFECYCLE][..]),
    ] {
        let mut rows: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
        for change in plan
            .changes
            .iter()
            .filter(|c| c.collection == collection.name)
        {
            let key = CellKey::parse(&change.key).unwrap();
            rows.entry(key.pk).or_default().insert(key.col);
        }
        assert_eq!(rows.len(), 1, "{}", collection.name);
        let written = rows.into_values().next().unwrap();
        assert!(
            columns.iter().all(|column| written.contains(column)),
            "{}",
            collection.name
        );
    }

    // `pack`: rows of births, each whole in one record, around every record boundary.
    let row = |n: usize, cells: usize| -> Vec<Change> {
        (0..cells)
            .map(|c| Change {
                collection: RULED.name.into(),
                key: format!("rl-{n:016x}.c{c}"),
                value: Some(json!({"r": 2, "x": "x"})),
            })
            .collect()
    };
    for size in [
        1,
        3,
        7,
        crate::database::MAX_WRITES - 1,
        crate::database::MAX_WRITES,
    ] {
        let rows: Vec<Vec<Change>> = (0..40).map(|n| row(n, size.min(1 + n % size))).collect();
        let packed = crate::att::pack(rows.clone()).unwrap();
        for whole in &rows {
            let holders = packed
                .iter()
                .filter(|record| record.iter().any(|c| whole.contains(c)))
                .count();
            assert_eq!(
                holders,
                1,
                "a row of {} cells went into {holders} records",
                whole.len()
            );
            let record = packed.iter().find(|r| r.contains(&whole[0])).unwrap();
            assert!(whole.iter().all(|c| record.contains(c)));
        }
    }

    let mut whole = History::new();
    whole.write(1, &[], birth(automatic2));
    assert_eq!(shown(&whole.read(&TABLE).0, "realm"), Some(json!("claude")));
    let mut split = History::new();
    split.write(1, &[], vec![automatic(2, "row", json!("live"))]);
    split.write(
        1,
        &[],
        vec![
            automatic(2, "realm", json!("claude")),
            automatic(2, "status", json!("pending")),
        ],
    );
    assert_eq!(shown(&split.read(&TABLE).0, "realm"), None);
}

/// The rules are declared only where they can judge, and a collection's stored schema row
/// names them only where it opts in: a row that never opted in is the one earlier builds wrote.
/// NEGATIVE CONTROL: `PLAIN`, the same collection never opted in, stores a row with no
/// `authority` member; the test reads it so.
#[test]
fn the_rules_are_declared_only_where_they_judge_and_named_only_where_set() {
    assert_eq!(RULED.schema_row()["x"]["authority"], json!(2));
    assert!(
        RULED.schema_row()["x"]["columns"]
            .as_array()
            .unwrap()
            .iter()
            .any(|column| column["name"] == json!("label") && column["writer"] == json!("verb"))
    );
    assert!(PLAIN.schema_row()["x"].get("authority").is_none());
    let refused = |collection: Collection| collection.check().unwrap_err();
    assert_eq!(
        refused(Collection {
            authority: Some(3),
            ..RULED
        }),
        "the authority rules start at one of the collection's versions"
    );
    assert_eq!(
        refused(Collection {
            authority: Some(0),
            ..RULED
        }),
        "the authority rules start at one of the collection's versions"
    );
    const VERB: &[Column] = &[
        Column::new("label", Ty::Text(32)).writer(Writer::Verb),
        Column::new("row", Ty::Enum(LIFE)),
    ];
    const OWNER_BORN_BIRTH: &[Column] = &[
        Column::new("realm", Ty::Text(32)).writer(Writer::Birth),
        Column::new("row", Ty::Enum(LIFE)),
    ];
    const CLAIMED: &[Column] = &[
        Column::new("label", Ty::Label).claim(crate::typed::ClaimRule::Label),
        Column::new("row", Ty::Enum(LIFE)),
    ];
    const PREFERENCE: &[Column] = &[
        Column::new("rank", Ty::Uint).null(),
        Column::new("row", Ty::Enum(LIFE)),
    ];
    assert_eq!(
        refused(Collection {
            columns: VERB,
            authority: None,
            ..RULED
        }),
        "a verb column belongs to a collection under the authority rules"
    );
    assert_eq!(
        refused(Collection {
            columns: OWNER_BORN_BIRTH,
            ..RULED
        }),
        "under the authority rules, a collection with a Birth column is born by it"
    );
    assert_eq!(
        refused(Collection {
            columns: CLAIMED,
            ..RULED
        }),
        "a collection under the authority rules declares no identity claim"
    );
    // An owner-born collection under the rules with no Birth column (a preference) is one.
    assert_eq!(
        Collection {
            columns: PREFERENCE,
            ..RULED
        }
        .check(),
        Ok(())
    );
}

/// Rules 3a, 3b and 4 judge only writes made at or after the collection's authority version: an
/// older peer's automatic state after the birth (3a) or beside the owner's (3b), and an owner's
/// rewrite of a fixed cell (4), each made at version 1, stand. A lifecycle write is one that
/// gives the row a value and that rule 2 leaves standing: in a collection its owner births, an
/// automatic lifecycle (inert by rule 2) and a revert of it restoring absence are no birth, so
/// the owner's birth after them keeps its identity.
/// NEGATIVE CONTROL: each write made at version 2 instead is inert, and an identity rewritten
/// after the owner's birth is inert; the test reads them so.
#[test]
fn rules_3_and_4_judge_from_the_authority_version_and_only_a_standing_value_is_a_birth() {
    for (r, held) in [(1, false), (2, true)] {
        let mut history = History::new();
        history.write(1, &[], birth(automatic2));
        let later = history.write(1, &[], vec![automatic(r, "status", json!("active"))]);
        let (cells, inert) = history.read(&TABLE);
        assert_eq!(
            inert.changes.contains(&at(later, "status")),
            held,
            "3a at {r}"
        );
        let status = if held { "pending" } else { "active" };
        assert_eq!(shown(&cells, "status"), Some(json!(status)), "3a at {r}");
    }
    for (r, held) in [(1, false), (2, true)] {
        // The peer has not heard the owner's birth.
        let mut history = History::new();
        history.write(0, &[], birth(owner));
        let theirs = history.write(1, &[], vec![automatic(r, "status", json!("active"))]);
        let (cells, inert) = history.read(&TABLE);
        assert_eq!(
            inert.changes.contains(&at(theirs, "status")),
            held,
            "3b at {r}"
        );
        assert_eq!(
            field(&cells, "status").unwrap().conflicted(),
            !held,
            "3b at {r}"
        );
    }
    for (r, held) in [(1, false), (2, true)] {
        let mut history = History::new();
        history.write(0, &[], birth(owner));
        let rewrite = history.write(0, &[], vec![owner_at(r, "realm", json!("codex"))]);
        let (cells, inert) = history.read(&TABLE);
        assert_eq!(
            inert.changes.contains(&at(rewrite, "realm")),
            held,
            "4 at {r}"
        );
        let realm = if held { "claude" } else { "codex" };
        assert_eq!(shown(&cells, "realm"), Some(json!(realm)), "4 at {r}");
    }

    let mut history = History::new();
    let auto = history.write(1, &[], vec![automatic(2, "row", json!("live"))]);
    let undo = history.revert(
        2,
        auto,
        vec![Change {
            collection: RULED.name.into(),
            key: format!("{PK}.row"),
            value: None,
        }],
    );
    let born = history.write(
        0,
        &history.all(),
        vec![owner("realm", json!("claude")), owner("row", json!("live"))],
    );
    let (cells, inert) = history.read(&OWNER_BORN_TABLE);
    assert_eq!(inert.changes, BTreeSet::from([at(auto, "row")]));
    assert!(!inert.changes.contains(&at(undo, "row")));
    assert!(!inert.changes.contains(&at(born, "realm")));
    assert_eq!(shown(&cells, "realm"), Some(json!("claude")));
    let rewrite = history.write(0, &[], vec![owner("realm", json!("codex"))]);
    let (cells, inert) = history.read(&OWNER_BORN_TABLE);
    assert!(inert.changes.contains(&at(rewrite, "realm")));
    assert_eq!(shown(&cells, "realm"), Some(json!("claude")));
}

/// The revert rule: each cell of a revert must restore what this read admitted. A build that
/// read its target's past by no rules reverts the owner's status by restoring the automatic
/// status the owner replaced, which rule 3a holds inert here: that cell is inert, so the owner's
/// status still shows. A revert restoring the birth's status, as this build's compensation
/// builds it, stands; so does one that restates, as it reads, a key where its target's own
/// write is inert.
/// NEGATIVE CONTROL: with the revert rule off (`Control::Restored`), the other build's revert
/// shows the automatic status rule 3a held inert; the test reads it so.
#[test]
fn a_revert_cell_that_restores_what_the_read_holds_inert_is_inert_whoever_signed_it() {
    let mut history = History::new();
    history.write(0, &[], birth(owner));
    let auto = history.write(
        1,
        &history.all(),
        vec![automatic(2, "status", json!("active"))],
    );
    let set = history.write(0, &history.all(), vec![owner("status", json!("paused"))]);
    let other = history.revert(2, set, vec![automatic(2, "status", json!("active"))]);
    let (cells, inert) = history.read(&TABLE);
    assert!(inert.changes.contains(&at(auto, "status")));
    assert!(inert.changes.contains(&at(other, "status")));
    assert_eq!(shown(&cells, "status"), Some(json!("paused")));
    {
        let _off = seam::control(Control::Restored);
        let (cells, inert) = history.read(&TABLE);
        assert_eq!(shown(&cells, "status"), Some(json!("active")));
        assert!(!inert.changes.contains(&at(other, "status")));
    }
    let ours = history.revert(0, set, vec![owner("status", json!("pending"))]);
    let (cells, inert) = history.read(&TABLE);
    assert!(!inert.changes.contains(&at(ours, "status")));
    assert_eq!(shown(&cells, "status"), Some(json!("pending")));

    // An import's name is inert by the verb rule; its revert restates the owner's later name
    // there, and restores the note before the import.
    let mut history = History::new();
    history.write(0, &[], birth(owner));
    let import = history.write(
        2,
        &history.all(),
        vec![
            imported("label", json!("imported")),
            imported("note", json!("noted")),
        ],
    );
    history.write(0, &history.all(), vec![owner("label", json!("work"))]);
    let undo = history.revert(
        0,
        import,
        vec![
            owner("label", json!("work")),
            Change {
                collection: RULED.name.into(),
                key: format!("{PK}.note"),
                value: None,
            },
        ],
    );
    let (cells, inert) = history.read(&TABLE);
    assert_eq!(inert.changes, BTreeSet::from([at(import, "label")]));
    assert!(!inert.changes.contains(&at(undo, "label")));
    assert_eq!(shown(&cells, "label"), Some(json!("work")));
    // A restored absence: the revert's null candidate.
    assert_eq!(shown(&cells, "note"), Some(Value::Null));
}

/// A revert that restores a row's lifecycle to its absence returns the row to unborn on every
/// read, as the owner's write check reads it (`check::fixed_after_birth`): the row's next birth,
/// an automatic one included, stands whole (its fixed identity, its Birth columns and its
/// lifecycle) and the check passes it, while a write after that birth is judged after it again,
/// on the read (rules 3a and 4) and by the check alike.
/// NEGATIVE CONTROL: with `Lifecycle::born_before` ignoring restored absences (its `&& !self
/// .absences…` clause made `&& true`), the observer's second birth is held inert whole, rule 4
/// on its realm and rule 3a on the rest: `` left: {(…, "ruled", "rl-0000000000000001.realm"), …}
/// right: {} ``.
#[test]
fn a_revert_to_a_rows_absence_returns_it_to_unborn_and_its_next_birth_stands() {
    let absent = |column: &str| Change {
        collection: RULED.name.into(),
        key: format!("{PK}.{column}"),
        value: None,
    };
    let mut history = History::new();
    let first = history.write(1, &[], birth(automatic2));
    history.revert(
        0,
        first,
        vec![absent("realm"), absent("status"), absent("row")],
    );
    let (unborn, inert) = history.read(&TABLE);
    assert_eq!(inert, Inert::default());
    assert_eq!(shown(&unborn, "row"), Some(Value::Null));
    let again = vec![
        automatic2("realm", json!("codex")),
        automatic2("status", json!("pending")),
        automatic2("row", json!("live")),
    ];
    assert_eq!(
        crate::typed::check::fixed_after_birth(&TABLE, std::slice::from_ref(&again), &unborn),
        None
    );
    let reborn = history.write(1, &history.all(), again);
    let (cells, inert) = history.read(&TABLE);
    assert_eq!(inert.changes, BTreeSet::new());
    assert_eq!(shown(&cells, "realm"), Some(json!("codex")));
    assert_eq!(shown(&cells, "status"), Some(json!("pending")));
    assert_eq!(shown(&cells, "row"), Some(json!("live")));
    // Born again: what follows the second birth is after it.
    let status = history.write(1, &[reborn], vec![automatic2("status", json!("active"))]);
    let rewrite = vec![owner("realm", json!("claude"))];
    let refused =
        crate::typed::check::fixed_after_birth(&TABLE, std::slice::from_ref(&rewrite), &cells)
            .expect("refused: realm");
    assert_eq!(refused.column, "realm");
    let realm = history.write(0, &history.all(), rewrite);
    let (cells, inert) = history.read(&TABLE);
    assert_eq!(
        inert.changes,
        BTreeSet::from([at(status, "status"), at(realm, "realm")])
    );
    assert_eq!(inert.records, records(2));
    assert_eq!(shown(&cells, "realm"), Some(json!("codex")));
    assert_eq!(shown(&cells, "status"), Some(json!("pending")));
}

/// Rule 3b judges a roll's carried items as it judges a fresh write: a carried automatic state
/// beside a carried owner's state, as a build from before the rules can roll it, is left out of
/// the value; a carried value of automatic states beside a fresh owner's state is inert whole.
/// NEGATIVE CONTROL: with rule 3b off (`Control::Contested`), each register is a conflict of
/// the owner's state and the automatic one; the test reads it so.
#[test]
fn rule_3b_judges_each_carried_item_as_it_judges_a_fresh_write() {
    let mut history = History::new();
    let roll = history.write(
        9,
        &[],
        vec![
            stored(
                "row",
                carried(&[(0xa1, CellValue::owner(2, json!("live")))]),
            ),
            stored(
                "status",
                carried(&[
                    (0xc1, CellValue::owner(2, json!("pending"))),
                    (0xc2, CellValue::automatic(2, json!("active"))),
                ]),
            ),
        ],
    );
    let (cells, inert) = history.read(&TABLE);
    assert_eq!(shown(&cells, "status"), Some(json!("pending")));
    assert!(!field(&cells, "status").unwrap().conflicted());
    assert_eq!(
        inert.carried,
        BTreeMap::from([(at(roll, "status"), BTreeSet::from([[0xc2; 32]]))])
    );
    assert_eq!(inert.records, records(1));
    {
        let _off = seam::control(Control::Contested);
        assert!(
            field(&history.read(&TABLE).0, "status")
                .unwrap()
                .conflicted()
        );
    }

    let mut history = History::new();
    let roll = history.write(
        9,
        &[],
        vec![
            stored(
                "row",
                carried(&[(0xa1, CellValue::owner(2, json!("live")))]),
            ),
            stored(
                "status",
                carried(&[(0xd1, CellValue::automatic(2, json!("active")))]),
            ),
        ],
    );
    // The owner had not heard the roll.
    history.write(0, &[], vec![owner("status", json!("paused"))]);
    let (cells, inert) = history.read(&TABLE);
    assert_eq!(shown(&cells, "status"), Some(json!("paused")));
    assert!(!field(&cells, "status").unwrap().conflicted());
    assert_eq!(inert.changes, BTreeSet::from([at(roll, "status")]));
    assert!(inert.carried.is_empty());
    {
        let _off = seam::control(Control::Contested);
        assert!(
            field(&history.read(&TABLE).0, "status")
                .unwrap()
                .conflicted()
        );
    }
}

#[cfg(unix)]
mod store {
    //! Reads of a real store (`View::read`) of the scratch table, and its counts.

    use super::{Control, PK, RULED, TABLE, birth, imported, owner, records, seam, shown};
    use crate::{
        att::{Compiled, Usage, command},
        catalog::Descriptor,
        database::{Change, Command},
        documents::{application, log::Log},
        file::{self, Class, Computed, Type, materialize},
        table::tests::Lab,
        typed::View,
    };
    use astream_wire::sync::Digest;
    use aterm_json::{Value, json};
    use aterm_secrets::commons::roster::Role;
    use std::collections::BTreeSet;

    const COMPILED: Compiled = Compiled {
        table: &TABLE,
        generation: 1,
        successor: None,
        ..crate::att::REGISTRY
    };

    /// The scratch collection as an owner's file.
    const FILE: Type = Type {
        name: "test/ruled",
        about: "the authority rules' scratch collection",
        class: Class::Owner,
        store: file::Store::Table {
            family: &COMPILED,
            collection: &RULED,
        },
        computed: Computed::None,
        handles: &[],
    };

    fn descriptor(
        bootstrap: &aterm_secrets::commons::bootstrap::Bootstrap,
    ) -> crate::Result<Descriptor> {
        Descriptor::application(bootstrap.trust(), TABLE.application, TABLE.role, 1)
    }

    /// A one-Writer lab whose first home appends commands to the scratch table.
    struct Store(Lab);

    impl Store {
        fn new() -> Self {
            Self(Lab::with_foundation(&[Role::Writer]))
        }

        fn append(&self, command: &Command) -> Digest {
            application::with_store(&self.0.paths[0], true, descriptor, |session, store, _| {
                Ok(application::append(session, store, &command.encode()?)?.id)
            })
            .unwrap()
        }

        fn write(&self, changes: Vec<Change>) -> Digest {
            self.append(&Command::Write {
                request: crate::crypto::random().unwrap(),
                changes,
            })
        }

        /// `read` over the log, its view, and this Mac's writer.
        fn read<T>(&self, read: impl FnOnce(&Log, &View, Usage) -> T) -> T {
            application::observe_store(&self.0.paths[0], descriptor, |session, store, scope| {
                let log = Log::read_with(session, store, scope, crate::att::decode)?;
                let view = View::read(&TABLE, session, &log)?;
                let usage = Usage {
                    actor: session.actor(),
                    records: 0,
                    bytes: 0,
                    mine_records: 0,
                    mine_bytes: 0,
                };
                Ok(read(&log, &view, usage))
            })
            .unwrap()
        }
    }

    fn note(changes: &[Change]) -> Option<&Value> {
        changes
            .iter()
            .find(|change| change.key == format!("{PK}.note"))
            .and_then(|change| change.value.as_ref())
    }

    /// The store's replay leaves the inert write out while the view keeps its record, every
    /// count shows it (`aterm file status`, `aterm db table ls` and `check`) and `aterm db alab
    /// check` names its row. A revert never restores it: the owner's later note reverts to the
    /// owner's first one, which the inert write replaced as written; the birth's note, its
    /// register's only admitted candidate, reverts; the inert write itself never projected.
    /// NEGATIVE CONTROL: the later note's compensation without the inert changes restores the
    /// automatic note, and the birth's finds the inert write in its register and refuses (`an
    /// affected proposal changed; resolve it before reverting`); read with every write admitted
    /// (`Control::AdmitAll`), every count is 0 and nothing is named. The test reads them so.
    #[test]
    fn a_store_read_leaves_each_inert_write_out_counts_it_and_never_restores_it() {
        let store = Store::new();
        let mut born = birth(owner);
        born.push(owner("note", json!("first")));
        let born = store.write(born);
        let auto = store.write(vec![super::automatic(2, "note", json!("automatic"))]);
        store.read(|log, view, usage| {
            assert_eq!(shown(&view.cells, "note"), Some(json!("first")));
            assert_eq!(view.inert.records, records(1));
            assert!(view.ops.iter().any(|op| op.id == auto));
            assert_eq!(materialize::inert(&FILE, view), 1);
            assert_eq!(view.quota.over.len() + view.inert.total(), 1);
            assert_eq!(command::summary(&COMPILED, view, &usage)["inert"], json!(1));
            assert_eq!(command::check(&COMPILED, view, &usage)["inert"], json!(1));
            let found = crate::alab::checks::inert(&view.inert);
            assert_eq!(found.len(), 1);
            assert_eq!(
                (found[0].kind, found[0].collection, &found[0].pks[..]),
                ("inert-write", RULED.name, &[PK.to_owned()][..])
            );
            let undo = log
                .compensate([1; 16], born, &view.quota.over, &view.inert.changes)
                .unwrap();
            assert!(
                undo.changes()
                    .unwrap()
                    .iter()
                    .any(|change| change.key == format!("{PK}.note") && change.value.is_none())
            );
            assert_eq!(
                log.compensate([2; 16], auto, &view.quota.over, &view.inert.changes)
                    .unwrap_err()
                    .to_string(),
                "every write of the target proposal is inert; it never projected"
            );
            assert_eq!(
                log.compensate([3; 16], born, &view.quota.over, &BTreeSet::new())
                    .unwrap_err()
                    .to_string(),
                "an affected proposal changed; resolve it before reverting"
            );
        });
        {
            let _all = seam::control(Control::AdmitAll);
            store.read(|_, view, usage| {
                assert_eq!(materialize::inert(&FILE, view), 0);
                assert_eq!(command::summary(&COMPILED, view, &usage)["inert"], json!(0));
                assert_eq!(command::check(&COMPILED, view, &usage)["inert"], json!(0));
                assert!(crate::alab::checks::inert(&view.inert).is_empty());
            });
        }

        let later = store.write(vec![owner("note", json!("second"))]);
        store.read(|log, view, _| {
            assert_eq!(shown(&view.cells, "note"), Some(json!("second")));
            let undo = log
                .compensate([4; 16], later, &view.quota.over, &view.inert.changes)
                .unwrap();
            assert_eq!(
                note(undo.changes().unwrap()),
                Some(&crate::typed::cell::CellValue::owner(2, json!("first")).encode())
            );
            let unfiltered = log
                .compensate([5; 16], later, &view.quota.over, &BTreeSet::new())
                .unwrap();
            assert_eq!(
                note(unfiltered.changes().unwrap()),
                Some(&crate::typed::cell::CellValue::automatic(2, json!("automatic")).encode())
            );
        });
    }

    /// A revert of a record holding an inert write and an admitted one names both keys, as
    /// every reader requires of a revert: the admitted key goes back, and the inert one is
    /// restated as it reads, the owner's later name kept. Read back, the revert is eligible,
    /// it applies, and this Mac can still write the table.
    /// NEGATIVE CONTROL: a revert naming the admitted key alone, as the compensation built before
    /// it named every key, is refused by the reader (it counts as refused, and this Mac's writer
    /// needs recovery); the test appends one and reads it so.
    #[test]
    fn a_revert_of_a_record_partly_inert_names_every_key_and_applies() {
        let store = Store::new();
        store.write(birth(owner));
        let import = store.write(vec![
            imported("label", json!("imported")),
            imported("note", json!("noted")),
        ]);
        store.write(vec![owner("label", json!("work"))]);
        let undo = store.read(|log, view, _| {
            assert_eq!(view.inert.records, records(1));
            assert_eq!(shown(&view.cells, "note"), Some(json!("noted")));
            log.compensate([1; 16], import, &view.quota.over, &view.inert.changes)
                .unwrap()
        });
        let keys: BTreeSet<&str> = undo
            .changes()
            .unwrap()
            .iter()
            .map(|change| change.key.as_str())
            .collect();
        let label = format!("{PK}.label");
        let noted = format!("{PK}.note");
        assert_eq!(keys, BTreeSet::from([label.as_str(), noted.as_str()]));
        let reverted = store.append(&undo);
        store.read(|log, view, usage| {
            assert!(view.refused.is_empty(), "{:?}", view.refused);
            assert!(view.ops.iter().any(|op| op.id == reverted));
            assert_eq!(shown(&view.cells, "note"), Some(Value::Null));
            assert_eq!(shown(&view.cells, "label"), Some(json!("work")));
            log.writable(usage.actor).unwrap();
        });

        let partial = store.append(&Command::Revert {
            request: [2; 16],
            operation: import,
            changes: vec![Change {
                collection: RULED.name.into(),
                key: noted.clone(),
                value: None,
            }],
        });
        store.read(|log, view, usage| {
            assert!(view.refused.contains(&partial));
            assert!(log.writable(usage.actor).is_err());
        });
    }
}
