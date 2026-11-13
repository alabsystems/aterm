// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `<prefix>/vendor/<program>.toml`: persistent vendor-lane state ([`ProgramStamp`]).

use std::collections::BTreeMap;
use std::io;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::digests::AuthenticatedDigest;
use super::durable;
use super::table::is_vendor;
use super::version::Version;
use crate::store::Layout;

const STAMP_SCHEMA: u32 = 1;

const MAX_STAMP_BYTES: usize = 8 * 1024;

const MAX_ETAG_BYTES: usize = 256;

const MAX_VERDICT_CHARS: usize = 300;

/// Duration (in seconds) that a successful head check vouches for the vendor's latest build.
pub(crate) const CHECK_VOUCHES_FOR_SECS: i64 = 24 * 60 * 60;

/// Maximum future clock skew (in seconds) tolerated on a recorded head check.
pub(crate) const CHECK_CLOCK_SLACK_SECS: i64 = 60 * 60;

/// One program's stamp, as read.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProgramStamp {
    /// When the head was last checked (Unix seconds).
    pub last_checked_at: i64,
    /// The head version last seen.
    pub last_head: Option<Version>,
    /// The head's `ETag`, sent back as `If-None-Match`.
    pub etag: Option<String>,
    /// The highest version this machine has installed, lowered only by a signed-yank move.
    pub high_water: Option<Version>,
    /// The lane's last verdict: one line, no control or bidi characters.
    pub verdict: String,
    /// Last stage refusal `(version, payload digest)` to avoid re-downloading.
    pub(crate) refused: Option<(Version, AuthenticatedDigest)>,
    /// Cached `(legacy index build, version)` read from its signed manifest.
    pub(crate) legacy: Option<(u64, Version)>,
    /// Cached `(legacy index build, signed tree_root)` for rollback attestation.
    pub(crate) legacy_root: Option<(u64, String)>,
}

#[derive(Serialize, Deserialize)]
struct StampFile {
    schema: u32,
    last_checked_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_head: Option<Version>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    etag: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    high_water: Option<Version>,
    #[serde(default)]
    verdict: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    refused_version: Option<Version>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    refused_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    refused_sha512: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    legacy_build: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    legacy_version: Option<Version>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    legacy_root_build: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    legacy_tree_root: Option<String>,
    /// Every key this build does not know.
    #[serde(flatten)]
    extra: BTreeMap<String, aterm_toml::Value>,
}

/// `<prefix>/vendor/<program>.toml`, for a vendor program only.
#[must_use]
pub fn stamp_path(layout: &Layout, program: &str) -> Option<PathBuf> {
    if !is_vendor(program) {
        return None;
    }
    Some(vendor_dir(layout).join(format!("{program}.toml")))
}

/// `<prefix>/vendor/`.
pub(super) fn vendor_dir(layout: &Layout) -> PathBuf {
    layout.prefix.join("vendor")
}

impl ProgramStamp {
    /// Reads `program`'s stamp, returning `None` if absent, unreadable, or malformed.
    #[must_use]
    pub fn read(layout: &Layout, program: &str) -> Option<Self> {
        read_file(layout, program).map(|file| Self::from_file(file).0)
    }

    fn from_file(file: StampFile) -> (Self, BTreeMap<String, aterm_toml::Value>) {
        let refused = file.refused_version.zip(AuthenticatedDigest::recorded(
            file.refused_sha256.as_deref().unwrap_or_default(),
            file.refused_sha512.as_deref().unwrap_or_default(),
        ));
        let legacy = match (file.legacy_build, file.legacy_version) {
            (Some(b), Some(v)) if !super::is_vendor_build(b) => Some((b, v)),
            _ => None,
        };
        let legacy_root = match (file.legacy_root_build, file.legacy_tree_root) {
            (Some(b), Some(root)) if !super::is_vendor_build(b) && super::is_hex64(&root) => {
                Some((b, root))
            }
            _ => None,
        };
        let stamp = Self {
            last_checked_at: file.last_checked_at,
            last_head: file.last_head,
            etag: file.etag.filter(|e| etag_ok(e)),
            high_water: file.high_water,
            verdict: one_line(&file.verdict),
            refused,
            legacy,
            legacy_root,
        };
        (stamp, file.extra)
    }

    /// Returns whether a head check succeeded within the validity window around `now`.
    #[must_use]
    pub(crate) fn checked_recently(&self, now: i64) -> bool {
        let age = now.saturating_sub(self.last_checked_at);
        self.last_checked_at > 0
            && (-CHECK_CLOCK_SLACK_SECS..=CHECK_VOUCHES_FOR_SECS).contains(&age)
    }

    /// The version legacy index build `build` carries, when an earlier pass read it.
    #[must_use]
    pub(crate) fn legacy_version_of(&self, build: u64) -> Option<Version> {
        self.legacy.filter(|(b, _)| *b == build).map(|(_, v)| v)
    }

    /// Records that legacy index build `build` carries `version`.
    /// # Errors
    /// `build` is a vendor build id, `program` is not vendor-direct, or the stamp could not be
    /// locked or written.
    pub(crate) fn record_legacy_version(
        layout: &Layout,
        program: &str,
        build: u64,
        version: Version,
    ) -> io::Result<()> {
        if super::is_vendor_build(build) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a vendor build names its own version",
            ));
        }
        update(layout, program, |s| s.legacy = Some((build, version)))
    }

    /// The signed root kept for legacy index build `build`, when one was.
    #[must_use]
    pub(crate) fn legacy_root_of(&self, build: u64) -> Option<&str> {
        self.legacy_root
            .as_ref()
            .filter(|(b, _)| *b == build)
            .map(|(_, root)| root.as_str())
    }

    /// Records the signed `root` for legacy index build `build`.
    /// # Errors
    /// `build` is a vendor build id, `root` is not 64 hex digits, `program` is not vendor-direct,
    /// or the stamp could not be locked or written.
    pub(crate) fn record_legacy_root(
        layout: &Layout,
        program: &str,
        build: u64,
        root: &str,
    ) -> io::Result<()> {
        let root = root.to_ascii_lowercase();
        if super::is_vendor_build(build) || !super::is_hex64(&root) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "not a legacy build's tree root",
            ));
        }
        update(layout, program, |s| s.legacy_root = Some((build, root)))
    }

    /// Records a head check timestamp, head version, and `ETag`.
    /// # Errors
    /// `program` is not vendor-direct, or the stamp could not be locked or written.
    pub fn record_check(
        layout: &Layout,
        program: &str,
        checked_at: i64,
        head: Option<Version>,
        etag: Option<&str>,
    ) -> io::Result<()> {
        update(layout, program, |s| {
            s.last_checked_at = checked_at;
            s.last_head = head;
            s.etag = etag.filter(|e| etag_ok(e)).map(str::to_string);
        })
    }

    /// Records the lane's `verdict` and raises `high_water` to `active`.
    /// # Errors
    /// Returns an error if `program` is not vendor-direct or the stamp cannot be written.
    pub(crate) fn record_outcome(
        layout: &Layout,
        program: &str,
        verdict: &str,
        active: Option<Version>,
    ) -> io::Result<()> {
        update(layout, program, |s| {
            s.verdict = one_line(verdict);
            if let Some(v) = active {
                s.high_water = Some(s.high_water.map_or(v, |hw| hw.max(v)));
            }
        })
    }

    /// Records a signed-yank move to `active`, setting `high_water` to `active`.
    /// # Errors
    /// Returns an error if `program` is not vendor-direct or the stamp cannot be written.
    pub(crate) fn record_yank_move(
        layout: &Layout,
        program: &str,
        verdict: &str,
        active: Version,
    ) -> io::Result<()> {
        update(layout, program, |s| {
            s.verdict = one_line(verdict);
            s.high_water = Some(active);
        })
    }

    /// Records a stage refusal of `version` with `digest`.
    /// # Errors
    /// Returns an error if `program` is not vendor-direct or the stamp cannot be written.
    pub(crate) fn record_refusal(
        layout: &Layout,
        program: &str,
        verdict: &str,
        version: Version,
        digest: &AuthenticatedDigest,
    ) -> io::Result<()> {
        update(layout, program, |s| {
            s.verdict = one_line(verdict);
            s.refused = Some((version, digest.clone()));
        })
    }

    /// Clears any recorded stage refusal for `program`.
    /// # Errors
    /// Returns an error if `program` is not vendor-direct or the stamp cannot be written.
    pub(crate) fn clear_refusal(layout: &Layout, program: &str) -> io::Result<()> {
        if Self::read(layout, program).is_none_or(|s| s.refused.is_none()) {
            return Ok(());
        }
        update(layout, program, |s| s.refused = None)
    }

    /// Whether the lane refused `version` with `digest` at stage.
    #[must_use]
    pub(crate) fn is_refused(&self, version: Version, digest: &AuthenticatedDigest) -> bool {
        self.refused
            .as_ref()
            .is_some_and(|(v, d)| *v == version && d == digest)
    }
}

fn read_file(layout: &Layout, program: &str) -> Option<StampFile> {
    let file: StampFile = durable::read_toml(&stamp_path(layout, program)?, MAX_STAMP_BYTES)?;
    (file.schema == STAMP_SCHEMA).then_some(file)
}

fn update(
    layout: &Layout,
    program: &str,
    change: impl FnOnce(&mut ProgramStamp),
) -> io::Result<()> {
    let path = stamp_path(layout, program).ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "not a vendor-direct program")
    })?;
    layout.ensure_dir(&vendor_dir(layout))?;
    let _lock = durable::lock_for_update(&path)?;
    let (mut stamp, extra) = read_file(layout, program)
        .map(ProgramStamp::from_file)
        .unwrap_or_default();
    change(&mut stamp);
    let (refused_version, (refused_sha256, refused_sha512)) =
        stamp.refused.map_or((None, (None, None)), |(v, digest)| {
            let (sha256, sha512) = digest.keyed();
            let set = |hex: String| (!hex.is_empty()).then_some(hex);
            (Some(v), (set(sha256), set(sha512)))
        });
    let (legacy_build, legacy_version) = stamp
        .legacy
        .map_or((None, None), |(b, v)| (Some(b), Some(v)));
    let (legacy_root_build, legacy_tree_root) = stamp
        .legacy_root
        .map_or((None, None), |(b, root)| (Some(b), Some(root)));
    let file = StampFile {
        schema: STAMP_SCHEMA,
        last_checked_at: stamp.last_checked_at,
        last_head: stamp.last_head,
        etag: stamp.etag.filter(|e| etag_ok(e)),
        high_water: stamp.high_water,
        verdict: one_line(&stamp.verdict),
        refused_version,
        refused_sha256,
        refused_sha512,
        legacy_build,
        legacy_version,
        legacy_root_build,
        legacy_tree_root,
        extra,
    };
    let text = durable::to_toml(&file, MAX_STAMP_BYTES)?;
    if crate::metadata_io::read_bounded_regular_utf8(&path, MAX_STAMP_BYTES)
        .is_ok_and(|have| have.as_bytes() == text)
    {
        return Ok(());
    }
    durable::write_ordered(&path, &text)
}

fn etag_ok(etag: &str) -> bool {
    !etag.is_empty()
        && etag.len() <= MAX_ETAG_BYTES
        && etag.bytes().all(|b| (0x20..0x7f).contains(&b))
}

fn one_line(text: &str) -> String {
    text.chars()
        .map(|c| if printable(c) { c } else { ' ' })
        .take(MAX_VERDICT_CHARS)
        .collect()
}

fn printable(c: char) -> bool {
    !c.is_control()
        && !matches!(
            c,
            '\u{2028}'
                | '\u{2029}'
                | '\u{061c}'
                | '\u{200e}'
                | '\u{200f}'
                | '\u{202a}'..='\u{202e}'
                | '\u{2066}'..='\u{2069}'
        )
}

#[path = "stamp_corpus_tests.rs"]
#[cfg(test)]
mod corpus_tests;

#[cfg(test)]
#[path = "stamp_tests.rs"]
mod tests;
