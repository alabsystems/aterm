// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Fill this invocation's raw network memos before it owns the package store.

use crate::config::PackagesConfig;
use crate::flow::Fetcher;
use crate::sig::{Anchor, BuildFloor};
use crate::store::Layout;

/// Preparation grants no installation authority. The ordinary locked pass must
/// read its current policy and installed state, then verify these same raw bytes
/// against its current clock, trust sources and durable floors before effects.
pub(super) fn warm(
    layout: &Layout,
    cfg: &PackagesConfig,
    fetcher: &dyn Fetcher,
    anchor: &Anchor,
    floor: BuildFloor,
    now: i64,
) {
    let installed = crate::active_builds(layout);
    let complete_the_set = super::completes_the_set(layout, cfg);
    if installed.is_empty() && !complete_the_set {
        return;
    }
    let Ok(candidates) = fetcher.index_candidates() else {
        return;
    };
    // Derive package origins only from a cryptographically and structurally
    // admitted index. This pure selection publishes none of its observations.
    let record = crate::succession::Record::read(layout);
    let selection = crate::select::select_index(
        anchor,
        candidates,
        floor,
        crate::succession::Record::sources_of(record.as_ref()),
        now,
    );
    let Some(selected) = selection.selected else {
        return;
    };
    if complete_the_set && crate::flow::index_is_fresh(&selected.index, now) {
        super::prefetch_set_manifests(layout, fetcher, cfg, &installed, &selected.index);
    }
}
