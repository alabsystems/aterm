// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A two-generation table family that declares no secret column, for the roll that keeps every
//! signed record (tracker TK-31) and reads each Mac's own rows and log entries from the sealed
//! log (tracker TK-40): no build registers it, so the test that rolls it registers it for its
//! own replicas (`att::register_for_test`).

use super::*;
use crate::{
    alab::registry,
    said::Said,
    typed::{
        Column, Derive, Kind, Ty, Writers,
        grid::tests::put,
        values::{Evidence, Principal},
    },
};
use aterm_json::json;

/// The most bytes one page's body holds: with its record's envelope and what the record says
/// of who made it, inside one record's 64 KiB (`crypto::MAX_BODY`).
pub(crate) const BODY: usize = 48 * 1024;

crate::att_collection!(
    /// A page of prose: one cells collection, no secret column.
    pub(crate) PAGE = Collection {
        name: "page",
        version: 1,
        kind: Kind::Cells,
        writers: Writers::Owner,
        prefix: "",
        derive: Derive::Word,
        columns: &[
            Column::new("title", Ty::Text(64)).null(),
            Column::new("body", Ty::Prose(BODY)).null(),
            Column::new("row", Ty::Enum(&["live", "retired"])),
        ],
        unique: &[],
        max_row: 0,
        authority: None,
    }
);

crate::att_collection!(
    /// What a Mac states of itself, at its own Commons machine uuid: one owned row a Mac.
    pub(crate) SEAT = Collection {
        name: "seat",
        version: 1,
        kind: Kind::Owned,
        writers: Writers::SelfKey,
        prefix: "",
        derive: Derive::SignerUuid,
        columns: &[
            Column::new("n", Ty::Uint),
            Column::new("label", Ty::Text(64)),
        ],
        unique: &[],
        max_row: 1024,
        authority: None,
    }
);

crate::att_collection!(
    /// One remark a Mac appends, at `<uuid32>.<n>`.
    pub(crate) REMARK = Collection {
        name: "remark",
        version: 1,
        kind: Kind::Log,
        writers: Writers::SelfSuffix,
        prefix: "",
        derive: Derive::Composite,
        columns: &[
            Column::new("n", Ty::Uint),
            Column::new("text", Ty::Text(64)),
        ],
        unique: &[],
        max_row: 1024,
        authority: None,
    }
);

/// The family's first generation: its pages, each Mac's seat and remarks, and nothing a roll
/// writes.
const KEPT_TABLE: Table = Table {
    application: "lab",
    role: "kept",
    collections: &[&PAGE, &SEAT, &REMARK],
    faces: &[],
    self_quota: registry::SELF_QUOTA,
    ends: &[],
};

/// Its second: the same collections, and the cover rows a roll into it writes
/// (`registry::ROLL`).
const KEPT_2_TABLE: Table = Table {
    application: "lab",
    role: "kept.2",
    collections: &[&PAGE, &SEAT, &REMARK, &registry::ROLL],
    faces: &[],
    self_quota: registry::SELF_QUOTA,
    ends: &[],
};

const fn generation(
    table: &'static Table,
    generation: u32,
    successor: Option<&'static Compiled>,
) -> Compiled {
    Compiled {
        table,
        title: "Kept",
        version: 1,
        birth: no_birth,
        guard: no_guard,
        seeded: no_seed,
        generation,
        successor,
        records: 2,
        rows_apart: false,
    }
}

/// No row of the fixture family is born through the grid.
fn no_birth(collection: &Collection, _: &BTreeMap<String, Value>, _: &Cells) -> Result<String> {
    Err(Error::new(&format!(
        "{}: rows are not born through the grid here",
        collection.name
    )))
}

/// No rule beyond the ones every table keeps.
fn no_guard(_: &mut [Vec<Change>], _: &Cells) -> Result<Option<Refused>> {
    Ok(None)
}

/// [`KEPT_TABLE`], compiled: generation 1 of a family that keeps every signed record across a
/// roll ([`keeps_records`]), taking version 2 records.
pub(crate) static KEPT: Compiled = generation(&KEPT_TABLE, 1, Some(&KEPT_2));

/// [`KEPT_2_TABLE`], compiled: generation 2, the family's last.
pub(crate) static KEPT_2: Compiled = generation(&KEPT_2_TABLE, 2, None);

/// Who `session` says it is, with `reason` and one piece of evidence.
pub(crate) fn said(session: &str, reason: &str) -> Said {
    Said {
        principal: Principal {
            session: Some(session.to_owned()),
            program: Some("claude".to_owned()),
            ..Principal::default()
        },
        reason: Some(reason.to_owned()),
        evidence: vec![Evidence::parse("commit:aterm@02f6fd363bb").unwrap()],
    }
}

/// Page `pk` born live with a body of `bytes` bytes of ordinary prose, as one row.
pub(crate) fn page(pk: &str, bytes: usize) -> Vec<Change> {
    let mut body = format!("page {pk}:");
    while body.len() < bytes {
        body.push_str(" a record kept under its own signature");
    }
    body.truncate(bytes);
    vec![
        put(&PAGE, pk, "row", json!("live")),
        put(&PAGE, pk, "body", json!(body)),
    ]
}

/// Page `pk`'s title, as one row.
pub(crate) fn title(pk: &str, title: &str) -> Vec<Change> {
    vec![put(&PAGE, pk, "title", json!(title))]
}

/// Declare `compiled` on Mac `paths` and enroll its store.
pub(crate) fn declare(paths: &Paths, compiled: &'static Compiled) {
    application::with_store(
        paths,
        true,
        |bootstrap| compiled.descriptor(bootstrap.trust()),
        |_, _, _| Ok(()),
    )
    .unwrap();
}

/// How Mac `paths`'s catalog reads `compiled`'s table.
pub(crate) fn state(paths: &Paths, compiled: &'static Compiled) -> Option<crate::catalog::State> {
    let (trust, snapshot) = application::peek_catalog(paths).unwrap()?;
    let id = compiled.descriptor(trust).unwrap().scope.digest();
    snapshot.entries.get(&id).map(|entry| entry.state)
}

/// The folder Mac `n` of `lab` stores `compiled`'s table in, there or not.
pub(crate) fn storage(
    lab: &crate::table::fixture::Lab,
    n: usize,
    compiled: &'static Compiled,
) -> std::path::PathBuf {
    let trust = lab.bootstrap(n).trust();
    crate::catalog::Directory::new(&lab.paths[n], trust)
        .path(compiled.descriptor(trust).unwrap().scope)
        .unwrap()
}
