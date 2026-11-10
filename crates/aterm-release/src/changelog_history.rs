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
/// once, in bounded batches, and reduced to candidate-entry membership bits.
pub fn gate(
    git: &dyn crate::ledger::GitRunner,
    commit: &str,
    text: &str,
    section: &str,
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
    let candidates: Vec<_> = note_keys(text, section)
        .into_iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
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
            objects.push((*oid, size));
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
                let keys: BTreeSet<_> = note_keys(snapshot, "Unreleased").into_iter().collect();
                membership.insert(
                    oid,
                    candidates
                        .iter()
                        .map(|key| keys.contains(key))
                        .collect::<Vec<_>>(),
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
    let absent = vec![false; candidates.len()];
    let mut restores: Vec<BTreeSet<&str>> = vec![BTreeSet::new(); candidates.len()];
    let mut ancestry_queries = 0;
    for (at, merge, before, after) in edges {
        let before = membership.get(before).unwrap_or(&absent);
        let after = membership.get(after).unwrap_or(&absent);
        for (idx, key) in candidates.iter().enumerate() {
            if merge && after[idx] && !before[idx] {
                restores[idx].insert(at);
            }
            if before[idx] && !after[idx] {
                for restored in &restores[idx] {
                    ancestry_queries += 1;
                    if ancestry_queries > HISTORY_ANCESTRY_QUERIES {
                        return Err(Error::new(
                            "changelog history: exceeds 4096 ancestry queries; increase the audited bound before cutting",
                        ));
                    }
                    let ancestor = git.git_read(&history_args(&[
                        "merge-base",
                        "--is-ancestor",
                        at,
                        restored,
                    ]))?;
                    match ancestor.status {
                        0 => {
                            return Err(Error::new(format!(
                                "{CHANGELOG_FILE}: [{section}] resurrects an entry removed or rewritten \
                             by {at}, restored by merge {restored}: {key} — remove the stale entry"
                            )));
                        }
                        1 => {}
                        _ => {
                            return Err(Error::new(format!(
                                "changelog history: ancestry unreadable: {}",
                                ancestor.stderr_utf8()
                            )));
                        }
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
