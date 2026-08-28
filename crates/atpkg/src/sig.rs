// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The signature anchor: detached Ed25519 verification over the **exact raw bytes**
//! of a manifest asset, with verification enforced *before any parse* by
//! construction.
//!
//! # Raw bytes, verified before parse
//!
//! Ed25519 verifies over the exact asset bytes as downloaded — no lossy UTF-8
//! conversion, no re-serialization, no newline/BOM/whitespace normalization, no
//! "canonical TOML" (TOML has no canonical form; re-serializing would open a
//! parser-differential gap). The verifier reads the raw asset into a `Vec<u8>`,
//! verifies the detached signature over *those* bytes, and only on success hands the
//! *same* bytes — wrapped in a [`VerifiedBytes`] — to the parser. The TOML parser is
//! itself attack surface and must never touch unverified input; the reused
//! `String::from_utf8_lossy` step (which substitutes U+FFFD and so changes the bytes)
//! is therefore **never** used on the signed path.
//!
//! **Enforced by construction.** [`VerifiedBytes`] has a private field and no public
//! constructor: the only way to mint one is through [`TrustedRoster::authorize_bytes`]
//! (and the two wrappers over it, [`TrustedRoster::authorize_index`] and
//! [`TrustedIndex::verify_pkg`]), every one of which runs the signature check first. The
//! post-verify parse entry points ([`crate::manifest::parse_index`] /
//! [`crate::manifest::parse_pkg`]) consume `&VerifiedBytes`, so calling them on unverified
//! bytes does not even type-check. A runtime test in `crate::manifest` additionally proves
//! the parser never runs after a failed verification.
//!
//! # ONE ROOT: the paper master's roster authorizes atpkg too
//!
//! There used to be a SECOND trust root here — a `PKG_ROOT_PUBKEY` living in
//! `~/.config/atpkg/root.key` that signed `index.toml`, which in turn delegated a
//! rotatable release key to sign each `pkg-*.toml`. It is gone. atpkg's index is now
//! authorized by the SAME master-signed machine roster that authorizes an aterm release
//! (`aterm_update_core::roster`): one thing written on paper, one revocation story.
//!
//! The chain, in the order it runs:
//!
//! ```text
//!   PAPER MASTER  --signs-->  aterm-machines.toml  --names-->  m3, m11, …
//!                                                                 |
//!                                          m3 --signs--> index.toml
//!                                          m3 --signs--> pkg-<program>-<build>.toml
//! ```
//!
//! Why MACHINE-signed rather than master-signed: the whole custody argument is that the
//! master exists on no computer and is typed in only to mint or revoke. An index re-cut
//! every time a pin moves would put the paper phrase on the publishing laptop on every
//! publish, which is the property that made the retired "offline root" worthless. The
//! cost is one extra indirection and one extra fetched pair per candidate — paid for by
//! revocation that takes minutes (bump the roster) instead of an index re-publish.
//!
//! # Empty means unpinned means INERT — and inert installs NOTHING
//!
//! An empty [`Anchor`] (the shipped default, because `pins::PAPER_MASTER_PUBKEYS` is
//! `&[]`) authorizes no machine at all: [`admit_roster`] returns [`Reject::Disabled`]
//! before any crypto, [`crate::select_index`] returns `None`, and every install path
//! ends in `FlowError::NoIndex`. It never means "accept anything". This is the single
//! most dangerous property in the module and it is asserted from both directions in the
//! tests below.
//!
//! # Cheapest-first reject ordering
//!
//! Every gate fails CLOSED; any error is a [`Reject`], and the expensive crypto runs as
//! late as it can:
//!
//! 1. anchor empty ⇒ [`Reject::Disabled`]. Free.
//! 2. roster signature length (exactly 64 bytes). Cheap, local.
//! 3. verify the roster under the pinned master keyset. **CRYPTO #1.**
//! 4. parse the roster — only from `VerifiedRoster`, which has no public constructor.
//! 5. roster `schema` newer than we understand ⇒ refuse rather than misread.
//! 6. `roster_seq` below the durable floor ⇒ [`Reject::Rollback`]. THE replay ratchet.
//! 7. roster `valid_until` lapsed ⇒ [`Reject::Stale`]. The only defence a brand-new
//!    install has, since it carries no floor.
//! 8. revoked and expired machines leave the candidate set — BEFORE any artifact
//!    crypto, so a revoked machine's valid signature is never checked.
//! 9. verify `index.toml` under the survivors. **CRYPTO #2.**
//! 10. parse it — only from [`VerifiedBytes`] — then bind its self-declared
//!     `machine_id`/`roster_seq` to what actually verified ([`Attribution::bind`]).
//!
//! [`check_freshness`] (the index's own window, gate 2, §8) and the [`Floor`] high-water
//! mark over `index_build` (gate 3, §8) are pure/durable functions the caller sequences
//! after all of the above; they are unchanged by the single-root move.

use std::io;
use std::path::{Path, PathBuf};

use crate::platform::FileLock;
use aterm_update_core::roster::{Attribution, Roster, RosterReject, VerifiedRoster, verify_roster};

const MAX_FLOOR_BYTES: usize = 128;

/// A blob of bytes that has **passed signature verification** under a machine the pinned
/// master's roster authorizes. The inner `Vec<u8>` is private and there is no public
/// constructor, so the only way to obtain a `VerifiedBytes` is via
/// [`TrustedRoster::authorize_bytes`], [`TrustedRoster::authorize_index`], or
/// [`TrustedIndex::verify_pkg`]. Parsers take `&VerifiedBytes`, which makes "parse only
/// after verify" a *compile-time* guarantee rather than a convention.
///
/// The derives add no constructor — the field stays private and the only way to mint a
/// `VerifiedBytes` remains the verify functions; `Debug`/`PartialEq` exist purely so
/// tests can assert over `Result<VerifiedBytes, Reject>` (the verified bytes are public
/// manifest content, not a secret).
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
    /// No master anchor is pinned (an unarmed build). Fail-closed default, returned
    /// *before* any crypto — and it authorizes NOTHING, ever.
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
    /// The roster authorizes no machine that could have signed this: it names none that
    /// is still live, or the document's own `machine_id` names a machine the roster does
    /// not list (or a DIFFERENT machine than the key that actually verified — a
    /// relabelling attempt). Was `NotDelegated`, when the index carried its own
    /// delegation; the roster carries the authority now, so the name follows.
    NotAuthorized,
    /// The signing machine's own `not_after` has lapsed — defence in depth, so a machine
    /// key ages out and is re-minted from paper even when nothing went wrong.
    Expired,
    /// The document carries no `machine_id`, so it cannot be attributed. Under an armed
    /// anchor an unattributed index is refused: attribution is a requirement.
    Unattributed,
    /// The document's `roster_seq` is NEWER than the authorizing roster's (or absent) —
    /// an attempt to pair an old roster with a new index. A newer roster with an older
    /// index is admitted (`aterm_update_core::roster::Attribution::bind`, 2026-08-18: the
    /// roster travels on the channel head, and a join re-dresses published releases).
    SeqMismatch,
    /// The index's freshness window has lapsed (`now >= valid_until`), or the roster's
    /// has.
    Stale,
    /// A document below a durable high-water floor — an attempted rollback to an older,
    /// once-valid, still-validly-signed roster (`roster_seq`) or index (`index_build`).
    /// Signatures do not expire; documents do, which is why both ratchets exist.
    Rollback,
    /// A document that passed signature verification but is not valid UTF-8, not valid
    /// TOML, or is missing a required field / has a duplicate key. Distinct from the
    /// crypto rejects above and **not** a verification oracle — it is a *post-verify*
    /// parse failure over already-authenticated bytes (`crate::manifest`). Fail closed.
    Malformed,
    /// A document whose `schema` is newer than this build understands
    /// ([`crate::manifest::SUPPORTED_SCHEMA`]); refused rather than misread. Also
    /// post-verify, not a crypto oracle.
    Schema,
    /// A `pkg-*.toml` row spelled with a RETIRED `kind` — today only `vendor-fetch`, which
    /// was never published and was split into `kind` (the payload/apply shape) and
    /// `protocol` (how the bytes are obtained) before any client saw it. Post-verify, like
    /// [`Reject::Malformed`], but it carries the split's spelling so the authoring machine's
    /// own `atpkg install` names the fix instead of a bare "malformed".
    RetiredKind(&'static str),
    /// An `index.toml` whose `[programs.<name>].requires` relation is not one a client can
    /// honour: a name the index does not carry, a program requiring itself, or a cycle —
    /// over programs, or over the coherence groups the plan installs atomically
    /// ([`crate::manifest::validate_requires`]). Post-verify, like [`Reject::Malformed`];
    /// it carries the offending edge (a cycle is spelled out, `a → b → a`) so the
    /// publisher's own `atpkg verify-index` names the row to fix. The dependency relation
    /// is SIGNED metadata, so this can only ever be an authoring mistake, never an
    /// adversary's — and a client refuses the whole index rather than plan an order it
    /// could not satisfy. (`String`, so the enum is no longer `Copy`; nothing copied it.)
    Requires(String),
}

/// Translate the roster tier's verdict into atpkg's. One-to-one and total, deliberately:
/// a `match` with no catch-all means a new [`RosterReject`] variant is a COMPILE error
/// here rather than a silently-mapped one, and the mapping never collapses a refusal
/// into an acceptance.
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

/// What this build trusts, and how far its replay ratchet has already turned.
///
/// The keyset is the paper master's — [`crate::PKG_TRUST_ANCHORS`], i.e.
/// `pins::PAPER_MASTER_PUBKEYS` — and NOT a package-specific root: that is the whole
/// point of folding atpkg under the master. `roster_floor` is the highest `roster_seq`
/// this store has ever durably recorded (`<prefix>/roster.floor`).
///
/// # An empty keyset is INERT, and inert grants nothing
///
/// [`Anchor::is_armed`] is false, [`admit_roster`] refuses before any crypto, and
/// `select_index` refuses to consider a single candidate. There is deliberately no
/// constructor that can produce an anchor which accepts an arbitrary index: the only
/// ways to build one are [`Anchor::pinned`] (the committed keyset) and [`Anchor::of`]
/// (an explicit keyset — the seam a fork with its OWN paper master uses, and what the
/// tests drive so they need neither the compiled pin nor any environment).
#[derive(Debug, Clone)]
pub struct Anchor {
    masters: Vec<String>,
    /// The durable `roster_seq` high-water this client has already accepted.
    pub roster_floor: u64,
}

impl Anchor {
    /// The anchor this binary ships with: the committed paper-master keyset, plus the
    /// caller's durable roster floor. In THIS tree the keyset is empty, so the anchor is
    /// inert and atpkg installs nothing until an operator arms it in a reviewed commit.
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

    /// Whether any master is pinned at all. FALSE is the fail-closed state — see the
    /// type docs. A member that is empty or malformed can neither grant nor deny (it is
    /// skipped by `verify_roster`, which reports `BadKey` if NONE was usable), so
    /// armedness is decided by the slice being non-empty and nothing else — exactly the
    /// rule `aterm-update`'s `RosterPolicy` uses, so the two tiers cannot drift on what
    /// "armed" means.
    #[must_use]
    pub fn is_armed(&self) -> bool {
        !self.masters.is_empty()
    }

    /// The keyset as `verify_roster` wants it.
    fn keys(&self) -> Vec<&str> {
        self.masters.iter().map(String::as_str).collect()
    }
}

/// A roster generation that verified under the pinned master AND passed admission (the
/// replay ratchet and the freshness window). Holding one is proof steps 1–7 ran.
///
/// `now_unix` is captured at admission so every later authorization under this roster
/// sees ONE clock reading: a whole channel apply must not have a machine expire out from
/// under it halfway through, which would leave a coherence group split across two
/// authorization states.
///
/// # THE FREEZE IS A CONTRACT: valid for ONE apply — do not cache
///
/// The captured clock is deliberate for a single select/install pass and WRONG for
/// anything longer: a `TrustedRoster` held past its roster's `valid_until` (or past a
/// machine's `not_after`) keeps authorizing under the frozen reading forever, because
/// nothing here re-reads a clock. Every current caller mints one, uses it within a
/// single CLI command, and drops it. A future long-lived holder (a daemonized atpkg, a
/// cache) MUST call [`Self::still_fresh`] with a current clock before each authorization
/// batch — that is the revalidation hook this contract names, and it re-runs exactly the
/// admission-time freshness gate.
#[derive(Debug, Clone)]
pub struct TrustedRoster {
    roster: Roster,
    now_unix: i64,
    master_index: usize,
}

/// **Steps 1–7.** Verify the roster's exact bytes under the pinned master, parse it only
/// from `VerifiedRoster`, and admit it against the replay floor and its freshness window.
///
/// Every arm is a refusal. There is no path through here that returns "accept anyway",
/// and in particular an unarmed anchor short-circuits at the top, before any crypto.
pub fn admit_roster(
    anchor: &Anchor,
    raw: Vec<u8>,
    sig: &[u8],
    now_unix: i64,
) -> Result<TrustedRoster, Reject> {
    // STEP 1 — free, and the direction that must never invert: no anchor, no authority.
    // `verify_roster` would refuse an empty keyset by itself; this is stated here as well
    // because "the loop skipped every candidate" and "the loop accepted every candidate"
    // are one negated condition apart, and only one of them is survivable.
    if !anchor.is_armed() {
        return Err(Reject::Disabled);
    }
    // STEPS 2–3 — signature length, then the master crypto.
    let verified: VerifiedRoster = verify_roster(&anchor.keys(), raw, sig).map_err(from_roster)?;
    let master_index = verified.master_index();
    // STEPS 4–5 — parse ONLY from the verified wrapper (no public constructor), which
    // also applies the reject-newer schema gate.
    let roster = Roster::parse(&verified).map_err(from_roster)?;
    // STEPS 6–7 — the ratchet, then freshness. Both cheap, both before any artifact
    // crypto, and a `valid_until` we cannot read counts as LAPSED, never as absent.
    roster
        .admit(anchor.roster_floor, now_unix)
        .map_err(from_roster)?;
    Ok(TrustedRoster {
        roster,
        now_unix,
        master_index,
    })
}

impl TrustedRoster {
    /// This generation's `roster_seq` — what the caller ratchets the durable floor to.
    #[must_use]
    pub fn seq(&self) -> u64 {
        self.roster.roster_seq
    }

    /// Which master keyset member verified it. Index 0 is the master this build considers
    /// current; anything else means a master rotation is in flight or stalled, which is
    /// worth SAYING (doctor prints it) and is never a rejection.
    #[must_use]
    pub fn master_index(&self) -> usize {
        self.master_index
    }

    /// Re-run the roster's own freshness gate at a LATER clock reading — the
    /// revalidation hook the freeze contract on this type names (see the type doc).
    ///
    /// The floor argument is `0` because this generation already cleared the caller's
    /// floor at admission and a floor can only have RISEN to at most this sequence via
    /// this very generation's own ratchet; what lapses with time is `valid_until`, and
    /// that is what this re-checks. `Err(Reject::Stale)` means the frozen `now_unix` has
    /// been outlived: drop this value and re-admit from the published roster.
    pub fn still_fresh(&self, now_unix: i64) -> Result<(), Reject> {
        self.roster.admit(0, now_unix).map_err(from_roster)
    }

    /// **Steps 8–9.** Verify one artifact's exact bytes under this roster's LIVE machines.
    ///
    /// Revoked and expired machines leave the candidate set before any crypto runs, so a
    /// revoked machine's perfectly valid signature is never checked. The underlying
    /// primitive is `Roster::authorize_appcast` — named for the release manifest it was
    /// written for, but it is a signature check over opaque bytes under the authorized
    /// set, and reusing it verbatim is the point: atpkg and the updater must not grow two
    /// dialects of one gate.
    fn authorize(&self, bytes: &[u8], sig: &[u8]) -> Result<Attribution, Reject> {
        self.roster
            .authorize_appcast(bytes, sig, self.now_unix)
            .map_err(from_roster)
    }

    /// [`Self::authorize`], minting the [`VerifiedBytes`] the post-verify parsers consume.
    ///
    /// This is THE place bytes become parseable under the single root, and it is one
    /// function rather than one per document type on purpose: `index.toml` and every
    /// `pkg-*.toml` are authorized by the identical rule (any listed, unrevoked,
    /// unexpired machine on this generation), so two spellings of it would be two things
    /// to keep right.
    pub fn authorize_bytes(
        &self,
        raw: Vec<u8>,
        sig: &[u8],
    ) -> Result<(VerifiedBytes, Attribution), Reject> {
        let who = self.authorize(&raw, sig)?;
        // Minting VerifiedBytes here — inside the one module that can — is what keeps
        // "parse only after verify" a compile-time fact rather than a habit.
        Ok((VerifiedBytes(raw), who))
    }

    /// **Steps 8–10 for `index.toml`:** verify under the live machines, parse only from
    /// the resulting [`VerifiedBytes`], then bind the index's own `machine_id` /
    /// `roster_seq` to what actually verified.
    ///
    /// The bind is two string/integer compares over already-authenticated bytes and it
    /// closes both directions at once: `machine_id` sits INSIDE the signed bytes, so a
    /// genuine m3 signature cannot be relabelled m11, and a thief holding m11's key
    /// cannot claim to be m3 because the roster maps m3 to m3's key.
    pub fn authorize_index(
        &self,
        raw: Vec<u8>,
        sig: &[u8],
    ) -> Result<(TrustedIndex, VerifiedBytes), Reject> {
        // (9) THE ARTIFACT CRYPTO, under survivors only.
        let (verified, who) = self.authorize_bytes(raw, sig)?;
        let index = crate::manifest::parse_index(&verified)?;
        // (10) THE ID BIND, over bytes that are authenticated by the time it runs.
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

/// An `index.toml` that a master-signed roster generation authorized — and the authority
/// every `pkg-*.toml` under it is verified against.
///
/// Carrying the roster with the index is not bookkeeping: under the retired design the
/// index CARRIED its own delegation, so "who may sign this program's manifest" was a
/// property of the index document. It is now a property of the roster generation that
/// authorized the index, and this type is that pairing. It derefs to [`crate::Index`], so
/// every existing read of `programs` / `channels` / `index_build` is unchanged.
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
    /// WHICH machine signed this index, proved by its key rather than claimed by a label.
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

    /// The generation THIS INDEX ITSELF DECLARES, from inside its own signed bytes.
    ///
    /// Not the same question as [`Self::roster_seq`], which reports the generation of
    /// the roster blob the candidate was SERVED WITH. `Attribution::bind` deliberately
    /// admits `index.roster_seq <= roster.roster_seq`, so an old index verifies
    /// perfectly well beside a newer roster — and whoever assembles a candidate
    /// chooses which pair to publish. Anything that WAIVES a durable floor has to ask
    /// this one: pairing the owner's own unmodified older index with the owner's own
    /// unmodified newer roster otherwise re-based the anti-rollback floor downward
    /// without forging anything (2026-08-20 round-8 audit).
    ///
    /// The master's rescue lever is unaffected: minting a generation and PUBLISHING AN
    /// INDEX THAT DECLARES IT is still the master's alone, and that index waives the
    /// floor exactly as designed.
    #[must_use]
    pub fn authorizing_seq(&self) -> u64 {
        // An index that declares NO generation cannot waive a generation floor. Zero
        // is never strictly greater than a recorded floor, so such a candidate falls
        // through to the plain anti-rollback comparison — which is exactly the
        // pre-roster behaviour.
        self.index.roster_seq.unwrap_or(0)
    }

    /// The roster generation behind it, for a caller that must authorize something else
    /// under the very same generation.
    #[must_use]
    pub fn roster(&self) -> &TrustedRoster {
        &self.roster
    }

    /// Verify a `pkg-<program>-<build>.toml`'s raw bytes under the SAME roster generation
    /// that authorized this index — i.e. under any machine that is listed, not revoked and
    /// not expired. A manifest signed by a revoked machine is refused without its
    /// signature ever being checked; a manifest signed by a key nobody on the roster holds
    /// fails the Ed25519 check.
    ///
    /// This replaces `verify_pkg(raw, sig, &index.delegation())`. The delegation tier is
    /// retired: the roster supplies both the grant and the deny, and supplies the deny
    /// faster than an index re-publish ever could.
    pub fn verify_pkg(&self, raw: Vec<u8>, sig: &[u8]) -> Result<VerifiedBytes, Reject> {
        self.roster.authorize_bytes(raw, sig).map(|(v, _)| v)
    }
}

/// Genuinely-signed [`VerifiedBytes`] for OTHER modules' tests.
///
/// `VerifiedBytes` has no public constructor, which is the point — so a test that needs
/// one must produce it the way production does. Before the single-root move that meant
/// `verify_index_with(&root_pk, raw, &sig)`; it now means a synthetic paper master, a
/// synthetic roster, and a real machine signature through the real chain. Doing that by
/// hand in every module would be forty lines of fixture per module and an invitation to
/// add a `pub(crate) fn new()` instead — which would quietly retire the guarantee.
///
/// `#[cfg(test)]` throughout: this cannot exist in a shipped build.
#[cfg(test)]
pub(crate) mod testkit {
    use super::{Anchor, TrustedRoster, VerifiedBytes, admit_roster};
    use aterm_update_core::roster::{Machine, Roster};
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD;
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
        STANDARD.encode(keypair(seed).public_key().as_ref())
    }

    pub(crate) fn sign(seed: &[u8; 32], msg: &[u8]) -> Vec<u8> {
        keypair(seed).sign(msg).as_ref().to_vec()
    }

    /// The one-machine roster the whole crate's tests publish under.
    pub(crate) fn roster() -> Roster {
        Roster {
            schema: 1,
            roster_seq: SEQ,
            // Far out on purpose: this fixture exists so OTHER gates can be the thing
            // under test. A roster that lapsed mid-suite would turn every unrelated
            // failure into "Stale", which is the least informative refusal there is.
            // Roster staleness itself is tested directly, with an explicit clock.
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

    /// `raw`, machine-signed and taken through the REAL authorization path. Panics if the
    /// chain refuses — a test fixture that cannot be signed is a broken fixture.
    pub(crate) fn machine_signed(raw: Vec<u8>) -> VerifiedBytes {
        let sig = sign(&MACHINE_SEED, &raw);
        trusted_roster()
            .authorize_bytes(raw, &sig)
            .expect("the roster's own machine signed these bytes")
            .0
    }
}

/// The durable anti-rollback floor over `index.toml`, **and the roster generation that set
/// it** — the two are one value, because `index_build` is a number a MACHINE chooses.
///
/// # Why the generation travels with the number
///
/// Folding index signing down to the machine tier means every rostered machine can now
/// write this floor. `index_build` is an arbitrary u64 inside machine-signed bytes, so one
/// machine — a stolen key, the exact case revocation exists for — can publish
/// `index_build = 9223372036854775807` (the TOML integer ceiling) and permanently raise a
/// monotonic high-water above anything the owner will ever publish. Every later index,
/// **including the one carrying the revocation**, is then filtered out before its roster is
/// even consulted: the store is bricked AND the revocation can never be delivered by any
/// republish. That is the "unrecoverable by re-publish" property the retired design
/// reserved for compromise of the *offline* index root, and it must not be reachable from a
/// tier the roster exists to be able to revoke.
///
/// So the floor binds only against the generation that set it. A strictly NEWER
/// master-signed roster generation re-bases it ([`Floor::rebase`]) — which is the paper
/// master's rescue lever and nobody else's, since only the master can mint a generation.
/// Within one generation the ratchet is exactly the monotonic high-water it always was.
///
/// # It still fails CLOSED in the direction that matters
///
/// [`Self::admits`] waives the floor **only** for a strictly-newer generation. An equal or
/// (impossible, but) older generation is still floored, so a corrupt or missing
/// `floor.gen` — which reads as `0` — never waives anything it should not: it makes the
/// floor bind at generation 0, which no real roster carries, and the first genuine
/// generation re-bases it. A store carrying a floor from BEFORE the single-root move is in
/// exactly that position, and re-basing it is right: no index that floor ever admitted can
/// verify under the new chain at all (schema 1, no `machine_id` ⇒ `Unattributed`), so the
/// number refers to a lineage that no longer exists.
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
    /// clears this floor.
    ///
    /// The `>` is deliberate and is the whole gate: only a STRICTLY newer generation waives
    /// the floor. Making it `!=` would waive it for an older generation too, which is the
    /// one direction that would let a replayed index in.
    #[must_use]
    pub fn admits(&self, roster_seq: u64, index_build: u64) -> bool {
        if roster_seq > self.roster_seq {
            return true;
        }
        index_build >= self.index_build
    }
}

/// Freshness gate (gate 2, §8): an index is fresh iff `now_unix < valid_until_unix`.
/// `now` is **injected** so the logic is pure and deterministic — the real clock and
/// the RFC3339 → unix conversion of `valid_until` are wired by the (post-verify) caller,
/// never read inside this function. A lapsed window is refused fail-closed
/// ([`Reject::Stale`]), closing the offline-window / stale-proxy replay gap.
pub fn check_freshness(now_unix: i64, valid_until_unix: i64) -> Result<(), Reject> {
    if now_unix >= valid_until_unix {
        Err(Reject::Stale)
    } else {
        Ok(())
    }
}

/// A durable, anti-rollback high-water mark over the index's monotonic `index_build`
/// (gate 3, §8). The highest seen build is persisted to a `0600`, owned-by-uid file
/// under a private directory; any index with a *lower* `index_build` is rejected, so an
/// attacker who can pin a client to an older signed index cannot roll it back below what
/// it has already durably seen.
pub struct Floor {
    path: PathBuf,
}

impl Floor {
    /// A floor backed by `path`. The parent directory must be private (0700,
    /// owned-by-uid) at write time; the file itself is written `0600` via temp+rename.
    #[must_use]
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// The recorded high-water floor (`0` if none/unreadable). For a caller that needs the
    /// current floor as a SELECT filter (§5) *before* advancing it via
    /// [`Self::check_and_record`].
    #[must_use]
    pub fn current(&self) -> u64 {
        self.read_floor()
    }

    /// Reject any `index_build` below the recorded floor ([`Reject::Rollback`]); on
    /// accept, durably advance the floor to `max(index_build, recorded)`.
    ///
    /// First contact (no recorded floor) reads as `0` and accepts — the genuine
    /// residual the §8 freshness gate bounds. Persisting the advance is best-effort: a
    /// write failure never turns an already-passed check into a reject (the rollback
    /// decision was made against the value read *before* the write) — refusing there
    /// would trade a bounded replay residual for a permanently wedged store. But
    /// best-effort is not SILENT: a failed persist is a standing replay window (the
    /// next run re-reads the old floor and accepts the same generation again), so the
    /// failure is said out loud on stderr rather than discarded.
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

    /// [`Self::check_and_record`] with the persist failure returned instead of printed —
    /// the accept/refuse DECISION and the durability of the advance are two different
    /// claims, and tests must be able to observe the second without scraping stderr.
    pub(crate) fn check_and_record_classified(
        &self,
        index_build: u64,
    ) -> (Result<(), Reject>, Option<String>) {
        // Serialize the whole read -> compare -> write across concurrent atpkg
        // processes (CWE-362). Without this, two processes that both `read_floor()` floor F
        // before either `write()`s can regress the durable floor BELOW the higher of
        // their builds (A=50 and B=45 both read 41; if B writes last it stores 45,
        // clobbering 50) — after which a later, older-but-once-valid signed index in
        // the lost interval would be ACCEPTED instead of refused, weakening the
        // anti-rollback guarantee. The lock makes the advance monotonic. If the lock
        // cannot be taken it degrades to best-effort (never a false reject — the
        // rollback decision is still made against the value `read_floor()` returns).
        let _lock = self.acquire_file_lock();
        let recorded = self.read_floor();
        if index_build < recorded {
            return (Err(Reject::Rollback), None);
        }
        // Durable advance under the lock; never downgrades the recorded value, and a
        // failure is REPORTED (never a reject — see `check_and_record`'s doc).
        let persist_failure = self
            .write(index_build.max(recorded))
            .err()
            .map(|e| e.to_string());
        (Ok(()), persist_failure)
    }

    /// Write `value` unconditionally, **including downward** — the one operation on this
    /// type that is not monotonic.
    ///
    /// The ONLY legitimate caller is the build-floor re-base a strictly newer, master-signed
    /// roster generation earns (see [`BuildFloor`]): a floor set by a machine that is now
    /// revoked must not outlive the generation that revoked it, or the revocation can never
    /// be delivered. It is deliberately NOT reachable from anything an index or a machine
    /// says — the caller must already hold proof that `admit_roster` accepted a generation
    /// above the one that recorded the floor, and only the paper master can mint one.
    ///
    /// Best-effort, like [`Self::check_and_record`]'s write: a failed re-base leaves the old
    /// floor, which is the fail-closed direction (the store stays refusing rather than
    /// silently opening).
    pub fn rebase(&self, value: u64) {
        let _lock = self.acquire_file_lock();
        if let Err(error) = self.write(value) {
            // Fail-closed direction (the store keeps refusing under the old, higher
            // floor), but the master's rescue lever silently not landing would read as
            // "the re-base did nothing" — say so.
            eprintln!(
                "atpkg: could not re-base the floor at {} to {value}: {error}",
                self.path.display()
            );
        }
    }

    /// Acquire the advisory lock guarding [`Self::check_and_record`]'s critical
    /// section: `LOCK_EX` on a sibling `<floor>.lock` file (created `0600`), released
    /// on drop. `None` if it cannot be taken (e.g. the directory does not exist yet) —
    /// callers then proceed best-effort, which is still rollback-safe, only not
    /// strictly monotonic under concurrency.
    fn acquire_file_lock(&self) -> Option<FileLock> {
        // `Path::file_name` / `OsStr::to_str` go via `call1`: std's INLINED
        // `unsafe` (the `from_utf8_unchecked` fast path, the `OsStr` byte-slice
        // casts) is otherwise attributed to this function's spans as
        // missing-SAFETY-comment refutations under the strict Trust gate (see
        // `lib.rs`). Same calls, same receivers; behavior identical. The
        // `format!("{name}.lock")` is a manual concat for the same reason (its
        // expansion embeds `fmt::Arguments` construction the gate cannot lower)
        // — byte-identical.
        let name = match crate::call1(std::path::Path::file_name, self.path.as_path()) {
            Some(n) => crate::call1(std::ffi::OsStr::to_str, n),
            None => None,
        }
        .unwrap_or("floor");
        let mut lock_name = String::from(name);
        lock_name.push_str(".lock");
        let lockpath = self.path.with_file_name(lock_name);
        FileLock::acquire(&lockpath).ok()
    }

    /// The recorded floor, or `0` if the file is missing or unparseable (fail-open
    /// only for first contact, per §8).
    ///
    /// A file that EXISTS but will not read as a floor is reported on stderr before the
    /// `0` is returned. The value must still be `0` — refusing outright would let anyone
    /// who can scribble on the file wedge the store permanently (a trivial local DoS,
    /// and the system itself never produces a malformed floor: writes are temp+rename
    /// atomic) — but corruption reading silently as "first contact" is how a rollback
    /// window reopens with nobody told. The classification is separated so a test can
    /// pin "corrupt is detected AND still reads 0" without scraping stderr.
    fn read_floor(&self) -> u64 {
        let (value, corrupt) = self.read_floor_classified();
        if corrupt {
            eprintln!(
                "atpkg: the rollback floor at {} exists but is unreadable or not a \
                 number — treating it as first contact (0); older signed documents will \
                 be accepted again until the ratchet re-advances",
                self.path.display()
            );
        }
        value
    }

    /// `(recorded floor, corrupt)`: `corrupt` is true iff the file EXISTS but did not
    /// yield a `u64` — indistinguishable from first contact by value alone, which is
    /// exactly why it is worth distinguishing here.
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

    /// Atomically publish `value` to the floor file: refuse a non-private parent dir
    /// (CWE-379 — a foreign-owned or group/other-writable dir lets another local user
    /// pre-create or swap the file), then write a sibling `0600` temp and `rename` it
    /// over the target so a reader never sees a half-written floor.
    fn write(&self, value: u64) -> io::Result<()> {
        use std::io::Write as _;

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
        {
            let mut f = crate::platform::open_create_write(&tmp, 0o600)?;
            f.write_all(value.to_string().as_bytes())?;
            // The advance must be DURABLE, not merely renamed: a rename of bytes still
            // in the page cache can, after a power loss, expose a correctly-named empty
            // file — which reads as first contact and silently reopens the rollback
            // window the write existed to close.
            f.sync_all()?;
        }
        // Force 0600 even if the temp pre-existed with looser bits, then publish.
        crate::platform::harden_file(&tmp)?;
        std::fs::rename(&tmp, &self.path)?;
        // Best-effort directory sync so the rename itself survives a power loss. The
        // floor value is already safe either way (old value or new, never torn).
        if let Ok(dir) = std::fs::File::open(parent) {
            let _ = dir.sync_all();
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_update_core::roster::Machine;
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD;
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
        STANDARD.encode(kp.public_key().as_ref())
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

    /// An `index.toml` body naming `machine_id` / `roster_seq` — the attribution the
    /// bind (step 10) checks against the key that actually verified.
    /// Waiving the anti-rollback floor must require an index that DECLARES the newer
    /// generation, not merely one served beside a newer roster. `Attribution::bind`
    /// admits `index.roster_seq <= roster.roster_seq`, so re-pairing the owner's own
    /// unmodified old index with the owner's own unmodified new roster forged nothing
    /// and still re-based the floor downward (2026-08-20 round-8 audit).
    #[test]
    fn only_the_generation_an_index_declares_waives_the_floor() {
        let floor = BuildFloor {
            index_build: 100,
            roster_seq: 4,
        };
        // The master's rescue: a NEW index that declares generation 5.
        assert!(
            floor.admits(5, 1),
            "a genuinely newer generation re-bases the floor"
        );
        // The replay: an OLD index (declaring 4) served beside a generation-5 roster.
        assert!(
            !floor.admits(4, 99),
            "an index below the floor must not be admitted by its neighbour's roster"
        );
        assert!(floor.admits(4, 100), "…while the floor itself still passes");
        // An index that declares nothing cannot waive a generation floor.
        assert!(!floor.admits(0, 99));
    }

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

    fn private_tmp_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("atpkg-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        #[cfg(unix)]
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        dir
    }

    // ---------------------------------------------------------------------------------
    // THE DANGEROUS DIRECTION FIRST.
    // ---------------------------------------------------------------------------------

    /// EMPTY MEANS UNPINNED MEANS INERT. An unarmed anchor authorizes NOTHING — not even
    /// a roster genuinely signed by a real paper master, and not even one whose signature
    /// would verify under a key that IS in the document. There is no "accept anything"
    /// path, and the refusal happens before any crypto.
    ///
    /// This is the mutation that matters: flip `!anchor.is_armed()` to `anchor.is_armed()`
    /// in `admit_roster` and this test fails on the FIRST assertion, because an unpinned
    /// build would then hand `verify_roster` an empty keyset and — were that to change
    /// too — trust whatever index it was handed.
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
        // NON-VACUITY: the very same bytes and signature are accepted the moment a master
        // is pinned, so the refusal above is about the anchor and not about the fixture.
        assert!(admit_roster(&armed(0), bytes, &sig, NOW).is_ok());
    }

    /// The SHIPPED anchor is ARMED (2026-08-15) and the manager is live because of it.
    /// If this ever fails, the anchor was emptied or broken in a commit — a deliberate
    /// act that must be reviewed, not a surprise discovered by a test.
    #[test]
    fn this_build_ships_armed_and_the_pinned_anchor_is_live() {
        // Flipped 2026-08-15 with the arming commit (this was the unset-anchor
        // tripwire the arming initially missed — atpkg's fifth).
        assert!(
            !crate::PKG_TRUST_ANCHORS.is_empty(),
            "atpkg's anchor is pins::PAPER_MASTER_PUBKEYS, armed 2026-08-15"
        );
        assert!(
            Anchor::pinned(0).is_armed(),
            "so the pinned anchor is live by construction"
        );
        assert!(
            crate::enabled() || std::env::var_os("ATPKG_DISABLE").is_some(),
            "and the manager acts at the CLI edge unless explicitly disabled"
        );
    }

    // ---------------------------------------------------------------------------------
    // THE HAPPY PATH, AND THE THREE WAYS IT IS NOT.
    // ---------------------------------------------------------------------------------

    /// ARMED: an index signed by a machine the roster names is ACCEPTED, and the verifier
    /// can say WHICH machine signed it.
    #[test]
    fn an_index_signed_by_a_roster_named_machine_is_accepted() {
        let anchor = armed(0);
        let roster = admit(&anchor, &roster()).expect("master-signed, fresh, above the floor");
        assert_eq!(roster.seq(), 3);
        assert_eq!(roster.master_index(), 0);

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

    /// ARMED: an index signed by a machine that is NOT on the roster is REFUSED, though
    /// the roster is genuine and the signature is mathematically perfect.
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
        // NON-VACUITY: the same bytes signed by a listed machine are accepted.
        assert!(
            roster
                .authorize_index(raw, &sign(&M3_SEED, &index_body("m3", 3, 41)))
                .is_ok()
        );
    }

    /// REVOCATION, and the ORDERING that makes it worth having: m11's key is genuine, was
    /// once legitimately authorized, and still signs perfectly — and it is refused, with
    /// its signature never checked, because the roster that names it also revokes it.
    ///
    /// The proof that the signature was never checked is the VERDICT: a revoked machine
    /// leaves the candidate set in `live()`, so the failure is `Verify` ("nobody
    /// authorized signed this") and not `Revoked` ("we checked, then noticed"). The
    /// negative control below shows the very same bytes and signature verifying under a
    /// roster that does not revoke it, so the refusal is the deny-list and nothing else.
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

        // NEGATIVE CONTROL: identical bytes, identical signature, one roster generation
        // that does not revoke m11 — accepted. So the refusal above is the revocation.
        let mut same_but_live = roster();
        same_but_live.roster_seq = 4;
        let permissive = admit(&anchor, &same_but_live).unwrap();
        assert!(permissive.authorize_index(raw, &sig).is_ok());

        // TARGETED, not a channel-wide brick: m3 on the revoking roster still works.
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

    // ---------------------------------------------------------------------------------
    // THE ROSTER ITSELF: missing, unverifiable, stale, rolled back — all REFUSED, and
    // never with a fallback to any older root.
    // ---------------------------------------------------------------------------------

    /// A roster signed by a DIFFERENT master is refused under the pinned one, and the
    /// chain never reaches a parse.
    #[test]
    fn a_roster_signed_by_the_wrong_master_is_refused() {
        let (bytes, sig) = published(&roster(), &OTHER_MASTER_SEED);
        assert_eq!(
            admit_roster(&armed(0), bytes, &sig, NOW).err(),
            Some(Reject::Verify)
        );
    }

    /// A ONE-BYTE EDIT anywhere in the roster is refused: adding a machine to a genuine
    /// roster without the paper master cannot work.
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
        // NON-VACUITY: unflipped, the same pair verifies.
        assert!(admit_roster(&armed(0), bytes, &sig, NOW).is_ok());
    }

    /// A ROLLED-BACK roster is refused permanently once a newer generation has been
    /// durably seen — its master signature still verifies, which is exactly why the
    /// ratchet exists. Signatures do not expire; documents do.
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

    /// A LAPSED roster is refused, and an unreadable `valid_until` counts as lapsed
    /// rather than absent — freshness is the ONLY defence a brand-new install has,
    /// because it carries no floor.
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

    /// A roster from a format this build does not understand is REFUSED rather than
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

    // ---------------------------------------------------------------------------------
    // THE ARTIFACT AND THE BIND.
    // ---------------------------------------------------------------------------------

    /// A ONE-BYTE EDIT anywhere in the index is refused — over the exact raw bytes, with
    /// no normalization: a trailing newline or a stray space is a different document.
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

    /// THE ID BIND, both directions. A genuine signature by one machine cannot be
    /// relabelled as another (the id is inside the signed bytes), a machine cannot claim
    /// someone else's id (the roster maps id to key), an unattributed index is refused
    /// outright, and an index cannot be paired with a roster generation that is not its
    /// own.
    #[test]
    fn the_index_must_name_the_machine_and_generation_that_actually_signed_it() {
        let roster = admit(&armed(0), &roster()).unwrap();

        // m11 signs bytes that CLAIM m3. The key decides who signed; the claim disagrees.
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

    /// A signature of the wrong LENGTH is refused cheaply, before the crypto, and an
    /// index whose bytes verify but whose TOML is malformed or from a newer schema is a
    /// post-verify refusal — not an oracle, since it is decided from authenticated bytes.
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

    /// The raw-bytes property, on the signed path: a genuine signature over bytes that
    /// contain 0xFF verifies (a verifier that ran `from_utf8_lossy` first would substitute
    /// U+FFFD, change the bytes, and wrongly reject), and flipping that byte still fails.
    /// The parse afterwards is what refuses non-UTF-8 — and it refuses it as `Malformed`,
    /// AFTER the signature, never before.
    #[test]
    fn raw_non_utf8_bytes_are_verified_without_lossy_conversion() {
        let roster = admit(&armed(0), &roster()).unwrap();
        let mut raw = index_body("m3", 3, 41);
        let bad_idx = raw.len();
        raw.push(0xFF);
        let sig = sign(&M3_SEED, &raw);
        // The signature is over the exact bytes, so the failure is the PARSE, not Verify.
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

    // ---------------------------------------------------------------------------------
    // THE STORE'S OTHER GATES — unchanged by the single-root move, and re-proved here so
    // a regression in them shows up in this file rather than in the field.
    // ---------------------------------------------------------------------------------

    /// now >= valid_until ⇒ Stale; now < valid_until ⇒ Ok.
    #[test]
    fn freshness_gate_rejects_at_or_after_valid_until() {
        assert_eq!(check_freshness(100, 200), Ok(()));
        assert_eq!(check_freshness(199, 200), Ok(()));
        assert_eq!(check_freshness(200, 200), Err(Reject::Stale));
        assert_eq!(check_freshness(201, 200), Err(Reject::Stale));
    }

    /// index_build below the recorded Floor ⇒ Rollback; a higher build advances the
    /// durable floor (and the file is 0600).
    #[test]
    fn high_water_floor_blocks_rollback_and_advances() {
        let dir = private_tmp_dir("floor");
        let path = dir.join("index_build.floor");
        let floor = Floor::new(path.clone());
        // First contact: no recorded floor ⇒ accepted and recorded.
        assert_eq!(floor.check_and_record(41), Ok(()));
        // A LOWER build is a rollback.
        assert_eq!(floor.check_and_record(40), Err(Reject::Rollback));
        // Equal is allowed (the gate is index_build >= floor).
        assert_eq!(floor.check_and_record(41), Ok(()));
        // A higher build advances the durable floor...
        assert_eq!(floor.check_and_record(50), Ok(()));
        // ...so a build below the NEW floor is now rejected even though it beat the
        // first floor — proving the advance was persisted across calls.
        assert_eq!(floor.check_and_record(45), Err(Reject::Rollback));
        // A fresh Floor over the same path sees the durable value.
        assert_eq!(
            Floor::new(path.clone()).check_and_record(49),
            Err(Reject::Rollback)
        );
        // The floor file is 0600 — Unix-only mode check.
        #[cfg(unix)]
        {
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "floor file must be 0600");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The SAME `Floor` primitive now also carries the roster's `roster_seq` ratchet
    /// (`<prefix>/roster.floor`), so the replay defence a roster generation gets is the
    /// one the index has been getting all along — one implementation, one set of teeth.
    #[test]
    fn the_roster_floor_uses_the_same_durable_ratchet() {
        let dir = private_tmp_dir("roster-floor");
        let floor = Floor::new(dir.join("roster.floor"));
        assert_eq!(floor.check_and_record(3), Ok(()));
        assert_eq!(floor.current(), 3);
        assert_eq!(floor.check_and_record(2), Err(Reject::Rollback));
        // And the value it hands an Anchor is what refuses the replayed roster.
        assert!(
            admit(&armed(floor.current()), &roster()).is_ok(),
            "seq 3 == floor 3 is accepted"
        );
        assert_eq!(floor.check_and_record(4), Ok(()));
        assert_eq!(
            admit(&armed(floor.current()), &roster()).err(),
            Some(Reject::Rollback),
            "once seq 4 is durable, the seq-3 roster is refused forever"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// THE BUILD FLOOR IS SCOPED TO THE GENERATION THAT SET IT, because `index_build` is a
    /// number a MACHINE chooses and the roster tier must stay able to overrule a machine.
    ///
    /// Within one generation it is the monotonic ratchet it always was. A strictly NEWER
    /// generation — which only the paper master can mint — re-bases it, so one machine
    /// publishing `index_build = u64::MAX` cannot put the store beyond the reach of the very
    /// index that revokes it.
    ///
    /// MUTATION: weaken `>` to `>=` (or to `!=`) in `BuildFloor::admits` and the last two
    /// assertions fail — an equal or older generation would start waiving the floor, which
    /// is the direction that lets a replayed index in.
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
        // A STRICTLY newer generation re-bases it — the master's rescue lever.
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

    /// A FLOOR FILE THAT EXISTS BUT WILL NOT READ AS A FLOOR is detected as corruption —
    /// and still reads as `0`. Both halves matter: the `0` because a same-uid scribbler
    /// must not be able to wedge the store permanently (and the system never produces a
    /// malformed floor itself — writes are temp+rename atomic), the detection because
    /// corruption silently indistinguishable from first contact is a rollback window
    /// reopening with nobody told.
    ///
    /// MUTATION: collapse the corrupt class into "absent" in `read_floor_classified`
    /// (return `(0, false)` for unparseable content) and the three `corrupt` assertions
    /// fail; make corruption REFUSE instead of reading 0 and the value assertions fail.
    #[test]
    fn a_corrupt_floor_file_is_detected_and_still_reads_as_first_contact() {
        let dir = private_tmp_dir("floor-corrupt");
        let path = dir.join("roster.floor");
        let floor = Floor::new(path.clone());
        // Absent is GENUINE first contact — not corruption.
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

    /// A FLOOR ADVANCE THAT CANNOT PERSIST keeps the accept (refusing would trade a
    /// bounded replay residual for a permanently wedged store) but must SURFACE the lost
    /// advance — a silent persist failure is a standing replay window: the next run
    /// re-reads the old floor and accepts the same generation again, which this test
    /// demonstrates before asserting the failure is reported.
    ///
    /// MUTATION: restore the discarded `let _ = self.write(..)` (report nothing) and the
    /// `Some` assertion fails.
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

        // NEGATIVE CONTROL: with the directory healthy the same advance persists and
        // reports nothing.
        let (decision, persist_failure) = floor.check_and_record_classified(8);
        assert_eq!(decision, Ok(()));
        assert_eq!(persist_failure, None);
        assert_eq!(Floor::new(path).current(), 8);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// THE FREEZE CONTRACT'S REVALIDATION HOOK: a held `TrustedRoster` authorizes under
    /// the clock captured at admission, so a long-lived holder must be able to ask "has
    /// my frozen clock been outlived?" — and get `Stale` once the roster's own window
    /// has lapsed, from the same gate admission ran.
    #[test]
    fn a_cached_trusted_roster_can_be_revalidated_against_a_later_clock() {
        let trusted = admit(&armed(0), &roster()).unwrap();
        // Within the window (the fixture's valid_until is 2027-02-01): still fresh.
        assert_eq!(trusted.still_fresh(NOW), Ok(()));
        // A later clock past the window: the frozen reading has been outlived, and the
        // holder must drop this value and re-admit — even though `authorize` itself
        // would still happily verify under the stale freeze (which is the hazard).
        assert_eq!(trusted.still_fresh(1_900_000_000), Err(Reject::Stale));
        let raw = index_body("m3", 3, 41);
        assert!(
            trusted
                .authorize_index(raw.clone(), &sign(&M3_SEED, &raw))
                .is_ok(),
            "precondition: the frozen clock alone would never notice the lapse"
        );
    }

    #[test]
    fn oversized_sparse_floor_is_bounded_first_contact() {
        let dir = private_tmp_dir("floor-sparse");
        let path = dir.join("index_build.floor");
        let file = std::fs::File::create(&path).unwrap();
        file.set_len((MAX_FLOOR_BYTES + 1) as u64).unwrap();
        assert_eq!(Floor::new(path).current(), 0);
        let _ = std::fs::remove_dir_all(dir);
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
        let _ = std::fs::remove_dir_all(dir);
    }

    // The post-verify parsers live in `crate::manifest`; the "parser never runs on
    // unverified bytes" guarantee, the table-scoping/duplicate-key cases, and the
    // attribution fields are tested there against genuine `VerifiedBytes`.
}
