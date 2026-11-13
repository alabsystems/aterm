// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The reference `https` row — shared with the lanes' tests.

use std::collections::BTreeMap;

use crate::manifest::{Artifact, Cost};

/// The reference `https` row.
pub fn row() -> Artifact {
    Artifact {
        target: "aarch64-apple-darwin".into(),
        kind: "binary".into(),
        protocol: "https".into(),
        asset: "claude-2.1.231-darwin-arm64".into(),
        sha256: "7b09f01c".repeat(8),
        sha512: String::new(),
        tree_root: "abc".into(),
        size: 230_824_016,
        reloc: "self-contained".into(),
        cost: Cost::default(),
        url: "https://downloads.claude.ai/claude-code-releases/2.1.231/darwin-arm64/claude".into(),
        payload: "raw-binary".into(),
        entry: "claude".into(),
        strip_components: 0,
        links: BTreeMap::new(),
        vendor: "Anthropic PBC".into(),
        provenance: None,
    }
}
