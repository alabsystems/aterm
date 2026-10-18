// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Detached Ed25519 verification over the exact raw bytes of a manifest asset, enforced
//! before any parse by construction.
//!
//! # Raw bytes, verified before parse
//!
//! The signature covers the asset bytes as downloaded: no lossy UTF-8 conversion, no
//! re-serialization, no newline/BOM/whitespace normalization (TOML has no canonical form,
//! and re-serializing would open a parser-differential gap). Only on success are the same
//! bytes handed to the parser, as [`VerifiedBytes`], which only a verify can mint: the TOML
//! parser is attack surface and never sees unverified input.
//!
//! # One root: the paper master's roster
//!
//! atpkg's index is authorized by the same master-signed machine roster that authorizes an
//! aterm release (`aterm_update_core::roster`): one thing written on paper, one revocation
//! story.
//!
//! ```text
//!   PAPER MASTER  --signs-->  aterm-machines.toml  --names-->  m3, m11, …
//!                                                                 |
//!                                          m3 --signs--> index.toml
//!                                          m3 --signs--> pkg-<program>-<build>.toml
//! ```
//!
//! Machine-signed rather than master-signed because the master exists on no computer and is
//! typed in only to mint or revoke; an index re-cut whenever a pin moves would put the paper
//! phrase on the publishing laptop at every publish. Revocation takes a roster bump.
//!
//! # Empty means unpinned means inert
//!
//! An empty [`Anchor`] (a fork's fail-closed starting point; this build is armed)
//! authorizes no machine: [`admit_roster`] returns [`Reject::Disabled`] before any crypto,
//! [`crate::select_index`] selects nothing, and every install path ends in
//! `FlowError::NoIndex`. It never means "accept anything", and the tests assert it from
//! both directions.
//!
//! # Cheapest-first reject ordering
//!
//! Every gate fails closed with a [`Reject`], and the expensive crypto runs as late as it
//! can:
//!
//! 1. anchor empty ⇒ [`Reject::Disabled`].
//! 2. roster signature length (exactly 64 bytes).
//! 3. verify the roster under the pinned master keyset (crypto #1).
//! 4. parse the roster, only from `VerifiedRoster`.
//! 5. roster `schema` newer than this build understands ⇒ refuse rather than misread.
//! 6. `roster_seq` below the durable floor ⇒ [`Reject::Rollback`], the replay ratchet.
//! 7. roster `valid_until` lapsed ⇒ [`Reject::Stale`]. Every roster this tooling mints
//!    stamps 9999-12-31 (`crates/atpkg-keys/src/roster_ops.rs`), so a brand-new install,
//!    which carries no floor, has no replay defence at first contact; revocation answers
//!    a stolen key.
//! 8. revoked and expired machines leave the candidate set, before any artifact crypto.
//! 9. verify `index.toml` under the survivors (crypto #2).
//! 10. parse it, only from [`VerifiedBytes`], then bind its self-declared
//!     `machine_id`/`roster_seq` to what verified ([`Attribution::bind`]).
//!
//! [`check_freshness`] (the index's own window, gate 2, §8) and the [`Floor`] high-water
//! over `index_build` (gate 3, §8) are sequenced by the caller after all of the above.

use std::io;
use std::path::{Path, PathBuf};

use crate::platform::FileLock;
use aterm_update_core::roster::{Attribution, Roster, RosterReject, VerifiedRoster, verify_roster};

const MAX_FLOOR_BYTES: usize = 128;

/// Bytes that passed signature verification under a machine the pinned master's roster
/// authorizes. The field is private and there is no public constructor, so the only way to
/// get one is [`TrustedRoster::authorize_bytes`] (or [`TrustedRoster::authorize_index`],
/// [`TrustedIndex::verify_pkg`]); parsers take `&VerifiedBytes`, making "parse only after
/// verify" a compile-time guarantee. The derives add no constructor; the bytes are public
/// manifest content, not a secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedBytes(Vec<u8>);

impl VerifiedBytes {
    /// The verified raw bytes — the *same* bytes the signature was checked over, with
    /// no normalization or lossy conversion applied.
    #[must_use]
    pub fn as_slice(&self) -> &[u8] {
        &self.0
    }
}

/// The single opaque rejection set. Deliberately coarse: a verifier that reported a
/// *different* reason per failure mode would be a verification oracle. Callers map any
/// variant to "refuse, fail closed".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reject {
    /// No master is pinned (an unarmed build): the fail-closed default, returned before any
    /// crypto.
    Disabled,
    /// The public key did not base64-decode, or was not exactly 32 bytes.
    BadKey,
    /// The detached signature was not exactly 64 bytes.
    BadSig,
    /// The Ed25519 signature did not verify against the key over these bytes.
    Verify,
    /// The signing machine is on the roster's deny-list. Checked before any artifact
    /// crypto, so a revoked machine's mathematically valid signature is never verified.
    Revoked,
    /// The roster authorizes no machine that could have signed this: none is still live, or
    /// the document's own `machine_id` names a machine the roster does not list, or one other
    /// than the key that verified (a relabelling attempt).
    NotAuthorized,
    /// The signing machine's own `not_after` has lapsed — defence in depth, so a machine
    /// key ages out and is re-minted from paper even when nothing went wrong.
    Expired,
    /// The document carries no `machine_id`, so it cannot be attributed. Under an armed
    /// anchor an unattributed index is refused: attribution is a requirement.
    Unattributed,
    /// The document's `roster_seq` is newer than the authorizing roster's, or absent: an old
    /// roster paired with a new index. A newer roster with an older index is admitted
    /// (`aterm_update_core::roster::Attribution::bind`).
    SeqMismatch,
    /// The index's freshness window has lapsed (`now >= valid_until`), or the roster's
    /// has.
    Stale,
    /// A document below a durable high-water floor — an attempted rollback to an older,
    /// once-valid, still-validly-signed roster (`roster_seq`) or index (`index_build`).
    /// Signatures do not expire; documents do, which is why both ratchets exist.
    Rollback,
    /// Bytes that passed signature verification but are not valid UTF-8 or TOML, or miss a
    /// required field or repeat a key: a post-verify parse failure over authenticated bytes
    /// (`crate::manifest`), so not a verification oracle.
    Malformed,
    /// A document whose `schema` is newer than this build understands
    /// ([`crate::manifest::SUPPORTED_SCHEMA`]); refused rather than misread. Also
    /// post-verify, not a crypto oracle.
    Schema,
    /// A `pkg-*.toml` row spelled with a retired `kind` (today only `vendor-fetch`, split into
    /// `kind` and `protocol` before any client saw it). Post-verify, like [`Reject::Malformed`];
    /// it carries the new spelling so the authoring machine's own `atpkg install` names the
    /// fix.
    RetiredKind(&'static str),
    /// A `pkg-*.toml` whose `shim_env` list breaks a rule a shim can honour
    /// ([`crate::shim_env::ShimEnv::admit`]). Post-verify, like [`Reject::Malformed`]; it
    /// carries the entry and the reason so the publisher's own `atpkg verify-pkg` names the
    /// fix. Signed metadata, so only ever an authoring mistake; the whole manifest is refused.
    ShimEnv(String),
}

/// Translate the roster tier's verdict into atpkg's: one-to-one and total, with no catch-all,
/// so a new [`RosterReject`] variant is a compile error here.
fn from_roster(e: RosterReject) -> Reject {
    match e {
        RosterReject::Disabled => Reject::Disabled,
        RosterReject::BadKey => Reject::BadKey,
        RosterReject::BadSig => Reject::BadSig,
        RosterReject::Verify => Reject::Verify,
        RosterReject::Malformed => Reject::Malformed,
        RosterReject::Schema => Reject::Schema,
        RosterReject::Rollback => Reject::Rollback,
        RosterReject::Stale => Reject::Stale,
        RosterReject::SeqMismatch => Reject::SeqMismatch,
        RosterReject::Unattributed => Reject::Unattributed,
        RosterReject::UnknownMachine => Reject::NotAuthorized,
        RosterReject::Revoked => Reject::Revoked,
        RosterReject::Expired => Reject::Expired,
    }
}

/// What this build trusts, and how far its replay ratchet has turned.
///
/// The keyset is the paper master's ([`crate::PKG_TRUST_ANCHORS`], i.e.
/// `pins::PAPER_MASTER_PUBKEYS`), not a package-specific root. `roster_floor` is the highest
/// `roster_seq` this store has durably recorded (`<prefix>/roster.floor`).
///
/// An empty keyset is inert: [`Anchor::is_armed`] is false, [`admit_roster`] refuses before
/// any crypto, and `select_index` considers no candidate. No constructor yields an anchor
/// that accepts an arbitrary index: [`Anchor::pinned`] takes the committed keyset and
/// [`Anchor::of`] an explicit one (a fork's own master, or the tests').
#[derive(Debug, Clone)]
pub struct Anchor {
    masters: Vec<String>,
    /// The durable `roster_seq` high-water this client has already accepted.
    pub roster_floor: u64,
}

impl Anchor {
    /// The anchor this binary ships with: the committed paper-master keyset plus the caller's
    /// durable roster floor. This tree's keyset is armed; an empty one (a fork's starting
    /// point) is inert until an operator arms it in a reviewed commit.
    #[must_use]
    pub fn pinned(roster_floor: u64) -> Self {
        Self::of(
            crate::PKG_TRUST_ANCHORS
                .iter()
                .map(|k| (*k).to_string())
                .collect(),
            roster_floor,
        )
    }

    /// An anchor over an explicit master keyset.
    #[must_use]
    pub fn of(masters: Vec<String>, roster_floor: u64) -> Self {
        Self {
            masters,
            roster_floor,
        }
    }

    /// Whether any master is pinned. False is the fail-closed state. An empty or malformed
    /// member is skipped by `verify_roster` (which reports `BadKey` if none was usable), so
    /// armedness is the slice being non-empty, the same rule as `aterm-update`'s
    /// `RosterPolicy`.
    #[must_use]
    pub fn is_armed(&self) -> bool {
        !self.masters.is_empty()
    }

    /// The keyset as `verify_roster` wants it.
    fn keys(&self) -> Vec<&str> {
        self.masters.iter().map(String::as_str).collect()
    }
}

/// A roster generation that verified under the pinned master and passed admission: proof
/// steps 1–7 ran.
///
/// `now_unix` is captured at admission so a whole channel apply sees one clock reading and
/// no machine expires halfway through a coherence group. That makes it valid for one apply
/// only: nothing here re-reads a clock, so a holder kept past the roster's `valid_until` or
/// a machine's `not_after` would keep authorizing. Every caller uses one within a single CLI
/// command; a long-lived holder must re-admit with a current clock.
#[derive(Debug, Clone)]
pub struct TrustedRoster {
    roster: Roster,
    now_unix: i64,
}

/// Steps 1–7: verify the roster's exact bytes under the pinned master, parse it only from
/// `VerifiedRoster`, and admit it against the replay floor and its freshness window. Every
/// failure is a refusal; an unarmed anchor refuses before any crypto.
pub fn admit_roster(
    anchor: &Anchor,
    raw: Vec<u8>,
    sig: &[u8],
    now_unix: i64,
) -> Result<TrustedRoster, Reject> {
    // Step 1, the direction that must never invert. `verify_roster` refuses an empty keyset
    // too; stated here because skipping and accepting every candidate are one negated
    // condition apart.
    if !anchor.is_armed() {
        return Err(Reject::Disabled);
    }
    // Steps 2–3: signature length, then the master crypto.
    let verified: VerifiedRoster = verify_roster(&anchor.keys(), raw, sig).map_err(from_roster)?;
    // Steps 4–5: parse only from the verified wrapper, with the reject-newer schema gate.
    let roster = Roster::parse(&verified).map_err(from_roster)?;
    // Steps 6–7: the ratchet, then freshness; an unreadable `valid_until` counts as lapsed.
    roster
        .admit(anchor.roster_floor, now_unix)
        .map_err(from_roster)?;
    Ok(TrustedRoster { roster, now_unix })
}

impl TrustedRoster {
    /// This generation's `roster_seq` — what the caller ratchets the durable floor to.
    #[must_use]
    pub fn seq(&self) -> u64 {
        self.roster.roster_seq
    }

    /// Steps 8–9: verify one artifact's exact bytes under this roster's live machines;
    /// revoked and expired ones leave the set before any crypto. Reuses
    /// `Roster::authorize_appcast` (a signature check over opaque bytes) so atpkg and the
    /// updater share one gate.
    fn authorize(&self, bytes: &[u8], sig: &[u8]) -> Result<Attribution, Reject> {
        self.roster
            .authorize_appcast(bytes, sig, self.now_unix)
            .map_err(from_roster)
    }

    /// [`Self::authorize`], minting the [`VerifiedBytes`] the post-verify parsers consume:
    /// the one place bytes become parseable, since `index.toml` and every `pkg-*.toml` are
    /// authorized by the same rule.
    pub fn authorize_bytes(
        &self,
        raw: Vec<u8>,
        sig: &[u8],
    ) -> Result<(VerifiedBytes, Attribution), Reject> {
        let who = self.authorize(&raw, sig)?;
        // The one place `VerifiedBytes` is minted.
        Ok((VerifiedBytes(raw), who))
    }

    /// Steps 8–10 for `index.toml`: verify under the live machines, parse only from the
    /// resulting [`VerifiedBytes`], then bind the index's own `machine_id` / `roster_seq` to
    /// what verified. `machine_id` is inside the signed bytes, so a genuine m3 signature
    /// cannot be relabelled m11, and m11's key cannot claim m3, whose key the roster names.
    pub fn authorize_index(
        &self,
        raw: Vec<u8>,
        sig: &[u8],
    ) -> Result<(TrustedIndex, VerifiedBytes), Reject> {
        // (9) The artifact crypto, under survivors only.
        let (verified, who) = self.authorize_bytes(raw, sig)?;
        let index = crate::manifest::parse_index(&verified)?;
        // (10) The id bind, over authenticated bytes.
        who.bind(index.machine_id.as_deref(), index.roster_seq)
            .map_err(from_roster)?;
        Ok((
            TrustedIndex {
                index,
                roster: self.clone(),
                attribution: who,
            },
            verified,
        ))
    }
}

/// An `index.toml` a master-signed roster generation authorized, paired with that
/// generation: the authority every `pkg-*.toml` under it is verified against. Derefs to
/// [`crate::Index`].
#[derive(Debug, Clone)]
pub struct TrustedIndex {
    index: crate::manifest::Index,
    roster: TrustedRoster,
    attribution: Attribution,
}

impl std::ops::Deref for TrustedIndex {
    type Target = crate::manifest::Index;
    fn deref(&self) -> &Self::Target {
        &self.index
    }
}

impl TrustedIndex {
    /// Which machine signed this index, proved by its key rather than claimed by a label.
    #[must_use]
    pub fn attribution(&self) -> &Attribution {
        &self.attribution
    }

    /// The `roster_seq` that authorized it — what the caller ratchets the durable roster
    /// floor to once the index has actually been used.
    #[must_use]
    pub fn roster_seq(&self) -> u64 {
        self.roster.seq()
    }

    /// The generation this index declares inside its own signed bytes.
    ///
    /// Not [`Self::roster_seq`], the generation of the roster it was served with:
    /// `Attribution::bind` admits `index.roster_seq <= roster.roster_seq`, so an old index
    /// verifies beside a newer roster, and whoever assembles a candidate picks the pair.
    /// Anything that waives a durable floor must ask this one, or re-pairing the owner's own
    /// old index with the owner's new roster re-bases the floor downward without forging
    /// anything. An index can declare only a generation the master has minted, so the
    /// master's rescue lever is unaffected.
    #[must_use]
    pub fn authorizing_seq(&self) -> u64 {
        // An index that declares no generation cannot waive a generation floor: 0 is never
        // strictly greater than a recorded floor.
        self.index.roster_seq.unwrap_or(0)
    }

    /// Verify a `pkg-<program>-<build>.toml`'s raw bytes under the roster generation that
    /// authorized this index: any listed, unrevoked, unexpired machine. A revoked machine's
    /// manifest is refused without its signature being checked.
    pub fn verify_pkg(&self, raw: Vec<u8>, sig: &[u8]) -> Result<VerifiedBytes, Reject> {
        self.roster.authorize_bytes(raw, sig).map(|(v, _)| v)
    }
}

/// Genuinely signed [`VerifiedBytes`] for other modules' tests: a synthetic paper master and
/// roster and a real machine signature through the real chain, so no test needs a
/// `pub(crate)` constructor that would retire the guarantee. Test-only.
#[cfg(test)]
pub(crate) mod testkit {
    use super::{Anchor, TrustedRoster, VerifiedBytes, admit_roster};
    use aterm_update_core::roster::{Machine, Roster};
    use ring::signature::{Ed25519KeyPair, KeyPair};

    /// Obviously synthetic, and shared with `sig`'s own tests so one fixture describes
    /// the whole crate's notion of "the owner".
    pub(crate) const MASTER_SEED: [u8; 32] = [0xA1; 32];
    /// The one machine on the test roster.
    pub(crate) const MACHINE_SEED: [u8; 32] = [0xB1; 32];
    /// A key that is on NO roster — the attacker.
    pub(crate) const OUTSIDER_SEED: [u8; 32] = [0xC1; 32];
    /// 2026-08-04T00:00:00Z.
    pub(crate) const NOW: i64 = 1_785_801_600;
    /// The roster generation every fixture below publishes at.
    pub(crate) const SEQ: u64 = 3;
    /// The machine id every fixture below signs as.
    pub(crate) const MACHINE_ID: &str = "m3";

    pub(crate) fn keypair(seed: &[u8; 32]) -> Ed25519KeyPair {
        Ed25519KeyPair::from_seed_unchecked(seed).expect("synthetic 32-byte seed")
    }

    pub(crate) fn pubkey_b64(seed: &[u8; 32]) -> String {
        aterm_codec::base64::encode(keypair(seed).public_key().as_ref()).expect("32-byte key")
    }

    pub(crate) fn sign(seed: &[u8; 32], msg: &[u8]) -> Vec<u8> {
        keypair(seed).sign(msg).as_ref().to_vec()
    }

    /// The one-machine roster the whole crate's tests publish under.
    pub(crate) fn roster() -> Roster {
        Roster {
            schema: 1,
            roster_seq: SEQ,
            // Far out, so a lapsed roster never masks the gate under test; staleness is
            // tested with an explicit clock.
            valid_until: "2099-01-01T00:00:00Z".into(),
            machines: vec![Machine {
                id: MACHINE_ID.into(),
                pubkey: pubkey_b64(&MACHINE_SEED),
                added_at: "2026-08-04T00:00:00Z".into(),
                not_after: None,
            }],
            revoked: vec![],
        }
    }

    /// The roster's published bytes + the paper master's detached signature.
    pub(crate) fn published_roster() -> (Vec<u8>, Vec<u8>) {
        let bytes = roster()
            .to_toml()
            .expect("a valid roster emits")
            .into_bytes();
        let sig = sign(&MASTER_SEED, &bytes);
        (bytes, sig)
    }

    /// An anchor armed with the synthetic paper master, floor 0.
    pub(crate) fn anchor() -> Anchor {
        Anchor::of(vec![pubkey_b64(&MASTER_SEED)], 0)
    }

    /// The admitted roster generation, through the real chain.
    pub(crate) fn trusted_roster() -> TrustedRoster {
        let (bytes, sig) = published_roster();
        admit_roster(&anchor(), bytes, &sig, NOW).expect("the synthetic chain admits")
    }

    /// The index `raw`, signed by the test machine and authorized under the test roster
    /// re-issued at generation `seq` — the real chain end to end. Panics if the chain
    /// refuses.
    pub(crate) fn trusted_index_at(seq: u64, raw: Vec<u8>) -> super::TrustedIndex {
        let bytes = Roster {
            roster_seq: seq,
            ..roster()
        }
        .to_toml()
        .expect("a valid roster emits")
        .into_bytes();
        let roster_sig = sign(&MASTER_SEED, &bytes);
        let sig = sign(&MACHINE_SEED, &raw);
        admit_roster(&anchor(), bytes, &roster_sig, NOW)
            .expect("the synthetic chain admits")
            .authorize_index(raw, &sig)
            .expect("the roster's own machine signed this index")
            .0
    }

    /// `raw`, machine-signed and taken through the real authorization path. Panics if the
    /// chain refuses.
    pub(crate) fn machine_signed(raw: Vec<u8>) -> VerifiedBytes {
        let sig = sign(&MACHINE_SEED, &raw);
        trusted_roster()
            .authorize_bytes(raw, &sig)
            .expect("the roster's own machine signed these bytes")
            .0
    }
}

/// The durable anti-rollback floor over `index.toml`, and the roster generation that set it.
///
/// `index_build` is a number a machine chooses, so one rostered machine (a stolen key) could
/// publish `index_build = 9223372036854775807` and raise a monotonic floor above anything
/// the owner will publish, filtering out every later index, including the one carrying its
/// revocation. So the floor binds only within the generation that set it, and a strictly
/// newer master-signed generation re-bases it ([`Floor::rebase`]): the paper master's rescue
/// lever, since only the master mints a generation.
///
/// An equal or older generation is still floored, so a missing or corrupt `floor.gen`
/// (reading `0`) waives nothing: the floor binds at generation 0, which no real roster
/// carries, and the first genuine generation re-bases it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BuildFloor {
    /// The highest `index_build` durably recorded (`<prefix>/floor`).
    pub index_build: u64,
    /// The roster generation the index above was authorized by (`<prefix>/floor.gen`).
    pub roster_seq: u64,
}

impl BuildFloor {
    /// A first-contact floor: nothing recorded, belonging to no generation.
    #[must_use]
    pub fn none() -> Self {
        Self::default()
    }

    /// Whether an index at `index_build`, authorized by roster generation `roster_seq`,
    /// clears this floor. Only a strictly newer generation waives it: `!=` would waive it for
    /// an older one too, letting a replayed index in.
    #[must_use]
    pub fn admits(&self, roster_seq: u64, index_build: u64) -> bool {
        if roster_seq > self.roster_seq {
            return true;
        }
        index_build >= self.index_build
    }
}

/// Freshness gate (gate 2, §8): an index is fresh iff `now_unix < valid_until_unix`. Pure:
/// the caller supplies the clock and the converted `valid_until`. A lapsed window is
/// [`Reject::Stale`].
pub fn check_freshness(now_unix: i64, valid_until_unix: i64) -> Result<(), Reject> {
    if now_unix >= valid_until_unix {
        Err(Reject::Stale)
    } else {
        Ok(())
    }
}

/// A durable, anti-rollback high-water mark over the index's monotonic `index_build`
/// (gate 3, §8). The highest seen build is persisted to a `0644`, owned-by-uid file
/// under a private directory; any index with a *lower* `index_build` is rejected, so an
/// attacker who can pin a client to an older signed index cannot roll it back below what
/// it has already durably seen.
pub struct Floor {
    path: PathBuf,
}

impl Floor {
    /// A floor backed by `path`. Its parent directory must be private (owned by this uid,
    /// not group/other-writable) at write time; the file is published `0644`.
    #[must_use]
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// The recorded high-water floor (`0` if none or unreadable), for a caller that filters
    /// on it (§5) before advancing it with [`Self::check_and_record`].
    #[must_use]
    pub fn current(&self) -> u64 {
        self.read_floor()
    }

    /// Reject any `index_build` below the recorded floor ([`Reject::Rollback`]); on accept,
    /// durably advance the floor to `max(index_build, recorded)`.
    ///
    /// First contact (no recorded floor) reads as `0` and accepts, the residual the §8
    /// freshness gate bounds. A failed persist never turns a passed check into a reject (that
    /// would wedge the store), but it is reported on stderr: until a later run persists, the
    /// same generation is accepted again.
    pub fn check_and_record(&self, index_build: u64) -> Result<(), Reject> {
        let (decision, persist_failure) = self.check_and_record_classified(index_build);
        if let Some(error) = persist_failure {
            eprintln!(
                "atpkg: accepted build {index_build} but could not persist the rollback \
                 floor to {}: {error} — until a later run persists, a replay of this \
                 generation will be accepted again",
                self.path.display()
            );
        }
        decision
    }

    /// [`Self::check_and_record`] with the persist failure returned instead of printed, so
    /// tests can observe durability apart from the decision.
    pub(crate) fn check_and_record_classified(
        &self,
        index_build: u64,
    ) -> (Result<(), Reject>, Option<String>) {
        // Serialize read -> compare -> write across processes (CWE-362): two processes that
        // both read floor F before either writes could otherwise regress the floor below the
        // higher of their builds. If the lock cannot be taken this degrades to best-effort,
        // never a false reject.
        let _lock = self.acquire_file_lock();
        let (recorded, corrupt) = self.read_floor_reported();
        if index_build < recorded {
            return (Err(Reject::Rollback), None);
        }
        // Nothing to persist: skip the rewrite (temp, two syncs, rename; on Apple targets
        // each sync is a full drive flush), the common case on every pass over an unchanged
        // index. Taken only when the file already holds a value at or above this one, so the
        // durable floor is what the write would have left; the skip gives up only a retry of
        // an earlier rename's best-effort directory sync. Two cases still write: a `corrupt`
        // file (it read as the fail-open `0`, and the ratchet must re-arm), and a floor not
        // yet `0644`, which a non-root reader under a system prefix reads as first contact
        // (docs/AUDIT-nux-first-open-toolchain-2026-08-31.md).
        if !corrupt && index_build <= recorded && self.is_published() {
            return (Ok(()), None);
        }
        // Durable advance under the lock, never downward; a failure is reported, never a
        // reject.
        let persist_failure = self
            .write(index_build.max(recorded))
            .err()
            .map(|e| e.to_string());
        (Ok(()), persist_failure)
    }

    /// Write `value` unconditionally, including downward: the one non-monotonic operation.
    ///
    /// Only the build-floor re-base a strictly newer master-signed generation earns may call
    /// it ([`BuildFloor`]): a floor set by a now-revoked machine must not outlive the
    /// generation that revoked it. The caller must already hold `admit_roster`'s acceptance of
    /// a generation above the one that recorded the floor; nothing an index or a machine says
    /// reaches it. Best-effort; a failed re-base leaves the old floor, the fail-closed
    /// direction.
    pub fn rebase(&self, value: u64) {
        let _lock = self.acquire_file_lock();
        if let Err(error) = self.write(value) {
            // Fail-closed (the old, higher floor keeps refusing), but a rescue that did not
            // land is said.
            eprintln!(
                "atpkg: could not re-base the floor at {} to {value}: {error}",
                self.path.display()
            );
        }
    }

    /// The advisory lock over [`Self::check_and_record`]'s critical section: `LOCK_EX` on a
    /// sibling `<floor>.lock` (created `0600`), released on drop. `None` when it cannot be
    /// taken; callers then go on best-effort, still rollback-safe but not strictly monotonic
    /// under concurrency.
    fn acquire_file_lock(&self) -> Option<FileLock> {
        let name = self
            .path
            .file_name()
            .and_then(std::ffi::OsStr::to_str)
            .unwrap_or("floor");
        let lockpath = self.path.with_file_name(format!("{name}.lock"));
        FileLock::acquire(&lockpath).ok()
    }

    /// The recorded floor, or `0` if the file is missing or unparseable (fail-open only for
    /// first contact, per §8). A file that exists but will not read as a floor is reported
    /// on stderr and still reads `0`: refusing would let anyone who can scribble on it wedge
    /// the store, and the system never writes a malformed floor (temp+rename is atomic).
    fn read_floor(&self) -> u64 {
        self.read_floor_reported().0
    }

    /// [`Self::read_floor`]'s value and its corruption verdict, for the caller that must
    /// rewrite a corrupt file even when the value recorded advances nothing.
    fn read_floor_reported(&self) -> (u64, bool) {
        let (value, corrupt) = self.read_floor_classified();
        if corrupt {
            eprintln!(
                "atpkg: the rollback floor at {} exists but is unreadable or not a \
                 number — treating it as first contact (0); older signed documents will \
                 be accepted again until the ratchet re-advances",
                self.path.display()
            );
        }
        (value, corrupt)
    }

    /// Whether the floor file is already published at the mode every reader needs
    /// (`0644` — see [`Self::write`]). `false` when it cannot be stat'd at all, so an
    /// absent floor always takes the write path. A platform without POSIX bits reports
    /// `0` from `permission_mode` and has nothing to migrate, so it reads as published.
    fn is_published(&self) -> bool {
        match std::fs::symlink_metadata(&self.path) {
            Ok(meta) => {
                let mode = crate::platform::permission_mode(&meta) & 0o777;
                mode == 0 || mode == 0o644
            }
            Err(_) => false,
        }
    }

    /// `(recorded floor, corrupt)`: `corrupt` is true iff the file exists but did not yield a
    /// `u64`, which the value alone cannot tell from first contact.
    pub(crate) fn read_floor_classified(&self) -> (u64, bool) {
        match crate::metadata_io::read_bounded_regular_utf8(&self.path, MAX_FLOOR_BYTES) {
            Ok(text) => match text.trim().parse::<u64>() {
                Ok(value) => (value, false),
                Err(_) => (0, true),
            },
            // Read failure: absent is genuine first contact; present-but-unreadable
            // (oversized, non-regular, non-UTF-8, EACCES) is the corrupt class.
            Err(_) => (0, std::fs::symlink_metadata(&self.path).is_ok()),
        }
    }

    /// Atomically publish `value` to the floor file: refuse a parent dir that is not private
    /// (CWE-379: another local user could pre-create or swap the file), then write a sibling
    /// `0600` temp, publish it `0644` and `rename` it over the target.
    fn write(&self, value: u64) -> io::Result<()> {
        let parent = self.path.parent().unwrap_or_else(|| Path::new("."));
        let meta = std::fs::metadata(parent)?;
        if !crate::platform::dir_meta_is_private(&meta) {
            let uid = crate::platform::our_uid();
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "{}: high-water floor directory must be owned by uid {uid} and not \
                     group/other-writable",
                    parent.display()
                ),
            ));
        }

        let tmp = self.path.with_file_name(format!(
            "{}.{}.tmp",
            self.path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("floor"),
            std::process::id()
        ));
        // One exit for the whole staged sequence, so no failure skips the cleanup below.
        if let Err(error) = self.stage_and_publish(&tmp, value) {
            // Reported, never fatal, and run on every accepted pass: a persist that keeps
            // failing (ENOSPC) would otherwise leave a `<floor>.<pid>.tmp` per pid beside each
            // floor, a name no other sweep matches. The removal itself is best-effort.
            let _ = std::fs::remove_file(&tmp);
            return Err(error);
        }
        // Best-effort directory sync so the rename itself survives a power loss. The
        // floor value is already safe either way (old value or new, never torn).
        crate::store::sync_dir(parent);
        Ok(())
    }

    /// Stage `value` into `tmp` (`0600`, durably synced), publish it `0644`, and `rename` it
    /// over the floor. Split out of [`Self::write`] so the sequence has one failure exit,
    /// where the temp is removed.
    fn stage_and_publish(&self, tmp: &Path, value: u64) -> io::Result<()> {
        use std::io::Write as _;

        {
            let mut f = crate::platform::open_create_write(tmp, 0o600)?;
            f.write_all(value.to_string().as_bytes())?;
            // Durable, not merely renamed: after a power loss a rename of unsynced bytes can
            // expose an empty file, which reads as first contact.
            crate::platform::sync_durable(&f)?;
        }
        // Published world-readable (0644): the value is a high-water mark, not a secret, and
        // under a root-owned system prefix a 0600 floor reads as "first contact (0)" for
        // every non-root user (docs/AUDIT-nux-first-open-toolchain-2026-08-31.md). Write
        // authority rests on ownership and the `dir_meta_is_private` check.
        crate::platform::set_mode(tmp, 0o644)?;
        std::fs::rename(tmp, &self.path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_update_core::roster::Machine;
    use ring::signature::{Ed25519KeyPair, KeyPair};
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    // Obviously synthetic seeds — constant byte-fills that could not be mistaken for a
    // real key, and none of which appears anywhere in `pins.rs`.
    const MASTER_SEED: [u8; 32] = [0xA1; 32];
    const OTHER_MASTER_SEED: [u8; 32] = [0xA2; 32];
    const M3_SEED: [u8; 32] = [0xB1; 32];
    const M11_SEED: [u8; 32] = [0xB2; 32];
    const THIEF_SEED: [u8; 32] = [0xC1; 32];

    /// 2026-08-04T00:00:00Z — comfortably inside every fixture window below.
    const NOW: i64 = 1_785_801_600;

    fn keypair(seed: &[u8; 32]) -> Ed25519KeyPair {
        Ed25519KeyPair::from_seed_unchecked(seed).expect("synthetic 32-byte seed")
    }

    fn pubkey_b64(kp: &Ed25519KeyPair) -> String {
        aterm_codec::base64::encode(kp.public_key().as_ref()).expect("32-byte key")
    }

    fn sign(seed: &[u8; 32], msg: &[u8]) -> Vec<u8> {
        keypair(seed).sign(msg).as_ref().to_vec()
    }

    /// A two-machine roster at `roster_seq = 3`: m3 and m11 both live.
    fn roster() -> Roster {
        Roster {
            schema: 1,
            roster_seq: 3,
            valid_until: "2027-02-01T00:00:00Z".into(),
            machines: vec![
                Machine {
                    id: "m3".into(),
                    pubkey: pubkey_b64(&keypair(&M3_SEED)),
                    added_at: "2026-08-04T00:00:00Z".into(),
                    not_after: None,
                },
                Machine {
                    id: "m11".into(),
                    pubkey: pubkey_b64(&keypair(&M11_SEED)),
                    added_at: "2026-08-04T00:00:00Z".into(),
                    not_after: None,
                },
            ],
            revoked: vec![],
        }
    }

    /// The roster as it is published: emitted bytes + the paper master's detached
    /// signature over exactly those bytes.
    fn published(r: &Roster, master_seed: &[u8; 32]) -> (Vec<u8>, Vec<u8>) {
        let bytes = r.to_toml().expect("a valid roster emits").into_bytes();
        let sig = sign(master_seed, &bytes);
        (bytes, sig)
    }

    /// An anchor armed with the real paper master, at roster floor `floor`.
    fn armed(floor: u64) -> Anchor {
        Anchor::of(vec![pubkey_b64(&keypair(&MASTER_SEED))], floor)
    }

    /// The published roster, taken through the real client path.
    fn admit(anchor: &Anchor, r: &Roster) -> Result<TrustedRoster, Reject> {
        let (bytes, sig) = published(r, &MASTER_SEED);
        admit_roster(anchor, bytes, &sig, NOW)
    }

    /// An `index.toml` body naming `machine_id` / `roster_seq`, the attribution step 10 binds.
    fn index_body(machine_id: &str, roster_seq: u64, index_build: u64) -> Vec<u8> {
        let mut s = String::from("schema = 2\nindex_build = ");
        s.push_str(&index_build.to_string());
        s.push_str("\nvalid_until = \"2027-02-01T00:00:00Z\"\nmachine_id = \"");
        s.push_str(machine_id);
        s.push_str("\"\nroster_seq = ");
        s.push_str(&roster_seq.to_string());
        s.push_str("\n[programs.ay]\nrepo = \"ay\"\n");
        s.into_bytes()
    }

    const PKG: &[u8] = b"schema = 2\nprogram = \"ay\"\nbuild_number = 18\n";

    fn private_tmp_dir(label: &str) -> crate::testdir::Dir {
        crate::testdir::dir(&format!("atpkg-{label}-"))
    }

    // The dangerous direction first.

    /// An unarmed anchor authorizes nothing, not even a roster genuinely signed by a real
    /// paper master, and refuses before any crypto. Flip `!anchor.is_armed()` in
    /// `admit_roster` and the first assertion fails.
    #[test]
    fn an_unarmed_anchor_is_inert_and_authorizes_nothing() {
        let unarmed = Anchor::of(vec![], 0);
        assert!(
            !unarmed.is_armed(),
            "precondition: this anchor really does pin nothing"
        );
        let (bytes, sig) = published(&roster(), &MASTER_SEED);
        assert_eq!(
            admit_roster(&unarmed, bytes.clone(), &sig, NOW).err(),
            Some(Reject::Disabled),
            "an unpinned anchor must REFUSE, never wave through"
        );
        // The same bytes and signature are accepted once a master is pinned, so the
        // refusal above is the anchor, not the fixture.
        assert!(admit_roster(&armed(0), bytes, &sig, NOW).is_ok());
    }

    // The happy path, and the ways it is not.

    /// An index signed by a machine the roster names is accepted, attributed to that machine.
    #[test]
    fn an_index_signed_by_a_roster_named_machine_is_accepted() {
        let anchor = armed(0);
        let roster = admit(&anchor, &roster()).expect("master-signed, fresh, above the floor");
        assert_eq!(roster.seq(), 3);

        let raw = index_body("m3", 3, 41);
        let sig = sign(&M3_SEED, &raw);
        let (index, verified) = roster
            .authorize_index(raw.clone(), &sig)
            .expect("m3 is live and signed it");
        assert_eq!(index.index_build, 41, "Deref reaches the parsed index");
        assert_eq!(index.attribution().machine_id, "m3");
        assert_eq!(index.roster_seq(), 3);
        assert_eq!(
            verified.as_slice(),
            raw.as_slice(),
            "the verified bytes are the SAME bytes the signature covered"
        );
        // And the pkg tier rides the same roster generation: any live machine may sign.
        let pkg_sig = sign(&M11_SEED, PKG);
        assert_eq!(
            index
                .verify_pkg(PKG.to_vec(), &pkg_sig)
                .expect("m11 is live too")
                .as_slice(),
            PKG
        );
    }

    /// An index signed by a machine not on the roster is refused, though the roster is
    /// genuine and the signature valid.
    #[test]
    fn an_index_signed_by_a_machine_outside_the_roster_is_refused() {
        let anchor = armed(0);
        let roster = admit(&anchor, &roster()).unwrap();
        let raw = index_body("m3", 3, 41);
        let thief = sign(&THIEF_SEED, &raw);
        assert_eq!(
            roster.authorize_index(raw.clone(), &thief).err(),
            Some(Reject::Verify)
        );
        // The same bytes signed by a listed machine are accepted.
        assert!(
            roster
                .authorize_index(raw, &sign(&M3_SEED, &index_body("m3", 3, 41)))
                .is_ok()
        );
    }

    /// A revoked machine is refused with its signature never checked: it leaves the candidate
    /// set in `live()`, so the verdict is `Verify`, not `Revoked`. The control verifies the
    /// same bytes under a roster that does not revoke it.
    #[test]
    fn a_revoked_machine_is_refused_and_its_signature_is_never_verified() {
        let anchor = armed(0);
        let mut r = roster();
        r.roster_seq = 4;
        r.revoked = vec!["m11".into()];
        let revoking = admit(&anchor, &r).unwrap();

        let raw = index_body("m11", 4, 41);
        let sig = sign(&M11_SEED, &raw);
        assert_eq!(
            revoking.authorize_index(raw.clone(), &sig).err(),
            Some(Reject::Verify),
            "a revoked machine must not be in the candidate set at all"
        );
        // The pkg tier inherits the same exclusion — one deny-list, both documents.
        assert_eq!(
            revoking
                .authorize_bytes(PKG.to_vec(), &sign(&M11_SEED, PKG))
                .err(),
            Some(Reject::Verify)
        );

        // Control: identical bytes and signature under a generation that does not revoke
        // m11 are accepted, so the refusal above is the revocation.
        let mut same_but_live = roster();
        same_but_live.roster_seq = 4;
        let permissive = admit(&anchor, &same_but_live).unwrap();
        assert!(permissive.authorize_index(raw, &sig).is_ok());

        // Targeted, not a channel-wide brick: m3 on the revoking roster still works.
        let m3_raw = index_body("m3", 4, 41);
        assert!(
            revoking
                .authorize_index(m3_raw.clone(), &sign(&M3_SEED, &m3_raw))
                .is_ok()
        );
    }

    /// An expired machine ages out of the candidate set on its own schedule, with no
    /// revocation and no master touch.
    #[test]
    fn an_expired_machine_leaves_the_candidate_set() {
        let anchor = armed(0);
        let mut r = roster();
        r.machines[0].not_after = Some("2026-01-01T00:00:00Z".into());
        let roster = admit(&anchor, &r).unwrap();
        let raw = index_body("m3", 3, 41);
        assert_eq!(
            roster
                .authorize_index(raw, &sign(&M3_SEED, &index_body("m3", 3, 41)))
                .err(),
            Some(Reject::Verify)
        );
    }

    // The roster itself: missing, unverifiable, stale or rolled back is refused, with no
    // fallback to any other root.

    /// A roster signed by a different master is refused under the pinned one, and the
    /// chain never reaches a parse.
    #[test]
    fn a_roster_signed_by_the_wrong_master_is_refused() {
        let (bytes, sig) = published(&roster(), &OTHER_MASTER_SEED);
        assert_eq!(
            admit_roster(&armed(0), bytes, &sig, NOW).err(),
            Some(Reject::Verify)
        );
    }

    /// A one-byte edit anywhere in the roster is refused: no machine is added without the
    /// paper master.
    #[test]
    fn a_tampered_roster_is_refused_byte_by_byte() {
        let (bytes, sig) = published(&roster(), &MASTER_SEED);
        for i in 0..bytes.len() {
            let mut bad = bytes.clone();
            bad[i] ^= 0x01;
            assert_eq!(
                admit_roster(&armed(0), bad, &sig, NOW).err(),
                Some(Reject::Verify),
                "a flip at roster byte {i} must reject"
            );
        }
        // Unflipped, the same pair verifies.
        assert!(admit_roster(&armed(0), bytes, &sig, NOW).is_ok());
    }

    /// A rolled-back roster is refused once a newer generation is durably seen, though its
    /// master signature still verifies.
    #[test]
    fn a_rolled_back_roster_is_refused_by_the_durable_floor() {
        let old = roster(); // seq 3
        assert_eq!(
            admit(&armed(4), &old).err(),
            Some(Reject::Rollback),
            "a client that has seen seq 4 must never accept seq 3 again"
        );
        // Equal is allowed, so re-fetching the current roster is not read as an attack.
        assert!(admit(&armed(3), &old).is_ok());
    }

    /// A lapsed roster is refused, and an unreadable `valid_until` counts as lapsed, not
    /// absent.
    #[test]
    fn a_lapsed_or_unreadable_roster_window_is_refused() {
        let mut r = roster();
        r.valid_until = "2026-01-01T00:00:00Z".into();
        assert_eq!(admit(&armed(0), &r).err(), Some(Reject::Stale));
        r.valid_until = "not a date".into();
        assert_eq!(
            admit(&armed(0), &r).err(),
            Some(Reject::Stale),
            "an unreadable deadline must not become an unbounded one"
        );
    }

    /// A roster from a format this build does not understand is refused rather than
    /// misread, and the refusal is post-verify (so it is not a crypto oracle).
    #[test]
    fn a_newer_roster_schema_is_refused_not_misread() {
        let mut r = roster();
        r.schema = 99;
        assert_eq!(admit(&armed(0), &r).err(), Some(Reject::Schema));
    }

    /// An anchor list of nothing but garbage is `BadKey`, never `Verify`: a broken keyset
    /// must not be reported as "this roster's signature was bad", and must certainly not
    /// be reported as success.
    #[test]
    fn a_keyset_of_pure_garbage_is_bad_key_not_a_bad_signature() {
        let (bytes, sig) = published(&roster(), &MASTER_SEED);
        let garbage = Anchor::of(vec![String::new(), "!!not base64!!".into()], 0);
        assert!(
            garbage.is_armed(),
            "precondition: a non-empty list of unusable members still reads as ARMED"
        );
        assert_eq!(
            admit_roster(&garbage, bytes, &sig, NOW).err(),
            Some(Reject::BadKey)
        );
    }

    // The artifact and the bind.

    /// A one-byte edit anywhere in the index is refused, over the exact raw bytes with no
    /// normalization: a trailing newline is a different document.
    #[test]
    fn a_tampered_index_is_refused_byte_by_byte() {
        let roster = admit(&armed(0), &roster()).unwrap();
        let raw = index_body("m3", 3, 41);
        let sig = sign(&M3_SEED, &raw);
        for i in 0..raw.len() {
            let mut bad = raw.clone();
            bad[i] ^= 0x01;
            assert_eq!(
                roster.authorize_index(bad, &sig).err(),
                Some(Reject::Verify),
                "a flip at index byte {i} must reject"
            );
        }
        let mut trailing = raw.clone();
        trailing.push(b'\n');
        assert_eq!(
            roster.authorize_index(trailing, &sig).err(),
            Some(Reject::Verify),
            "no newline normalization on the signed path"
        );
        assert!(roster.authorize_index(raw, &sig).is_ok());
    }

    /// The id bind, both directions: a signature cannot be relabelled as another machine, an
    /// unattributed index is refused, and an index cannot claim a generation ahead of its
    /// roster.
    #[test]
    fn the_index_must_name_the_machine_and_generation_that_actually_signed_it() {
        let roster = admit(&armed(0), &roster()).unwrap();

        // m11 signs bytes that claim m3. The key decides who signed; the claim disagrees.
        let raw = index_body("m3", 3, 41);
        assert_eq!(
            roster
                .authorize_index(raw, &sign(&M11_SEED, &index_body("m3", 3, 41)))
                .err(),
            Some(Reject::NotAuthorized),
            "a genuine signature must not be wearable under another machine's name"
        );

        // No attribution at all.
        let mut s = String::from("schema = 2\nindex_build = 41\n");
        s.push_str("valid_until = \"2027-02-01T00:00:00Z\"\n[programs.ay]\nrepo = \"ay\"\n");
        let bare = s.into_bytes();
        assert_eq!(
            roster
                .authorize_index(bare.clone(), &sign(&M3_SEED, &bare))
                .err(),
            Some(Reject::Unattributed)
        );

        // An OLDER attribution under this newer roster is admitted (the post-join steady
        // state); a generation AHEAD of the roster is the old-roster/new-index pairing
        // that stays refused.
        let older = index_body("m3", 2, 41);
        assert!(
            roster
                .authorize_index(older.clone(), &sign(&M3_SEED, &older))
                .is_ok(),
            "an index attributed under an older generation verifies under a newer roster"
        );
        let ahead = index_body("m3", 4, 41);
        assert_eq!(
            roster
                .authorize_index(ahead.clone(), &sign(&M3_SEED, &ahead))
                .err(),
            Some(Reject::SeqMismatch)
        );
    }

    /// A signature of the wrong length is refused before the crypto, and an index whose
    /// bytes verify but whose TOML is malformed or from a newer schema is a post-verify
    /// refusal, not an oracle, since it is decided from authenticated bytes.
    #[test]
    fn cheap_local_gates_and_post_verify_parse_failures_both_fail_closed() {
        let roster = admit(&armed(0), &roster()).unwrap();
        assert_eq!(
            roster
                .authorize_index(index_body("m3", 3, 41), &[0u8; 10])
                .err(),
            Some(Reject::BadSig)
        );
        let garbage = b"this is not toml {{{".to_vec();
        assert_eq!(
            roster
                .authorize_index(garbage.clone(), &sign(&M3_SEED, &garbage))
                .err(),
            Some(Reject::Malformed)
        );
        let newer =
            b"schema = 99\nindex_build = 1\nvalid_until = \"2027-01-01T00:00:00Z\"\n".to_vec();
        assert_eq!(
            roster
                .authorize_index(newer.clone(), &sign(&M3_SEED, &newer))
                .err(),
            Some(Reject::Schema)
        );
    }

    /// A genuine signature over bytes holding 0xFF verifies (a `from_utf8_lossy` first would
    /// change the bytes), flipping that byte fails, and the parse refuses non-UTF-8 as
    /// `Malformed` after the signature.
    #[test]
    fn raw_non_utf8_bytes_are_verified_without_lossy_conversion() {
        let roster = admit(&armed(0), &roster()).unwrap();
        let mut raw = index_body("m3", 3, 41);
        let bad_idx = raw.len();
        raw.push(0xFF);
        let sig = sign(&M3_SEED, &raw);
        // The signature is over the exact bytes, so the failure is the parse, not Verify.
        assert_eq!(
            roster.authorize_index(raw.clone(), &sig).err(),
            Some(Reject::Malformed),
            "verification passed over the raw byte; the parser refused it afterwards"
        );
        let mut tampered = raw;
        tampered[bad_idx] ^= 0x01;
        assert_eq!(
            roster.authorize_index(tampered, &sig).err(),
            Some(Reject::Verify)
        );
    }

    // The store's other gates: freshness and the durable floors.

    /// now >= valid_until ⇒ Stale; now < valid_until ⇒ Ok.
    #[test]
    fn freshness_gate_rejects_at_or_after_valid_until() {
        assert_eq!(check_freshness(100, 200), Ok(()));
        assert_eq!(check_freshness(199, 200), Ok(()));
        assert_eq!(check_freshness(200, 200), Err(Reject::Stale));
        assert_eq!(check_freshness(201, 200), Err(Reject::Stale));
    }

    /// index_build below the recorded Floor ⇒ Rollback; a higher build advances the
    /// durable floor (and the file is 0644, readable).
    #[test]
    fn high_water_floor_blocks_rollback_and_advances() {
        let dir = private_tmp_dir("floor");
        let path = dir.join("index_build.floor");
        let floor = Floor::new(path.clone());
        // First contact: no recorded floor ⇒ accepted and recorded.
        assert_eq!(floor.check_and_record(41), Ok(()));
        // A lower build is a rollback.
        assert_eq!(floor.check_and_record(40), Err(Reject::Rollback));
        // Equal is allowed (the gate is index_build >= floor).
        assert_eq!(floor.check_and_record(41), Ok(()));
        // A higher build advances the durable floor...
        assert_eq!(floor.check_and_record(50), Ok(()));
        // ...so a build below the new floor is now rejected even though it beat the
        // first floor: the advance persisted across calls.
        assert_eq!(floor.check_and_record(45), Err(Reject::Rollback));
        // A fresh Floor over the same path sees the durable value.
        assert_eq!(
            Floor::new(path.clone()).check_and_record(49),
            Err(Reject::Rollback)
        );
        // The floor file is 0644, readable by every user (see `stage_and_publish`).
        #[cfg(unix)]
        {
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o644, "floor file must be readable (0644)");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A ratchet touch that advances nothing does not rewrite the file (on Apple targets a
    /// rewrite costs two full drive flushes). The witness is a hard link taken before the
    /// touch: a temp+`rename` rewrite moves the path to a new inode. A not-yet-0644 or corrupt
    /// floor is still rewritten.
    #[cfg(unix)]
    #[test]
    fn an_unchanged_floor_is_not_rewritten() {
        use std::os::unix::fs::MetadataExt as _;
        let dir = private_tmp_dir("floor-noop");
        let path = dir.join("roster.floor");
        let floor = Floor::new(path.clone());
        assert_eq!(floor.check_and_record(4), Ok(()));

        let witness = dir.join("witness");
        std::fs::hard_link(&path, &witness).unwrap();
        let ino = |p: &Path| std::fs::metadata(p).unwrap().ino();
        assert_eq!(
            ino(&path),
            ino(&witness),
            "precondition: the witness is the same file"
        );

        // A no-op tick: the recorded value is already 4.
        assert_eq!(floor.check_and_record(4), Ok(()));
        assert_eq!(
            ino(&path),
            ino(&witness),
            "an unchanged floor must not be re-published (temp + 2 fsyncs + rename)"
        );
        assert_eq!(
            floor.current(),
            4,
            "and the value is of course still recorded"
        );
        // Nothing needed persisting, so there is no persist failure to report either.
        assert_eq!(floor.check_and_record_classified(4), (Ok(()), None));
        assert_eq!(ino(&path), ino(&witness));

        // A genuine advance is still published durably...
        assert_eq!(floor.check_and_record(5), Ok(()));
        assert_ne!(ino(&path), ino(&witness), "an advance must reach the disk");
        assert_eq!(Floor::new(path.clone()).current(), 5);

        // ...and so is a floor not yet `0644`, even at an unchanged value
        // (docs/AUDIT-nux-first-open-toolchain-2026-08-31.md).
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(floor.check_and_record(5), Ok(()));
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o644,
            "a legacy 0600 floor is re-published readable even at an unchanged value"
        );

        // ...and so is a corrupt file: it reads as the fail-open 0, so the ratchet has to
        // re-arm by rewriting it even at `index_build` 0, where nothing "advances".
        std::fs::write(&path, b"not-a-floor").unwrap();
        assert_eq!(floor.read_floor_classified(), (0, true));
        assert_eq!(floor.check_and_record(0), Ok(()));
        assert_eq!(
            floor.read_floor_classified(),
            (0, false),
            "corruption is HEALED, never skipped over as 'nothing to persist'"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The build floor binds within its generation and a strictly newer one re-bases it, so
    /// one machine publishing `index_build = u64::MAX` cannot put the store beyond the index
    /// that revokes it. An equal or older generation still binds.
    #[test]
    fn the_build_floor_binds_within_its_generation_and_is_rebased_by_a_newer_one() {
        let floor = BuildFloor {
            index_build: 50,
            roster_seq: 7,
        };
        // Same generation: exactly the old high-water, inclusive at the boundary.
        assert!(floor.admits(7, 50), "equal to the floor passes");
        assert!(floor.admits(7, 51));
        assert!(
            !floor.admits(7, 49),
            "below the floor, same generation ⇒ refused"
        );
        // A strictly newer generation re-bases it: the master's rescue lever.
        assert!(
            floor.admits(8, 1),
            "a newer master-signed generation is not bound by a floor an older one set"
        );
        // ...and nothing else waives it. An older generation is still floored (it can only
        // arrive at all if the roster ratchet let it, and it must not open a second door).
        assert!(
            !floor.admits(6, 49),
            "an OLDER generation must not waive the floor"
        );
        assert!(!floor.admits(0, 49), "nor an absent/unknown one");
        // First contact admits everything, which is the genuine residual §8 freshness bounds.
        assert!(BuildFloor::none().admits(0, 0));
        assert!(BuildFloor::none().admits(1, u64::MAX));
    }

    /// The poisoned-floor scenario, end to end over the durable file: a machine drives the
    /// floor to the u64 ceiling, and `rebase` — and only `rebase` — brings it back.
    #[test]
    fn rebase_is_the_only_way_a_floor_ever_comes_down() {
        let dir = private_tmp_dir("floor-rebase");
        let path = dir.join("index_build.floor");
        let floor = Floor::new(path.clone());
        assert_eq!(floor.check_and_record(u64::MAX), Ok(()));
        assert_eq!(floor.current(), u64::MAX);
        // The monotonic ratchet cannot recover: every sane build the owner can publish is
        // now a "rollback".
        assert_eq!(floor.check_and_record(101), Err(Reject::Rollback));
        assert_eq!(floor.current(), u64::MAX, "and the refusal changed nothing");
        // The re-base does, and it persists across a fresh handle on the same path.
        floor.rebase(101);
        assert_eq!(Floor::new(path).current(), 101);
        // ...after which the ratchet is a ratchet again.
        assert_eq!(floor.check_and_record(100), Err(Reject::Rollback));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A floor file that exists but will not read as a floor is detected as corrupt and still
    /// reads as `0`: refusing would let a same-uid scribbler wedge the store, and silent
    /// first contact would reopen a rollback window unannounced.
    #[test]
    fn a_corrupt_floor_file_is_detected_and_still_reads_as_first_contact() {
        let dir = private_tmp_dir("floor-corrupt");
        let path = dir.join("roster.floor");
        let floor = Floor::new(path.clone());
        // Absent is genuine first contact, not corruption.
        assert_eq!(floor.read_floor_classified(), (0, false));
        assert_eq!(floor.check_and_record(7), Ok(()));
        assert_eq!(floor.read_floor_classified(), (7, false));
        // Garbage text, non-UTF-8 bytes, over the size bound: all corrupt, all read 0.
        let oversized = vec![b'9'; MAX_FLOOR_BYTES + 1];
        for garbage in [
            b"garbage-not-a-number".as_slice(),
            &[0xFF, 0xFE, 0x00],
            &oversized,
        ] {
            std::fs::write(&path, garbage).unwrap();
            let (value, corrupt) = floor.read_floor_classified();
            assert_eq!(
                value, 0,
                "corrupt must read 0 — a refusal would be a local DoS"
            );
            assert!(
                corrupt,
                "corruption must be DETECTED, never silent first contact"
            );
        }
        // The documented residual, pinned: after corruption the ratchet re-arms from 0...
        assert_eq!(floor.check_and_record(1), Ok(()));
        // ...and the healed file is an ordinary floor again.
        assert_eq!(floor.read_floor_classified(), (1, false));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An advance that cannot persist keeps the accept but reports the lost advance: the next
    /// run re-reads the old floor and accepts the same generation again.
    #[cfg(unix)]
    #[test]
    fn a_failed_floor_persist_keeps_the_accept_and_reports_the_lost_advance() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = private_tmp_dir("floor-persist");
        let path = dir.join("roster.floor");
        let floor = Floor::new(path.clone());
        assert_eq!(floor.check_and_record(7), Ok(()));

        // The parent refuses writes at exactly the advance moment.
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).unwrap();
        let (decision, persist_failure) = floor.check_and_record_classified(8);
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(
            decision,
            Ok(()),
            "a persist failure never rejects a passed check"
        );
        assert!(
            persist_failure.is_some(),
            "the lost advance must be reported, not discarded"
        );

        // The replay window the report exists to make visible: the floor still says 7,
        // so the build-8 generation — and 7 itself — will be accepted again.
        assert_eq!(Floor::new(path.clone()).current(), 7);
        assert_eq!(floor.check_and_record(7), Ok(()));

        // Control: with the directory healthy the same advance persists and reports
        // nothing.
        let (decision, persist_failure) = floor.check_and_record_classified(8);
        assert_eq!(decision, Ok(()));
        assert_eq!(persist_failure, None);
        assert_eq!(Floor::new(path).current(), 8);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A persist that fails after the temp exists removes the temp: nothing else sweeps
    /// `<floor>.<pid>.tmp`, so a store that keeps refusing writes would collect one per pid.
    #[test]
    fn a_failed_floor_persist_removes_its_temp_file() {
        let dir = private_tmp_dir("floor-temp-litter");
        let path = dir.join("index_build.floor");
        // A directory where the floor file goes: only the final `rename` fails, the shape of
        // every post-create failure, reached without filling a disk.
        std::fs::create_dir(&path).unwrap();
        let floor = Floor::new(path.clone());
        let (decision, persist_failure) = floor.check_and_record_classified(9);
        assert_eq!(
            decision,
            Ok(()),
            "a persist failure never rejects a passed check"
        );
        assert!(
            persist_failure.is_some(),
            "precondition: this fixture must make the persist fail"
        );

        let litter: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".tmp"))
            .collect();
        assert!(
            litter.is_empty(),
            "a failed persist left its temp behind: {litter:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn fifo_and_symlink_floor_return_without_blocking() {
        use std::os::unix::ffi::OsStrExt as _;

        let dir = private_tmp_dir("floor-special");
        let path = dir.join("index_build.floor");
        let path_c = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: `path_c` is a live NUL-terminated path in our private fixture.
        assert_eq!(unsafe { libc::mkfifo(path_c.as_ptr(), 0o600) }, 0);
        assert_eq!(Floor::new(path.clone()).current(), 0);
        std::fs::remove_file(&path).unwrap();
        let target = dir.join("foreign-floor");
        std::fs::write(&target, "99\n").unwrap();
        std::os::unix::fs::symlink(&target, &path).unwrap();
        assert_eq!(Floor::new(path).current(), 0, "floor links are refused");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // The post-verify parsers are tested in `crate::manifest`, against genuine
    // `VerifiedBytes`.
}
