// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bounded, offline resurrection audit over immutable changelog Git objects.

use super::{CHANGELOG_FILE, note_keys};
use crate::ledger::{Error, Result};

/// Hard bounds on the complete local audit. Exhaustion refuses the cut rather
/// than treating truncated history as evidence of absence.
const HISTORY_COMMITS: usize = 131072;
const HISTORY_SNAPSHOTS: usize = 16384;
const HISTORY_ENTRIES: usize = 256;
const HISTORY_EDGES: usize = 131072;
const HISTORY_ANCESTRY_QUERIES: usize = 4096;
const HISTORY_BYTES: u64 = 32 * 1024 * 1024 * 1024;
const SNAPSHOT_BYTES: usize = 8 * 1024 * 1024;
const BATCH_BYTES: usize = 32 * 1024 * 1024;

/// Audit every reachable changelog parent edge. Peers merge main into their
/// branch before landing, so the final first-parent spine can omit a rewrite.
/// Raw Git diffs name the exact before/after blobs. Each distinct blob is read
/// once, in bounded batches, and reduced to candidate-entry copy counts.
///
/// Count copies in every section (after a recut's roll, any but the next release's
/// Unreleased). Moving a copy preserves its count. Rewriting one still removes a
/// copy even when an older misplaced copy survives elsewhere in the file.
///
/// A merge that adds an entry against one parent resurrects it only when its copy
/// comes from a line that never saw the removal: a parent that holds those copies and
/// descends from the removal carries an author's later re-addition, which is the
/// entry's current text, not a stale one.
pub fn gate(
    git: &dyn crate::ledger::GitRunner,
    commit: &str,
    text: &str,
    section: &str,
    keys: &std::collections::BTreeSet<String>,
) -> Result<()> {
    let mut counter = super::NoteCounter::default();
    gate_with_keys(
        git,
        commit,
        text,
        section,
        keys,
        |oid, snapshot, section, candidates| counter.counts(oid, snapshot, section, candidates),
    )
}

/// Reuse decoded immutable snapshots when auditing many candidates from one
/// object database. Bounds are checked against the complete object listing even
/// when an already decoded immutable blob supplies its entry counts.
pub(crate) fn gate_with_keys(
    git: &dyn crate::ledger::GitRunner,
    commit: &str,
    text: &str,
    section: &str,
    keys: &std::collections::BTreeSet<String>,
    mut keys_of: impl FnMut(&str, Option<&str>, &str, &[String]) -> Option<Vec<super::NoteMembership>>,
) -> Result<()> {
    use std::collections::{BTreeMap, BTreeSet};

    if history_read(git, &["rev-parse", "--is-shallow-repository"])?
        .stdout_utf8()
        .trim()
        != "false"
    {
        return Err(Error::new(
            "changelog history: a shallow clone cannot prove no resurrection; fetch complete history",
        ));
    }
    let count = history_read(
        git,
        &[
            "rev-list",
            "--count",
            &format!("--max-count={}", HISTORY_COMMITS + 1),
            commit,
        ],
    )?
    .stdout_utf8();
    if count
        .trim()
        .parse::<usize>()
        .map_err(|_| Error::new("changelog history: invalid commit count"))?
        > HISTORY_COMMITS
    {
        return Err(Error::new(
            "changelog history: more than 131072 reachable commits; increase the audited bound before cutting",
        ));
    }
    let present: BTreeSet<_> = note_keys(text, section).into_iter().collect();
    let candidates: Vec<_> = keys.intersection(&present).cloned().collect();
    if candidates.len() > HISTORY_ENTRIES {
        return Err(Error::new(
            "changelog history: more than 256 entries; increase the audited bound before cutting",
        ));
    }
    let listing = history_read(
        git,
        &[
            "log",
            "--full-history",
            "--topo-order",
            "--diff-merges=separate",
            "--raw",
            "--no-abbrev",
            "--no-renames",
            "--format=%H %P",
            commit,
            "--",
            CHANGELOG_FILE,
        ],
    )?
    .stdout_utf8();
    if listing.len() > 64 * 1024 * 1024 {
        return Err(Error::new("changelog history: metadata exceeds 64 MiB"));
    }
    let mut edges = Vec::new();
    let mut blobs = BTreeSet::new();
    let mut parents: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    let mut at = "";
    let mut merge = false;
    for line in listing.lines().filter(|line| !line.is_empty()) {
        let fields: Vec<_> = line.split_whitespace().collect();
        if line.starts_with(':') {
            if fields.len() != 6 || fields[5] != CHANGELOG_FILE || at.is_empty() {
                return Err(Error::new("changelog history: invalid raw parent diff"));
            }
            let (before, after) = (fields[2], fields[3]);
            edges.push((at, merge, before, after));
            for oid in [before, after] {
                if !oid.bytes().all(|b| b == b'0') {
                    blobs.insert(oid);
                }
            }
        } else {
            at = fields[0];
            merge = fields.len() > 2;
            if merge {
                parents.insert(at, fields[1..].to_vec());
            }
        }
    }
    if edges.len() > HISTORY_EDGES || blobs.len() > HISTORY_SNAPSHOTS {
        return Err(Error::new(
            "changelog history: exceeds 131072 parent edges or 16384 distinct snapshots; increase the audited bound before cutting",
        ));
    }
    let mut membership = BTreeMap::new();
    if !blobs.is_empty() {
        let input = format!("{}\n", blobs.iter().copied().collect::<Vec<_>>().join("\n"));
        let sizes = history_input(git, &["cat-file", "--batch-check"], input.as_bytes())?;
        let sizes = String::from_utf8(sizes)
            .map_err(|_| Error::new("changelog history: invalid size listing"))?;
        let mut objects = Vec::new();
        let mut bytes = 0_u64;
        let rows: Vec<_> = sizes.lines().collect();
        if rows.len() != blobs.len() {
            return Err(Error::new("changelog history: incomplete size listing"));
        }
        for (oid, row) in blobs.iter().zip(rows) {
            let fields: Vec<_> = row.split_whitespace().collect();
            if fields.len() != 3 || fields[0] != *oid || fields[1] != "blob" {
                return Err(Error::new(format!(
                    "changelog history: missing or invalid blob {oid}"
                )));
            }
            let size = fields[2]
                .parse::<usize>()
                .map_err(|_| Error::new("changelog history: invalid blob size"))?;
            bytes += size as u64;
            if size > SNAPSHOT_BYTES || bytes > HISTORY_BYTES {
                return Err(Error::new(
                    "changelog history: exceeds 8 MiB per snapshot or 32 GiB total; increase the audited bound before cutting",
                ));
            }
            if let Some(held) = keys_of(oid, None, section, &candidates) {
                membership.insert(*oid, held);
            } else {
                objects.push((*oid, size));
            }
        }
        let mut cursor = 0;
        while cursor < objects.len() {
            let start = cursor;
            let mut bytes = 0;
            while cursor < objects.len() && bytes + objects[cursor].1 + 128 <= BATCH_BYTES {
                bytes += objects[cursor].1 + 128;
                cursor += 1;
            }
            let batch = &objects[start..cursor];
            let input = format!(
                "{}\n",
                batch
                    .iter()
                    .map(|(oid, _)| *oid)
                    .collect::<Vec<_>>()
                    .join("\n")
            );
            let output = history_input(git, &["cat-file", "--batch"], input.as_bytes())?;
            let mut rest = output.as_slice();
            for &(oid, size) in batch {
                let head = format!("{oid} blob {size}\n");
                rest = rest
                    .strip_prefix(head.as_bytes())
                    .ok_or_else(|| Error::new("changelog history: invalid batch header"))?;
                let body = rest
                    .get(..size)
                    .ok_or_else(|| Error::new("changelog history: incomplete batch body"))?;
                let snapshot = std::str::from_utf8(body)
                    .map_err(|_| Error::new("changelog history: non-UTF-8 snapshot"))?;
                // An entry leaves the notes only when it leaves the changelog: one a merge
                // misplaces under a release heading is still held, and a shipped copy merged
                // back is the release gate's repeated-entry refusal. A recut ships its
                // existing version section, so after the roll Unreleased belongs to the next
                // release and cannot witness this one's membership.
                membership.insert(
                    oid,
                    keys_of(oid, Some(snapshot), section, &candidates)
                        .ok_or_else(|| Error::new("changelog history: snapshot was not decoded"))?,
                );
                rest = rest
                    .get(size..)
                    .and_then(|tail| tail.strip_prefix(b"\n"))
                    .ok_or_else(|| Error::new("changelog history: invalid batch ending"))?;
            }
            if !rest.is_empty() {
                return Err(Error::new("changelog history: extra batch data"));
            }
        }
    }
    let absent = vec![super::NoteMembership::default(); candidates.len()];
    let held = |oid: &str| membership.get(oid).unwrap_or(&absent);
    // Each restoring merge's parents' snapshots. A parent equal to the merge has no raw
    // edge, so its blob is read by name; every such blob is one this audit already read.
    let restoring: BTreeSet<&str> = edges
        .iter()
        .filter(|(_, merge, before, after)| {
            *merge
                && held(before)
                    .iter()
                    .zip(held(after))
                    .any(|(b, a)| b.active == 0 && a.active > 0)
        })
        .map(|(at, ..)| *at)
        .collect();
    let mut holders: BTreeMap<&str, Vec<(&str, &Vec<super::NoteMembership>)>> = BTreeMap::new();
    if !restoring.is_empty() {
        let mut queried = Vec::new();
        for merge in &restoring {
            let listed = parents
                .get(merge)
                .ok_or_else(|| Error::new("changelog history: a merge's parents are unlisted"))?;
            queried.extend(listed.iter().map(|parent| (*merge, *parent)));
        }
        if queried.len() > HISTORY_EDGES {
            return Err(Error::new(
                "changelog history: exceeds 131072 restoring merge parents; increase the audited bound before cutting",
            ));
        }
        let input: String = queried
            .iter()
            .map(|(_, parent)| format!("{parent}:{CHANGELOG_FILE}\n"))
            .collect();
        let rows = history_input(git, &["cat-file", "--batch-check"], input.as_bytes())?;
        let rows = String::from_utf8(rows)
            .map_err(|_| Error::new("changelog history: invalid parent snapshot listing"))?;
        let rows: Vec<_> = rows.lines().collect();
        if rows.len() != queried.len() {
            return Err(Error::new(
                "changelog history: incomplete parent snapshot listing",
            ));
        }
        for (&(merge, parent), row) in queried.iter().zip(rows) {
            let snapshot = match row.split_whitespace().collect::<Vec<_>>().as_slice() {
                [_, "missing"] => &absent,
                [oid, "blob", _] => membership.get(*oid).ok_or_else(|| {
                    Error::new(format!(
                        "changelog history: merge {merge}'s parent {parent} has a snapshot outside the audited listing"
                    ))
                })?,
                _ => {
                    return Err(Error::new(
                        "changelog history: invalid parent snapshot listing",
                    ));
                }
            };
            holders.entry(merge).or_default().push((parent, snapshot));
        }
    }
    let mut restores: Vec<BTreeSet<&str>> = vec![BTreeSet::new(); candidates.len()];
    let mut ancestry_queries = 0;
    let mut descends = |ancestor: &str, of: &str| -> Result<bool> {
        ancestry_queries += 1;
        if ancestry_queries > HISTORY_ANCESTRY_QUERIES {
            return Err(Error::new(
                "changelog history: exceeds 4096 ancestry queries; increase the audited bound before cutting",
            ));
        }
        let ancestry = git.git_read(&history_args(&[
            "merge-base",
            "--is-ancestor",
            ancestor,
            of,
        ]))?;
        match ancestry.status {
            0 => Ok(true),
            1 => Ok(false),
            _ => Err(Error::new(format!(
                "changelog history: ancestry unreadable: {}",
                ancestry.stderr_utf8()
            ))),
        }
    };
    for (at, merge, before, after) in edges {
        let (before, after) = (held(before), held(after));
        for (idx, key) in candidates.iter().enumerate() {
            if merge && before[idx].active == 0 && after[idx].active > 0 {
                restores[idx].insert(at);
            }
            if before[idx].copies > after[idx].copies
                && ((before[idx].active > 0 && after[idx].active == 0) || after[idx].copies == 0)
            {
                for restored in &restores[idx] {
                    if !descends(at, restored)? {
                        continue;
                    }
                    let mut reauthored = false;
                    for (parent, snapshot) in holders.get(restored).map_or(&[][..], Vec::as_slice) {
                        if snapshot[idx].active > 0 && descends(at, parent)? {
                            reauthored = true;
                            break;
                        }
                    }
                    // Entry identity is presence in the selected window, not
                    // formatting multiplicity. Reducing two stale copies to
                    // one never re-authors the surviving obsolete entry.
                    if !reauthored {
                        if after[idx].anywhere > after[idx].copies {
                            return Err(Error::new(format!(
                                "{CHANGELOG_FILE}: [{section}] resurrects an entry reserved for [Unreleased] \
                                 by {at}, copied back by merge {restored}: {key}\nfix: keep this \
                                 unshipped change in [Unreleased] for the next release; retain \
                                 [{section}]'s corrected notes"
                            )));
                        }
                        return Err(Error::new(format!(
                            "{CHANGELOG_FILE}: [{section}] resurrects an entry removed or rewritten \
                             by {at}, restored by merge {restored}: {key}\nfix: reconcile the merge \
                             with that correction; retain the unshipped change using its current wording"
                        )));
                    }
                }
            }
        }
    }
    Ok(())
}

fn history_input(
    git: &dyn crate::ledger::GitRunner,
    args: &[&str],
    input: &[u8],
) -> Result<Vec<u8>> {
    let out = git.git_input(&history_args(args), input)?;
    if !out.success() {
        return Err(Error::new(format!(
            "changelog history: git {}: {}",
            args.join(" "),
            out.stderr_utf8()
        )));
    }
    Ok(out.stdout)
}

/// Local object reads never fetch a missing promisor object or honor replace refs.
fn history_args<'a>(args: &[&'a str]) -> Vec<&'a str> {
    ["--no-lazy-fetch", "--no-replace-objects"]
        .into_iter()
        .chain(args.iter().copied())
        .collect()
}

fn history_read(
    git: &dyn crate::ledger::GitRunner,
    args: &[&str],
) -> Result<crate::ledger::RunOut> {
    crate::ledger::git_read_ok(git, &history_args(args))
}
