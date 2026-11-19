// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The markers that prove a folder is build output, and the class a node's facts give
//! (docs/DESIGN-disk-map-2026-11-18.md §3.2, §6.1, §6.2).
//!
//! Every input is data the walker reads at the candidate's own root; nothing here
//! touches a disk. Showing and removing are two judgments: a class says what the map
//! draws, and the proof words say what software may act on.

use crate::{Class, Evidence, Part, Proof, Tally};

/// A build tool's marker, counted only at the candidate's own root.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Marker {
    /// Cargo's or targo's.
    Cargo,
    /// A name of Trust's bootstrap layout inside its git-ignored `build/`.
    TrustBootstrap,
    /// A dependency store beside its tracked, unmodified lockfile.
    DependencyStore,
    /// An out-of-source CMake or Ninja tree.
    CMake,
}

/// The names Cargo's and targo's own layouts leave at a build folder's root.
pub const CARGO_MARKER_NAMES: [&str; 3] = [".rustc_info.json", ".targo-receipts", ".aterm-used"];

/// What the walker read at a candidate's own root.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Root {
    /// The names directly inside the candidate.
    pub names: Vec<String>,
    /// The candidate's `CACHEDIR.TAG`, when it has one.
    pub cachedir_tag: Option<String>,
    /// The candidate's path below a Trust checkout's git-ignored `build/` (or the
    /// `build.noindex` its `build` link names), when that checkout tracks `x.py` and
    /// `src/bootstrap/`: `x86_64-apple-darwin/stage1`, `bootstrap`…
    pub trust_build_path: Option<String>,
    /// For a `node_modules`: whether the lockfile beside it is tracked and unmodified.
    pub lockfile_tracked_unmodified: bool,
    /// For a `node_modules`: whether any file in it was written after its own
    /// `.package-lock.json`.
    pub written_after_package_lock: bool,
    /// The candidate's own name.
    pub name: String,
    /// The candidate's absolute path.
    pub path: String,
    /// `CMAKE_HOME_DIRECTORY` from its `CMakeCache.txt`, when it has one.
    pub cmake_home: Option<String>,
}

/// Whether a `CACHEDIR.TAG` was written by Cargo or targo: the standard signature line,
/// and a comment that names its producer. A bare tag proves nothing: people place
/// them by hand to keep folders out of backups.
#[must_use]
pub fn cargo_cachedir_tag(tag: &str) -> bool {
    let mut lines = tag.lines();
    let signed = lines.next().map(str::trim) == Some("Signature: 8a477f597d28d172789f06886806bc55");
    signed
        && lines.any(|l| {
            let l = l.trim_start();
            l.starts_with('#') && (l.contains("created by cargo") || l.contains("created by targo"))
        })
}

/// Whether `rel` (a path below `build/`) is a name of Trust's bootstrap layout, a closed
/// list: `<triple>/stage*`, `<triple>/llvm`, `bootstrap`, `tmp`, `cache`.
#[must_use]
pub fn trust_bootstrap_name(rel: &str) -> bool {
    let parts: Vec<&str> = rel.split('/').collect();
    match parts.as_slice() {
        [one] => matches!(*one, "bootstrap" | "tmp" | "cache"),
        [triple, leaf] => is_triple(triple) && (leaf.starts_with("stage") || *leaf == "llvm"),
        _ => false,
    }
}

/// A target triple: three or four non-empty, dash-separated words.
fn is_triple(s: &str) -> bool {
    let words: Vec<&str> = s.split('-').collect();
    (3..=4).contains(&words.len())
        && words.iter().all(|w| {
            !w.is_empty()
                && w.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'.')
        })
}

/// The marker at a candidate's own root, if any. Only the root counts: a marker deeper
/// down proves its own folder, never this one (`data/vendor/x/target/CACHEDIR.TAG`
/// proves `target`, never `data`).
#[must_use]
pub fn marker(root: &Root) -> Option<Marker> {
    let has = |name: &str| root.names.iter().any(|n| n == name);
    if CARGO_MARKER_NAMES.iter().any(|n| has(n))
        || root.cachedir_tag.as_deref().is_some_and(cargo_cachedir_tag)
    {
        return Some(Marker::Cargo);
    }
    if root
        .trust_build_path
        .as_deref()
        .is_some_and(trust_bootstrap_name)
    {
        return Some(Marker::TrustBootstrap);
    }
    if root.name == "node_modules"
        && has(".package-lock.json")
        && root.lockfile_tracked_unmodified
        && !root.written_after_package_lock
    {
        return Some(Marker::DependencyStore);
    }
    if has("CMakeCache.txt")
        && root
            .cmake_home
            .as_deref()
            .is_some_and(|home| home.trim_end_matches('/') != root.path.trim_end_matches('/'))
    {
        return Some(Marker::CMake);
    }
    None
}

/// Where a candidate sits: a marker proves a folder only where git ignores it and tracks
/// nothing under it, or in a tool cache, or under a temp root.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Place {
    /// Git ignores the folder.
    pub git_ignored: bool,
    /// Git tracks a file under it.
    pub tracks_under: bool,
    /// It is inside a tool cache.
    pub in_tool_cache: bool,
    /// It is under a temp root.
    pub under_temp_root: bool,
}

impl Place {
    /// Whether a marker found here proves anything.
    #[must_use]
    pub const fn marker_counts(self) -> bool {
        (self.git_ignored && !self.tracks_under) || self.in_tool_cache || self.under_temp_root
    }
}

/// A rule saved for one folder (an AI's answer or a person's): it names a class and
/// never removes. Only `data` (with the source it re-fetches from), `costly` and
/// `system` can be named this way.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Rule {
    /// Re-fetchable data, from this source.
    Data {
        /// The listing the files' own hashes are read from.
        source: String,
    },
    /// Hand-made VM images.
    Costly,
    /// The system's own.
    System,
}

/// The facts the classifier reads about one leaf.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Facts {
    /// The survey has read it.
    pub read: bool,
    /// The part of a checkout it is, when it is one.
    pub part: Option<Part>,
    /// What its own root holds.
    pub root: Root,
    /// Where it sits.
    pub place: Place,
    /// It is a tool cache: a tag with a known producer or a known layout.
    pub tool_cache: bool,
    /// A saved rule bound to it.
    pub rule: Option<Rule>,
    /// Its bytes by kind and age.
    pub tally: Tally,
}

/// The class a leaf's facts give, and the evidence for it. In order: unread; a part of
/// a checkout (ignored bytes only under a marker); a marker where it counts; a tool
/// cache; a temp root (rule 3: a person's files there are temporary because of where
/// they were put); a saved rule; a person's text, documents and media; else unexplained.
#[must_use]
pub fn classify(facts: &Facts) -> (Class, Evidence) {
    let mut ev = Evidence::default();
    if !facts.read {
        return (Class::Unread, ev);
    }
    let found = marker(&facts.root);
    let proves = found.is_some() && facts.place.marker_counts();
    if let Some(m) = found {
        ev.facts.push(format!("{m:?} marker"));
        if !proves {
            ev.facts
                .push("git tracks a file under it, or it is not ignored".into());
        }
    } else if facts.root.cachedir_tag.is_some() {
        ev.facts.push("a bare CACHEDIR.TAG proves nothing".into());
    }
    if let Some(part) = &facts.part {
        if matches!(part, Part::Ignored(_)) {
            ev.facts.push("git-ignored".into());
        }
        let class = part.class(proves);
        if proves && class == Class::Build {
            ev.proofs.push(Proof::Marker);
        }
        return (class, ev);
    }
    if proves {
        ev.proofs.push(Proof::Marker);
        return (Class::Build, ev);
    }
    if facts.tool_cache {
        ev.proofs.push(Proof::ToolCache);
        return (Class::Cache, ev);
    }
    if facts.place.under_temp_root {
        ev.proofs.push(Proof::TempRoot);
        return (Class::Temp, ev);
    }
    match &facts.rule {
        Some(Rule::Data { source }) => {
            ev.facts.push(format!("re-fetchable from {source}"));
            return (Class::Data, ev);
        }
        Some(Rule::Costly) => return (Class::Costly, ev),
        Some(Rule::System) => return (Class::System, ev),
        None => {}
    }
    if facts.tally.personal() > 0 && facts.tally.personal() * 2 >= facts.tally.total() {
        return (Class::Personal, ev);
    }
    (Class::Unexplained, ev)
}
