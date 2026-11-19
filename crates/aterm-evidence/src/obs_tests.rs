// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn reading(at: u64) -> Obs<bool> {
    Obs {
        value: true,
        at,
        witness: WitnessId::Reading("r".to_string()),
        source: Source::Reading,
        subject: Subject::Tab("s1".to_string()),
    }
}

/// A print is a function of namespace, row and generation alone; each part changes it, a renewal
/// (the next generation) is another print, and moving bytes between the parts never gives the
/// same print. The digest is SHA-256 over the domain tag and the length-framed parts.
#[test]
fn a_print_is_namespace_row_and_generation_and_nothing_else() {
    let print = CredentialPrint::new("claude", "row-1", 3);
    assert_eq!(print, CredentialPrint::new("claude", "row-1", 3));
    for other in [
        CredentialPrint::new("codex", "row-1", 3),
        CredentialPrint::new("claude", "row-2", 3),
        CredentialPrint::new("claude", "row-1", 4),
        CredentialPrint::new("claud", "erow-1", 3),
        CredentialPrint::new("", "clauderow-1", 3),
    ] {
        assert_ne!(print, other);
    }
    let mut framed = PRINT_DOMAIN.to_vec();
    for part in [&b"claude"[..], &b"row-1"[..]] {
        framed.extend_from_slice(&(part.len() as u64).to_be_bytes());
        framed.extend_from_slice(part);
    }
    framed.extend_from_slice(&3u64.to_be_bytes());
    assert_eq!(print.bytes(), &aterm_digest::Sha256::digest(&framed));
    let shown = format!("{print:?}");
    assert_eq!(shown.len(), "CredentialPrint()".len() + 16, "{shown}");
}

/// An observation is fresh at `now` exactly when it is dated in `[now - horizon, now]`; the
/// freshness it carries says until when, and keeps the observation whole.
#[test]
fn an_observation_is_fresh_only_within_its_horizon_and_never_from_the_future() {
    let horizon = Duration::from_secs(60);
    let obs = reading(1_000);
    let fresh = obs.fresh(1_000, horizon).expect("as old as nothing");
    assert_eq!(fresh.until(), 1_060);
    assert_eq!(fresh.obs(), &obs);
    assert!(*fresh.value());
    assert!(obs.fresh(1_060, horizon).is_some(), "the horizon itself");
    assert!(obs.fresh(1_061, horizon).is_none(), "past the horizon");
    assert!(obs.fresh(999, horizon).is_none(), "dated after now");
    assert_eq!(obs.fresh(1_030, horizon).map(Fresh::into_obs), Some(obs));
}
