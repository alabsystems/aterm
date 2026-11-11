// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Master-signed machine [`Roster`] ([`ROSTER_ASSET`], [`ROSTER_SIG_ASSET`]) verification
//! ([`verify_roster`], [`VerifiedRoster`], [`Roster::authorize_appcast`], [`Attribution::bind`])
//! and rejection ([`RosterReject::Disabled`], [`RosterReject::Malformed`],
//! [`RosterReject::Rollback`], [`RosterReject::Stale`]).

use serde::{Deserialize, Serialize};

/// The highest roster `schema` this build understands.
pub const SUPPORTED_SCHEMA: u32 = 1;

/// The roster's published asset name, beside `aterm-appcast.toml` on the same release.
pub const ROSTER_ASSET: &str = "aterm-machines.toml";

/// The roster's detached master signature, beside [`ROSTER_ASSET`].
pub const ROSTER_SIG_ASSET: &str = "aterm-machines.toml.sig";

/// A roster larger than this is refused as malformed.
pub const MAX_MACHINES: usize = 16;

/// Why the roster chain refused verification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RosterReject {
    /// No master anchor is pinned.
    Disabled,
    /// No master key was usable (each failed base64 or was not 32 bytes).
    BadKey,
    /// A detached signature was not exactly 64 bytes.
    BadSig,
    /// Ed25519 verification failed.
    Verify,
    /// Verified bytes that are not UTF-8, not TOML, or structurally invalid.
    Malformed,
    /// The roster declares a schema newer than this build understands.
    Schema,
    /// `roster_seq` is below the durable high-water floor.
    Rollback,
    /// `now >= valid_until`: the roster's freshness window has lapsed.
    Stale,
    /// The appcast's `roster_seq` is newer than the roster it verified under, or absent.
    SeqMismatch,
    /// The appcast carries no `machine_id`.
    Unattributed,
    /// The appcast's `machine_id` names a machine the roster does not list.
    UnknownMachine,
    /// The named machine is on the roster's deny-list.
    Revoked,
    /// The named machine's own `not_after` has lapsed.
    Expired,
}

/// Roster bytes that passed master verification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedRoster {
    bytes: Vec<u8>,
    master_index: usize,
}

impl VerifiedRoster {
    /// The verified raw bytes, exactly as the signature covered them.
    #[must_use]
    pub fn as_slice(&self) -> &[u8] {
        &self.bytes
    }

    /// Which master keyset member verified the roster.
    #[must_use]
    pub const fn master_index(&self) -> usize {
        self.master_index
    }
}

/// Verify a detached Ed25519 signature under one key, cheapest check first.
fn verify_under(pubkey_b64: &str, msg: &[u8], sig: &[u8]) -> Result<(), RosterReject> {
    if pubkey_b64.is_empty() {
        return Err(RosterReject::Disabled);
    }
    let pk = aterm_codec::base64::decode_strict(pubkey_b64.as_bytes())
        .map_err(|_| RosterReject::BadKey)?;
    let pk = <&[u8; 32]>::try_from(pk.as_slice()).map_err(|_| RosterReject::BadKey)?;
    let sig = <&[u8; 64]>::try_from(sig).map_err(|_| RosterReject::BadSig)?;
    if astream_aead::signature::verify(pk, msg, sig) {
        Ok(())
    } else {
        Err(RosterReject::Verify)
    }
}

/// Verify the roster's exact bytes under the pinned master keyset.
pub fn verify_roster(
    master_pubkeys: &[&str],
    raw: Vec<u8>,
    sig: &[u8],
) -> Result<VerifiedRoster, RosterReject> {
    // No anchor, no authority.
    if master_pubkeys.is_empty() {
        return Err(RosterReject::Disabled);
    }
    if sig.len() != 64 {
        return Err(RosterReject::BadSig);
    }
    let mut usable = 0usize;
    for (master_index, key) in master_pubkeys.iter().enumerate() {
        match verify_under(key, &raw, sig) {
            Ok(()) => {
                return Ok(VerifiedRoster {
                    bytes: raw,
                    master_index,
                });
            }
            Err(RosterReject::Verify) => usable += 1,
            Err(_) => {}
        }
    }
    if usable == 0 {
        Err(RosterReject::BadKey)
    } else {
        Err(RosterReject::Verify)
    }
}

/// One authorized machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Machine {
    /// The machine identity (e.g. `"m3"`).
    pub id: String,
    /// The machine's base64 Ed25519 public key (32 raw bytes).
    pub pubkey: String,
    /// RFC3339 mint time.
    #[serde(default)]
    pub added_at: String,
    /// Optional RFC3339 expiry for this machine alone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub not_after: Option<String>,
}

/// The master-signed roster.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Roster {
    /// Format version; `> SUPPORTED_SCHEMA` is refused rather than misread.
    pub schema: u32,
    /// Monotonic roster counter, the replay counter.
    pub roster_seq: u64,
    /// RFC3339 freshness deadline.
    pub valid_until: String,
    /// The authorized machines.
    #[serde(default, rename = "machine")]
    pub machines: Vec<Machine>,
    /// Withdrawn machine ids, never public keys.
    #[serde(default)]
    pub revoked: Vec<String>,
}

/// Which machine signed a release, proved by its key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attribution {
    /// The signing machine's id.
    pub machine_id: String,
    /// The signing machine's base64 public key (public identity only).
    pub pubkey_b64: String,
    /// The `roster_seq` of the roster that granted the authority.
    pub roster_seq: u64,
}

impl std::fmt::Display for Attribution {
    /// The one-line form recorded in status and the log: `m3 (roster seq 3)`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} (roster seq {})", self.machine_id, self.roster_seq)
    }
}

impl Roster {
    /// Parse a roster from bytes that passed master verification.
    pub fn parse(verified: &VerifiedRoster) -> Result<Self, RosterReject> {
        /// The schema alone, every other key ignored, so a newer schema is refused for its
        /// version before the closed key set can call its new keys malformed.
        #[derive(Deserialize)]
        struct Head {
            schema: u32,
        }
        let text = std::str::from_utf8(verified.as_slice()).map_err(|_| RosterReject::Malformed)?;
        let roster: Roster = match aterm_toml::from_str(text) {
            Ok(roster) => roster,
            Err(_) => {
                if aterm_toml::from_str::<Head>(text)
                    .is_ok_and(|head| head.schema > SUPPORTED_SCHEMA)
                {
                    return Err(RosterReject::Schema);
                }
                return Err(RosterReject::Malformed);
            }
        };
        if roster.schema > SUPPORTED_SCHEMA {
            return Err(RosterReject::Schema);
        }
        roster.validate()?;
        Ok(roster)
    }

    /// Structural invariants shared by parsing and the minting tool's emission.
    pub fn validate(&self) -> Result<(), RosterReject> {
        if self.machines.len() > MAX_MACHINES {
            return Err(RosterReject::Malformed);
        }
        for (i, m) in self.machines.iter().enumerate() {
            if m.id.is_empty() || m.pubkey.is_empty() {
                return Err(RosterReject::Malformed);
            }
            if self.machines[..i]
                .iter()
                .any(|p| p.id == m.id || p.pubkey == m.pubkey)
            {
                return Err(RosterReject::Malformed);
            }
        }
        if self.revoked.iter().any(String::is_empty) {
            return Err(RosterReject::Malformed);
        }
        Ok(())
    }

    /// Admit the roster itself.
    pub fn admit(&self, floor_seq: u64, now_unix: i64) -> Result<(), RosterReject> {
        // The ratchet: strong for a client that has seen a newer roster, nothing for
        // a fresh install.
        if self.roster_seq < floor_seq {
            return Err(RosterReject::Rollback);
        }
        let until = rfc3339_to_unix(&self.valid_until).ok_or(RosterReject::Stale)?;
        if now_unix >= until {
            return Err(RosterReject::Stale);
        }
        Ok(())
    }

    /// Whether `id` is on the deny-list.
    #[must_use]
    pub fn is_revoked(&self, id: &str) -> bool {
        self.revoked.iter().any(|r| r == id)
    }

    fn check_machine_live(&self, m: &Machine, now_unix: i64) -> Result<(), RosterReject> {
        if self.is_revoked(&m.id) {
            return Err(RosterReject::Revoked);
        }
        if let Some(not_after) = &m.not_after {
            let until = rfc3339_to_unix(not_after).ok_or(RosterReject::Expired)?;
            if now_unix >= until {
                return Err(RosterReject::Expired);
            }
        }
        Ok(())
    }

    /// The named machine, iff it is listed, not revoked, and not itself expired.
    pub fn machine(&self, id: &str, now_unix: i64) -> Result<&Machine, RosterReject> {
        if self.is_revoked(id) {
            return Err(RosterReject::Revoked);
        }
        let m = self
            .machines
            .iter()
            .find(|m| m.id == id)
            .ok_or(RosterReject::UnknownMachine)?;
        self.check_machine_live(m, now_unix)?;
        Ok(m)
    }

    /// The machines eligible to have signed anything now.
    #[must_use]
    pub fn live(&self, now_unix: i64) -> Vec<&Machine> {
        self.machines
            .iter()
            .filter(|m| self.check_machine_live(m, now_unix).is_ok())
            .collect()
    }

    /// Verify the appcast's exact bytes under the roster's live machines.
    pub fn authorize_appcast(
        &self,
        appcast: &[u8],
        sig: &[u8],
        now_unix: i64,
    ) -> Result<Attribution, RosterReject> {
        // Once, before the set: a wrong-length signature is BadSig regardless of the roster.
        if sig.len() != 64 {
            return Err(RosterReject::BadSig);
        }
        let live = self.live(now_unix);
        if live.is_empty() {
            return Err(RosterReject::UnknownMachine);
        }
        let mut usable = 0usize;
        for m in live {
            match verify_under(&m.pubkey, appcast, sig) {
                Ok(()) => {
                    return Ok(Attribution {
                        machine_id: m.id.clone(),
                        pubkey_b64: m.pubkey.clone(),
                        roster_seq: self.roster_seq,
                    });
                }
                Err(RosterReject::Verify) => usable += 1,
                Err(_) => {}
            }
        }
        if usable == 0 {
            Err(RosterReject::BadKey)
        } else {
            Err(RosterReject::Verify)
        }
    }
}

impl Attribution {
    /// Bind the manifest's self-declared identity to what verified.
    pub fn bind(
        &self,
        manifest_machine_id: Option<&str>,
        manifest_roster_seq: Option<u64>,
    ) -> Result<(), RosterReject> {
        let claimed = manifest_machine_id.ok_or(RosterReject::Unattributed)?;
        if claimed != self.machine_id {
            return Err(RosterReject::UnknownMachine);
        }
        match manifest_roster_seq {
            Some(seq) if seq <= self.roster_seq => Ok(()),
            _ => Err(RosterReject::SeqMismatch),
        }
    }
}

/// RFC3339 (`YYYY-MM-DDTHH:MM:SSZ`) → unix seconds.
use aterm_types::rfc3339::parse_utc as rfc3339_to_unix;

impl Roster {
    /// Serialize to the published TOML shape.
    pub fn to_toml(&self) -> Result<String, RosterReject> {
        self.validate()?;
        aterm_toml::to_string(self).map_err(|_| RosterReject::Malformed)
    }
}

/// Unix seconds now, for the roster's gates ([`Roster::admit`], [`Roster::machine`]).
#[cfg_attr(trust_verify, trust::skip)]
#[must_use]
pub fn now_unix() -> i64 {
    fail_closed(aterm_time::unix_secs())
}

/// [`now_unix`]'s decision, apart from the clock read so its direction is tested.
fn fail_closed(unix_secs: Option<u64>) -> i64 {
    unix_secs.map_or(i64::MAX, |s| i64::try_from(s).unwrap_or(i64::MAX))
}

#[cfg(test)]
#[path = "roster_tests.rs"]
mod tests;
