// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Pack manifest row queries and local/remote manifest loading.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use atpkg::PkgManifest;

use crate::api::Build;
use crate::artifact::pkgdoc::{manifest_name, sig_name};
use crate::artifact::upload::tag;
use crate::error::{Error, Result};
use crate::host::{HostError, HostRef, Probe, Slug};
use crate::quad::VerifiedQuad;
use crate::triple::Triple;

/// The client targets `m` carries rows for (a row naming no client target is ignored).
#[must_use]
pub fn targets(m: &PkgManifest) -> BTreeSet<Triple> {
    m.artifacts
        .iter()
        .filter_map(|a| Triple::parse(&a.target).ok())
        .collect()
}

/// The targets whose rows record a compiler admitted with no lineage receipt.
#[must_use]
pub fn unproven(m: &PkgManifest) -> BTreeSet<Triple> {
    m.artifacts
        .iter()
        .filter(|a| {
            a.provenance
                .as_ref()
                .is_some_and(|p| p.lineage == "unproven")
        })
        .filter_map(|a| Triple::parse(&a.target).ok())
        .collect()
}

/// A manifest this lane staged (unsigned, on local disk), read into the client's type.
///
/// # Errors
/// A refusal when it cannot be read or does not parse.
pub fn local(path: &Path) -> Result<PkgManifest> {
    let text = crate::identity::read_regular_text(path, crate::identity::MAX_META_BYTES)
        .map_err(|e| Error::io(format!("read {}", path.display()), &e))?;
    aterm_toml::from_str(&text).map_err(|e| {
        Error::refused(format!(
            "{} is not a manifest the client parses: {e}",
            path.display()
        ))
    })
}

/// A published manifest, fetched.
#[derive(Debug)]
pub enum Fetched {
    /// It verified: its rows, and where its bytes and signature landed.
    Present {
        /// The client's reading of it.
        manifest: PkgManifest,
        /// The downloaded manifest.
        path: PathBuf,
    },
    /// The host answered that there is no such release or asset.
    Absent(String),
    /// The host could not be read: nothing is known (never "absent").
    Unreachable(String),
    /// It is there and the client's verifier refuses it.
    Invalid(String),
}

/// Fetch a published package without requesting a draft-inclusive writer read.
/// The package must still verify under the supplied fresh quad and exact program/build.
#[must_use]
pub fn fetch_published(
    host: &HostRef,
    slug: &Slug,
    program: &str,
    build: Build,
    dir: &Path,
    quad: &VerifiedQuad,
) -> Fetched {
    fetch_with(host, slug, program, build, dir, quad, Visibility::Published)
}

#[derive(Clone, Copy)]
enum Visibility {
    Drafts,
    Published,
}

/// Downloads a draft-inclusive publisher view of the package and its signature, then verifies
/// it. Public-only readers use [`fetch_published`] without requiring the writer identity.
#[must_use]
pub fn fetch(
    host: &HostRef,
    slug: &Slug,
    program: &str,
    build: Build,
    dir: &Path,
    quad: &VerifiedQuad,
) -> Fetched {
    fetch_with(host, slug, program, build, dir, quad, Visibility::Drafts)
}

fn fetch_with(
    host: &HostRef,
    slug: &Slug,
    program: &str,
    build: Build,
    dir: &Path,
    quad: &VerifiedQuad,
    visibility: Visibility,
) -> Fetched {
    let tag = tag(program, build);
    let (pkg, sig) = (manifest_name(program, build), sig_name(program, build));
    if matches!(visibility, Visibility::Drafts) {
        match host.release(slug, &tag) {
            Ok(_) => {}
            Err(HostError::NotFound) => {
                return Fetched::Absent(format!("{slug} has no release {tag}"));
            }
            Err(e) => return Fetched::Unreachable(format!("{slug} {tag}: {e}")),
        }
    }
    let mut last = String::new();
    let mut fetched = false;
    for _ in 0..3 {
        let download = match visibility {
            Visibility::Drafts => host.download(slug, &tag, &[pkg.as_str(), sig.as_str()], dir),
            // The published asset transport proves visibility itself; a tag-existence read
            // would add a round trip without contributing package signature authority.
            Visibility::Published => download_public(host, slug, &tag, &pkg, &sig, dir),
        };
        match download {
            Ok(()) => {
                fetched = true;
                break;
            }
            Err(HostError::NotFound) => {
                return Fetched::Absent(format!("{slug} {tag} carries no {pkg} with its .sig"));
            }
            Err(e) => last = e.to_string(),
        }
    }
    if !fetched {
        return Fetched::Unreachable(format!("{slug} {tag}: {last}"));
    }
    let path = dir.join(&pkg);
    let (Ok(bytes), Ok(signature)) = (
        crate::identity::read_regular_bytes(&path, crate::identity::MAX_META_BYTES),
        crate::identity::read_regular_bytes(&dir.join(&sig), crate::identity::MAX_META_BYTES),
    ) else {
        return Fetched::Unreachable(format!("{pkg} did not land in {}", dir.display()));
    };
    match quad.verify_pkg(&bytes, &signature) {
        Ok(manifest) if manifest.program == program && manifest.build_number == build => {
            Fetched::Present { manifest, path }
        }
        Ok(manifest) => Fetched::Invalid(format!(
            "{pkg} on {slug} names {} build {}, not {program} build {build}",
            manifest.program, manifest.build_number
        )),
        Err(e) => Fetched::Invalid(format!("{pkg} on {slug}: {e}")),
    }
}

fn download_public(
    host: &HostRef,
    slug: &Slug,
    tag: &str,
    package: &str,
    signature: &str,
    dir: &Path,
) -> std::result::Result<(), HostError> {
    for (name, cap) in [
        (package, atpkg::net::MANIFEST_CAP),
        (signature, atpkg::net::SIG_CAP),
    ] {
        match host.fetch_public(slug, tag, name, &dir.join(name), cap) {
            Probe::Present => {}
            Probe::Absent => return Err(HostError::NotFound),
            Probe::Unknown(why) => return Err(HostError::Unreachable(why)),
        }
    }
    Ok(())
}
