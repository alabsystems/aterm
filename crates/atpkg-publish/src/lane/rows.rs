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
use crate::host::{HostError, HostRef, Slug};
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
    let text = std::fs::read_to_string(path)
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

/// Downloads `pkg-<program>-<build>.toml` and its `.sig` from `slug` into `dir` and verifies it.
#[must_use]
pub fn fetch(
    host: &HostRef,
    slug: &Slug,
    program: &str,
    build: Build,
    dir: &Path,
    quad: &VerifiedQuad,
) -> Fetched {
    let tag = tag(program, build);
    let (pkg, sig) = (manifest_name(program, build), sig_name(program, build));
    match host.release(slug, &tag) {
        Ok(_) => {}
        Err(HostError::NotFound) => return Fetched::Absent(format!("{slug} has no release {tag}")),
        Err(e) => return Fetched::Unreachable(format!("{slug} {tag}: {e}")),
    }
    let mut last = String::new();
    let mut fetched = false;
    for _ in 0..3 {
        match host.download(slug, &tag, &[pkg.as_str(), sig.as_str()], dir) {
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
    let (Ok(bytes), Ok(signature)) = (std::fs::read(&path), std::fs::read(dir.join(&sig))) else {
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
