// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Signature-**during**-selection (§5) — choosing which published index to trust.
//!
//! The reused release-listing flow (`aterm-update`'s `github.rs`) picks the highest
//! `build_number` release and *then* parses it. That ordering is exploitable here: a
//! repo-write adversary could publish a release carrying a huge (API-reported) build
//! number but an **unsigned / garbage `index.toml`**, and either DoS updates (the real
//! signed index is never reached) or shadow a lower, genuinely-signed index. So the order
//! is inverted: verify **first**, select **after**.
//!
//! [`select_index`] considers a candidate only if its `index.toml` bytes verify under the
//! pinned root key (§8) **and** its *signed* `index_build` (the one inside the verified
//! bytes — never the unsigned API-reported number) is ≥ the durable high-water `floor`.
//! Among those it returns the highest signed `index_build`. A candidate that fails the
//! signature or the parse is **skipped, not a global abort**, so an unsigned high-build
//! release can never suppress a lower signed one. Freshness (`valid_until`) and the
//! *advancing* floor write are applied by the caller to the winner (§8 gates 2–3).

use crate::manifest::{Index, parse_index};
use crate::sig::{VerifiedBytes, verify_index_with};

/// One release's index bytes + detached signature, as fetched from the releases list.
/// `label` is the release tag/id, carried only for diagnostics / `status.toml`.
#[derive(Debug, Clone)]
pub struct Candidate {
    /// Release tag / id (diagnostics only — never trusted for selection).
    pub label: String,
    /// The raw `index.toml` asset bytes (verified as-is; never lossily converted).
    pub index_bytes: Vec<u8>,
    /// The detached `index.toml.sig` bytes.
    pub sig: Vec<u8>,
}

/// The chosen, root-verified, floor-passing index plus its raw verified bytes (for the
/// caller to record / re-use) and the originating release label.
#[derive(Debug)]
pub struct Selected {
    /// The release the winning index came from.
    pub label: String,
    /// The parsed index.
    pub index: Index,
    /// The exact verified bytes the index was parsed from.
    pub verified: VerifiedBytes,
}

/// Verify-then-select over `candidates` (see the module docs). `root_pubkey_b64` is the
/// pinned root key (or a test/override root); `floor` is the current durable high-water
/// `index_build`. Returns the highest-signed-`index_build` candidate that both verifies
/// and is ≥ `floor`, or `None` if none qualify. Skips — never aborts on — a candidate
/// that fails verification or parsing.
///
/// The caller still applies the freshness gate and the *advancing* floor write to the
/// returned [`Selected`] (this function only reads `floor` as a filter, never advances it).
#[must_use]
pub fn select_index(
    root_pubkey_b64: &str,
    candidates: Vec<Candidate>,
    floor: u64,
) -> Option<Selected> {
    let mut best: Option<Selected> = None;
    for c in candidates {
        // Verify FIRST — an unsigned / wrong-key index is skipped (not a global abort).
        let Ok(verified) = verify_index_with(root_pubkey_b64, c.index_bytes, &c.sig) else {
            continue;
        };
        // Parse only the verified bytes; a malformed/newer-schema index is skipped too.
        let Ok(index) = parse_index(&verified) else {
            continue;
        };
        // Anti-rollback filter on the SIGNED build (never the unsigned API number).
        if index.index_build < floor {
            continue;
        }
        // Highest signed index_build wins.
        if best
            .as_ref()
            .is_none_or(|b| index.index_build > b.index.index_build)
        {
            best = Some(Selected {
                label: c.label,
                index,
                verified,
            });
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD;
    use ring::signature::{Ed25519KeyPair, KeyPair};

    const ROOT_SEED: [u8; 32] = [7u8; 32];
    const RELEASE_SEED: [u8; 32] = [1u8; 32];

    fn keypair(seed: &[u8; 32]) -> Ed25519KeyPair {
        Ed25519KeyPair::from_seed_unchecked(seed).expect("valid seed")
    }
    fn root_pk() -> String {
        STANDARD.encode(keypair(&ROOT_SEED).public_key().as_ref())
    }

    /// A minimal but complete, valid index naming one program, at `build`.
    fn index_body(build: u64) -> String {
        format!(
            "schema = 1\nindex_build = {build}\nvalid_until = \"2026-07-05T12:00:00Z\"\n\
             [keys]\nrelease_key_id = \"rk\"\nrelease_key_pubkey = \"{rk}\"\n\
             [programs.ay]\nrepo = \"ay\"\n",
            rk = STANDARD.encode(keypair(&RELEASE_SEED).public_key().as_ref())
        )
    }

    /// A candidate genuinely root-signed at `build` (with `label` for traceability).
    fn signed(label: &str, build: u64) -> Candidate {
        let raw = index_body(build).into_bytes();
        let sig = keypair(&ROOT_SEED).sign(&raw).as_ref().to_vec();
        Candidate {
            label: label.into(),
            index_bytes: raw,
            sig,
        }
    }

    /// A candidate carrying a (high) build but an INVALID signature (garbage sig).
    fn unsigned(label: &str, build: u64) -> Candidate {
        Candidate {
            label: label.into(),
            index_bytes: index_body(build).into_bytes(),
            sig: vec![0u8; 64], // valid length, but not a real signature
        }
    }

    // The highest SIGNED index_build wins; an unsigned higher-build release is skipped and
    // can never suppress it (§5 DoS-resistance).
    #[test]
    fn picks_highest_signed_skips_unsigned() {
        let cands = vec![
            signed("v50", 50),
            unsigned("v999-attack", 999), // huge claimed build, bad signature
            signed("v60", 60),
            signed("v55", 55),
        ];
        let sel = select_index(&root_pk(), cands, 40).expect("a signed index qualifies");
        assert_eq!(sel.label, "v60");
        assert_eq!(sel.index.index_build, 60);
    }

    // A signed index BELOW the floor is filtered out (anti-rollback); the winner is the
    // highest signed build at-or-above the floor.
    #[test]
    fn floor_filters_below_high_water() {
        let cands = vec![signed("v30", 30), signed("v45", 45), signed("v42", 42)];
        // Floor 44 ⇒ only v45 qualifies.
        let sel = select_index(&root_pk(), cands.clone(), 44).expect("v45 qualifies");
        assert_eq!(sel.index.index_build, 45);
        // Floor 100 ⇒ nothing qualifies.
        assert!(select_index(&root_pk(), cands, 100).is_none());
    }

    // Wrong root key ⇒ every candidate fails verification ⇒ None (fail closed).
    #[test]
    fn wrong_root_key_selects_nothing() {
        let attacker_pk = STANDARD.encode(keypair(&RELEASE_SEED).public_key().as_ref());
        assert!(select_index(&attacker_pk, vec![signed("v60", 60)], 0).is_none());
    }

    // Empty candidate list ⇒ None.
    #[test]
    fn no_candidates_selects_nothing() {
        assert!(select_index(&root_pk(), vec![], 0).is_none());
    }

    // An equal-build tie does not regress below floor; equal to floor is allowed (>=).
    #[test]
    fn equal_to_floor_is_allowed() {
        let sel = select_index(&root_pk(), vec![signed("v41", 41)], 41).expect("equal passes");
        assert_eq!(sel.index.index_build, 41);
    }
}
