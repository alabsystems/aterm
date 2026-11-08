// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Public authenticated version claims. A decoded claim is evidence to check against a
//! verified source, never authority by itself. No plaintext entry commitment is carried.

use crate::membership_log::{Log, MAX_HELD_RECORDS, RecordId};
use crate::{Error, Result};
use std::collections::BTreeSet;
use std::sync::Arc;

pub(crate) fn retained_reference(paths: &super::Paths, claim: &VersionClaim) -> Result<Vec<u8>> {
    super::retained_history::version_reference(paths, claim)
}

pub(crate) fn resolve_reference(paths: &super::Paths, bytes: &[u8]) -> Result<VersionClaim> {
    super::retained_history::resolve_version_reference(paths, bytes)
}

const MAGIC: &[u8; 4] = b"AVV1";
/// The largest complete claim, derived from the admitted history's record bound.
pub const MAX_VERSION_BYTES: usize = 4 + 1 + 63 + 32 + 1 + 40 + 32 + 4 + 32 * MAX_HELD_RECORDS;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnvelopeVersion {
    pub seq: u64,
    pub digest: [u8; 32],
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogVersion {
    pub network: RecordId,
    pub heads: Vec<RecordId>,
}

/// A bounded public claim, including the complete causal frontier where a log was used.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VersionClaim {
    vault: String,
    signer: [u8; 32],
    envelope: Option<EnvelopeVersion>,
    log: Option<Arc<LogVersion>>,
}

impl VersionClaim {
    /// Check a claim's representation, without authenticating its source.
    /// # Errors
    /// The vault, signing key, version kind or complete frontier is malformed.
    pub fn new(
        vault: String,
        signer: [u8; 32],
        envelope: Option<EnvelopeVersion>,
        log: Option<LogVersion>,
    ) -> Result<Self> {
        if !crate::envelope::valid_name(&vault)
            || signer == [0; 32]
            || (envelope.is_none() && log.is_none())
            || log.as_ref().is_some_and(|log| {
                log.network == [0; 32]
                    || log.heads.is_empty()
                    || log.heads.len() > MAX_HELD_RECORDS
                    || log.heads.contains(&[0; 32])
                    || log.heads.windows(2).any(|pair| pair[0] >= pair[1])
            })
        {
            return Err(Error::new("invalid authenticated version claim"));
        }
        Ok(Self {
            vault,
            signer,
            envelope,
            log: log.map(Arc::new),
        })
    }

    #[must_use]
    pub fn vault(&self) -> &str {
        &self.vault
    }
    #[must_use]
    pub const fn signer(&self) -> &[u8; 32] {
        &self.signer
    }
    #[must_use]
    pub const fn envelope(&self) -> Option<&EnvelopeVersion> {
        self.envelope.as_ref()
    }
    #[must_use]
    pub fn log(&self) -> Option<&LogVersion> {
        self.log.as_deref()
    }

    /// Canonical public metadata, suitable for bounded transport parts.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(MAGIC);
        out.push(self.vault.len() as u8);
        out.extend_from_slice(self.vault.as_bytes());
        out.extend_from_slice(&self.signer);
        out.push(u8::from(self.envelope.is_some()) | (u8::from(self.log.is_some()) << 1));
        if let Some(envelope) = &self.envelope {
            out.extend_from_slice(&envelope.seq.to_be_bytes());
            out.extend_from_slice(&envelope.digest);
        }
        if let Some(log) = &self.log {
            out.extend_from_slice(&log.network);
            out.extend_from_slice(&(log.heads.len() as u32).to_be_bytes());
            for head in &log.heads {
                out.extend_from_slice(head);
            }
        }
        out
    }

    /// Read exactly one bounded canonical claim. This grants no current source authority.
    /// # Errors
    /// The encoding is incomplete, oversized, noncanonical or malformed.
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let invalid = || Error::new("invalid authenticated version encoding");
        if bytes.len() > MAX_VERSION_BYTES {
            return Err(invalid());
        }
        let mut left = bytes;
        fn take<'a>(left: &mut &'a [u8], n: usize) -> Result<&'a [u8]> {
            let (part, rest) = left
                .split_at_checked(n)
                .ok_or_else(|| Error::new("incomplete authenticated version encoding"))?;
            *left = rest;
            Ok(part)
        }
        if take(&mut left, 4)? != MAGIC {
            return Err(invalid());
        }
        let n = take(&mut left, 1)?[0] as usize;
        let vault = std::str::from_utf8(take(&mut left, n)?)
            .map_err(|_| invalid())?
            .to_owned();
        let signer = take(&mut left, 32)?.try_into().map_err(|_| invalid())?;
        let flags = take(&mut left, 1)?[0];
        if flags == 0 || flags & !3 != 0 {
            return Err(invalid());
        }
        let envelope = if flags & 1 != 0 {
            Some(EnvelopeVersion {
                seq: u64::from_be_bytes(take(&mut left, 8)?.try_into().map_err(|_| invalid())?),
                digest: take(&mut left, 32)?.try_into().map_err(|_| invalid())?,
            })
        } else {
            None
        };
        let log = if flags & 2 != 0 {
            let network = take(&mut left, 32)?.try_into().map_err(|_| invalid())?;
            let n =
                u32::from_be_bytes(take(&mut left, 4)?.try_into().map_err(|_| invalid())?) as usize;
            if n > MAX_HELD_RECORDS {
                return Err(invalid());
            }
            let heads = take(&mut left, 32 * n)?.as_chunks::<32>().0.to_vec();
            Some(LogVersion { network, heads })
        } else {
            None
        };
        if !left.is_empty() {
            return Err(invalid());
        }
        Self::new(vault, signer, envelope, log)
    }

    /// An integrity/lookup key for the public claim, not proof of ancestry by inequality.
    #[must_use]
    pub fn id(&self) -> [u8; 32] {
        aterm_digest::Sha256::digest(self.encode())
    }
}

/// Historical scalar requests remain explicitly distinct from complete version claims.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RefusalClaim {
    LegacySequence(u64),
    Authenticated(VersionClaim),
}

impl RefusalClaim {
    /// Envelope metadata, when there is an envelope. A log-only version invents no sequence.
    #[must_use]
    pub fn sequence(&self) -> Option<u64> {
        match self {
            Self::LegacySequence(seq) => Some(*seq),
            Self::Authenticated(version) => version.envelope().map(|envelope| envelope.seq),
        }
    }
}

impl std::fmt::Display for RefusalClaim {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LegacySequence(seq) => write!(out, "seq {seq}"),
            Self::Authenticated(version) if version.log().is_some() => {
                out.write_str("authenticated log version")
            }
            Self::Authenticated(version) => write!(out, "seq {}", version.envelope().unwrap().seq),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VersionRelation {
    Same,
    Advanced,
    BehindOrFork,
    OtherAuthority,
}

/// An actual admitted vault view. Its log is the verified graph accepted with that baseline,
/// without asserting that every entry was read from the graph. Callers cannot turn a decoded
/// claim into this witness.
#[derive(Clone)]
pub struct AuthenticatedVersion {
    claim: VersionClaim,
    lineage: BTreeSet<[u8; 32]>,
    log: Option<Arc<Log>>,
    /// Independently authenticated admission context, even when selected bytes come only
    /// from the log. It is not an extra data component or typed-log advancement evidence.
    envelope_context: Option<EnvelopeVersion>,
}

impl std::fmt::Debug for AuthenticatedVersion {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        out.debug_struct("AuthenticatedVersion")
            .field("version", &self.claim.id())
            .finish_non_exhaustive()
    }
}

impl PartialEq for AuthenticatedVersion {
    fn eq(&self, other: &Self) -> bool {
        self.claim == other.claim
            && self.lineage == other.lineage
            && self.envelope_context == other.envelope_context
    }
}
impl Eq for AuthenticatedVersion {}

impl AuthenticatedVersion {
    /// The caller has authenticated the exact accepted view and signer lineage. `log` is
    /// precisely its accepted graph, never an unreadable/raw candidate or unrelated state.
    pub(crate) fn from_source(
        vault: &str,
        signer: [u8; 32],
        lineage: impl IntoIterator<Item = [u8; 32]>,
        envelope: Option<EnvelopeVersion>,
        log: Option<&Log>,
    ) -> Result<Self> {
        let claim = VersionClaim::new(
            vault.to_owned(),
            signer,
            envelope.clone(),
            log.map(|log| LogVersion {
                network: *log.network(),
                heads: log.heads(),
            }),
        )?;
        let mut lineage: BTreeSet<_> = lineage.into_iter().collect();
        lineage.insert(signer);
        Ok(Self {
            claim,
            lineage,
            log: log.map(|log| Arc::new(log.clone())),
            envelope_context: envelope,
        })
    }

    #[must_use]
    pub const fn claim(&self) -> &VersionClaim {
        &self.claim
    }

    /// A genuinely verified envelope's sequence, for comparing historical scalar floors.
    /// Its appearance never advances a typed log-source claim.
    #[must_use]
    pub fn envelope_sequence(&self) -> Option<u64> {
        self.envelope_context.as_ref().map(|envelope| envelope.seq)
    }

    /// Whether an explicit refusal may confirm the current paired value after this exact
    /// envelope acquired its first accepted log witness. This is not causal advancement or
    /// provider acceptance. A definitive provider refusal must retain the complete strengthened
    /// floor; an unanswered confirmation proves no refusal.
    #[must_use]
    pub fn refines_for_recovery(&self, floor: &VersionClaim) -> bool {
        floor.log.is_none()
            && self.log.is_some()
            && self
                .claim
                .envelope
                .as_ref()
                .zip(floor.envelope.as_ref())
                .is_some_and(|(current, previous)| current == previous)
            && self.relation(floor) == VersionRelation::Same
    }

    /// Compare against an authenticated causal prefix, without trusting a sender's digest.
    #[must_use]
    pub fn relation(&self, floor: &VersionClaim) -> VersionRelation {
        use VersionRelation::{Advanced, BehindOrFork, OtherAuthority, Same};
        if self.claim.vault != floor.vault || !self.lineage.contains(&floor.signer) {
            return OtherAuthority;
        }
        if let Some(previous) = &floor.envelope {
            let Some(current) = &self.claim.envelope else {
                return BehindOrFork;
            };
            if current.seq < previous.seq
                || (current.seq == previous.seq && current.digest != previous.digest)
            {
                return BehindOrFork;
            }
        }
        if let Some(previous) = &floor.log {
            let Some(current) = &self.claim.log else {
                return BehindOrFork;
            };
            if current.network != previous.network {
                return OtherAuthority;
            }
            if self
                .log
                .as_ref()
                .is_none_or(|log| previous.heads.iter().any(|head| log.get(head).is_none()))
            {
                return BehindOrFork;
            }
        }
        let envelope_advanced = match (&self.claim.envelope, &floor.envelope) {
            (Some(current), Some(previous)) => current.seq > previous.seq,
            (Some(_), None) => false,
            _ => false,
        };
        // A public claim may redundantly list an ancestor beside a maximal head. Once every
        // old head is held, strict extension requires a *current maximal* head not named there;
        // representation inequality alone cannot turn the same causal prefix into progress.
        let log_advanced = match (&self.claim.log, &floor.log) {
            (Some(current), Some(previous)) => current
                .heads
                .iter()
                .any(|head| previous.heads.binary_search(head).is_err()),
            (Some(_), None) => false,
            _ => false,
        };
        if envelope_advanced || log_advanced {
            Advanced
        } else {
            Same
        }
    }

    /// Compare a custodian-attested candidate prefix with a floor using this actual verified
    /// graph. The caller must authenticate the candidate's envelope/authority attestation;
    /// this only proves its causal claims, and returns no new authenticated source witness.
    #[must_use]
    pub fn relation_between(
        &self,
        candidate: &VersionClaim,
        floor: &VersionClaim,
    ) -> VersionRelation {
        use VersionRelation::{Advanced, BehindOrFork, OtherAuthority, Same};
        if self.claim.vault != candidate.vault
            || candidate.vault != floor.vault
            || !self.lineage.contains(&candidate.signer)
            || !self.lineage.contains(&floor.signer)
        {
            return OtherAuthority;
        }
        // The custodian's attestation authenticates its envelope even when this recipient
        // has only a log bundle. Only the candidate's log prefix must be present here.
        if let Some(candidate_log) = &candidate.log {
            let Some(current_log) = &self.log else {
                return BehindOrFork;
            };
            if candidate_log.network != *current_log.network() {
                return OtherAuthority;
            }
            if candidate_log
                .heads
                .iter()
                .any(|head| current_log.get(head).is_none())
            {
                return BehindOrFork;
            }
        }
        let envelope_advanced = if let Some(previous) = &floor.envelope {
            let Some(current) = &candidate.envelope else {
                return BehindOrFork;
            };
            if current.seq < previous.seq
                || (current.seq == previous.seq && current.digest != previous.digest)
            {
                return BehindOrFork;
            }
            current.seq > previous.seq
        } else {
            false
        };
        let log_advanced = if let Some(previous) = &floor.log {
            let Some(current) = &candidate.log else {
                return BehindOrFork;
            };
            if current.network != previous.network {
                return OtherAuthority;
            }
            let Some(log) = &self.log else {
                return BehindOrFork;
            };
            if !log.prefix_covers(&current.heads, &previous.heads) {
                return BehindOrFork;
            }
            !log.prefix_covers(&previous.heads, &current.heads)
        } else {
            false
        };
        if envelope_advanced || log_advanced {
            Advanced
        } else {
            Same
        }
    }
}
