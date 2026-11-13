// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! WHICH PROGRAMS HAVE A BUILD FOR A TARGET, decided from signed data this store holds.
//!
//! An index program serves a target when the build the verified index pins for that target
//! ([`crate::manifest::Channel::for_target`]) carries an `[[artifact]]` row for it in its
//! machine-signed `pkg-<program>-<build>.toml`, and, for a member of a coherence group, when every
//! other pinned member does too: the tuple installs together or not at all, as the set
//! completion's prescan judges it ([`crate::flow::group_missing_triple`]). A vendor row serves
//! where its vendor publishes a build the lane verifies ([`crate::vendor_direct::VendorSpec::serves`])
//! or the index pins an ALab build of it there ([`crate::alab::index_pin`]). The update pass reads
//! those manifests ([`learn`]) whether or not it may install anything, and keeps each verified
//! pair in the signed-artifact cache under the key prerequisites use
//! ([`crate::signed_artifact_cache::SignedArtifactKey::package`]); every reader here re-verifies
//! the cached pair under the index's roster before it believes a row, and no pass outcome or
//! `status.toml` row is consulted. A pair the store does not hold is [`Serving::Unknown`],
//! never a verdict.
//!
//! The index itself names no per-target availability: one would have to track every pin's
//! manifest at every cut, and a Linux builder merges a target's row onto a published build's
//! manifest without moving its pin (docs/LINUX-LOCAL-TOOLCHAIN.md), so [`learn`] reads again
//! any pinned manifest whose cached copy lacks this target's row.

use std::collections::{BTreeMap, BTreeSet};

use crate::flow::Fetcher;
use crate::manifest::PkgManifest;
use crate::sig::TrustedIndex;
use crate::signed_artifact_cache::{SignedArtifactCache, SignedArtifactKey};
use crate::store::Layout;

/// What the store's signed data says of one program on one target.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Serving {
    /// A build of it serves the target.
    Served,
    /// None does: the `unavailable on <target>: <hint>` state ([`crate::state::unavailable`]).
    Unserved(String),
    /// The store holds no verified manifest of the pinned build.
    Unknown,
}

/// The verdicts on one target for the programs one verified index names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Targets {
    triple: String,
    verdicts: BTreeMap<String, Serving>,
}

const UNKNOWN: Serving = Serving::Unknown;

impl Targets {
    /// No verdict on `triple`: every program [`Serving::Unknown`], so nothing is set aside.
    #[must_use]
    pub fn unread(triple: &str) -> Self {
        Self {
            triple: triple.to_string(),
            verdicts: BTreeMap::new(),
        }
    }

    /// The verdict on `triple` for every program `index` names, from `layout`'s cache.
    #[must_use]
    pub fn read(layout: &Layout, index: &TrustedIndex, triple: &str) -> Self {
        Self::read_with(layout, index, triple, &crate::source::package_source_id)
    }

    /// [`Self::read`] with the cache source of a program's manifests (`repo`, `program`) named
    /// by `source_of`, the way the fetcher that cached them names it
    /// ([`Fetcher::package_source_id`]).
    #[must_use]
    pub fn read_with(
        layout: &Layout,
        index: &TrustedIndex,
        triple: &str,
        source_of: &dyn Fn(&str, &str) -> Option<String>,
    ) -> Self {
        let held = held_in(layout, source_of);
        Self {
            triple: triple.to_string(),
            verdicts: index
                .programs
                .keys()
                .map(|program| (program.clone(), judge(index, program, triple, &held)))
                .collect(),
        }
    }

    /// Verdicts given outright, for a test that holds no signed cache.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn of_verdicts<'a>(
        triple: &str,
        verdicts: impl IntoIterator<Item = (&'a str, Serving)>,
    ) -> Self {
        Self {
            triple: triple.to_string(),
            verdicts: verdicts
                .into_iter()
                .map(|(program, verdict)| (program.to_string(), verdict))
                .collect(),
        }
    }

    /// The target these verdicts are about.
    #[must_use]
    pub fn triple(&self) -> &str {
        &self.triple
    }

    /// The verdict on `program`.
    #[must_use]
    pub fn of(&self, program: &str) -> &Serving {
        self.verdicts.get(program).unwrap_or(&UNKNOWN)
    }

    /// Why no build of `program` serves this target, when none does.
    #[must_use]
    pub fn unserved(&self, program: &str) -> Option<&str> {
        match self.of(program) {
            Serving::Unserved(state) => Some(state),
            Serving::Served | Serving::Unknown => None,
        }
    }
}

/// The verdict on `program` for this machine under the last index it verified, or
/// [`Serving::Unknown`] with no index cached.
#[must_use]
pub fn here(layout: &Layout, program: &str) -> Serving {
    crate::cli::cached_index(layout).map_or(Serving::Unknown, |index| {
        judge(
            &index,
            program,
            crate::cli::current_triple(),
            &held_in(layout, &crate::source::package_source_id),
        )
    })
}

/// Whether the store holds Trust build `build`'s signed pack for `triple` with tree root `tree`,
/// the one a `trust.lock` names: `None` when it does or nothing is known, else why no install
/// of that build can satisfy the lock here (no row for the target, or another tree).
#[must_use]
pub fn lock_unpublished(
    layout: &Layout,
    index: &TrustedIndex,
    build: u64,
    tree: &str,
    triple: &str,
) -> Option<String> {
    lock_unpublished_with(
        layout,
        index,
        build,
        tree,
        triple,
        &crate::source::package_source_id,
    )
}

/// [`lock_unpublished`] with the cache source named by `source_of` ([`Targets::read_with`]).
#[must_use]
pub fn lock_unpublished_with(
    layout: &Layout,
    index: &TrustedIndex,
    build: u64,
    tree: &str,
    triple: &str,
    source_of: &dyn Fn(&str, &str) -> Option<String>,
) -> Option<String> {
    let program = crate::trust_lock::PROGRAM;
    let row = index.program(program)?;
    let (raw, signature) = held_in(layout, source_of)(&row.repo, program, build)?;
    let pkg = verified(index, program, build, raw, &signature)?;
    match pkg.artifact_for(triple) {
        None => Some(crate::state::unavailable(
            triple,
            row.unavailable_hint.as_deref().unwrap_or_default(),
        )),
        Some(artifact) if artifact.tree_root != tree => Some(format!(
            "its {triple} pack is tree {}, not the tree the lock names",
            artifact.tree_root.get(..12).unwrap_or(&artifact.tree_root)
        )),
        Some(_) => None,
    }
}

/// What the store remedies a program with no build for this target with: a local build linked
/// in, a whole sysroot for a sysroot bundle (Trust's,
/// [`crate::trust_lock::LOCAL_SYSROOT_REMEDY`]).
#[must_use]
pub fn local_remedy(program: &str) -> String {
    if program == crate::trust_lock::PROGRAM {
        crate::trust_lock::LOCAL_SYSROOT_REMEDY.to_string()
    } else {
        format!(
            "a local build links in with `aterm pkg link {program} <checkout> <bin>…` \
             (docs/LINUX-LOCAL-TOOLCHAIN.md)"
        )
    }
}

/// THE UPDATE PASS'S READ: the pinned manifest on `triple` of each of `programs` (index programs;
/// a vendor row is decided without one) whose cached pair does not already say it serves there,
/// fetched, verified under `index` and kept in `layout`'s cache for every reader of [`Targets`].
/// A pair that does not arrive or does not verify leaves what the cache held.
pub(crate) fn learn(
    fetcher: &dyn Fetcher,
    layout: &Layout,
    index: &TrustedIndex,
    triple: &str,
    programs: &BTreeSet<String>,
) {
    let Some(channel) = index.channel_for(crate::config::CHANNEL, triple) else {
        return;
    };
    let source_of = |repo: &str, program: &str| fetcher.package_source_id(repo, program);
    let held = held_in(layout, &source_of);
    let ask: Vec<(&str, &str, u64, String)> = programs
        .iter()
        .filter(|program| crate::vendor_direct::spec(program).is_none())
        .filter_map(|program| {
            let repo = index.program(program)?.repo.as_str();
            let build = *channel.pin.get(program)?;
            Some((program.as_str(), repo, build, source_of(repo, program)?))
        })
        .filter(|(program, ..)| judge_alone(index, program, triple, &held) != Serving::Served)
        .collect();
    if ask.is_empty() {
        return;
    }
    crate::flow::prefetch_pinned(fetcher, index, &channel, ask.iter().map(|(p, ..)| *p));
    let cache = SignedArtifactCache::for_layout(layout);
    let (document_cap, signature_cap) = caps();
    for (program, repo, build, source) in ask {
        let Ok((raw, signature)) = fetcher.pkg_manifest(repo, program, build) else {
            continue;
        };
        if verified(index, program, build, raw.clone(), &signature).is_none() {
            continue;
        }
        let key = SignedArtifactKey::package(&source, repo, program, build);
        if let Err(error) =
            cache.store_verified(&key, &raw, &signature, document_cap, signature_cap)
        {
            eprintln!(
                "atpkg: the verified {program} {build} package document was not cached ({error})"
            );
        }
    }
}

/// The bounds a cached package pair is read and kept under: the fetch's own.
fn caps() -> (usize, usize) {
    let cap = |bound: u64| usize::try_from(bound).unwrap_or(usize::MAX);
    (cap(crate::net::MANIFEST_CAP), cap(crate::net::SIG_CAP))
}

type Held<'a> = dyn Fn(&str, &str, u64) -> Option<(Vec<u8>, Vec<u8>)> + 'a;

/// The cached pair of `(repo, program, build)` in `layout`, under the source `source_of` names.
fn held_in<'a>(
    layout: &'a Layout,
    source_of: &'a dyn Fn(&str, &str) -> Option<String>,
) -> impl Fn(&str, &str, u64) -> Option<(Vec<u8>, Vec<u8>)> + 'a {
    let cache = SignedArtifactCache::for_layout(layout);
    let (document_cap, signature_cap) = caps();
    move |repo, program, build| {
        let source = source_of(repo, program)?;
        cache.load(
            &SignedArtifactKey::package(&source, repo, program, build),
            document_cap,
            signature_cap,
        )
    }
}

/// THE RULE: `program` on `triple` under `index`, each pinned manifest read through `held`. A
/// member of a coherence group is unserved wherever another pinned member of its group is
/// ([`judge_alone`]), since the tuple installs together or not at all.
fn judge(index: &TrustedIndex, program: &str, triple: &str, held: &Held<'_>) -> Serving {
    let alone = judge_alone(index, program, triple, held);
    if matches!(alone, Serving::Unserved(_)) {
        return alone;
    }
    let group = index
        .program(program)
        .and_then(|row| row.coherence_group.as_deref());
    let partner = group.and_then(|group| {
        let channel = index.channel_for(crate::config::CHANNEL, triple)?;
        channel
            .pin
            .keys()
            .filter(|member| member.as_str() != program)
            .filter(|member| {
                index
                    .program(member)
                    .is_some_and(|row| row.coherence_group.as_deref() == Some(group))
            })
            .find(|member| {
                matches!(
                    judge_alone(index, member, triple, held),
                    Serving::Unserved(_)
                )
            })
            .cloned()
    });
    match partner {
        Some(partner) => Serving::Unserved(crate::state::unavailable(
            triple,
            &format!("it installs only together with {partner}, which has no build for it"),
        )),
        None => alone,
    }
}

/// `program` on `triple` by its own pinned manifest (or, for a vendor row, its vendor), its
/// coherence group unweighed.
fn judge_alone(index: &TrustedIndex, program: &str, triple: &str, held: &Held<'_>) -> Serving {
    if let Some(spec) = crate::vendor_direct::spec(program) {
        return if spec.serves(triple) || crate::alab::index_pin(index, program, triple).is_some() {
            Serving::Served
        } else {
            Serving::Unserved(crate::state::unavailable(
                triple,
                &format!("{} publishes no build for it", spec.vendor),
            ))
        };
    }
    let Some(row) = index.program(program) else {
        return Serving::Unknown;
    };
    let unserved = || {
        Serving::Unserved(crate::state::unavailable(
            triple,
            row.unavailable_hint.as_deref().unwrap_or_default(),
        ))
    };
    let Some(build) = index
        .channel_for(crate::config::CHANNEL, triple)
        .and_then(|channel| channel.pin.get(program).copied())
    else {
        return unserved();
    };
    match held(&row.repo, program, build)
        .and_then(|(raw, signature)| verified(index, program, build, raw, &signature))
    {
        Some(pkg) if pkg.artifact_for(triple).is_some() => Serving::Served,
        Some(_) => unserved(),
        None => Serving::Unknown,
    }
}

/// `raw` as `program`'s build `build`, iff `index`'s roster signed it and it says so itself.
fn verified(
    index: &TrustedIndex,
    program: &str,
    build: u64,
    raw: Vec<u8>,
    signature: &[u8],
) -> Option<PkgManifest> {
    let bytes = index.verify_pkg(raw, signature).ok()?;
    let pkg = crate::manifest::parse_pkg(&bytes).ok()?;
    (pkg.is_for(program) && pkg.build_number == build).then_some(pkg)
}

#[cfg(test)]
#[path = "serving_tests.rs"]
mod tests;
