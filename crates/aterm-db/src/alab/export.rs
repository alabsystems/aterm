// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The generated exports' one writer (docs/DESIGN-alab-lab-model-2026-11-01.md §9, step 11):
//! `aterm db alab export [--dry-run|--apply]`, an owner act on an admin Mac, and `check`'s
//! export drift.
//!
//! The harness renders the files older Macs still read (`fleet-accounts.toml`, each other
//! pooled CLI's `ai-accounts-<cli>.toml`, `ai-pool.toml`) from the registry and the journal's
//! place rows ([`Vendors::exports`], which only the `aterm` executable links); this module stamps
//! each with the generator's seal (`aterm_census::seal`), compares it with what the vault holds
//! and, with `--apply`, sets every changed entry the vault already holds (it never creates one:
//! [`plan`]) and every roster `hw_hint` the facts rows state (§4.7) in one admin publish. An unchanged lab publishes nothing (compare-before-write), so no
//! Mac's subscriber wakes for it. Nothing here runs on a timer: the exports change only when the
//! owner runs the verb.
//!
//! Drift (`check`): each file as this Mac's `~/.secrets` holds it against what the lab renders
//! now: `same`; `stale` (the generator wrote it and the lab moved since); `edited` (its seal no
//! longer holds: a hand edit); `unsealed` (no seal: never generated, or written by an older
//! tool); `missing`.

use super::import::Vendors;
use super::journal::place::Places;
use super::view::LabView;
use crate::typed::owned::OwnedRead;
use crate::{Error, Result};
use aterm_census::seal;
use aterm_json::{Value, json};
use aterm_secrets::{
    client::Paths,
    swarm_roster::{ROSTER_ENTRY, SwarmRoster},
    text::{Audience, Document},
};
use std::collections::BTreeMap;

/// The command each export's seal names.
pub const GENERATOR: &str = "aterm db alab export --apply";

/// `body` as the writer publishes it: the generator's seal line first, then the body, stamped.
///
/// # Errors
/// Never in practice: the line it stamps is the one it writes.
pub fn sealed(body: &str) -> Result<String> {
    let text = format!("{}\n{body}", seal::line("#", GENERATOR));
    seal::stamp(&text).map_err(|why| Error::new(&why))
}

/// What a file holds against what the lab renders now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Drift {
    Same,
    Stale,
    Edited,
    Unsealed,
    Missing,
}

impl Drift {
    /// The word `check` says.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Same => "same",
            Self::Stale => "stale",
            Self::Edited => "edited",
            Self::Unsealed => "unsealed",
            Self::Missing => "missing",
        }
    }
}

/// How `held` (the file as it is, `None` where it is not there) stands against `rendered`.
#[must_use]
pub fn drift(held: Option<&str>, rendered: &str) -> Drift {
    match held {
        None => Drift::Missing,
        Some(held) if held == rendered => Drift::Same,
        Some(held) => match seal::check(held) {
            seal::Seal::Absent => Drift::Unsealed,
            seal::Seal::Broken { .. } => Drift::Edited,
            seal::Seal::Holds { .. } => Drift::Stale,
        },
    }
}

/// Every export of the registry `view` and the journal's `places`, sealed, by name, given
/// `current` (each file as it is now).
///
/// # Errors
/// The harness refuses the render (a current file does not parse).
pub fn rendered(
    vendors: &dyn Vendors,
    view: &LabView,
    places: &Places,
    current: &BTreeMap<String, String>,
) -> Result<Vec<(String, String)>> {
    vendors
        .exports(view, places, current)
        .map_err(|why| Error::new(&format!("the exports cannot be rendered: {why}")))?
        .into_iter()
        .map(|(name, body)| Ok((name, sealed(&body)?)))
        .collect()
}

/// Each of `names` as `~/.secrets` under `home` holds it, where it is there and is UTF-8.
///
/// # Errors
/// A file is there and cannot be read.
pub fn materialized(home: &std::path::Path, names: &[String]) -> Result<BTreeMap<String, String>> {
    let dir = home.join(".secrets");
    let mut out = BTreeMap::new();
    for name in names {
        let path = dir.join(name);
        if !path.is_file() {
            continue;
        }
        let bytes = crate::import::read_bounded(&path)?;
        if let Ok(text) = String::from_utf8(bytes) {
            out.insert(name.clone(), text);
        }
    }
    Ok(out)
}

/// Each of `names` as the vault text `doc` holds it, in whichever section holds it.
#[must_use]
pub fn in_doc(doc: &Document, names: &[String]) -> BTreeMap<String, String> {
    doc.entries()
        .into_iter()
        .filter(|entry| names.contains(&entry.name))
        .filter_map(|entry| Some((entry.name.clone(), entry.value.as_str()?.to_owned())))
        .collect()
}

/// Give each member of `roster` the `hw_hint` the facts rows state for it (design §4.7, step 7:
/// the roster's own field, written by this owner verb and never by a registration pass): the
/// facts rows that state the member's fleet node, when they all state one hint. A machine label
/// is a name, not an identity (a retired Mac's row and a new Mac may state the same one), so a
/// member with no node id, and a facts row stating none, bind nothing; nor does a row merged into
/// another machine. Several hints for one node are a clone or a reinstall for the owner to judge,
/// and a member no facts row binds keeps what it has. Returns the labels whose hint changed.
pub fn hints(roster: &mut SwarmRoster, facts: &OwnedRead) -> Vec<String> {
    let mut changed = Vec::new();
    for member in &mut roster.members {
        let Some(mine) = member.node_id.as_deref() else {
            continue;
        };
        let mut found: Vec<&str> = facts
            .rows
            .values()
            .filter(|row| {
                let value = &row.value;
                value.get("node").and_then(Value::as_str) == Some(mine)
                    && value.get("merged_into").is_none_or(Value::is_null)
            })
            .filter_map(|row| row.value.get("hw_hint").and_then(Value::as_str))
            .filter(|hint| aterm_codec::hex::is_lower(hint, 16))
            .collect();
        found.sort_unstable();
        found.dedup();
        if let [hint] = found[..]
            && member.hw_hint.as_deref() != Some(hint)
        {
            member.hw_hint = Some(hint.to_owned());
            changed.push(member.label.clone());
        }
    }
    changed
}

/// The section entry `name` is written in: the one that holds it ([`plan`] writes only files the
/// vault holds), else `[everyone]`, where every Mac's subscriber reads these files today.
fn section(doc: &Document, name: &str) -> Audience {
    doc.entries()
        .into_iter()
        .find(|entry| entry.name == name)
        .map_or(Audience::Everyone, |entry| entry.audience)
}

/// What one `export` would write: the files whose bytes differ, and the roster with its hints.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Planned {
    /// `(name, text)` of each file whose text differs from what is held.
    pub files: Vec<(String, String)>,
    /// Every file's drift, by name.
    pub drift: Vec<(String, Drift)>,
    /// The roster with its new hints, when any changed, and the labels that changed.
    pub roster: Option<(SwarmRoster, Vec<String>)>,
}

/// Plan an export of `rendered` over `current`, and of `facts`' hints over `roster`. Pure. Only a
/// file `current` holds is written: an older Mac reads the first of these files it finds (the pool
/// ahead of the rest), so a file created here would switch its source to one that never carried
/// what its old source gave it. A missing file is reported and left missing.
#[must_use]
pub fn plan(
    rendered: &[(String, String)],
    current: &BTreeMap<String, String>,
    roster: Option<&SwarmRoster>,
    facts: &OwnedRead,
) -> Planned {
    let drift: Vec<(String, Drift)> = rendered
        .iter()
        .map(|(name, text)| {
            (
                name.clone(),
                drift(current.get(name).map(String::as_str), text),
            )
        })
        .collect();
    let files = rendered
        .iter()
        .filter(|(name, text)| current.get(name).is_some_and(|held| held != text))
        .cloned()
        .collect();
    let roster = roster.and_then(|roster| {
        let mut roster = roster.clone();
        let changed = hints(&mut roster, facts);
        (!changed.is_empty()).then_some((roster, changed))
    });
    Planned {
        files,
        drift,
        roster,
    }
}

/// `doc` with `planned`'s changes set: each changed file in the section that holds it, and the
/// roster entry.
///
/// # Errors
/// The vault text refuses an entry.
pub fn apply_to(doc: &mut Document, planned: &Planned) -> Result<()> {
    let refused = |why: aterm_secrets::text::TextError| Error::new(&format!("vault: {why}"));
    for (name, text) in &planned.files {
        let audience = section(doc, name);
        doc.set_entry(&audience, name, text.as_bytes())
            .map_err(refused)?;
    }
    if let Some((roster, _)) = &planned.roster {
        doc.set_entry(
            &Audience::Everyone,
            ROSTER_ENTRY,
            roster.to_toml().as_bytes(),
        )
        .map_err(refused)?;
    }
    Ok(())
}

/// The registry view, this Mac's projection of it and its facts rows, for an export or a drift
/// check; `None` where the registry is not held here or its history is not complete.
///
/// # Errors
/// The registry cannot be read.
fn lab_now(paths: &Paths) -> Result<Option<(LabView, OwnedRead)>> {
    let view = match super::Registry::at(paths.clone()).view() {
        Ok(view) => view,
        Err(_) => return Ok(None),
    };
    if !view.complete {
        return Ok(None);
    }
    let owned = |name: &str| view.owned.get(name).cloned().unwrap_or_default();
    let facts = owned(super::registry::MACHINE_FACTS.name);
    let lab = LabView::read(
        &view.cells,
        &facts,
        &owned(super::registry::MAILBOX_OBS.name),
        None,
    );
    Ok(Some((lab, facts)))
}

/// `check`'s `exports`: each file's drift against `~/.secrets`, `null` where no harness renders
/// or the registry is not held whole here, an `error` where the render is refused.
pub(super) fn check(paths: &Paths, vendors: Option<&dyn Vendors>, now: u64) -> Value {
    let Some(vendors) = vendors else {
        return Value::Null;
    };
    let Ok(Some((lab, _))) = lab_now(paths) else {
        return Value::Null;
    };
    let places = super::journal::place::read(paths, now)
        .ok()
        .flatten()
        .unwrap_or_default();
    let names = vendors.export_names();
    let report = (|| {
        let current = materialized(&paths.home, &names)?;
        let rendered = rendered(vendors, &lab, &places, &current)?;
        Ok::<_, Error>(plan(&rendered, &current, None, &OwnedRead::default()).drift)
    })();
    match report {
        Ok(drift) => Value::Array(
            drift
                .into_iter()
                .map(|(file, drift)| json!({"file": file, "state": drift.word()}))
                .collect(),
        ),
        Err(why) => json!({"error": why.to_string()}),
    }
}

/// `aterm db alab export [--dry-run|--apply]` at `now` (Unix seconds): render the exports from the
/// registry and the journal's place rows and, with `apply`, publish every changed file and roster
/// hint in one admin publish of the vault. A dry run compares with `~/.secrets` and writes
/// nothing. The report names files and labels, never a value.
///
/// # Errors
/// No harness renders, the registry is not held whole here, a current file does not parse, or
/// (applying) this Mac is not a vault admin or the publish fails.
pub(super) fn run(
    paths: &Paths,
    vendors: Option<&dyn Vendors>,
    apply: bool,
    now: u64,
) -> Result<Value> {
    let vendors = vendors.ok_or_else(|| {
        Error::new("the exports are rendered the harness's way; run it as `aterm db alab export`")
    })?;
    let (lab, facts) = lab_now(paths)?.ok_or_else(|| {
        Error::new(
            "this Mac does not hold the whole registry yet (a peer's repair round brings it), so \
             nothing was exported",
        )
    })?;
    let places = super::journal::place::read(paths, now)?.unwrap_or_default();
    let names = vendors.export_names();
    let admin = if apply {
        Some(aterm_secrets::client::admin_open(paths).map_err(crate::table::failure)?)
    } else {
        None
    };
    let (current, roster) = match &admin {
        Some(admin) => (
            in_doc(&admin.doc, &names),
            admin
                .doc
                .entry(&Audience::Everyone, ROSTER_ENTRY)
                .and_then(|entry| entry.value.as_str())
                .map(SwarmRoster::from_toml)
                .transpose()
                .map_err(crate::table::failure)?,
        ),
        None => (
            materialized(&paths.home, &names)?,
            aterm_secrets::client::swarm_roster(paths).ok().flatten(),
        ),
    };
    let rendered = rendered(vendors, &lab, &places, &current)?;
    let planned = plan(&rendered, &current, roster.as_ref(), &facts);
    let published = match admin {
        Some(admin) if !planned.files.is_empty() || planned.roster.is_some() => {
            let mut doc = admin.doc.clone();
            apply_to(&mut doc, &planned)?;
            aterm_secrets::client::admin_publish(paths, &admin, &doc)
                .map_err(crate::table::failure)?;
            true
        }
        _ => false,
    };
    Ok(json!({
        "apply": apply,
        "published": published,
        "files": planned.drift.iter().map(|(file, drift)| json!({
            "file": file,
            "state": drift.word(),
            "write": planned.files.iter().any(|(name, _)| name == file),
        })).collect::<Vec<_>>(),
        "hw_hints": planned.roster.as_ref().map_or_else(Vec::new, |(_, labels)| labels.clone()),
    }))
}

#[cfg(test)]
mod tests;
