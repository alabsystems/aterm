// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `lab/tracker`, generation 1 (docs/DESIGN-lab-tracker-2026-11-14.md §3): the lab tracker's
//! table family. Its cells tables are what the lab plans and works from ([`tables`]: a goal, a
//! package, a decision, a request, …), a row keyed by the word it is named by or that word's
//! mapped form ([`mapped`]) and held to the tracker's own rules ([`checks`]); a package's state
//! is settled where it is read, from the records that wrote it ([`state`]), and so are its exit
//! and outcome, frozen once it is claimed but by an approved change ([`frozen`]). Its two log tables
//! of record are the append-only ones (TK-05):
//!
//! - [`MEASURE`]: one sample of a goal's metric, as `aterm goals measure` states it;
//! - [`NOTE`]: one note about any row of the tracker, with its evidence.
//!
//! A third log keeps the leases ([`lease::RENEWAL`]): each machine's renewals of the claims its
//! sessions hold, from which every machine reads alike who holds what (TK-14).
//!
//! The schema and its rules are pure and compile on every target; only what reaches the store
//! is `cfg(unix)` (`att::TRACKER`). The lab tracker's TSVs, publication's `goals/data` at a named
//! commit, are read into its rows and logs by [`import`], which names every cell it cannot hold
//! as the TSV says (TK-11). No build registers the family for production before the
//! cutover (TK-32): `att::FAMILIES` and the catalog do not hold it, and only a test declares it.
//! `aterm goals` (`command`, Unix) writes it, and until that cutover its front door writes nothing.
//!
//! A log lives in this catalog table, not in the journal's week ring (`catalog::ring`): the ring
//! forgets a week's tables, and the tracker's history is what the lab reads its progress from.
//! Each entry is keyed `<uuid32>.<n>`: the signing Mac's Commons machine uuid and that Mac's own
//! counter, which the append takes under the Mac's writer lock as one past the highest it has
//! used (`att::Handle::append_log`). Two agents on one Mac so get two keys, and two Macs never
//! share one; no key is read off a clock (`typed::owned::read_log` keeps the first entry of a
//! key and counts any other inert).
//!
//! No release holds `lab/tracker` yet, so generation 1 may still change until the cutover.

pub mod checks;
#[cfg(unix)]
pub mod command;
pub mod frozen;
pub mod import;
pub mod lease;
pub mod mapped;
pub mod state;
pub mod tables;
#[cfg(test)]
pub(crate) mod tests;

use crate::typed::{
    Collection, Column, Derive, Face, Faced, Kind, MAX_OWNED_ROW, Table, Ty, Writer, Writers,
    values::MAX_COMMAND,
};
use tables::{
    CHANGE, COMPETITION, DECISION, GOAL, GROUP, LANE, METRIC, PACKAGE, PLAN, PROJECT, RELEASE,
    REPO, REQUEST, RULING, STOP, VIEW,
};

/// The family's application and role: `lab/tracker`.
pub const APPLICATION: &str = "lab";
pub const ROLE: &str = "tracker";
/// The table descriptor's version.
pub const VERSION: u32 = 1;
/// What a person calls the family: the heading the Tables page lists its tables under.
pub const TITLE: &str = "Lab tracker";

/// Longest `source` command of a measurement: a command's own bound. Four `source` cells of
/// publication 384b3a9f's `metrics.tsv` are 1,335 bytes, past the 1,024 the logs first took.
pub const MAX_SOURCE: usize = MAX_COMMAND;
/// Longest text of a note.
pub const MAX_TEXT: usize = 4096;

crate::att_collection!(
    /// One sample of a metric (§3 `measure`): `<uuid32>.<n>`, the value written exactly as a
    /// decimal, when the Mac measured it, the command that measured it and the commit it ran at,
    /// and the machine it was taken on with that machine's class. The machine is a link to its
    /// registry row (§4.6), by its machine uuid; it is the signing Mac only where the sample was
    /// taken there, so it is stated, and null for a sample carried from a record that named a
    /// machine in words.
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
            Column::new("machine", tables::MEASURED_ON)
                .null()
                .writer(Writer::Mine),
            Column::new("class", Ty::Handle(64)).null().writer(Writer::Mine),
        ],
        unique: &[],
        max_row: MAX_OWNED_ROW,
        // An entry is its signer's own statement at its own key: no cell is judged by the
        // authority rules.
        authority: None,
    }
);

crate::att_collection!(
    /// One note (§3 `note`): `<uuid32>.<n>`, the table and the key of the row it is about, its
    /// text, the evidence it cites and when the Mac wrote it. The table is stated because a key
    /// alone names no row: a competition is keyed as its goal is, and a project as its
    /// repository.
    pub NOTE = Collection {
        name: "note",
        version: 1,
        kind: Kind::Log,
        writers: Writers::SelfSuffix,
        prefix: "",
        derive: Derive::Composite,
        columns: &[
            Column::new("n", Ty::Uint).writer(Writer::Mine),
            Column::new("on", Ty::Enum(tables::NOTED)).writer(Writer::Mine),
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
    collections: &[
        &GROUP,
        &GOAL,
        &METRIC,
        &LANE,
        &REPO,
        &PACKAGE,
        &CHANGE,
        &DECISION,
        &RULING,
        &STOP,
        &COMPETITION,
        &PROJECT,
        &PLAN,
        &REQUEST,
        &RELEASE,
        &VIEW,
        &MEASURE,
        &NOTE,
        &lease::RENEWAL,
    ],
    faces: &[
        // What the owner states: the word a row is named by first, then what a person reads of
        // it next.
        owner("group", ("Groups", "Group"), &["id", "name", "horizon"]),
        owner(
            "goal",
            ("Goals", "Goal"),
            &["id", "title", "group", "metric"],
        ),
        owner("metric", ("Metrics", "Metric"), &["id", "goal", "unit"]),
        owner("lane", ("Lanes", "Lane"), &["id", "name", "prefix", "rank"]),
        owner("repo", ("Repositories", "Repository"), &["id", "remote"]),
        owner(
            "package",
            ("Packages", "Package"),
            &["id", "title", "state", "lane", "rank", "depends", "claim"],
        ),
        owner(
            "change",
            ("Changes", "Change"),
            &[
                "package",
                "column",
                "exit",
                "outcome",
                "state",
                "reason",
                "approved_by",
            ],
        ),
        owner(
            "decision",
            ("Decisions", "Decision"),
            &["id", "decision", "state", "decided_by", "decided_on"],
        ),
        owner("ruling", ("Rulings", "Ruling"), &["date", "about", "quote"]),
        owner("stop", ("Stops", "Stop"), &["lane", "item"]),
        owner(
            "competition",
            ("Competitions", "Competition"),
            &["id", "name", "state"],
        ),
        owner("project", ("Projects", "Project"), &["id", "size", "score"]),
        owner("plan", ("Plan", "Plan line"), &["id", "value"]),
        owner(
            "request",
            ("Requests", "Request"),
            &["id", "title", "kind", "state", "priority"],
        ),
        owner(
            "release",
            ("Releases", "Release"),
            &["id", "repo", "version", "state"],
        ),
        owner("view", ("Views", "View"), &["id", "of"]),
        Faced {
            collection: "measure",
            title: "Measurements",
            singular: "Measurement",
            face: Face::Mac,
            // What was measured names the sample; its value and when follow, as a person reads
            // a reading. The command and the commit are its provenance, and the counter last.
            order: &["metric", "value", "at", "machine", "source", "commit"],
        },
        Faced {
            collection: "note",
            title: "Notes",
            singular: "Note",
            face: Face::Mac,
            // The row a note is about names it, by its table and key; then what it says, on
            // what evidence, and when.
            order: &["on", "about", "text", "evidence", "at"],
        },
        Faced {
            collection: "renewal",
            title: "Lease renewals",
            singular: "Lease renewal",
            face: Face::Mac,
            // When the machine renewed its leases, then how long that keeps them.
            order: &["at", "period"],
        },
    ],
    self_quota: usize::MAX,
    ends: &[],
};

/// How a person is shown a table of what the owner states.
const fn owner(
    collection: &'static str,
    (title, singular): (&'static str, &'static str),
    order: &'static [&'static str],
) -> Faced {
    Faced {
        collection,
        title,
        singular,
        face: Face::Owner,
        order,
    }
}

// A collection added to the tracker without a face does not compile, nor does a display order
// that names a column its collection does not have.
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
