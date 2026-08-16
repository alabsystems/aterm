// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The **machine roster** — one paper master, many machine keys.
//!
//! The owner's decision, in one sentence: there is ONE master key, written on paper and
//! present on no computer, and it mints a SEPARATE signing key for every machine that
//! publishes. A machine key never leaves the machine that minted it. Adding a laptop, or
//! reformatting one, mints another. See `docs/SIGNING-KEY-DESIGN.md` for the decision and
//! why it reverses the one-key record it replaces.
//!
//! # This tier authorizes atpkg too
//!
//! The roster is not the update channel's private tier. `atpkg` — the toolchain package
//! manager — folds under the same master: `atpkg::sig::admit_roster` runs steps 1–7 below
//! verbatim, and `atpkg::sig::TrustedRoster::authorize_bytes` delegates step 8–9 to
//! [`Roster::authorize_appcast`], which is a signature check over opaque bytes under the
//! authorized set and is named for the first artifact it verified rather than the only
//! one. atpkg's `index.toml` and every `pkg-*.toml` are authorized by exactly this
//! machinery, so a machine revoked here stops publishing BOTH products at once — the
//! property two independent roots could not express. atpkg publishes
//! [`ROSTER_ASSET`]/[`ROSTER_SIG_ASSET`] beside its index on the same release, so the
//! grant and the deny reach a toolchain client the same way they reach an app client.
//!
//! # The document
//!
//! The roster (`aterm-machines.toml`) is a small TOML file published as a release asset
//! beside the appcast, with a detached Ed25519 signature by the MASTER
//! (`aterm-machines.toml.sig`). It names every machine authorized to sign, and every
//! machine whose authority has been withdrawn:
//!
//! ```toml
//! schema      = 1
//! roster_seq  = 3                       # monotonic; the replay counter
//! valid_until = "2027-02-01T00:00:00Z"  # the freshness bound
//!
//! [[machine]]
//! id       = "m3"
//! pubkey   = "<base64 Ed25519>"
//! added_at = "2026-08-04T00:00:00Z"
//!
//! revoked = ["m11"]
//! ```
//!
//! # ONE roster, not per-machine delegation files
//!
//! This is the single most important structural choice here, and it is not an
//! implementation detail. If each machine carried its OWN master-signed delegation file,
//! a thief holding a revoked machine's key would also hold that machine's still-valid,
//! still-master-signed delegation — and a client would have no reason to ever fetch the
//! document that revokes it. Putting the grant and the deny in the SAME signed document
//! means a client cannot learn that a machine is authorized without simultaneously
//! learning who is revoked. It is why the deny-list must never become a separate asset —
//! and, since atpkg folded under this master, why ONE revocation stops both products.
//!
//! # Cheapest-first, fail-closed, and parse-only-after-verify
//!
//! Every gate below refuses rather than accepts, and the expensive crypto runs last:
//!
//! 1. master anchor empty ⇒ [`RosterReject::Disabled`]. Free. The tier grants NOTHING —
//!    it never means "accept anything".
//! 2. signature length (both signatures are exactly 64 bytes). Cheap, local.
//! 3. verify the roster under the master keyset. **CRYPTO #1.**
//! 4. parse the roster — only from [`VerifiedRoster`], which has a private field and no
//!    public constructor, so parsing unverified roster bytes does not type-check.
//! 5. `schema > SUPPORTED_SCHEMA` ⇒ refuse rather than misread. Cheap.
//! 6. `roster_seq` below the durable floor ⇒ [`RosterReject::Rollback`]. Cheap. THE
//!    replay defence for a client that has already seen a newer roster.
//! 7. `valid_until` lapsed ⇒ [`RosterReject::Stale`]. Cheap, pure. The ONLY thing that
//!    protects a brand-new install, which has no floor yet.
//! 8. the revoked and expired machines are removed from the candidate set BEFORE any
//!    artifact crypto — a revoked machine's perfectly valid signature is never checked.
//! 9. verify the appcast under the surviving machines. **CRYPTO #2.**
//! 10. post-verify, bind the manifest's self-declared `machine_id` / `roster_seq` to what
//!     actually verified ([`Attribution::bind`]).
//!
//! # The id↔key bind is two-way, and free
//!
//! `machine_id` sits INSIDE the signed appcast bytes, so a genuine m3 signature cannot be
//! relabelled as m11 — the bytes, and therefore the signature, would change. Conversely a
//! thief holding m11's key cannot claim `machine_id = "m3"`, because the roster maps m3 to
//! m3's public key and step 9 verifies against m11's. No extra mechanism is needed for
//! either direction; step 10 is a string compare over already-authenticated data.

use serde::{Deserialize, Serialize};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use ring::signature::{ED25519, UnparsedPublicKey};

/// The highest roster `schema` this build understands. A roster declaring a higher schema
/// is from a newer format we cannot safely interpret, so it is REFUSED rather than
/// misread — the same reject-newer discipline as `manifest::SUPPORTED_SCHEMA` and
/// `atpkg::manifest::SUPPORTED_SCHEMA`.
pub const SUPPORTED_SCHEMA: u32 = 1;

/// The roster's published asset name, beside `aterm-appcast.toml` on the same release.
pub const ROSTER_ASSET: &str = "aterm-machines.toml";

/// The roster's detached master signature, beside [`ROSTER_ASSET`].
pub const ROSTER_SIG_ASSET: &str = "aterm-machines.toml.sig";

/// A roster larger than this is refused as malformed.
///
/// The bound is not about attacker-supplied input — the roster is master-signed, so its
/// size is the owner's own choice. It is about the OWNER: the roster is not a convenience
/// list of the machines you own, it is the set of machines that can publish to every user,
/// and any one of them can sign any release. A hard ceiling makes "the roster grew and
/// nobody noticed" impossible, and 16 is far above the handful of machines the decision
/// record contemplates. A machine that only BUILDS never needs to be here; only a machine
/// that PUBLISHES does.
pub const MAX_MACHINES: usize = 16;

/// Why the chain refused. Deliberately coarse on the crypto steps (every `ring` failure
/// collapses to [`RosterReject::Verify`], whose error is opaque by design), and specific
/// on the structural steps, which are all decidable from already-authenticated bytes and
/// so cannot act as an oracle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RosterReject {
    /// No master anchor is pinned — the tier is inert. Returned BEFORE any crypto, and it
    /// never means "accept anything": an inert tier authorizes no machine at all.
    Disabled,
    /// A master public key did not base64-decode, or was not exactly 32 bytes — and no
    /// usable master key remained. A keyset of nothing but garbage must not read as "the
    /// signature was bad".
    BadKey,
    /// A detached signature was not exactly 64 bytes.
    BadSig,
    /// Ed25519 verification failed: the roster is not signed by the pinned master, or the
    /// appcast is not signed by any live machine on it.
    Verify,
    /// Verified bytes that are not UTF-8, not TOML, or structurally invalid (a duplicate
    /// machine id, an empty id, an over-long roster). Post-verify, so not an oracle.
    Malformed,
    /// The roster declares a schema newer than this build understands.
    Schema,
    /// `roster_seq` is below the durable high-water floor — an attempted replay of an
    /// older, once-legitimate, still-validly-signed roster.
    Rollback,
    /// `now >= valid_until`: the roster's freshness window has lapsed.
    Stale,
    /// The appcast's `roster_seq` does not match the roster's — an attempt to pair an old
    /// roster with a new release, or vice versa.
    SeqMismatch,
    /// The appcast carries no `machine_id`, so the release cannot be attributed. Under an
    /// armed master anchor an unattributed release is refused: attribution is a
    /// requirement, not a nicety.
    Unattributed,
    /// The appcast's `machine_id` names a machine the roster does not list, or names a
    /// DIFFERENT machine than the key that actually verified (a relabelling attempt).
    UnknownMachine,
    /// The named machine is on the roster's deny-list. Checked before any artifact crypto.
    Revoked,
    /// The named machine's own `not_after` has lapsed — defence in depth, so a machine key
    /// ages out and is re-minted from paper even if nothing went wrong.
    Expired,
}

/// Roster bytes that have PASSED master verification. The inner `Vec<u8>` is private and
/// there is no public constructor, so the only way to obtain one is [`verify_roster`] —
/// which makes "parse only after verify" a compile-time guarantee rather than a
/// convention. Mirrors `atpkg::sig::VerifiedBytes`, deliberately: the property is the
/// same, and two spellings of it would be two things to keep right.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedRoster {
    bytes: Vec<u8>,
    master_index: usize,
}

impl VerifiedRoster {
    /// The verified raw bytes — the SAME bytes the signature was checked over, with no
    /// normalization, re-serialization or lossy UTF-8 conversion applied.
    #[must_use]
    pub fn as_slice(&self) -> &[u8] {
        &self.bytes
    }

    /// Which master keyset member verified. Index 0 is the master this build considers
    /// current; a hit anywhere else means a master rotation is in progress or stalled, and
    /// is worth saying out loud — never a rejection.
    #[must_use]
    pub fn master_index(&self) -> usize {
        self.master_index
    }
}

/// Verify a detached Ed25519 signature under ONE key, cheapest-first: empty key (free) →
/// base64 decode + 32-byte length (cheap, local) → 64-byte signature length (cheap,
/// local) → the crypto, last.
fn verify_under(pubkey_b64: &str, msg: &[u8], sig: &[u8]) -> Result<(), RosterReject> {
    if pubkey_b64.is_empty() {
        return Err(RosterReject::Disabled);
    }
    let pk = STANDARD
        .decode(pubkey_b64)
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
///
/// The master anchor is a LIST for the same reason the channel keyset is: a client that
/// accepts exactly one key cannot be told about a replacement by a document it would
/// refuse to verify. An EMPTY slice means the tier is unpinned, and unpinned means inert —
/// this returns [`RosterReject::Disabled`] and authorizes nothing. That is the fail-closed
/// default, not a bypass.
///
/// A malformed member can neither GRANT nor DENY: it is skipped, and the verdict comes
/// from the usable members. If NO member was usable the answer is [`RosterReject::BadKey`],
/// because a keyset of pure garbage must not be reported as a bad signature.
pub fn verify_roster(
    master_pubkeys: &[&str],
    raw: Vec<u8>,
    sig: &[u8],
) -> Result<VerifiedRoster, RosterReject> {
    // STEP 1 — free. No anchor, no authority. Never "accept anything".
    if master_pubkeys.is_empty() {
        return Err(RosterReject::Disabled);
    }
    // STEP 2 — cheap, local, and checked ONCE rather than per member: a wrong-length
    // signature is BadSig no matter how many masters are listed.
    if sig.len() != 64 {
        return Err(RosterReject::BadSig);
    }
    // STEP 3 — the crypto. `usable` distinguishes "every member is garbage" (BadKey) from
    // "the members are fine and this signature is not theirs" (Verify).
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

/// One authorized machine. `id` is the ATTRIBUTION handle — the thing a verifier reports
/// and a human reads later — and it is what the deny-list names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Machine {
    /// The machine identity, e.g. `"m3"`. Stable for the life of the key, and NEVER
    /// reused: see [`Roster::machine`] for why re-minting under an old id creates two
    /// live authorities under one name.
    pub id: String,
    /// The machine's base64 Ed25519 public key (32 raw bytes). The secret half was minted
    /// on that machine and never leaves it.
    pub pubkey: String,
    /// RFC3339 mint time. Informational, and the human-readable half of attribution.
    #[serde(default)]
    pub added_at: String,
    /// Optional RFC3339 expiry for this machine alone — defence in depth, so a key ages
    /// out on its own schedule and is re-minted from paper even when nothing went wrong.
    /// Absent ⇒ the machine lives until it is revoked or the whole roster lapses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub not_after: Option<String>,
}

/// The master-signed roster: who may sign, and who no longer may.
///
/// Field order is meaningful for EMISSION only (serde's TOML serializer writes keys in
/// declaration order, and `[[machine]]` tables must follow the scalar head or they would
/// swallow the keys after them). Parsing is order-insensitive as always.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Roster {
    /// Format version; `> SUPPORTED_SCHEMA` is refused rather than misread.
    pub schema: u32,
    /// Monotonic roster counter — THE replay counter. Every mint or revocation bumps it,
    /// and a client that has durably seen `n` permanently refuses anything below `n`.
    pub roster_seq: u64,
    /// RFC3339 freshness deadline. A lapsed roster is refused fail-closed.
    ///
    /// This is a DIAL, not a solution, and the tradeoff belongs where it is chosen rather
    /// than discovered: a short window bounds a stolen key's reach against FRESH installs
    /// (which have no `roster_seq` floor and so get nothing from the ratchet), but sends
    /// the owner back to the paper master that often. A long one honours "touch the master
    /// only to mint" and leaves a correspondingly long replay window. See
    /// `docs/SIGNING-KEY-DESIGN.md`.
    pub valid_until: String,
    /// The authorized machines. `[[machine]]` on the wire.
    #[serde(default, rename = "machine")]
    pub machines: Vec<Machine>,
    /// Withdrawn machine IDS — never public keys. A revoked id is refused before any
    /// artifact crypto, and an id never returns from the dead.
    ///
    /// Emitted AFTER `[[machine]]` would be invalid TOML (a scalar key following an array
    /// of tables belongs to the last table), so this is declared before it and serialized
    /// in that order.
    #[serde(default)]
    pub revoked: Vec<String>,
}

/// The attribution record: WHICH machine signed this release, proved by its key rather
/// than claimed by a label. Produced only by [`Roster::authorize_appcast`], i.e. only
/// after the signature verified, so every field here is authenticated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attribution {
    /// The signing machine's id — the answer to "which computer cut this?".
    pub machine_id: String,
    /// The signing machine's base64 public key. Public identity only; the secret never
    /// leaves the machine, is never logged, and is never journaled.
    pub pubkey_b64: String,
    /// The `roster_seq` of the roster that authorized it, so a recorded attribution can be
    /// matched back to the exact roster generation that granted the authority.
    pub roster_seq: u64,
}

impl std::fmt::Display for Attribution {
    /// The one-line human form recorded in status and the log: `m3 (roster seq 3)`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} (roster seq {})", self.machine_id, self.roster_seq)
    }
}

impl Roster {
    /// **Step 4–5.** Parse a roster from bytes that have already passed master
    /// verification, then check the structural invariants a signature cannot check.
    ///
    /// Taking `&VerifiedRoster` is what makes the ordering unrepresentable rather than
    /// merely tested: there is no way to call this on unverified bytes.
    pub fn parse(verified: &VerifiedRoster) -> Result<Self, RosterReject> {
        let text = std::str::from_utf8(verified.as_slice()).map_err(|_| RosterReject::Malformed)?;
        let roster: Roster = toml::from_str(text).map_err(|_| RosterReject::Malformed)?;
        // Reject-newer BEFORE any semantic use of the fields: a newer schema may mean
        // something different by them.
        if roster.schema > SUPPORTED_SCHEMA {
            return Err(RosterReject::Schema);
        }
        roster.validate()?;
        Ok(roster)
    }

    /// Structural invariants shared by parsing and by the minting tool's emission, so a
    /// roster this build would refuse can never be PRODUCED either.
    ///
    /// # The id → key map must be a BIJECTION, and both directions are load-bearing
    ///
    /// Duplicate IDS are the obvious half. The roster maps id → key, so two entries under
    /// one id would leave which key is authoritative up to iteration order, and the
    /// id-keyed deny-list could not name one without naming the other.
    ///
    /// Duplicate PUBKEYS are the half that is easy to miss and worse when it happens,
    /// because it defeats revocation — the single property this whole tier exists to
    /// provide, and, since the keyset stopped being an authorization input on the armed
    /// client path, the only authorization defence left. Authority is decided by KEY
    /// (`authorize_appcast` verifies against every live machine's key and reports the
    /// first that matches) while denial is expressed by ID. So one key listed under two
    /// ids means revoking either id withdraws nothing: `live()` drops the named entry,
    /// keeps its twin, and the same key goes on signing under the surviving name. The
    /// owner would have run `machine-revoke`, seen it succeed, and still be publishable
    /// by the machine they cut off.
    ///
    /// Refusing it here — in the client's own parser, over bytes the master signed — is
    /// what makes that unrepresentable rather than merely discouraged. `roster_ops::add`
    /// refuses it too, with a message; this is the backstop for a roster assembled by
    /// hand and re-signed from the paper master, which is the only way to produce one.
    pub fn validate(&self) -> Result<(), RosterReject> {
        if self.machines.len() > MAX_MACHINES {
            return Err(RosterReject::Malformed);
        }
        for (i, m) in self.machines.iter().enumerate() {
            if m.id.is_empty() || m.pubkey.is_empty() {
                return Err(RosterReject::Malformed);
            }
            // O(n²) over a list bounded by `MAX_MACHINES`, and deliberately so: a set
            // would need an allocation on a path that runs before the roster is trusted.
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

    /// **Step 6–7.** Admit the roster itself: the durable replay floor, then freshness.
    ///
    /// Both are cheap and both run before any artifact crypto. `floor_seq` is the highest
    /// `roster_seq` this client has ever durably recorded, and `now_unix` is injected so
    /// the whole gate stays pure and deterministic — the real clock is the caller's.
    pub fn admit(&self, floor_seq: u64, now_unix: i64) -> Result<(), RosterReject> {
        // STEP 6 — the ratchet. Strong for a client that has already seen a newer roster,
        // and worth exactly nothing to a brand-new install, which is why step 7 exists.
        if self.roster_seq < floor_seq {
            return Err(RosterReject::Rollback);
        }
        // STEP 7 — freshness. A `valid_until` we cannot parse is treated as LAPSED, not as
        // absent: an unreadable deadline must not become an unbounded one.
        let until = rfc3339_to_unix(&self.valid_until).ok_or(RosterReject::Stale)?;
        if now_unix >= until {
            return Err(RosterReject::Stale);
        }
        Ok(())
    }

    /// Whether `id` is on the deny-list. A cheap string compare, and the whole reason the
    /// grant and the deny live in one signed document.
    #[must_use]
    pub fn is_revoked(&self, id: &str) -> bool {
        self.revoked.iter().any(|r| r == id)
    }

    /// The named machine, iff it is listed, not revoked, and not itself expired.
    ///
    /// Revocation is checked FIRST and beats listing, deliberately: a producer bug that
    /// leaves a machine in both `[[machine]]` and `revoked` must resolve to REFUSED. The
    /// deny always wins.
    ///
    /// # Never reuse an id
    ///
    /// Because this lookup is id-keyed, re-minting under an existing id silently REPLACES
    /// that id's public key: a client still holding the old roster honours the old key
    /// while a client on the new one honours the new key — two live authorities under one
    /// name — and the deny-list, which names ids, cannot distinguish them. Reformatting a
    /// machine therefore means a NEW id plus a revocation of the old, and the minting tool
    /// refuses an id collision rather than leaving that to the docs.
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
            // Same fail-closed reading as the roster's own window: unparseable ⇒ lapsed.
            let until = rfc3339_to_unix(not_after).ok_or(RosterReject::Expired)?;
            if now_unix >= until {
                return Err(RosterReject::Expired);
            }
        }
        Ok(m)
    }

    /// The machines eligible to have signed anything right now: listed, not revoked, not
    /// expired. This is the set step 9 runs crypto against, and building it first is what
    /// keeps a revoked machine's key from ever reaching the verifier.
    #[must_use]
    pub fn live(&self, now_unix: i64) -> Vec<&Machine> {
        self.machines
            .iter()
            .filter(|m| self.machine(&m.id, now_unix).is_ok())
            .collect()
    }

    /// **Step 8–9.** Verify the appcast's exact bytes under the roster's LIVE machines,
    /// and report which one signed.
    ///
    /// # Why this tries the set instead of looking the machine up first
    ///
    /// The signing machine's id lives INSIDE the appcast, so it cannot be read before the
    /// appcast is verified — and reading it first would be a parse of unverified bytes,
    /// which is precisely the thing the whole design refuses to do. So the crypto runs
    /// against the authorized set (bounded by [`MAX_MACHINES`]), and the label the
    /// manifest carries is bound to the result afterwards by [`Attribution::bind`], over
    /// bytes that are by then authenticated.
    ///
    /// Excluding revoked and expired machines happens BEFORE any of that, so a revoked
    /// machine's otherwise-valid signature is never even checked — the ordering property
    /// `atpkg::sig::verify_pkg` established, adapted from "did the index revoke the key it
    /// also delegates" to "is the machine that signed this artifact on the deny-list".
    pub fn authorize_appcast(
        &self,
        appcast: &[u8],
        sig: &[u8],
        now_unix: i64,
    ) -> Result<Attribution, RosterReject> {
        // Cheap, local, once: a wrong-length signature is BadSig regardless of the roster.
        if sig.len() != 64 {
            return Err(RosterReject::BadSig);
        }
        let live = self.live(now_unix);
        if live.is_empty() {
            // Every machine is revoked or expired. Refuse — an empty authorized set
            // authorizes nothing, and must never degrade to "unsigned is fine".
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
                // A machine whose recorded pubkey is malformed can neither grant nor deny.
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
    /// **Step 10.** Bind the manifest's SELF-DECLARED identity to what actually verified.
    ///
    /// Both cross-checks are string/integer compares over already-authenticated bytes:
    ///
    /// * `machine_id` must be present (an unattributed release is refused under an armed
    ///   anchor — "I can track which computer does what" is a requirement) and must equal
    ///   the machine whose key verified, which is what stops a genuine signature from one
    ///   machine being relabelled as another;
    /// * `roster_seq` must equal the roster's, which stops an old roster being paired with
    ///   a new release or a new roster with an old one.
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
            Some(seq) if seq == self.roster_seq => Ok(()),
            // Absent is a mismatch, not a pass: a release cut without a roster_seq cannot
            // be paired with any particular roster generation, which is the whole point.
            _ => Err(RosterReject::SeqMismatch),
        }
    }
}

/// RFC3339 (`YYYY-MM-DDTHH:MM:SSZ`) → unix seconds, or `None` for anything this does not
/// recognise — which every caller treats as LAPSED, never as absent.
///
/// Deliberately local and deliberately strict. It is a near-twin of `atpkg::flow`'s
/// private copy; lifting one shared version into `aterm-types` would be the tidier move,
/// but that crate is outside this change's ownership and a date parser is not worth a
/// cross-crate edit to share. The duplication is 20 lines of pure arithmetic with a test
/// on each side, which is the cheap half of the tradeoff.
fn rfc3339_to_unix(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 19
        || b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
    {
        return None;
    }
    let y: i64 = s.get(0..4)?.parse().ok()?;
    let mo: i64 = s.get(5..7)?.parse().ok()?;
    let d: i64 = s.get(8..10)?.parse().ok()?;
    let h: i64 = s.get(11..13)?.parse().ok()?;
    let mi: i64 = s.get(14..16)?.parse().ok()?;
    let se: i64 = s.get(17..19)?.parse().ok()?;
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || se > 60 {
        return None;
    }
    let days = aterm_types::rfc3339::days_from_civil(y, mo, d);
    Some(days * 86400 + h * 3600 + mi * 60 + se)
}

impl Roster {
    /// `valid_until` as unix seconds, or `None` for a date the strict parser refuses.
    ///
    /// For TRANSCRIPTS and horizon warnings, not for gating: the freshness GATE is
    /// [`Roster::admit`], which treats an unparseable date as lapsed. This accessor
    /// exists so a producer can say "roster valid until X (N days)" on every cut
    /// without growing a second date parser — the silence between cuts is the only
    /// way the 180-day lapse ever arrives as an outage instead of a chore.
    #[must_use]
    pub fn valid_until_unix(&self) -> Option<i64> {
        rfc3339_to_unix(&self.valid_until)
    }

    /// Serialize to the published TOML shape — the producer half, used by the minting tool
    /// so the bytes the owner signs are produced by the same type the client parses.
    ///
    /// Validation runs FIRST: a roster this build would refuse must never be emitted,
    /// signed, and published only to be rejected by every client in the field.
    pub fn to_toml(&self) -> Result<String, RosterReject> {
        self.validate()?;
        toml::to_string(self).map_err(|_| RosterReject::Malformed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::signature::{Ed25519KeyPair, KeyPair};

    // Obviously synthetic seeds. These are constant byte-fills — they could not be
    // mistaken for a real key, and none of them ever appears in `pins.rs`.
    const MASTER_SEED: [u8; 32] = [0xA1; 32];
    const OTHER_MASTER_SEED: [u8; 32] = [0xA2; 32];
    const M3_SEED: [u8; 32] = [0xB1; 32];
    const M11_SEED: [u8; 32] = [0xB2; 32];
    const THIEF_SEED: [u8; 32] = [0xC1; 32];

    // 2026-08-04T00:00:00Z, comfortably inside the fixture's valid_until.
    const NOW: i64 = 1_785_801_600;

    fn kp(seed: &[u8; 32]) -> Ed25519KeyPair {
        Ed25519KeyPair::from_seed_unchecked(seed).expect("synthetic 32-byte seed")
    }

    fn pk(kp: &Ed25519KeyPair) -> String {
        STANDARD.encode(kp.public_key().as_ref())
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

    /// Emit + master-sign a roster, then take it back through the real client path:
    /// `verify_roster` → `Roster::parse`. Returns the parsed roster, so every test below
    /// exercises the bytes rather than the in-memory struct.
    fn through_the_chain(r: &Roster, master: &Ed25519KeyPair, anchors: &[&str]) -> Roster {
        let raw = r.to_toml().unwrap().into_bytes();
        let sig = sign(master, &raw);
        let verified = verify_roster(anchors, raw, &sig).expect("master-signed roster verifies");
        Roster::parse(&verified).expect("a valid roster parses")
    }

    const APPCAST: &[u8] = b"schema = 1\nversion = \"0.99\"\nmachine_id = \"m3\"\nroster_seq = 3\n";

    /// THE HAPPY PATH, end to end over real bytes: a master-signed roster verifies under
    /// the pinned master, admits, and an m3-signed appcast is attributed to m3.
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
        // The human-readable form is what lands in status/log.
        assert_eq!(a.to_string(), "m3 (roster seq 3)");
    }

    /// ATTRIBUTION IS NOT A LABEL. The very same appcast signed by m11 is attributed to
    /// m11 — the report follows the KEY, not the file's contents.
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
        // ...and because the appcast's bytes say `machine_id = "m3"`, the bind REFUSES.
        // This is the relabelling defence in both directions at once.
        assert_eq!(
            a.bind(Some("m3"), Some(3)),
            Err(RosterReject::UnknownMachine)
        );
    }

    /// THE WRONG MASTER. A roster signed by a different master is refused under the pinned
    /// anchor, and the chain never reaches a parse.
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

    /// A ONE-BYTE EDIT to the roster is refused: adding a machine to a genuine roster
    /// without the master cannot work.
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

    /// THE WRONG MACHINE KEY. A thief's key is not on the roster, so an appcast it signed
    /// verifies under nobody — even though the roster itself is perfectly genuine.
    #[test]
    fn an_appcast_signed_by_a_key_outside_the_roster_is_refused() {
        let master = kp(&MASTER_SEED);
        let anchor = pk(&master);
        let r = through_the_chain(&roster(), &master, &[&anchor]);
        assert_eq!(
            r.authorize_appcast(APPCAST, &sign(&kp(&THIEF_SEED), APPCAST), NOW),
            Err(RosterReject::Verify)
        );
    }

    /// EMPTY MEANS INERT. An unpinned master anchor authorizes NOTHING — not even a roster
    /// signed by a real master. There is no "accept anything" path.
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
        // The single-key primitive agrees, before any crypto.
        assert_eq!(verify_under("", &raw, &sig), Err(RosterReject::Disabled));
        // And an anchor list of nothing but empty/garbage members is BadKey — a broken
        // keyset must not read as "this roster's signature was bad".
        assert_eq!(
            verify_roster(&["", "!!not base64!!"], raw, &sig),
            Err(RosterReject::BadKey)
        );
    }

    /// REVOCATION, the case the retired design called unfixable. m11's key is genuine, was
    /// once legitimately authorized, and still signs perfectly — and is refused, before any
    /// crypto touches it, because the roster that names it also revokes it.
    #[test]
    fn a_revoked_machine_is_refused_though_its_signature_is_valid() {
        let master = kp(&MASTER_SEED);
        let anchor = pk(&master);
        let mut r = roster();
        r.roster_seq = 4;
        r.revoked = vec!["m11".into()];
        let r = through_the_chain(&r, &master, &[&anchor]);

        // Direct lookup: revoked beats listed.
        assert_eq!(r.machine("m11", NOW), Err(RosterReject::Revoked));
        // ...and its signature is never accepted, though it verifies mathematically.
        let sig = sign(&kp(&M11_SEED), APPCAST);
        assert_eq!(
            r.authorize_appcast(APPCAST, &sig, NOW),
            Err(RosterReject::Verify),
            "a revoked machine must not be in the candidate set at all"
        );
        // Negative control: m3, on the same roster, still works — revocation is targeted,
        // not a channel-wide brick.
        assert!(
            r.authorize_appcast(APPCAST, &sign(&kp(&M3_SEED), APPCAST), NOW)
                .is_ok()
        );
    }

    /// The deny-list is checked BEFORE the listing, so a producer bug that leaves a
    /// machine in both `[[machine]]` and `revoked` resolves to REFUSED. The deny wins.
    #[test]
    fn deny_beats_grant_when_a_roster_says_both() {
        let mut r = roster();
        r.revoked = vec!["m3".into()];
        assert_eq!(r.machine("m3", NOW), Err(RosterReject::Revoked));
    }

    /// REPLAY: the old roster (seq 3, still listing m11) carries a master signature that is
    /// valid FOREVER — signatures do not expire, documents do. A client that has durably
    /// seen seq 4 refuses it permanently.
    #[test]
    fn an_older_roster_is_refused_once_a_newer_one_has_been_seen() {
        let master = kp(&MASTER_SEED);
        let anchor = pk(&master);
        let old = through_the_chain(&roster(), &master, &[&anchor]); // seq 3
        // Its master signature still verifies — that is exactly why the floor exists.
        assert_eq!(old.roster_seq, 3);
        assert_eq!(old.admit(4, NOW), Err(RosterReject::Rollback));
        // Equal is allowed (the gate is seq >= floor), so a re-fetch of the current roster
        // is not mistaken for an attack.
        assert_eq!(old.admit(3, NOW), Ok(()));
    }

    /// FRESHNESS is the only thing a brand-new install has, since it has no floor. A lapsed
    /// roster is refused, and an unparseable deadline is treated as lapsed — never as
    /// absent, which would turn a typo into an unbounded window.
    #[test]
    fn a_lapsed_or_unreadable_valid_until_is_refused() {
        let mut r = roster();
        r.valid_until = "2026-01-01T00:00:00Z".into();
        assert_eq!(r.admit(0, NOW), Err(RosterReject::Stale));
        // The boundary itself: now == valid_until is already lapsed.
        let until = rfc3339_to_unix("2026-01-01T00:00:00Z").unwrap();
        assert_eq!(r.admit(0, until), Err(RosterReject::Stale));
        assert_eq!(r.admit(0, until - 1), Ok(()));
        r.valid_until = "not a date".into();
        assert_eq!(r.admit(0, NOW), Err(RosterReject::Stale));
    }

    /// A machine's own `not_after` ages it out without a revocation, and the expired
    /// machine leaves the candidate set entirely.
    #[test]
    fn an_expired_machine_ages_out_of_the_candidate_set() {
        let mut r = roster();
        r.machines[0].not_after = Some("2026-01-01T00:00:00Z".into());
        assert_eq!(r.machine("m3", NOW), Err(RosterReject::Expired));
        assert_eq!(
            r.authorize_appcast(APPCAST, &sign(&kp(&M3_SEED), APPCAST), NOW),
            Err(RosterReject::Verify)
        );
        // Before the deadline it is perfectly live — the gate is a date, not a brick.
        let before = rfc3339_to_unix("2025-12-31T23:59:59Z").unwrap();
        assert!(r.machine("m3", before).is_ok());
    }

    /// STEP 9's cross-check: an old roster cannot be paired with a new release, and an
    /// unattributed release is refused outright under an armed anchor.
    #[test]
    fn the_manifest_must_name_its_machine_and_its_roster_generation() {
        let a = Attribution {
            machine_id: "m3".into(),
            pubkey_b64: pk(&kp(&M3_SEED)),
            roster_seq: 3,
        };
        assert_eq!(a.bind(Some("m3"), Some(3)), Ok(()));
        assert_eq!(a.bind(None, Some(3)), Err(RosterReject::Unattributed));
        assert_eq!(a.bind(Some("m3"), None), Err(RosterReject::SeqMismatch));
        assert_eq!(a.bind(Some("m3"), Some(2)), Err(RosterReject::SeqMismatch));
        assert_eq!(
            a.bind(Some("m11"), Some(3)),
            Err(RosterReject::UnknownMachine)
        );
    }

    /// Reject-newer: a roster from a format this build does not understand is refused
    /// rather than misread, and the refusal is post-verify (it is not an oracle).
    #[test]
    fn a_newer_roster_schema_is_refused_not_misread() {
        let master = kp(&MASTER_SEED);
        let anchor = pk(&master);
        let mut r = roster();
        r.schema = SUPPORTED_SCHEMA + 1;
        // `to_toml` validates structure, not schema — the producer may legitimately emit a
        // newer schema for newer clients — so this reaches the client's gate.
        let raw = r.to_toml().unwrap().into_bytes();
        let sig = sign(&master, &raw);
        let v = verify_roster(&[&anchor], raw, &sig).unwrap();
        assert_eq!(Roster::parse(&v), Err(RosterReject::Schema));
    }

    /// Structural invariants a signature cannot check: duplicate ids (two live authorities
    /// under one name), empty ids, and an over-long roster. Refused on BOTH sides, so a
    /// roster the client would reject cannot be produced and published either.
    #[test]
    fn malformed_rosters_are_refused_on_parse_and_on_emission() {
        let mut dup = roster();
        dup.machines[1].id = "m3".into();
        assert_eq!(dup.validate(), Err(RosterReject::Malformed));
        assert_eq!(dup.to_toml(), Err(RosterReject::Malformed));

        let mut empty_id = roster();
        empty_id.machines[0].id = String::new();
        assert_eq!(empty_id.validate(), Err(RosterReject::Malformed));

        let mut empty_revoked = roster();
        empty_revoked.revoked = vec![String::new()];
        assert_eq!(empty_revoked.validate(), Err(RosterReject::Malformed));

        let mut huge = roster();
        huge.machines = (0..=MAX_MACHINES)
            .map(|i| Machine {
                id: i.to_string(),
                // DISTINCT keys, so this case tests the ceiling and only the ceiling. A
                // repeated key here would now be refused by the bijection rule below and
                // this assertion would pass for the wrong reason.
                pubkey: format!("key-{i}"),
                added_at: String::new(),
                not_after: None,
            })
            .collect();
        assert_eq!(huge.validate(), Err(RosterReject::Malformed));

        // The negative control that makes the four above non-vacuous.
        assert_eq!(roster().validate(), Ok(()));
    }

    /// ONE KEY UNDER TWO IDS DEFEATS REVOCATION, so no roster may contain one.
    ///
    /// Authority is decided by KEY — `authorize_appcast` verifies against every live
    /// machine's key — while denial is expressed by ID. A key listed twice therefore
    /// survives `machine-revoke` of either name: `live()` drops the revoked entry, keeps
    /// its twin, and the same key signs on under the surviving id. The owner would have
    /// revoked the machine, watched it succeed, and still be publishable by it.
    ///
    /// That matters more than it used to. With the master armed, the compiled-in keyset
    /// is no longer an authorization input on the client (`fetch_authoritative_release`
    /// branch B), so the roster's deny-list is the ONLY thing that can withdraw a
    /// machine's authority. A hole in it is a hole in the whole tier.
    ///
    /// Refused in the client's own parser rather than only in the minting tool, because
    /// the only way to produce such a document is to hand-edit one and re-sign it from
    /// the paper master — which the tool cannot intercept and the master's signature
    /// would otherwise make authoritative.
    ///
    /// Kills the mutation "check duplicate ids only" — under it, every assertion below
    /// flips.
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
        // Refused on emission too, so the shape cannot be published in the first place.
        assert_eq!(twinned.to_toml(), Err(RosterReject::Malformed));

        // AND THE REASON, demonstrated rather than asserted: revoking one name leaves the
        // other live and holding the same key. This is what the parse refusal prevents.
        let mut revoked_one = twinned.clone();
        revoked_one.revoked = vec![revoked_one.machines[0].id.clone()];
        let survivors = revoked_one.live(NOW);
        assert_eq!(survivors.len(), 1, "the twin outlives the revocation");
        assert_eq!(
            survivors[0].pubkey, shared,
            "and it holds the very key that was supposed to have been withdrawn"
        );

        // The negative control: the same roster with the keys left distinct is fine, so
        // the refusal above is about the collision and not about the fixture.
        assert_eq!(roster().validate(), Ok(()));
    }

    /// The wire round-trip: emit → master-sign → verify → parse reproduces the value
    /// exactly, so the bytes the owner signs are the bytes the client reads.
    #[test]
    fn the_roster_round_trips_through_emission_and_the_client_parser() {
        let master = kp(&MASTER_SEED);
        let anchor = pk(&master);
        let mut original = roster();
        original.revoked = vec!["m2".into()];
        original.machines[1].not_after = Some("2027-01-01T00:00:00Z".into());
        assert_eq!(through_the_chain(&original, &master, &[&anchor]), original);
    }

    /// A malformed anchor member can neither GRANT nor DENY: a good master beside it still
    /// verifies, and the reported index is the one that actually did the work — so a
    /// stalled master rotation is visible rather than silent.
    #[test]
    fn a_malformed_anchor_member_neither_grants_nor_denies() {
        let master = kp(&MASTER_SEED);
        let good = pk(&master);
        let raw = roster().to_toml().unwrap().into_bytes();
        let sig = sign(&master, &raw);
        let v = verify_roster(&["!!not base64!!", &good], raw.clone(), &sig).unwrap();
        assert_eq!(v.master_index(), 1);
        assert_eq!(v.as_slice(), raw.as_slice());
        // A wrong-length signature is BadSig once, before any member is tried.
        assert_eq!(
            verify_roster(&[&good, &good], raw, &[0u8; 10]),
            Err(RosterReject::BadSig)
        );
    }

    /// The date arithmetic the freshness gate rests on, pinned against hand-checked
    /// values — a wrong epoch here would silently widen or brick every window.
    #[test]
    fn rfc3339_parses_the_shapes_the_roster_uses() {
        assert_eq!(rfc3339_to_unix("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(rfc3339_to_unix("2026-08-04T00:00:00Z"), Some(NOW));
        assert_eq!(rfc3339_to_unix("2026-08-04T00:00:01Z"), Some(NOW + 1));
        for bad in [
            "",
            "2026-08-04",
            "2026-08-04 00:00:00Z",
            "2026-13-04T00:00:00Z",
            "2026-08-04T24:00:00Z",
            "20xx-08-04T00:00:00Z",
        ] {
            assert_eq!(rfc3339_to_unix(bad), None, "{bad:?} must not parse");
        }
    }
}
