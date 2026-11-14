// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! Exact commit-only reads. These never promise trees, blobs or a checkout.

use std::collections::BTreeSet;

use crate::error::{Error, Result};

/// One exact original Git parent-ancestry question, independent of ref names.
#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct AncestryQuery {
    /// The exact possible ancestor.
    pub ancestor: String,
    /// The exact tip observed in the caller's fresh published-ref inventory.
    pub descendant: String,
}

/// Wire input, with no implicit whole-history or checkout request.
#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommitReadInput {
    /// Exact commit objects to make readable, without fetching their trees.
    pub commits: Vec<String>,
    /// Exact questions; both outcomes require hashed original parent edges.
    /// A negative requires the complete reachable descendant parent walk.
    pub queries: Vec<AncestryQuery>,
}

/// Bounded and validated input; no ref, configuration or FETCH_HEAD is written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitRead {
    commits: Vec<String>,
    queries: Vec<AncestryQuery>,
}

impl CommitRead {
    /// Validate all identities before any local or remote operation.
    /// # Errors
    /// Empty, duplicate, oversized, zero or noncanonical object identities.
    pub fn new(input: CommitReadInput) -> Result<Self> {
        if input.commits.len() + input.queries.len() == 0
            || input.commits.len() > super::fetch::FETCH_ROWS_MAX
            || input.queries.len() > 128
        {
            return Err(Error::usage(
                "commit read needs 1–2048 commits or 1–128 queries",
            ));
        }
        let oid = |value: &str| crate::path::is_sha(value) && value.bytes().any(|b| b != b'0');
        let mut commits = BTreeSet::new();
        for value in &input.commits {
            if !oid(value) || !commits.insert(value) {
                return Err(Error::usage(
                    "commit reads need unique nonzero exact object IDs",
                ));
            }
        }
        let mut queries = BTreeSet::new();
        for query in &input.queries {
            if !oid(&query.ancestor)
                || !oid(&query.descendant)
                || !queries.insert((&query.ancestor, &query.descendant))
            {
                return Err(Error::usage(
                    "ancestry needs unique pairs of exact nonzero commit IDs",
                ));
            }
        }
        Ok(Self {
            commits: input.commits,
            queries: input.queries,
        })
    }

    /// Exact requested commits, preserving caller order.
    #[must_use]
    pub fn commits(&self) -> &[String] {
        &self.commits
    }

    /// Exact requested questions, preserving caller order.
    #[must_use]
    pub fn queries(&self) -> &[AncestryQuery] {
        &self.queries
    }
}
