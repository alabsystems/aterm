// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Content identities for reviewing a proposed login configuration. These are
//! untrusted proposal fingerprints, not activation certificates or secret pins.

use super::{proposals::Snapshot, references};
use crate::{Error, Result};
use aterm_codec::hex;
use aterm_digest::Sha256;
use aterm_json::{Value, json};
use std::collections::BTreeSet;

const MAX_OBJECTS: usize = 256;
const MAX_BYTES: usize = 256 * 1024;
type Key = (String, String);

/// Review one local proposal closure. Unknown offline edits remain unknowable;
/// known missing, conflicted or withheld data never produces a fingerprint.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbAccountPlan",
        action = "Clean",
        project = "aterm_db::accounts::plans::tests::plan_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbAccountPlan",
        action = "Label",
        project = "aterm_db::accounts::plans::tests::plan_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbAccountPlan",
        action = "Dependency",
        project = "aterm_db::accounts::plans::tests::plan_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbAccountPlan",
        action = "Alias",
        project = "aterm_db::accounts::plans::tests::plan_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbAccountPlan",
        action = "Auth",
        project = "aterm_db::accounts::plans::tests::plan_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbAccountPlan",
        action = "Source",
        project = "aterm_db::accounts::plans::tests::plan_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbAccountPlan",
        action = "Rewrite",
        project = "aterm_db::accounts::plans::tests::plan_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbAccountPlan",
        action = "Unrelated",
        project = "aterm_db::accounts::plans::tests::plan_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbAccountPlan",
        action = "Conflict",
        project = "aterm_db::accounts::plans::tests::plan_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbAccountPlan",
        action = "Missing",
        project = "aterm_db::accounts::plans::tests::plan_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbAccountPlan",
        action = "History",
        project = "aterm_db::accounts::plans::tests::plan_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbAccountPlan",
        action = "Limit",
        project = "aterm_db::accounts::plans::tests::plan_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbAccountPlan",
        action = "Scope",
        project = "aterm_db::accounts::plans::tests::plan_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbAccountPlan",
        action = "Cycle",
        project = "aterm_db::accounts::plans::tests::plan_transition"
    )
)]
pub(super) fn inspect(snapshot: &Snapshot, account: &str) -> Result<Value> {
    if !crate::crypto::name(account) {
        return Err(Error::new("invalid account proposal key"));
    }
    let root = ("accounts".into(), account.into());
    let (report, links) = references::inspect_with_links(snapshot);
    let mut keys = BTreeSet::from([root.clone()]);
    let mut pending = vec![root.clone()];
    let mut blockers = BTreeSet::new();
    while let Some(key) = pending.pop() {
        for target in links.get(&key).into_iter().flatten() {
            if keys.contains(target) {
                continue;
            }
            if keys.len() == MAX_OBJECTS {
                blockers.insert("dependency-limit");
                break;
            }
            keys.insert(target.clone());
            pending.push(target.clone());
        }
        if !blockers.is_empty() {
            break;
        }
    }
    if report["truncated"].as_bool() != Some(false) {
        blockers.insert("inspection-limit");
    }
    if snapshot.withheld > 0
        || snapshot
            .operations
            .iter()
            .any(|op| !op.available || op.refusal.is_some())
    {
        blockers.insert("incomplete-observed-history");
    }
    let issues: Vec<_> = report["issues"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|issue| {
            issue["collection"]
                .as_str()
                .zip(issue["key"].as_str())
                .is_some_and(|(collection, key)| keys.contains(&(collection.into(), key.into())))
        })
        .cloned()
        .collect();
    if !issues.is_empty() {
        blockers.insert("invalid-dependency");
    }
    // Also cover an absent root (which a whole-table inspection cannot visit).
    if keys.iter().any(|key| {
        snapshot.cells.get(key).is_none_or(|cell| {
            cell.stopped || cell.candidates.len() != 1 || cell.candidates[0].value.is_null()
        })
    }) {
        blockers.insert("unresolved-dependency");
    }
    let digests = if blockers.is_empty() {
        match fingerprints(snapshot, &root, &keys)? {
            Some(digests) => Some(digests),
            None => {
                blockers.insert("byte-limit");
                None
            }
        }
    } else {
        None
    };
    let dependencies: Vec<_> = keys
        .iter()
        .map(|key| {
            json!({
                "collection":key.0,"key":key.1,
                "evidence":references::evidence(snapshot.cells.get(key))
            })
        })
        .collect();
    Ok(json!({
        "schema":1,"kind":"account-proposal-plan","scope":"observed-local-proposals",
        "authority":"none","account":account,"table":hex::encode_lower(&snapshot.scope.digest()),
        "complete":digests.is_some(),
        "proposal_digest":digests.as_ref().map(|d|hex::encode_lower(&d.0)),
        "configuration_digest":digests.as_ref().map(|d|hex::encode_lower(&d.1)),
        "dependencies":dependencies,"issues":issues,"blockers":blockers.into_iter().collect::<Vec<_>>(),
        "observed_history":report["observed_history"].clone()
    }))
}

fn frame(hash: &mut Sha256, bytes: &[u8]) {
    hash.update((bytes.len() as u64).to_be_bytes());
    hash.update(bytes);
}

fn fingerprints(
    snapshot: &Snapshot,
    root: &Key,
    keys: &BTreeSet<Key>,
) -> Result<Option<([u8; 32], [u8; 32])>> {
    let mut proposal = Sha256::new();
    proposal.update(b"aterm/account-proposal-plan/1\0");
    let mut configuration = Sha256::new();
    configuration.update(b"aterm/account-proposal-configuration/1\0");
    for hash in [&mut proposal, &mut configuration] {
        hash.update(snapshot.scope.digest());
        frame(hash, root.1.as_bytes());
    }
    let mut total = 0_usize;
    for key in keys {
        let candidate = &snapshot.cells[key].candidates[0];
        let bytes = aterm_json::to_vec(&candidate.value)
            .map_err(|_| Error::new("account proposal cannot be encoded"))?;
        total = total.saturating_add(bytes.len());
        if total > MAX_BYTES {
            return Ok(None);
        }
        for hash in [&mut proposal, &mut configuration] {
            frame(hash, key.0.as_bytes());
            frame(hash, key.1.as_bytes());
        }
        proposal.update(candidate.operation);
        frame(&mut proposal, &bytes);
        // Only the selected account's display label is known to be inert here.
        // Keep every other field, including source details and machine routing,
        // until a provider adapter defines a narrower authenticated schema.
        let mut value = candidate.value.clone();
        if key == root {
            value.as_object_mut().unwrap().remove("label");
        }
        let bytes = aterm_json::to_vec(&value)
            .map_err(|_| Error::new("account configuration cannot be encoded"))?;
        frame(&mut configuration, &bytes);
    }
    Ok(Some((proposal.finalize(), configuration.finalize())))
}

#[cfg(test)]
mod tests;
