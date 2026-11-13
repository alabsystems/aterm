// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Target-resolved channel pin queries.

use std::collections::{BTreeMap, BTreeSet};

use atpkg::Channel;

use crate::error::{Error, Result};
use crate::triple::Triple;

/// Resolves the channel's pins across `triples` via [`Channel::for_target`].
///
/// # Errors
///
/// A usage error for an empty target list; a refusal naming the divergent program.
pub fn for_targets(channel: &Channel, triples: &[Triple]) -> Result<BTreeMap<String, u64>> {
    if triples.is_empty() {
        return Err(Error::usage(
            "no target triple given — a lane must say whom it packs for",
        ));
    }
    let mut resolved: BTreeMap<String, (u64, Triple)> = BTreeMap::new();
    for t in triples {
        for (program, build) in channel.for_target(t.as_str()).pin {
            match resolved.get(&program) {
                Some((b, first)) if *b != build => {
                    return Err(Error::refused(format!(
                        "{program} resolves to build {b} on {first} but {build} on {t} — one \
                         build cannot carry rows for both; split the lane or drop the \
                         divergent overlay"
                    )));
                }
                Some(_) => {}
                None => {
                    resolved.insert(program, (build, *t));
                }
            }
        }
    }
    Ok(resolved.into_iter().map(|(p, (b, _))| (p, b)).collect())
}

/// Each program's build on each of `triples` via [`Channel::for_target`], kept apart: a
/// program an overlay pins ahead on one target is a normal channel, not a divergence. A
/// program appears under the triples that resolve it.
///
/// # Errors
///
/// A usage error for an empty target list.
pub fn by_target(
    channel: &Channel,
    triples: &[Triple],
) -> Result<BTreeMap<String, BTreeMap<Triple, u64>>> {
    if triples.is_empty() {
        return Err(Error::usage(
            "no target triple given — a lane must say whom it packs for",
        ));
    }
    let mut resolved: BTreeMap<String, BTreeMap<Triple, u64>> = BTreeMap::new();
    for t in triples {
        for (program, build) in channel.for_target(t.as_str()).pin {
            resolved.entry(program).or_default().insert(*t, build);
        }
    }
    Ok(resolved)
}

/// The lowest of each program's per-target builds ([`by_target`]): the target a newer build
/// still moves.
#[must_use]
pub fn lowest(by_target: &BTreeMap<String, BTreeMap<Triple, u64>>) -> BTreeMap<String, u64> {
    by_target
        .iter()
        .filter_map(|(p, cells)| Some((p.clone(), *cells.values().min()?)))
        .collect()
}

/// The floors a lane holds for per-target pins: `program` at its lowest build, and
/// `program@triple` at its build on each triple, so a lowered overlay is caught as surely as a
/// lowered `pin`.
#[must_use]
pub fn floors(by_target: &BTreeMap<String, BTreeMap<Triple, u64>>) -> BTreeMap<String, u64> {
    let mut out = lowest(by_target);
    for (p, cells) in by_target {
        for (t, b) in cells {
            out.insert(format!("{p}@{t}"), *b);
        }
    }
    out
}

/// Every `(program, build)` reachable from `pin` or any `pin_by_target` overlay.
#[must_use]
pub fn union(channel: &Channel) -> BTreeSet<(String, u64)> {
    channel
        .pin
        .iter()
        .chain(channel.pin_by_target.values().flatten())
        .map(|(p, b)| (p.clone(), *b))
        .collect()
}
