// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Signature-during-selection (§5): choosing which published index to trust.

use crate::sig::{Anchor, BuildFloor, TrustedIndex, TrustedRoster, VerifiedBytes, admit_roster};
use crate::succession::{Refusal, Sources};

/// One release's signed index and the master-signed roster published beside it.
#[derive(Debug, Clone)]
pub struct Candidate {
    /// Release tag / id (diagnostics only).
    pub label: String,
    /// The raw `index.toml` asset bytes.
    pub index_bytes: Vec<u8>,
    /// The detached `index.toml.sig` bytes: a machine signature under the roster below.
    pub sig: Vec<u8>,
    /// The raw `aterm-machines.toml` bytes published on the same release.
    pub roster_bytes: Vec<u8>,
    /// The detached `aterm-machines.toml.sig` bytes.
    pub roster_sig: Vec<u8>,
}

/// The chosen index, its verified bytes and its release label.
#[derive(Debug)]
pub struct Selected {
    /// The release the winning index came from.
    pub label: String,
    /// The parsed index and the roster generation that authorized it.
    pub index: TrustedIndex,
    /// The exact verified bytes the index was parsed from.
    pub verified: VerifiedBytes,
    /// The winner's position in the candidates handed in (a label need not be unique).
    pub candidate: usize,
}

/// One verify-then-select pass: the winner, if any, and the newest roster generation it admitted.
#[derive(Debug)]
pub struct Selection {
    /// The winning index, or `None` if no candidate survived the whole chain.
    pub selected: Option<Selected>,
    /// The highest `roster_seq` admitted in this pass, `0` if none.
    pub observed_roster_seq: u64,
    /// The verified candidates refused as successors of the index trusted before them
    /// ([`crate::succession::check`]), oldest first.
    pub refused: Vec<Refusal>,
}

/// A candidate past the roster tier, its index still to be verified under `roster`.
struct Admitted {
    at: usize,
    label: String,
    index_bytes: Vec<u8>,
    sig: Vec<u8>,
    roster: TrustedRoster,
}

/// One distinct roster pair Pass 1 has weighed, and its verdict. A refusal is remembered
/// too: identical bytes under one anchor and clock refuse identically.
struct Weighed {
    roster_bytes: Vec<u8>,
    roster_sig: Vec<u8>,
    verdict: Option<TrustedRoster>,
}

/// Verify-then-select over `candidates`: the highest index of the newest roster generation
/// that clears `floor` and succeeds the index trusted before it.
///
/// `trusted` is the last index this client trusted ([`crate::succession::Record`]). It binds
/// only within its own generation, and with none that binds the oldest verified candidate
/// starts the chain. Each newer candidate, in build order, must succeed the last one admitted
/// ([`crate::succession::check`]); one that does not is refused whole and named in
/// [`Selection::refused`].
#[must_use]
pub fn select_index(
    anchor: &Anchor,
    candidates: Vec<Candidate>,
    floor: BuildFloor,
    trusted: Option<&Sources>,
    now_unix: i64,
) -> Selection {
    let nothing = Selection {
        selected: None,
        observed_roster_seq: 0,
        refused: Vec::new(),
    };
    if !anchor.is_armed() {
        return nothing;
    }

    let mut weighed: Vec<Weighed> = Vec::new();
    let mut admitted: Vec<Admitted> = Vec::new();
    let mut observed_roster_seq = 0u64;
    for (at, c) in candidates.into_iter().enumerate() {
        let seen = weighed
            .iter()
            .find(|w| w.roster_bytes == c.roster_bytes && w.roster_sig == c.roster_sig)
            .map(|w| w.verdict.clone());
        let verdict = match seen {
            Some(already) => already,
            None => {
                let fresh =
                    admit_roster(anchor, c.roster_bytes.clone(), &c.roster_sig, now_unix).ok();
                weighed.push(Weighed {
                    roster_bytes: c.roster_bytes,
                    roster_sig: c.roster_sig,
                    verdict: fresh.clone(),
                });
                fresh
            }
        };
        let Some(roster) = verdict else {
            continue;
        };
        observed_roster_seq = observed_roster_seq.max(roster.seq());
        admitted.push(Admitted {
            at,
            label: c.label,
            index_bytes: c.index_bytes,
            sig: c.sig,
            roster,
        });
    }
    if admitted.is_empty() {
        return nothing;
    }

    let mut verified_newest: Vec<Selected> = Vec::new();
    for a in admitted {
        if a.roster.seq() != observed_roster_seq {
            continue;
        }
        let Ok((index, verified)) = a.roster.authorize_index(a.index_bytes, &a.sig) else {
            continue;
        };
        if !release_label_matches_build(&a.label, index.index_build) {
            continue;
        }
        if !floor.admits(index.authorizing_seq(), index.index_build) {
            continue;
        }
        verified_newest.push(Selected {
            label: a.label,
            index,
            verified,
            candidate: a.at,
        });
    }
    // Stable, so of two candidates at one build the first handed in is weighed first.
    verified_newest.sort_by_key(|s| s.index.index_build);

    let mut chain: Option<Sources> = trusted
        .filter(|t| t.roster_seq == observed_roster_seq)
        .cloned();
    let mut best: Option<Selected> = None;
    let mut refused = Vec::new();
    for s in verified_newest {
        match &chain {
            Some(t) if s.index.index_build > t.index_build => {
                let breaks = crate::succession::check(t, &s.index);
                if !breaks.is_empty() {
                    refused.push(Refusal {
                        label: s.label.clone(),
                        index_build: s.index.index_build,
                        machine_id: s.index.attribution().machine_id.clone(),
                        trusted_build: t.index_build,
                        breaks,
                    });
                    continue;
                }
                chain = Some(Sources::of(&s.index, s.index.roster_seq()));
            }
            Some(_) => {}
            None => chain = Some(Sources::of(&s.index, s.index.roster_seq())),
        }
        if best
            .as_ref()
            .is_none_or(|b| s.index.index_build > b.index.index_build)
        {
            best = Some(s);
        }
    }
    Selection {
        selected: best,
        observed_roster_seq,
        refused,
    }
}

/// Production index-release labels carry their signed build number. Diagnostic fixture labels
/// remain free-form, but anything claiming the `atpkg-index-` namespace must be canonical and
/// must agree with the authenticated document.
fn release_label_matches_build(label: &str, build: u64) -> bool {
    let Some(digits) = label.strip_prefix("atpkg-index-") else {
        return true;
    };
    !digits.is_empty()
        && digits.bytes().all(|byte| byte.is_ascii_digit())
        && (digits == "0" || !digits.starts_with('0'))
        && digits.parse::<u64>() == Ok(build)
}

#[cfg(test)]
#[path = "select_tests.rs"]
mod tests;
