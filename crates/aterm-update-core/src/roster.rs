// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Master-signed machine [`Roster`] ([`ROSTER_ASSET`], [`ROSTER_SIG_ASSET`]) verification
//! ([`verify_roster`], [`VerifiedRoster`], [`Roster::authorize_appcast`], [`Attribution::bind`])
//! and rejection ([`RosterReject::Disabled`], [`RosterReject::Malformed`],
//! [`RosterReject::Rollback`], [`RosterReject::Stale`]):
//!
//! ```toml
//! schema      = 1
//! roster_seq  = 3                       # monotonic; the replay counter
//! valid_until = "9999-12-31T00:00:00Z"  # checked; minted as "forever"
//! revoked     = ["m11"]                 # ABOVE the first [[machine]], always
//!
//! [[machine]]
//! id       = "m3"
//! pubkey   = "<base64 Ed25519>"
//! added_at = "2026-08-04T00:00:00Z"
//! ```

use serde::{Deserialize, Serialize};

use ring::signature::{ED25519, UnparsedPublicKey};

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
    if pk.len() != 32 {
        return Err(RosterReject::BadKey);
    }
    if sig.len() != 64 {
        return Err(RosterReject::BadSig);
    }
    UnparsedPublicKey::new(&ED25519, &pk)
        .verify(msg, sig)
        .map_err(|_| RosterReject::Verify)
}

/// **Step 1–3.** Verify the roster's exact bytes under the pinned master keyset.
pub fn verify_roster(
    master_pubkeys: &[&str],
    raw: Vec<u8>,
    sig: &[u8],
) -> Result<VerifiedRoster, RosterReject> {
    // Step 1: no anchor, no authority.
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
    /// **Step 4–5.** Parse a roster from bytes that passed master verification.
    pub fn parse(verified: &VerifiedRoster) -> Result<Self, RosterReject> {
        /// The schema alone, every other key ignored, so a newer schema is refused for its
        /// version before the closed key set can call its new keys malformed.
        #[derive(Deserialize)]
        struct Head {
            schema: u32,
        }
        let text = std::str::from_utf8(verified.as_slice()).map_err(|_| RosterReject::Malformed)?;
        let head: Head = aterm_toml::from_str(text).map_err(|_| RosterReject::Malformed)?;
        if head.schema > SUPPORTED_SCHEMA {
            return Err(RosterReject::Schema);
        }
        let roster: Roster = aterm_toml::from_str(text).map_err(|_| RosterReject::Malformed)?;
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

    /// **Step 6–7.** Admit the roster itself.
    pub fn admit(&self, floor_seq: u64, now_unix: i64) -> Result<(), RosterReject> {
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
        if let Some(not_after) = &m.not_after {
            let until = rfc3339_to_unix(not_after).ok_or(RosterReject::Expired)?;
            if now_unix >= until {
                return Err(RosterReject::Expired);
            }
        }
        Ok(m)
    }

    /// The machines eligible to have signed anything now.
    #[must_use]
    pub fn live(&self, now_unix: i64) -> Vec<&Machine> {
        self.machines
            .iter()
            .filter(|m| self.machine(&m.id, now_unix).is_ok())
            .collect()
    }

    /// **Step 8–9.** Verify the appcast's exact bytes under the roster's live machines.
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
            // Every machine is revoked or expired: an empty set authorizes nothing.
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
    /// **Step 10.** Bind the manifest's self-declared identity to what verified.
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
mod tests {
    use super::*;
    use ring::signature::{Ed25519KeyPair, KeyPair};

    const MASTER_SEED: [u8; 32] = [0xA1; 32];
    const OTHER_MASTER_SEED: [u8; 32] = [0xA2; 32];
    const M3_SEED: [u8; 32] = [0xB1; 32];
    const M11_SEED: [u8; 32] = [0xB2; 32];
    const THIEF_SEED: [u8; 32] = [0xC1; 32];

    const NOW: i64 = 1_785_801_600;

    fn kp(seed: &[u8; 32]) -> Ed25519KeyPair {
        Ed25519KeyPair::from_seed_unchecked(seed).expect("synthetic 32-byte seed")
    }

    fn pk(kp: &Ed25519KeyPair) -> String {
        aterm_codec::base64::encode(kp.public_key().as_ref()).expect("32-byte key")
    }

    fn sign(kp: &Ed25519KeyPair, msg: &[u8]) -> Vec<u8> {
        kp.sign(msg).as_ref().to_vec()
    }

    /// A two-machine roster: m3 live, m11 live. `revoked` is empty.
    fn roster() -> Roster {
        Roster {
            schema: 1,
            roster_seq: 3,
            valid_until: "2027-02-01T00:00:00Z".into(),
            machines: vec![
                Machine {
                    id: "m3".into(),
                    pubkey: pk(&kp(&M3_SEED)),
                    added_at: "2026-08-04T00:00:00Z".into(),
                    not_after: None,
                },
                Machine {
                    id: "m11".into(),
                    pubkey: pk(&kp(&M11_SEED)),
                    added_at: "2026-08-04T00:00:00Z".into(),
                    not_after: None,
                },
            ],
            revoked: vec![],
        }
    }

    /// Emit and master-sign a roster, then take it back through `verify_roster` →
    /// `Roster::parse`, so tests exercise the bytes.
    fn through_the_chain(r: &Roster, master: &Ed25519KeyPair, anchors: &[&str]) -> Roster {
        let raw = r.to_toml().unwrap().into_bytes();
        let sig = sign(master, &raw);
        let verified = verify_roster(anchors, raw, &sig).expect("master-signed roster verifies");
        Roster::parse(&verified).expect("a valid roster parses")
    }

    const APPCAST: &[u8] = b"schema = 1\nversion = \"0.99\"\nmachine_id = \"m3\"\nroster_seq = 3\n";

    /// A master-signed roster verifies, admits, and an m3-signed appcast is attributed to m3.
    #[test]
    fn a_valid_chain_verifies_and_attributes_the_signing_machine() {
        let master = kp(&MASTER_SEED);
        let anchor = pk(&master);
        let r = through_the_chain(&roster(), &master, &[&anchor]);
        r.admit(0, NOW).expect("fresh, above the floor");
        let a = r
            .authorize_appcast(APPCAST, &sign(&kp(&M3_SEED), APPCAST), NOW)
            .expect("m3 is live and signed it");
        assert_eq!(a.machine_id, "m3");
        assert_eq!(a.pubkey_b64, pk(&kp(&M3_SEED)));
        assert_eq!(a.roster_seq, 3);
        a.bind(Some("m3"), Some(3)).expect("the manifest agrees");
    }

    /// The same appcast signed by m11 is attributed to m11: the key decides, not the label.
    #[test]
    fn attribution_follows_the_key_that_signed() {
        let master = kp(&MASTER_SEED);
        let anchor = pk(&master);
        let r = through_the_chain(&roster(), &master, &[&anchor]);
        let a = r
            .authorize_appcast(APPCAST, &sign(&kp(&M11_SEED), APPCAST), NOW)
            .expect("m11 is also live");
        assert_eq!(
            a.machine_id, "m11",
            "the key decides who signed, never the manifest's own claim"
        );
        assert_eq!(
            a.bind(Some("m3"), Some(3)),
            Err(RosterReject::UnknownMachine)
        );
    }

    /// A roster signed by a different master is refused before any parse.
    #[test]
    fn a_roster_signed_by_the_wrong_master_is_refused() {
        let anchor = pk(&kp(&MASTER_SEED));
        let raw = roster().to_toml().unwrap().into_bytes();
        let sig = sign(&kp(&OTHER_MASTER_SEED), &raw);
        assert_eq!(
            verify_roster(&[&anchor], raw, &sig),
            Err(RosterReject::Verify)
        );
    }

    /// Flipping the low bit of any byte of the roster is refused.
    #[test]
    fn a_tampered_roster_is_refused() {
        let master = kp(&MASTER_SEED);
        let anchor = pk(&master);
        let raw = roster().to_toml().unwrap().into_bytes();
        let sig = sign(&master, &raw);
        for i in 0..raw.len() {
            let mut bad = raw.clone();
            bad[i] ^= 0x01;
            assert_eq!(
                verify_roster(&[&anchor], bad, &sig),
                Err(RosterReject::Verify),
                "a flip at byte {i} must reject"
            );
        }
    }

    /// An appcast signed by a key outside a genuine roster verifies under nobody.
    #[test]
    fn an_appcast_signed_by_a_key_outside_the_roster_is_refused() {
        let master = kp(&MASTER_SEED);
        let anchor = pk(&master);
        let r = through_the_chain(&roster(), &master, &[&anchor]);
        assert_eq!(
            r.authorize_appcast(APPCAST, &sign(&kp(&THIEF_SEED), APPCAST), NOW),
            Err(RosterReject::Verify)
        );
        assert_eq!(
            r.authorize_appcast(APPCAST, &[0u8; 63], NOW),
            Err(RosterReject::BadSig)
        );
    }

    /// An unpinned master anchor authorizes nothing, not even a genuinely signed roster.
    #[test]
    fn an_empty_master_anchor_is_inert_and_never_accepts() {
        let master = kp(&MASTER_SEED);
        let raw = roster().to_toml().unwrap().into_bytes();
        let sig = sign(&master, &raw);
        assert_eq!(
            verify_roster(&[], raw.clone(), &sig),
            Err(RosterReject::Disabled),
            "an empty keyset must refuse, never wave through"
        );
        assert_eq!(verify_under("", &raw, &sig), Err(RosterReject::Disabled));
        let short = aterm_codec::base64::encode(&[1u8; 31]).expect("31 bytes");
        assert_eq!(
            verify_roster(&["", "!!not base64!!", &short], raw, &sig),
            Err(RosterReject::BadKey)
        );
    }

    /// A revoked machine's genuine signature is refused before any crypto touches it.
    #[test]
    fn a_revoked_machine_is_refused_though_its_signature_is_valid() {
        let master = kp(&MASTER_SEED);
        let anchor = pk(&master);
        let mut r = roster();
        r.roster_seq = 4;
        r.revoked = vec!["m11".into()];
        let r = through_the_chain(&r, &master, &[&anchor]);

        assert_eq!(r.machine("m11", NOW), Err(RosterReject::Revoked));
        let sig = sign(&kp(&M11_SEED), APPCAST);
        assert_eq!(
            r.authorize_appcast(APPCAST, &sig, NOW),
            Err(RosterReject::Verify),
            "a revoked machine must not be in the candidate set at all"
        );
        assert!(
            r.authorize_appcast(APPCAST, &sign(&kp(&M3_SEED), APPCAST), NOW)
                .is_ok()
        );
        let mut none = r.clone();
        none.revoked.push("m3".into());
        assert_eq!(
            none.authorize_appcast(APPCAST, &sign(&kp(&M3_SEED), APPCAST), NOW),
            Err(RosterReject::UnknownMachine)
        );
    }

    /// A machine both listed and revoked is refused.
    #[test]
    fn deny_beats_grant_when_a_roster_says_both() {
        let mut r = roster();
        r.revoked = vec!["m3".into()];
        assert_eq!(r.machine("m3", NOW), Err(RosterReject::Revoked));
    }

    /// An older roster's master signature stays valid forever, so a client that has durably
    /// seen seq 4 refuses seq 3.
    #[test]
    fn an_older_roster_is_refused_once_a_newer_one_has_been_seen() {
        let master = kp(&MASTER_SEED);
        let anchor = pk(&master);
        let old = through_the_chain(&roster(), &master, &[&anchor]);
        assert_eq!(old.roster_seq, 3);
        assert_eq!(old.admit(4, NOW), Err(RosterReject::Rollback));
        assert_eq!(old.admit(3, NOW), Ok(()));
    }

    /// An unreadable clock yields `i64::MAX`, so a lapsed roster is refused, not read as 1970.
    #[test]
    fn an_unreadable_clock_fails_closed_rather_than_reading_as_1970() {
        assert_eq!(fail_closed(None), i64::MAX);
        assert_eq!(fail_closed(Some(u64::MAX)), i64::MAX);
        let r = roster();
        assert_eq!(r.admit(0, 0), Ok(()), "1970 is inside every window");
        assert_eq!(r.admit(0, fail_closed(None)), Err(RosterReject::Stale));
        assert_eq!(fail_closed(Some(1_790_000_000)), 1_790_000_000);
        assert!(now_unix() > 1_700_000_000);
    }

    /// A lapsed roster is refused, and an unparseable deadline reads as lapsed, never absent.
    #[test]
    fn a_lapsed_or_unreadable_valid_until_is_refused() {
        let mut r = roster();
        r.valid_until = "2026-01-01T00:00:00Z".into();
        assert_eq!(r.admit(0, NOW), Err(RosterReject::Stale));
        let until = rfc3339_to_unix("2026-01-01T00:00:00Z").unwrap();
        assert_eq!(r.admit(0, until), Err(RosterReject::Stale));
        assert_eq!(r.admit(0, until - 1), Ok(()));
        r.valid_until = "not a date".into();
        assert_eq!(r.admit(0, NOW), Err(RosterReject::Stale));
    }

    /// A machine's own `not_after` ages it out of the candidate set without a revocation.
    #[test]
    fn an_expired_machine_ages_out_of_the_candidate_set() {
        let mut r = roster();
        r.machines[0].not_after = Some("2026-01-01T00:00:00Z".into());
        assert_eq!(r.machine("m3", NOW), Err(RosterReject::Expired));
        assert_eq!(
            r.authorize_appcast(APPCAST, &sign(&kp(&M3_SEED), APPCAST), NOW),
            Err(RosterReject::Verify)
        );
        let before = rfc3339_to_unix("2025-12-31T23:59:59Z").unwrap();
        assert!(r.machine("m3", before).is_ok());
        assert_eq!(r.machine("m3", before + 1), Err(RosterReject::Expired));
    }

    /// An old roster cannot be paired with a newer release, a newer roster with an older
    /// release is admitted, and an unattributed release is refused.
    #[test]
    fn the_manifest_must_name_its_machine_and_not_outrun_its_roster_generation() {
        let a = Attribution {
            machine_id: "m3".into(),
            pubkey_b64: pk(&kp(&M3_SEED)),
            roster_seq: 3,
        };
        assert_eq!(a.bind(Some("m3"), Some(3)), Ok(()), "same generation");
        assert_eq!(
            a.bind(Some("m3"), Some(2)),
            Ok(()),
            "an older attribution under a newer roster is the steady state after a join"
        );
        assert_eq!(a.bind(None, Some(3)), Err(RosterReject::Unattributed));
        assert_eq!(a.bind(Some("m3"), None), Err(RosterReject::SeqMismatch));
        assert_eq!(
            a.bind(Some("m3"), Some(4)),
            Err(RosterReject::SeqMismatch),
            "a release attributed under a NEWER generation than the roster presented is \
             the old-roster/new-release pairing that stays refused"
        );
        assert_eq!(
            a.bind(Some("m11"), Some(3)),
            Err(RosterReject::UnknownMachine)
        );
    }

    /// A newer schema is refused rather than misread, post-verify.
    #[test]
    fn a_newer_roster_schema_is_refused_not_misread() {
        let master = kp(&MASTER_SEED);
        let anchor = pk(&master);
        let mut r = roster();
        r.schema = SUPPORTED_SCHEMA + 1;
        let raw = r.to_toml().unwrap().into_bytes();
        let sig = sign(&master, &raw);
        let v = verify_roster(&[&anchor], raw, &sig).unwrap();
        assert_eq!(Roster::parse(&v), Err(RosterReject::Schema));
    }

    /// A key the schema does not declare is refused, never dropped: `revoked` written after
    /// the tables, a misspelt head key, an undeclared machine key.
    #[test]
    fn a_key_the_schema_does_not_declare_is_refused_not_dropped() {
        let master = kp(&MASTER_SEED);
        let anchor = pk(&master);
        let parse = |text: &str| {
            let raw = text.as_bytes().to_vec();
            let sig = sign(&master, &raw);
            Roster::parse(&verify_roster(&[&anchor], raw, &sig).expect("master-signed"))
        };
        let head = "schema      = 1\nroster_seq  = 3\nvalid_until = \"9999-12-31T00:00:00Z\"\n";
        let tables = format!(
            "\n[[machine]]\nid       = \"m3\"\npubkey   = \"{}\"\nadded_at = \"2026-08-04T00:00:00Z\"\n\
             \n[[machine]]\nid       = \"m11\"\npubkey   = \"{}\"\nadded_at = \"2026-08-04T00:00:00Z\"\n",
            pk(&kp(&M3_SEED)),
            pk(&kp(&M11_SEED)),
        );
        let m11_appcast = b"schema = 1\nmachine_id = \"m11\"\nroster_seq = 3\n";
        let m11_sig = sign(&kp(&M11_SEED), m11_appcast);

        let above = parse(&format!("{head}revoked     = [\"m11\"]\n{tables}"))
            .expect("the documented layout parses");
        assert!(above.is_revoked("m11"));
        assert_eq!(
            above.authorize_appcast(m11_appcast, &m11_sig, NOW),
            Err(RosterReject::Verify),
            "a revoked machine's signature is never checked"
        );

        assert_eq!(
            parse(&format!("{head}{tables}\nrevoked = [\"m11\"]\n")),
            Err(RosterReject::Malformed),
            "a revocation the parser cannot place must refuse, not vanish"
        );
        assert_eq!(
            parse(&format!("{head}revoke      = [\"m11\"]\n{tables}")),
            Err(RosterReject::Malformed)
        );
        assert_eq!(
            parse(&format!("{head}{tables}scope    = [\"atpkg\"]\n")),
            Err(RosterReject::Malformed)
        );

        let newer = format!(
            "schema = {}\nroster_seq = 3\nvalid_until = \"9999-12-31T00:00:00Z\"\nhorizon = 7\n{}",
            SUPPORTED_SCHEMA + 1,
            tables.replacen("[[machine]]\n", "[[machine]]\nscope = [\"atpkg\"]\n", 1),
        );
        assert_eq!(parse(&newer), Err(RosterReject::Schema));

        let mut emitted = roster();
        emitted.revoked = vec!["m2".into()];
        emitted.machines[0].not_after = Some("2027-01-01T00:00:00Z".into());
        let text = emitted.to_toml().expect("emits");
        assert_eq!(parse(&text), Ok(emitted));
    }

    /// Duplicate ids, empty ids and revocations, and an over-long roster are refused on both
    /// parse and emission.
    #[test]
    fn malformed_rosters_are_refused_on_parse_and_on_emission() {
        let mut dup = roster();
        dup.machines[1].id = "m3".into();
        assert_eq!(dup.validate(), Err(RosterReject::Malformed));
        assert_eq!(dup.to_toml(), Err(RosterReject::Malformed));

        let mut empty_id = roster();
        empty_id.machines[0].id = String::new();
        assert_eq!(empty_id.validate(), Err(RosterReject::Malformed));

        let mut empty_key = roster();
        empty_key.machines[1].pubkey = String::new();
        assert_eq!(empty_key.validate(), Err(RosterReject::Malformed));

        let mut empty_revoked = roster();
        empty_revoked.revoked = vec![String::new()];
        assert_eq!(empty_revoked.validate(), Err(RosterReject::Malformed));

        let mut huge = roster();
        huge.machines = (0..=MAX_MACHINES)
            .map(|i| Machine {
                id: i.to_string(),
                pubkey: format!("key-{i}"),
                added_at: String::new(),
                not_after: None,
            })
            .collect();
        assert_eq!(huge.validate(), Err(RosterReject::Malformed));

        let master = kp(&MASTER_SEED);
        let anchor = pk(&master);
        for bad in [&dup, &empty_id, &empty_key, &empty_revoked, &huge] {
            let raw = aterm_toml::to_string(bad).unwrap().into_bytes();
            let sig = sign(&master, &raw);
            let verified = verify_roster(&[&anchor], raw, &sig).expect("master-signed");
            assert_eq!(Roster::parse(&verified), Err(RosterReject::Malformed));
        }

        assert_eq!(roster().validate(), Ok(()));
    }

    /// One key under two ids would survive `machine-revoke` of either name (see
    /// [`Roster::validate`]), so it is refused on parse and on emission.
    #[test]
    fn one_public_key_may_not_appear_under_two_machine_ids() {
        let shared = pk(&kp(&M3_SEED));
        let mut twinned = roster();
        twinned.machines[1].pubkey.clone_from(&shared);
        assert_eq!(
            twinned.machines[0].pubkey, twinned.machines[1].pubkey,
            "precondition: the two entries really do carry one key"
        );
        assert_ne!(
            twinned.machines[0].id, twinned.machines[1].id,
            "precondition: and they are NOT a duplicate id, which is the other rule"
        );
        assert_eq!(twinned.validate(), Err(RosterReject::Malformed));
        assert_eq!(twinned.to_toml(), Err(RosterReject::Malformed));

        let mut revoked_one = twinned.clone();
        revoked_one.revoked = vec![revoked_one.machines[0].id.clone()];
        let survivors = revoked_one.live(NOW);
        assert_eq!(survivors.len(), 1, "the twin outlives the revocation");
        assert_eq!(
            survivors[0].pubkey, shared,
            "and it holds the very key that was supposed to have been withdrawn"
        );

        assert_eq!(roster().validate(), Ok(()));
    }

    /// Emit → master-sign → verify → parse reproduces the value exactly.
    #[test]
    fn the_roster_round_trips_through_emission_and_the_client_parser() {
        let master = kp(&MASTER_SEED);
        let anchor = pk(&master);
        let mut original = roster();
        original.revoked = vec!["m2".into()];
        original.machines[1].not_after = Some("2027-01-01T00:00:00Z".into());
        assert_eq!(through_the_chain(&original, &master, &[&anchor]), original);
    }

    /// A malformed anchor member neither grants nor denies: a good master beside it verifies,
    /// and the reported index is the one that did, so a stalled rotation is visible.
    #[test]
    fn a_malformed_anchor_member_neither_grants_nor_denies() {
        let master = kp(&MASTER_SEED);
        let good = pk(&master);
        let raw = roster().to_toml().unwrap().into_bytes();
        let sig = sign(&master, &raw);
        let v = verify_roster(&["!!not base64!!", &good], raw.clone(), &sig).unwrap();
        assert_eq!(v.master_index(), 1);
        assert_eq!(v.as_slice(), raw.as_slice());
        assert_eq!(
            verify_roster(&[&good, &good], raw, &[0u8; 10]),
            Err(RosterReject::BadSig)
        );
    }

    /// An offset stamp is refused rather than read as UTC (which would move the deadline the
    /// wrong way): `None`, lapsed at every caller, as `atpkg::flow` reads the same string.
    #[test]
    fn an_offset_stamp_is_refused_rather_than_silently_read_as_utc() {
        let mut r = roster();
        r.machines[0].not_after = Some("2026-12-31T23:59:59+05:30".into());
        let past_the_real_deadline = rfc3339_to_unix("2026-12-31T20:00:00Z").unwrap();
        assert_eq!(
            r.machine("m3", past_the_real_deadline),
            Err(RosterReject::Expired)
        );
        assert!(
            r.live(past_the_real_deadline).iter().all(|m| m.id != "m3"),
            "an unreadable not_after must not leave the machine in the live set"
        );

        let mut r = roster();
        r.valid_until = "2027-02-01T00:00:00+05:30".into();
        assert_eq!(r.admit(0, NOW), Err(RosterReject::Stale));
    }

    /// The roster reads only the shape `roster_ops` emits: a fractional-seconds stamp is
    /// lapsed, never truncated to its whole second (the strict `parse_utc`, not
    /// `parse_utc_fractional`).
    #[test]
    fn a_fractional_stamp_is_lapsed_not_truncated() {
        let mut r = roster();
        assert_eq!(r.admit(0, NOW), Ok(()));
        r.valid_until = "2027-02-01T00:00:00.500Z".into();
        assert_eq!(r.admit(0, NOW), Err(RosterReject::Stale));
    }
}
