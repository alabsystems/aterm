// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! Bounded exact-ref snapshots admitted before a batch fetch starts.

use std::collections::BTreeSet;

use crate::error::{Error, Result};

/// Maximum explicit refs in one network fetch, including a fresh Codex tag catalog.
pub const FETCH_ROWS_MAX: usize = 2048;
/// Maximum combined row bytes; generated stdin remains below the child input bound.
pub const FETCH_BYTES_MAX: usize = 512 * 1024;

/// One full source ref or object ID and its admitted private destination.
#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FetchRow {
    /// A full ref or forty lowercase hexadecimal digits.
    pub source: String,
    /// A ref beneath `refs/agh/`.
    pub into: String,
    /// The raw object ID the enumerated source must still name.
    pub expected_oid: String,
}

/// An immutable batch whose names, expected identities, uniqueness and size were checked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FetchBatch {
    rows: Vec<FetchRow>,
}

impl FetchBatch {
    /// Admit explicit source/destination rows, sorted by destination.
    ///
    /// # Errors
    /// Empty or oversized rows, invalid refs/OIDs, mismatched literal OIDs,
    /// duplicate sources/destinations, or ancestor/descendant destinations.
    pub fn new(mut rows: Vec<FetchRow>) -> Result<Self> {
        if rows.is_empty() || rows.len() > FETCH_ROWS_MAX {
            return Err(Error::usage("fetch batch needs 1–2048 explicit rows"));
        }
        let mut sources = BTreeSet::new();
        let mut destinations = BTreeSet::new();
        let mut bytes = 0_usize;
        for row in &rows {
            bytes = bytes
                .saturating_add(row.source.len())
                .saturating_add(row.into.len())
                .saturating_add(row.expected_oid.len());
            if bytes > FETCH_BYTES_MAX {
                return Err(Error::usage("fetch batch exceeds 512 KiB of row data"));
            }
            if !(crate::path::is_sha(&row.source)
                || (row.source.starts_with("refs/") && crate::path::ref_name_ok(&row.source)))
            {
                return Err(Error::usage("fetch source must be a full ref or object ID"));
            }
            if !row.into.starts_with("refs/agh/") || !crate::path::ref_name_ok(&row.into) {
                return Err(Error::usage("fetch destination must be beneath refs/agh/"));
            }
            if !crate::path::is_sha(&row.expected_oid)
                || row.expected_oid.bytes().all(|byte| byte == b'0')
                || (crate::path::is_sha(&row.source) && row.source != row.expected_oid)
            {
                return Err(Error::usage(
                    "fetch expected_oid must match a nonzero raw object ID",
                ));
            }
            if !sources.insert(row.source.as_str()) || !destinations.insert(row.into.as_str()) {
                return Err(Error::usage(
                    "fetch sources and destinations must be unique",
                ));
            }
        }
        for destination in &destinations {
            for (offset, _) in destination.match_indices('/') {
                if destinations.contains(&destination[..offset]) {
                    return Err(Error::usage(
                        "fetch destinations cannot contain one another",
                    ));
                }
            }
        }
        rows.sort_unstable_by(|left, right| left.into.cmp(&right.into));
        Ok(Self { rows })
    }

    /// The validated rows in canonical destination order.
    #[must_use]
    pub fn rows(&self) -> &[FetchRow] {
        &self.rows
    }
}

#[cfg(test)]
mod tests {
    use super::{FetchBatch, FetchRow};

    fn row(source: &str, into: &str) -> FetchRow {
        FetchRow {
            source: source.into(),
            into: into.into(),
            expected_oid: "a".repeat(40),
        }
    }

    #[test]
    fn batch_names_identity_and_capacity_are_admitted_before_dispatch() {
        let valid = row("refs/heads/main", "refs/agh/main");
        assert!(FetchBatch::new(vec![valid.clone()]).is_ok());
        for invalid in [
            Vec::new(),
            vec![row("main", "refs/agh/main")],
            vec![row("refs/heads/*", "refs/agh/main")],
            vec![row("refs/heads/main", "refs/heads/main")],
            vec![valid.clone(), valid.clone()],
            vec![valid.clone(), row("refs/tags/v1", "refs/agh/main/child")],
            vec![row(&"b".repeat(40), "refs/agh/literal")],
            vec![valid.clone(); super::FETCH_ROWS_MAX + 1],
        ] {
            assert!(FetchBatch::new(invalid).is_err());
        }
        let mut invalid = valid;
        invalid.expected_oid = "0".repeat(40);
        assert!(FetchBatch::new(vec![invalid]).is_err());
        let oversized = row(
            &format!("refs/tags/{}", "v".repeat(super::FETCH_BYTES_MAX)),
            "refs/agh/tag",
        );
        assert!(
            FetchBatch::new(vec![oversized])
                .expect_err("byte bound")
                .why
                .contains("512 KiB")
        );
    }
}
