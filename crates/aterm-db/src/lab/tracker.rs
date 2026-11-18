// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `lab/tracker`, generation 1 (docs/DESIGN-lab-tracker-2026-11-14.md §3): the lab tracker's
//! table family. This build compiles its two log tables, the append-only ones (TK-05):
//!
//! - [`MEASURE`]: one sample of a goal's metric, as `aterm goals measure` states it;
//! - [`NOTE`]: one note about any row of the tracker, with its evidence.
//!
//! A log lives in this catalog table, not in the journal's week ring (`catalog::ring`): the ring
//! forgets a week's tables, and the tracker's history is what the lab reads its progress from.
//! Each entry is keyed `<uuid32>.<n>`: the signing Mac's Commons machine uuid and that Mac's own
//! counter, which the append takes under the Mac's writer lock as one past the highest it has
//! used (`att::Handle::append_log`). Two agents on one Mac so get two keys, and two Macs never
//! share one; no key is read off a clock (`typed::owned::read_log` keeps the first entry of a
//! key and counts any other inert).
//!
//! The cells tables (goal, package, decision, …) join this generation with TK-10, which also
//! settles these columns: no release holds `lab/tracker` yet, so generation 1 may still change.

use crate::typed::{
    Collection, Column, Derive, Face, Faced, Kind, MAX_OWNED_ROW, Table, Ty, Writer, Writers,
};

/// The family's application and role: `lab/tracker`.
pub const APPLICATION: &str = "lab";
pub const ROLE: &str = "tracker";
/// The table descriptor's version.
pub const VERSION: u32 = 1;

/// Longest `source` command of a measurement.
pub const MAX_SOURCE: usize = 1024;
/// Longest text of a note.
pub const MAX_TEXT: usize = 4096;

crate::att_collection!(
    /// One sample of a metric (§3 `measure`): `<uuid32>.<n>`, the value written exactly as a
    /// decimal, when the Mac measured it, the command that measured it and the commit it ran at.
    pub MEASURE = Collection {
        name: "measure",
        version: 1,
        kind: Kind::Log,
        writers: Writers::SelfSuffix,
        prefix: "",
        derive: Derive::Composite,
        columns: &[
            Column::new("n", Ty::Uint).writer(Writer::Mine),
            Column::new("metric", Ty::Ref("metric")).writer(Writer::Mine),
            Column::new("value", Ty::Decimal).writer(Writer::Mine),
            Column::new("at", Ty::Time).writer(Writer::Mine),
            Column::new("source", Ty::Command(MAX_SOURCE))
                .null()
                .writer(Writer::Mine),
            Column::new("commit", Ty::Commit).null().writer(Writer::Mine),
        ],
        unique: &[],
        max_row: 2048,
        // An entry is its signer's own statement at its own key: no cell is judged by the
        // authority rules.
        authority: None,
    }
);

crate::att_collection!(
    /// One note (§3 `note`): `<uuid32>.<n>`, the key of the row it is about, its text, the
    /// evidence it cites and when the Mac wrote it.
    pub NOTE = Collection {
        name: "note",
        version: 1,
        kind: Kind::Log,
        writers: Writers::SelfSuffix,
        prefix: "",
        derive: Derive::Composite,
        columns: &[
            Column::new("n", Ty::Uint).writer(Writer::Mine),
            Column::new("about", Ty::Key).writer(Writer::Mine),
            Column::new("text", Ty::Prose(MAX_TEXT)).writer(Writer::Mine),
            Column::new(
                "evidence",
                Ty::List(&Ty::Evidence, crate::typed::values::MAX_EVIDENCE),
            )
            .null()
            .writer(Writer::Mine),
            Column::new("at", Ty::Time).writer(Writer::Mine),
        ],
        unique: &[],
        max_row: MAX_OWNED_ROW,
        // An entry is its signer's own statement at its own key: no cell is judged by the
        // authority rules.
        authority: None,
    }
);

/// `lab/tracker`. Its writers' entries are counted against no fixed number of records (TK-D6):
/// the store's own per-writer bounds hold until TK-02 replaces them with byte budgets.
pub const TRACKER: Table = Table {
    application: APPLICATION,
    role: ROLE,
    collections: &[&MEASURE, &NOTE],
    faces: &[
        Faced {
            collection: "measure",
            title: "Measurements",
            singular: "Measurement",
            face: Face::Mac,
            // What was measured names the sample; its value and when follow, as a person reads
            // a reading. The command and the commit are its provenance, and the counter last.
            order: &["metric", "value", "at", "source", "commit"],
        },
        Faced {
            collection: "note",
            title: "Notes",
            singular: "Note",
            face: Face::Mac,
            // The row a note is about names it; then what it says, on what evidence, and when.
            order: &["about", "text", "evidence", "at"],
        },
    ],
    self_quota: usize::MAX,
    ends: &[],
};

// Each log is one Mac's own appends, so each says so: a collection added to the tracker without
// a face does not compile, nor does a display order that names a column its log does not have.
const _: () = {
    assert!(
        TRACKER.faces_every_collection(),
        "every tracker collection says how a person is shown it, once"
    );
    assert!(
        TRACKER.orders_name_columns(),
        "every display order names its collection's columns, each once"
    );
};
