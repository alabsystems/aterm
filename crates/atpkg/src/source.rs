// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! The configured package source and durable trust floors, shared by CLI
//! operations and leased build-prerequisite admission.

pub(crate) fn github_fetcher(
    layout: &crate::store::Layout,
    published: Option<u64>,
) -> crate::GithubFetcher {
    let cfg = crate::config::cached();
    let owner = crate::resolve_account(cfg.account()).owner;
    let floor = crate::index_probe::verified_floor(layout);
    let hint = discovery_hint(
        floor,
        || crate::index_probe::cached_index_builds(layout, &crate::net::github_source_id(&owner)),
        (owner == aterm_update_core::ATPKG_INDEX_OWNER)
            .then(crate::net::stamped_index_hint)
            .flatten(),
        published,
    );
    crate::GithubFetcher::new(owner)
        .with_overrides(crate::repo_overrides(cfg))
        .with_index_floor(floor)
        .with_index_hint(hint)
        .with_unit_peers(crate::peers::unit_peers(cfg.peers.as_deref()))
}

pub(crate) fn discovery_hint(
    floor: u64,
    cached: impl FnOnce() -> Option<Vec<u64>>,
    stamped: Option<crate::net::IndexHint>,
    published: Option<u64>,
) -> Option<crate::net::IndexHint> {
    let fresh = floor == 0;
    let cached = fresh
        .then(cached)
        .flatten()
        .and_then(|b| b.into_iter().max());
    let stamped = stamped.filter(|_| fresh);
    let newest = [cached, stamped.map(|c| c.newest), published]
        .into_iter()
        .flatten()
        .max()?;
    Some(crate::net::IndexHint {
        low: stamped.and_then(|c| c.low),
        newest,
    })
}

/// The existing development registry seam changes transport, never signature policy.
pub(crate) const REGISTRY_SEAM: &str = "ATPKG_REGISTRY";

pub(crate) fn registry_seam() -> Option<std::ffi::OsString> {
    aterm_types::dev_seam!(REGISTRY_SEAM)
}

pub(crate) fn dir_registry(spec: Option<&std::ffi::OsStr>) -> Option<std::path::PathBuf> {
    spec?
        .to_string_lossy()
        .strip_prefix("dir:")
        .map(std::path::PathBuf::from)
}

pub(crate) fn resolve_fetcher(
    layout: &crate::store::Layout,
    published: Option<u64>,
) -> Box<dyn crate::flow::Fetcher> {
    if let Some(dir) = dir_registry(registry_seam().as_deref()) {
        return Box::new(crate::DirFetcher::new(dir));
    }
    Box::new(github_fetcher(layout, published))
}

pub(crate) fn effective_anchor(layout: &crate::store::Layout) -> crate::Anchor {
    crate::Anchor::pinned(crate::sig::Floor::new(layout.roster_floor()).current())
}

pub(crate) fn build_floor(layout: &crate::store::Layout) -> crate::sig::BuildFloor {
    crate::sig::BuildFloor {
        index_build: crate::sig::Floor::new(layout.floor()).current(),
        roster_seq: crate::sig::Floor::new(layout.floor_generation()).current(),
    }
}

pub(crate) fn advance_floors(layout: &crate::store::Layout, index_build: u64, roster_seq: u64) {
    let generation = crate::sig::Floor::new(layout.floor_generation());
    let build = crate::sig::Floor::new(layout.floor());
    if roster_seq > generation.current() {
        build.rebase(index_build);
    } else {
        let _ = build.check_and_record(index_build);
    }
    let _ = generation.check_and_record(roster_seq);
    crate::flow::observe_roster_generation(layout, roster_seq);
}
