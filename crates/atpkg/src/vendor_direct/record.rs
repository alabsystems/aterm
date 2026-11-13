// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `store/<program>/<build>.vendor`: what a vendor build was verified against ([`complete_record`]).

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::table::{Anchor, VendorSpec, spec};
use super::version::{BuildDate, Version};
use crate::manifest::one_digest;

/// The record's schema.
pub const RECORD_SCHEMA: u32 = 1;

/// The sibling suffix: `store/<program>/<build>.vendor`.
pub const RECORD_SUFFIX: &str = ".vendor";

/// Bound on a record read (it holds a dozen short fields).
const MAX_RECORD_BYTES: usize = 16 * 1024;

/// One verified vendor build.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VendorRecord {
    /// [`RECORD_SCHEMA`]; any other value is not read.
    pub schema: u32,
    /// The program; equals the store directory it sits in.
    pub program: String,
    /// The version; its build id equals the build directory's name.
    pub version: Version,
    /// The vendor's display name at verification.
    pub vendor: String,
    /// The URL the payload was fetched from.
    pub source_url: String,
    /// The payload's verified sha256 (lowercase hex), or empty beside a [`Self::sha512`]:
    /// a record names exactly one payload digest, under the key of its algorithm.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub sha256: String,
    /// The payload's verified sha512 (lowercase hex), for a vendor that signs only that;
    /// empty beside a [`Self::sha256`].
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub sha512: String,
    /// The payload's exact size in bytes.
    pub size: u64,
    /// The staged tree's root, folded at stage (lowercase hex).
    pub tree_root: String,
    /// The Apple team every Mach-O was checked against: the program's on darwin, `None`
    /// elsewhere (no codesign check runs).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub apple_team: Option<String>,
    /// What authenticated the digest.
    pub anchor: Anchor,
    /// The signed manifest's `buildDate` (claude only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build_date: Option<BuildDate>,
    /// When the build was verified (Unix seconds).
    pub verified_at: i64,
    /// The staged binary's sha256 (lowercase hex), measured from it and matched by the
    /// vendor's own copy at [`Self::agreed_with`] (grok's bucket); `None` for a build no
    /// second copy was held to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub measured_sha256: Option<String>,
    /// The vendor's own copy the staged binary agreed with, beside [`Self::measured_sha256`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agreed_with: Option<String>,
    /// The row whose complete build of these same signed bytes the tree was copied from in
    /// place of a download, one that [lends its tree](VendorSpec::lends_tree_to) to this
    /// record's; `None` for a tree staged from its own download.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tree_from: Option<String>,
}

impl VendorRecord {
    /// The bytes the stager writes to [`record_path`] (with
    /// [`super::durable::write_durable`]).
    ///
    /// # Errors
    /// A field is malformed, or the record does not serialize.
    pub(crate) fn record_bytes(&self) -> io::Result<Vec<u8>> {
        if !self.well_formed() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "vendor record is malformed",
            ));
        }
        super::durable::to_toml(self, MAX_RECORD_BYTES)
    }

    /// The installed-build view [`super::decide`] takes.
    #[must_use]
    pub(crate) const fn installed_view(&self) -> super::decide::InstalledView {
        super::decide::InstalledView {
            version: self.version,
            build: self.version.build_id(),
            build_date: self.build_date,
        }
    }

    /// Whether field shapes match what this build's compiled row would have written.
    fn well_formed(&self) -> bool {
        let Some(row) = spec(&self.program) else {
            return false;
        };
        let under_row = |url: &str| {
            crate::vendor::https_host(url).is_some()
                && row
                    .url_prefixes
                    .iter()
                    .any(|p| url.strip_prefix(p).is_some_and(|rest| !rest.is_empty()))
        };
        let agreement_ok = match (&self.measured_sha256, &self.agreed_with) {
            (None, None) => true,
            (Some(sha256), Some(url)) => super::is_hex64(sha256) && under_row(url),
            _ => false,
        };
        let lender_ok = self
            .tree_from
            .as_deref()
            .is_none_or(|from| spec(from).is_some_and(|lender| lender.lends_tree_to(row)));
        self.schema == RECORD_SCHEMA
            && self.vendor == row.vendor
            && row.anchor.reads_record_of(self.anchor)
            && under_row(&self.source_url)
            && agreement_ok
            && lender_ok
            && one_digest(&self.sha256, &self.sha512)
                .is_some_and(|(alg, hex)| alg.is_lower_hex(hex))
            && super::is_hex64(&self.tree_root)
            && self.size > 0
            && self.apple_team.as_deref() == expected_team(row)
    }
}

/// The Apple team a record of `row` names: the program's on darwin, where the stager
/// checks every Mach-O against it; none elsewhere.
fn expected_team(row: &VendorSpec) -> Option<&'static str> {
    cfg!(target_os = "macos").then_some(row.apple_team)
}

/// `store/<program>/<build>.vendor` beside `build_dir`.
#[must_use]
pub fn record_path(build_dir: &Path) -> Option<PathBuf> {
    let name = build_dir.file_name()?.to_str()?;
    let mut sidecar = String::from(name);
    sidecar.push_str(RECORD_SUFFIX);
    Some(build_dir.with_file_name(sidecar))
}

/// The record beside `build_dir`, or `None` when it is absent, unreadable, malformed, or
/// inconsistent with `build_dir` (see [`complete_record`]).
#[must_use]
pub fn read_record(build_dir: &Path) -> Option<VendorRecord> {
    let record: VendorRecord =
        super::durable::read_toml(&record_path(build_dir)?, MAX_RECORD_BYTES)?;
    let build = crate::store::parse_build_name(build_dir.file_name()?.to_str()?)?;
    let program = build_dir.parent()?.file_name()?.to_str()?;
    let consistent =
        record.well_formed() && record.program == program && record.version.build_id() == build;
    consistent.then_some(record)
}

/// [`read_record`] of a build the store holds as complete — the only record a reader
/// should act on.
#[must_use]
pub fn complete_record(build_dir: &Path) -> Option<VendorRecord> {
    if !crate::store::build_is_complete(build_dir) {
        return None;
    }
    read_record(build_dir)
}

#[cfg(test)]
#[path = "record_tests.rs"]
mod tests;
