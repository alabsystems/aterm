// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Canonical default-network identity under an independently trusted authority.
//! The empty public anchor contains no statement that needs a signature. Every
//! membership or restriction remains an authority-signed child control node.

use super::{Kind, Node};
use crate::{Error, Result};

const LENGTH: usize = 4 + 16 + 32;

impl Node {
    /// Derive the one empty default-network anchor for a trusted public key.
    /// This does not authenticate that key, authorize initialization, enroll a
    /// member, or expose an epoch key. Trust comes from authenticated enrollment
    /// or an existing pin, never from receiving these publicly reproducible bytes.
    #[cfg_attr(
        all(unix, any(test, debug_assertions)),
        aterm_spec_anchor::refines(
            machine = "DbFoundation",
            action = "Derive",
            project = "aterm_secrets::commons::node::foundation::tests::foundation_transition"
        )
    )]
    pub fn foundation(authority: [u8; 32]) -> Result<Self> {
        if authority == [0; 32] {
            return Err(Error::new("Commons foundation authority is zero"));
        }
        let mut hash = aterm_digest::Sha256::new();
        hash.update(b"aterm/commons/default-network/1\0");
        hash.update(authority);
        let digest = hash.finalize();
        let mut network: [u8; 16] = digest[..16].try_into().unwrap();
        if network == [0; 16] {
            network[0] = 1;
        }
        let mut wire = Vec::with_capacity(LENGTH);
        wire.extend_from_slice(b"ACF1");
        wire.extend_from_slice(&network);
        wire.extend_from_slice(&authority);
        let id = aterm_digest::Sha256::digest(&wire);
        Ok(Self {
            network,
            authority,
            kind: Kind::Genesis,
            parents: Vec::new(),
            selected: None,
            authored_ms: 0,
            ciphertext: Vec::new(),
            wire,
            id,
            foundation: true,
            legacy: false,
            #[cfg(unix)]
            binding_evidence: true,
            facts: Vec::new(),
        })
    }

    #[cfg_attr(
        all(unix, any(test, debug_assertions)),
        aterm_spec_anchor::refines(
            machine = "DbFoundation",
            action = "RoundTrip",
            project = "aterm_secrets::commons::node::foundation::tests::foundation_transition"
        )
    )]
    #[cfg_attr(
        all(unix, any(test, debug_assertions)),
        aterm_spec_anchor::refines(
            machine = "DbFoundation",
            action = "Altered",
            project = "aterm_secrets::commons::node::foundation::tests::foundation_transition"
        )
    )]
    #[cfg_attr(
        all(unix, any(test, debug_assertions)),
        aterm_spec_anchor::refines(
            machine = "DbFoundation",
            action = "ForeignRoot",
            project = "aterm_secrets::commons::node::foundation::tests::foundation_transition"
        )
    )]
    #[cfg_attr(
        all(unix, any(test, debug_assertions)),
        aterm_spec_anchor::refines(
            machine = "DbFoundation",
            action = "ForeignNetwork",
            project = "aterm_secrets::commons::node::foundation::tests::foundation_transition"
        )
    )]
    #[cfg_attr(
        all(unix, any(test, debug_assertions)),
        aterm_spec_anchor::refines(
            machine = "DbFoundation",
            action = "NonCanonicalDefault",
            project = "aterm_secrets::commons::node::foundation::tests::foundation_transition"
        )
    )]
    pub(super) fn verify_foundation(
        bytes: &[u8],
        network: [u8; 16],
        authority: [u8; 32],
    ) -> Result<Self> {
        if bytes.len() != LENGTH {
            return Err(Error::new("invalid Commons foundation length"));
        }
        let expected = Self::foundation(authority)?;
        if expected.network() != network || expected.bytes() != bytes {
            return Err(Error::new(
                "Commons foundation differs from its trusted context",
            ));
        }
        Ok(expected)
    }
}

#[cfg(all(test, unix))]
mod tests;
