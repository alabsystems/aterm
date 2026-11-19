// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Version 2 records (package TK-03, docs/DESIGN-lab-tracker-2026-11-14.md §6, §7 finding 2):
//! a generation that takes them, the previous release's reader frozen as it shipped, and what
//! that release does beside them.

use super::*;
use crate::{
    alab::{Registry, registry, registry::MACHINE},
    documents::application,
    said::Said,
    table::{
        Session,
        fixture::{Lab, repair_commons_among_first, repair_commons_first},
    },
    typed::{
        grid::tests::{MC, machine, put},
        values::{Evidence, Principal},
    },
};
use aterm_json::{Map, json};
use aterm_secrets::commons::roster::Role;
use std::collections::BTreeSet;

/// The registry's newest collections but those a roll writes (`archived`, `roll`, `history`):
/// a first generation, which nothing restates.
const SAID_COLLECTIONS: [&Collection; 16] = {
    use registry::*;
    [
        &PROVIDER,
        &CLI,
        &PLAN,
        &MODEL,
        &MACHINE,
        &MACHINE_FACTS,
        &ADDRESS,
        &MAILBOX,
        &MAILBOX_OBS,
        &ACCOUNT_OBS,
        &PHONE,
        &ACCOUNT,
        &SUBACCOUNT,
        &AUTH_ROUTE,
        &PREFERENCE,
        &SOURCE,
    ]
};

/// A typed table generation that takes version 2 records: the registry's newest collections,
/// under a family of its own that no release compiles. The first such generation a build
/// declares is the tracker's (TK-10); this one stands in for it here.
pub(crate) const SAID_TABLE: Table = Table {
    application: "lab",
    role: "said",
    collections: &SAID_COLLECTIONS,
    faces: &[],
    self_quota: registry::SELF_QUOTA,
    ends: &["preference"],
};

/// [`SAID_TABLE`], compiled: generation 1 of its family, taking version 2 records.
pub(crate) const SAID: Compiled = Compiled {
    table: &SAID_TABLE,
    title: "Said",
    version: 1,
    birth: registry::birth,
    guard: registry::guard,
    seeded: crate::alab::seeds::compiled,
    generation: 1,
    successor: None,
    records: 2,
    rows_apart: false,
};

/// The roles of the table generations the previous release, v0.203.0, compiles (`git show
/// v0.203.0:crates/aterm-db/src/att.rs`: `TABLES = &[REGISTRY, REGISTRY_2, REGISTRY_3,
/// REGISTRY_4]`; v0.202.0 compiles the first three): the only ones whose records it decodes.
const ROLES_0_203: [&str; 4] = ["registry", "registry.2", "registry.3", "registry.4"];

/// The most changes one record holds in v0.203.0 (`database::MAX_WRITES`).
const MAX_WRITES_0_203: usize = 64;

/// A typed record as the previous release, v0.203.0, reads it, frozen from that release:
/// `Command::decode` (`git show v0.203.0:crates/aterm-db/src/database.rs`, from line 269) and
/// `att::decode` (`…/att.rs`, line 442); both are byte for byte v0.202.0's. Exactly `v: 1`; a
/// write's members exactly `v`, `request`, `kind` and `changes` (a revert's also `operation`);
/// 1 to 64 changes, each exactly `collection`, `key` and `value`, both names, each pair once;
/// the bytes canonical (sorted members, as the release encodes them); and no write that stops a
/// cell. Anything else is refused: that record never projects there. The release also checks a
/// change to one of the private database's collections by that collection's rules; no typed
/// record writes one, and this reader refuses them all, which is stricter, never laxer.
fn read_by_0_203(payload: &[u8]) -> Result<Command> {
    let refused = |why: &str| Err(Error::new(why));
    if payload.len() > crate::crypto::MAX_BODY {
        return refused("database payload exceeds limit");
    }
    let Ok(v) = aterm_json::from_slice::<Value>(payload) else {
        return refused("invalid database payload JSON");
    };
    if v.get("v").and_then(Value::as_u64) != Some(1) {
        return refused("unsupported database application schema");
    }
    let Some(object) = v.as_object() else {
        return refused("invalid database payload JSON");
    };
    let kind = object.get("kind").and_then(Value::as_str);
    let members: &[&str] = match kind {
        Some("write") => &["changes", "kind", "request", "v"],
        Some("revert") => &["changes", "kind", "operation", "request", "v"],
        _ => return refused("typed table records are writes and reverts only"),
    };
    if object
        .keys()
        .map(String::as_str)
        .ne(members.iter().copied())
    {
        return refused("database payload has unknown or missing fields");
    }
    let Ok(request) = crate::database::decode_hex::<16>(
        object.get("request").and_then(Value::as_str).unwrap_or(""),
    ) else {
        return refused("invalid database hex identifier");
    };
    let Some(raw) = object
        .get("changes")
        .and_then(Value::as_array)
        .filter(|changes| (1..=MAX_WRITES_0_203).contains(&changes.len()))
    else {
        return refused("invalid transaction write count");
    };
    let mut changes = Vec::new();
    let mut unique = BTreeSet::new();
    for change in raw {
        let fields = change
            .as_object()
            .map(|c| c.keys().map(String::as_str).collect());
        if fields != Some(vec!["collection", "key", "value"]) {
            return refused("database payload has unknown or missing fields");
        }
        let (Some(collection), Some(key)) = (change["collection"].as_str(), change["key"].as_str())
        else {
            return refused("invalid database payload field");
        };
        // A name of 1 to 64 bytes of `[A-Za-z0-9._-]` (`crypto::name`), the collection's not
        // starting `_`, and each (collection, key) once.
        let name = |s: &str| {
            (1..=64).contains(&s.len())
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
        };
        if !name(collection)
            || collection.starts_with('_')
            || !name(key)
            || !unique.insert((collection, key))
        {
            return refused("invalid or duplicate transaction key");
        }
        // The private database's collections, which the release checks by their own rules, are
        // never a typed table's: refused here, as no typed record of this build writes one.
        if [
            "accounts",
            "machines",
            "emails",
            "mailboxes",
            "email-addresses",
            "phones",
        ]
        .contains(&collection)
        {
            return refused("a private database collection in a typed record");
        }
        changes.push(Change {
            collection: collection.to_owned(),
            key: key.to_owned(),
            value: Some(change["value"].clone()).filter(|value| !value.is_null()),
        });
    }
    // Canonical: the release encodes the same members, sorted, as compact JSON.
    let mut again = Map::new();
    for (name, value) in object {
        again.insert(name.clone(), value.clone());
    }
    if aterm_json::to_vec(&Value::Object(again)).ok().as_deref() != Some(payload) {
        return refused("typed table payload is not canonical");
    }
    if kind == Some("revert") {
        let Ok(operation) = crate::database::decode_hex::<32>(
            object
                .get("operation")
                .and_then(Value::as_str)
                .unwrap_or(""),
        ) else {
            return refused("invalid database hex identifier");
        };
        return Ok(Command::Revert {
            request,
            operation,
            changes,
            said: None,
        });
    }
    if changes.iter().any(|change| change.value.is_none()) {
        return refused("a typed table write never stops a cell");
    }
    Ok(Command::Write {
        request,
        changes,
        said: None,
    })
}

/// Who made the version 2 write: an agent's session on Mac 0, with its reason and evidence.
fn said() -> Said {
    Said {
        principal: Principal {
            session: Some("s-47051b9a207955a92b14".to_owned()),
            program: Some("claude".to_owned()),
            ..Principal::default()
        },
        reason: Some("TK-03: a version 2 write".to_owned()),
        evidence: vec![Evidence::parse("commit:aterm@6159fb1b1e5").unwrap()],
    }
}

/// Declare `compiled` on Mac `paths` and enroll its store.
fn declare(paths: &Paths, compiled: &'static Compiled) {
    application::with_store(
        paths,
        true,
        |bootstrap| compiled.descriptor(bootstrap.trust()),
        |_, _, _| Ok(()),
    )
    .unwrap();
}

/// `compiled`'s table on Mac `paths` read as `decode` reads it: the ids its store retains and the
/// view a reader decoding with `decode` makes of them.
fn read_as(
    paths: &Paths,
    compiled: &'static Compiled,
    decode: fn(&[u8]) -> Result<Command>,
) -> (BTreeSet<Digest>, View) {
    application::observe_store(
        paths,
        |bootstrap| compiled.descriptor(bootstrap.trust()),
        |session, store, scope| {
            let log = Log::read_with(session, store, scope, decode)?;
            let ids = log.retained_ids().copied().collect();
            Ok((ids, View::read(compiled.table, session, &log)?))
        },
    )
    .unwrap()
}

/// `payload` signed by Mac `paths` into `compiled`'s table as its next record, past every write
/// path's fence: what a build that did not keep the fence would sign. The Mac enrolls the
/// table's store where it has not.
pub(crate) fn sign_raw(paths: &Paths, compiled: &'static Compiled, payload: &[u8]) -> Digest {
    application::with_store(
        paths,
        true,
        |bootstrap| compiled.descriptor(bootstrap.trust()),
        |session: &Session<'_>, store, scope| {
            let evidence = store.metadata_evidence(session)?;
            let headers: Vec<_> = evidence
                .iter()
                .filter(|op| op.catalog_eligible)
                .map(|op| &op.header)
                .collect();
            let previous = headers
                .iter()
                .filter(|header| header.actor == session.actor())
                .max_by_key(|header| header.sequence);
            let mut parents: BTreeSet<_> = headers.iter().map(|header| header.id).collect();
            for header in &headers {
                for parent in header.parents.iter().chain(header.previous.iter()) {
                    parents.remove(parent);
                }
            }
            if let Some(previous) = previous {
                parents.insert(previous.id);
            }
            let parents: Vec<_> = parents.into_iter().collect();
            let wire = crate::table::fixture::seal_at(
                session,
                scope,
                crate::crypto::Causal {
                    sequence: previous.map_or(1, |header| header.sequence + 1),
                    previous: previous.map(|header| header.id),
                    parents: &parents,
                },
                payload,
                1_800_000_000_000,
            )?;
            Ok(store.ingest(session, &wire)?.id)
        },
    )
    .unwrap()
}

/// A VERSION 2 WRITE READS ON THE PREVIOUS RELEASE, AND ITS MAC STILL WRITES (TK-03; design §7,
/// finding 2). Mac 0 runs this build and writes a version 2 record, which says who made it, why
/// and on what evidence, into a generation that takes version 2 records. Mac 1 runs v0.203.0,
/// whose reader [`read_by_0_203`] freezes. On Mac 1:
///
/// * the generation is not one v0.203.0 compiles, so that release never decodes its records,
///   and yet Mac 1 stores the version 2 record as the signed bytes it relays;
/// * the registry reads as it did before: complete, nothing refused, Mac 0's row there;
/// * Mac 1 still writes the registry, a record v0.203.0's reader decodes, which Mac 0 reads.
///
/// Mac 2 runs this build and reaches Mac 0 only through Mac 1: it reads the version 2 record
/// with its principal, reason and evidence, signed by Mac 0 and relayed by the previous
/// release. Version 1 records are unchanged on the wire: one this build encodes is the bytes
/// v0.203.0 encodes and decodes.
///
/// IN-TEST NEGATIVE CONTROL: the same version 2 payload in a generation v0.203.0 reads (the
/// registry, which takes version 1 records only). Every write path's fence refuses it
/// (`Compiled::admits`). Signed there past the fence, v0.203.0 refuses it as one record, so
/// its cell never projects there, and this build's reader of that generation refuses it too,
/// so the two releases still agree on every cell (§7, finding 2; were this build to read it,
/// Mac 1's next write would observe a value it never saw, and overwrite it unseen).
///
/// NEGATIVE CONTROL, observed: with `Compiled::admits` admitting a version 2 record in any
/// generation, the test fails with `the registry's fence refuses a version 2 record`; with
/// `Compiled::decoder` reading the registry with `decode_said`, it fails with `this build
/// reads the registry as v0.203.0 does`.
#[test]
fn a_v2_write_reads_on_the_previous_release_and_its_machine_still_writes() {
    const NEW: usize = 0;
    const OLD: usize = 1;
    const LATE: usize = 2;
    let clocks = [1_800_000_000; 3];
    let lab = Lab::new(&[Role::Writer, Role::Writer, Role::Writer]);
    let trust = lab.bootstrap(NEW).trust();
    let catalog = crate::catalog::scope(trust).digest();
    let registry_scope = REGISTRY.descriptor(trust).unwrap().scope.digest();
    let said_scope = SAID.descriptor(trust).unwrap().scope.digest();

    // Every Mac holds the registry, Mac 0's row in it.
    let owner = Registry::at(lab.paths[NEW].clone());
    owner.open().unwrap();
    owner.write(vec![machine(MC)]).unwrap();
    repair_commons_first(&lab, 1, catalog);
    repair_commons_first(&lab, 1, registry_scope);

    // Mac 0 writes a version 2 record: a row, with who made it, why and on what evidence.
    declare(&lab.paths[NEW], &SAID);
    let handle = Handle::at(lab.paths[NEW].clone(), &SAID).saying(said());
    let ((), applied) = handle
        .commit_owner(|_| Ok(((), vec![machine(MC)])))
        .unwrap();
    let [written] = applied.operations[..] else {
        panic!("one record: {applied:?}")
    };
    let wire_said = handle.history().unwrap();
    assert_eq!(wire_said.len(), 1);
    assert_eq!(
        wire_said[0].said.as_ref(),
        Some(&said()),
        "Mac 0 reads its own"
    );

    // The previous release meets it: Mac 0 and Mac 1 alone.
    repair_commons_among_first(&lab, 1, &[NEW, OLD], &clocks, catalog);
    repair_commons_among_first(&lab, 2, &[NEW, OLD], &clocks, said_scope);
    assert!(
        !ROLES_0_203.contains(&SAID.table.role),
        "v0.203.0 compiles no version 2 generation, so it decodes none of its records"
    );
    let (held, _) = read_as(&lab.paths[OLD], &SAID, read_by_0_203);
    assert!(held.contains(&written), "Mac 1 stores the record it relays");

    // Mac 1 reads the registry as it did before: complete, nothing refused, Mac 0's row.
    let (_, before) = read_as(&lab.paths[OLD], &REGISTRY, read_by_0_203);
    assert!(before.complete && before.refused.is_empty(), "{before:?}");
    assert!(before.project(&MACHINE).rows.contains_key(MC));

    // Mac 1 still writes, a record v0.203.0 decodes, byte for byte as it encodes it.
    let parked = vec![put(&MACHINE, MC, "status", json!("parked"))];
    let v1 = Command::Write {
        request: [7; 16],
        changes: parked.clone(),
        said: None,
    };
    assert_eq!(read_by_0_203(&v1.encode().unwrap()).unwrap(), v1);
    Registry::at(lab.paths[OLD].clone())
        .write(vec![parked])
        .unwrap();
    let (_, after) = read_as(&lab.paths[OLD], &REGISTRY, read_by_0_203);
    assert!(after.complete && after.refused.is_empty(), "{after:?}");
    assert_eq!(
        after.project(&MACHINE).rows[MC].get("status"),
        Some(&json!("parked"))
    );
    repair_commons_among_first(&lab, 1, &[NEW, OLD], &clocks, registry_scope);
    let at_new = Handle::at(lab.paths[NEW].clone(), &REGISTRY)
        .view()
        .unwrap();
    assert_eq!(
        at_new.project(&MACHINE).rows[MC].get("status"),
        Some(&json!("parked")),
        "Mac 0 reads the previous release's write"
    );

    // Mac 2 reaches Mac 0 only through Mac 1, and reads who made the version 2 write.
    repair_commons_among_first(&lab, 1, &[OLD, LATE], &clocks, catalog);
    repair_commons_among_first(&lab, 2, &[OLD, LATE], &clocks, said_scope);
    let relayed = Handle::at(lab.paths[LATE].clone(), &SAID)
        .history()
        .unwrap();
    let entry = relayed
        .iter()
        .find(|entry| entry.operation == written)
        .expect("Mac 2 reads the record Mac 1 relayed");
    assert_eq!(entry.said.as_ref(), Some(&said()));
    assert_eq!(entry.machine, Some(lab.machine(NEW)), "Mac 0 signed it");
    assert!(
        Handle::at(lab.paths[LATE].clone(), &SAID)
            .view()
            .unwrap()
            .project(&MACHINE)
            .rows
            .contains_key(MC)
    );

    // NEGATIVE CONTROL: the same version 2 payload where v0.203.0 reads.
    let stray = Command::Write {
        request: [9; 16],
        changes: vec![put(&MACHINE, MC, "note", json!("unseen"))],
        said: Some(Box::new(said())),
    };
    assert!(
        REGISTRY.admits(&stray).is_err(),
        "the registry's fence refuses a version 2 record"
    );
    assert!(SAID.admits(&stray).is_ok());
    let payload = stray.encode().unwrap();
    assert!(read_by_0_203(&payload).is_err(), "v0.203.0 refuses it");
    let signed = sign_raw(&lab.paths[NEW], &REGISTRY, &payload);
    repair_commons_among_first(&lab, 1, &[NEW, OLD], &clocks, registry_scope);
    let (held, old) = read_as(&lab.paths[OLD], &REGISTRY, read_by_0_203);
    assert!(held.contains(&signed));
    assert_eq!(
        old.refused,
        vec![signed],
        "v0.203.0 refuses that one record"
    );
    assert_eq!(old.project(&MACHINE).rows[MC].get("note"), None);
    let new = Handle::at(lab.paths[NEW].clone(), &REGISTRY)
        .view()
        .unwrap();
    assert_eq!(
        (
            new.refused.clone(),
            new.project(&MACHINE).rows[MC].get("note")
        ),
        (vec![signed], None),
        "this build reads the registry as v0.203.0 does"
    );
}

/// A log append into a generation that takes version 2 records (`lab/tracker`'s `measure`)
/// carries who made it where its handle says so, and states nothing where it does not
/// ([`Handle::append_log`]): a log's records say who made them as a cell write's do.
///
/// NEGATIVE CONTROL, observed: with `append_log` packing its records stating nothing (`pack`,
/// `said: None`), the test fails at `the append says who made it`.
#[test]
fn a_log_append_says_who_made_it_where_its_table_takes_version_2_records() {
    use crate::lab::tracker::MEASURE;
    let lab = Lab::new(&[Role::Writer]);
    let paths = &lab.paths[0];
    let sample = |value: &str| -> Map {
        json!({
            "metric": "a10-lab-objects",
            "value": value,
            "at": 1_800_000_000_000_u64,
            "source": "aterm db table ls",
        })
        .as_object()
        .unwrap()
        .clone()
    };
    Handle::at(paths.clone(), &TRACKER)
        .saying(said())
        .append_log(&MEASURE, &[sample("1")])
        .unwrap();
    Handle::at(paths.clone(), &TRACKER)
        .append_log(&MEASURE, &[sample("2")])
        .unwrap();
    let said_by: Vec<Option<Said>> = Handle::at(paths.clone(), &TRACKER)
        .history()
        .unwrap()
        .into_iter()
        .map(|entry| entry.said)
        .collect();
    assert_eq!(
        said_by,
        [Some(said()), None],
        "the append says who made it, and the other nothing"
    );
}
