// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The channel's pins as a target sees them, read through the client's own
//! `Channel::for_target` rather than a second reader of the grammar.

use std::collections::{BTreeMap, BTreeSet};

use atpkg::Channel;

use crate::error::{Error, Result};
use crate::triple::Triple;

/// The builds `triples` resolve together: per target, `pin` with that target's overlay
/// laid over it (`Channel::for_target`). A program two targets resolve to different builds
/// is refused, not collapsed: one build cannot carry rows for both.
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

/// Every `(program, build)` some client can resolve from this channel: `pin` and every
/// `pin_by_target` overlay. An overlay masks a `pin` build for its own target only, so
/// both stay live; whatever serves every target at once (the public mirror) walks this.
#[must_use]
pub fn union(channel: &Channel) -> BTreeSet<(String, u64)> {
    channel
        .pin
        .iter()
        .chain(channel.pin_by_target.values().flatten())
        .map(|(p, b)| (p.clone(), *b))
        .collect()
}
