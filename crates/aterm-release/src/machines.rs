// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The PRODUCER half of the machine-roster tier: this machine's public identity, the
//! cut-time gate that proves it is still authorized, and the stamp that puts attribution
//! inside the signed appcast bytes.
//!
//! # What this module is for
//!
//! Under one paper master and many machine keys, a cut has to answer a question the
//! one-key design never had to: *may THIS machine publish?* The old answer was an
//! equality check — the configured key's public identity had to equal
//! `UPDATE_CHANNEL_PUBKEYS[0]`, which is exactly one key, so exactly one machine could
//! ever cut. That is precisely what the owner's decision removes, and it is the reason
//! this module exists.
//!
//! The new answer is [`authorize_cut`]: the material must be a machine NAMED by a valid,
//! unrevoked, master-signed roster whose master is one of the pinned anchors. The gate is
//! the client's own verifier over the exact bytes to be published, run at cut time.
//!
//! That is the WHOLE authorization question under an armed anchor — the committed channel
//! keyset is not part of it, because the armed client does not consult its keyset either
//! (`aterm_update::github::fetch_authoritative_release`). What the keyset still decides is
//! a different question, about a different audience: whether clients that PREDATE the
//! roster can verify this release at all. That is `publish::PreRosterClients`, and it is
//! deliberately not here — this module answers "may this machine publish?", and the answer
//! must not silently fold in "and is everyone able to read it?".
//!
//! It is HALF of the client's chain, not all of it, and saying so precisely matters. The
//! client admits a roster on two conditions: freshness, and a durable replay floor it
//! ratchets from every generation it has ever observed. Only the first is knowable from a
//! local file. The floor is channel state, so it belongs to — and is enforced by —
//! `publish::roster_floor_covered`, which reads it out of the published head's own
//! manifest, pre-claim and again under the release lease at lock, selfcheck, preflip and
//! flip. The two together are the client's chain; either alone is weaker than the fleet.
//!
//! # Why the identity file holds no secret
//!
//! [`MachineIdentity`] reads `~/.aterm/machine.toml`, which `atpkg-keys setup`/`join`
//! writes beside the key. It contains an id, a public key and a timestamp — nothing that
//! needs protecting. The SECRET half stays in `~/.aterm/machine.key` (`0600`) and is
//! loaded only by the signing path, which is `sign.rs`'s job and deliberately not this
//! module's. Keeping the two apart means the cut can answer "which machine am I?" — for
//! logging, for the journal, for the manifest — without any code path that touches a
//! secret needing to run.

//! # WIRED — and where
//!
//! This module used to carry a "pending wiring" note because `publish.rs` was owned by a
//! concurrent workstream. It is wired now, at three call sites, and they are worth naming
//! because the ORDER between them is the whole safety argument:
//!
//! * [`authorize_cut`] runs from `publish::preflight_signature_policy`, which runs with
//!   the other PRE-CLAIM gates — before the ledger claim burns a single-use build number
//!   and before anything remote is mutated. A machine that may not publish finds out
//!   while finding out is free. It runs only for a `publish::RosterDuty::Sign` entry:
//!   a re-entry that is finishing already-signed bytes has no attribution left to
//!   choose, and the roster it would read is not the one frozen in that cut's `dist/`.
//! * [`attribute`] runs in `publish::stage_manifest`, between assembly and serialization,
//!   so the attribution is inside the bytes the signature covers.
//! * [`RosterDocument`] is carried from the gate to the build step, so the roster that
//!   AUTHORIZED the cut is byte-identically the roster the cut PUBLISHES. Re-reading the
//!   file at staging time would leave a window in which the two could differ.
//! * [`verify_published_roster`] runs from `publish::recover_published_cut`, in the
//!   opposite direction: it proves a roster DOWNLOADED from an already-published release
//!   is the one that release's signed manifest names, so recovery can reconstruct
//!   `dist/` completely enough for the mirror to serve the public channel the same bytes
//!   `verify` proved live.
//!
//! # The tier is ARMED in this tree (2026-08-15)
//!
//! `pins::PAPER_MASTER_PUBKEYS` names the paper master, so every cut from this tree runs
//! [`authorize_cut`] — v0.21.0 was the first, signed by m3's rostered key. Everything
//! below is ALSO exercised by `tests/machine_roster.rs` with synthetic masters, so the
//! rule set stays proven independently of the tree's own arming state.

use std::path::{Path, PathBuf};

use aterm_update_core::Manifest;
use aterm_update_core::roster::{Attribution, Roster, verify_roster};

use crate::ledger::{Error, Result};

/// This machine's PUBLIC identity, as `atpkg-keys setup`/`join` recorded it.
///
/// No `Debug` redaction is needed and none is present, deliberately: every field here is
/// public by construction, and pretending otherwise would blur the line with
/// `sign.rs`'s `ReleaseCredentials`, where the redaction is load-bearing.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct MachineIdentity {
    /// The machine id — `"m3"`. What a verifier reports and a deny-list names.
    pub id: String,
    /// The machine's base64 Ed25519 public key. The secret half never leaves this machine.
    pub pubkey: String,
    /// RFC3339 mint time. Informational.
    #[serde(default)]
    pub minted_at: String,
}

impl MachineIdentity {
    /// Read the identity from a path (`~/.aterm/machine.toml` in practice).
    ///
    /// A missing file is `Ok(None)`, not an error: a machine that has never been minted is
    /// the ordinary state of every machine that does not publish, and of every machine at
    /// all until the tier is armed. A PRESENT but unreadable file IS an error — that is a
    /// half-provisioned machine, and guessing at its identity is exactly the kind of
    /// silent wrong answer attribution exists to prevent.
    pub fn read(path: &Path) -> Result<Option<Self>> {
        if !path.exists() {
            return Ok(None);
        }
        let text = std::fs::read_to_string(path)
            .map_err(|e| Error::new(format!("read {}: {e}", path.display())))?;
        let identity: Self = toml::from_str(&text)
            .map_err(|e| Error::new(format!("parse {}: {e}", path.display())))?;
        if identity.id.is_empty() || identity.pubkey.is_empty() {
            return Err(Error::new(format!(
                "{} names no machine id or no public key; re-mint with `atpkg-keys \
                 join --id <id>`",
                path.display()
            )));
        }
        Ok(Some(identity))
    }
}

/// Where a machine's public identity record lives by convention: `~/.aterm/machine.toml`,
/// beside the secret key `atpkg-keys setup`/`join` wrote and never moves.
///
/// Returning `None` when `HOME` is unreadable rather than erroring is deliberate: this
/// path is only ever a CROSS-CHECK, so "I could not find out who this machine claims to
/// be" must not by itself refuse a cut the roster is happy with.
pub fn conventional_identity_path() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".aterm/machine.toml"))
}

/// Resolve what this machine CLAIMS its id is, from the two places that may say so.
///
/// Explicit beats conventional: a `machine_id` in the release-credentials profile is part
/// of the command that ran, so it wins outright and the conventional file is not consulted
/// at all. Only when the profile is silent is `~/.aterm/machine.toml` read — the record
/// `atpkg-keys setup`/`join` leaves beside the key it minted.
///
/// This is ambient state, and it is admitted as an INPUT only because it cannot grant
/// anything. Both sources feed one comparison against the roster's own key→id map, and a
/// disagreement REFUSES the cut. Like `signing_identity_sha1`, it can narrow what is
/// accepted and can never widen it — which is why reading it does not re-open the ambient
/// discovery `--release-credentials` closed.
pub fn declared_machine_id(
    profile_machine_id: Option<&str>,
    identity_path: Option<&Path>,
) -> Result<Option<String>> {
    if let Some(id) = profile_machine_id {
        return Ok(Some(id.to_string()));
    }
    let Some(path) = identity_path else {
        return Ok(None);
    };
    Ok(MachineIdentity::read(path)?.map(|identity| identity.id))
}

/// The master-signed roster and its detached master signature, read ONCE.
///
/// Both halves travel together for the same reason the client fetches them together:
/// either alone proves nothing. The bytes are held rather than the path because the cut
/// must PUBLISH the same roster it was AUTHORIZED by — a path re-read at staging time
/// could name different bytes than the gate approved, which is precisely the
/// time-of-check/time-of-use hole that makes a producer-side gate decorative.
///
/// Nothing here is secret: a roster is a public release asset and a detached signature is
/// public by construction. It is `Clone` and stored in the cut context for that reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RosterDocument {
    /// The exact `aterm-machines.toml` bytes, unparsed and unmodified.
    pub bytes: Vec<u8>,
    /// The master's detached Ed25519 signature over [`Self::bytes`].
    pub signature: Vec<u8>,
}

impl RosterDocument {
    /// The conventional name of a roster's detached signature: `<roster>.sig`.
    ///
    /// One name in the credentials profile, not two. A second key could be pointed at a
    /// signature over a DIFFERENT roster, and the failure would read as "the master did
    /// not sign this" rather than "you named the wrong file".
    #[must_use]
    pub fn signature_path(roster: &Path) -> PathBuf {
        let mut name = roster.as_os_str().to_os_string();
        name.push(".sig");
        PathBuf::from(name)
    }

    /// Read both halves, naming whichever is missing.
    ///
    /// A missing file is a hard error, not `None`: this is only ever called once the
    /// roster tier is ARMED and the profile has NAMED a roster, and both of those are
    /// deliberate acts. Silently proceeding without the document is the one behaviour a
    /// fail-closed tier may never have.
    pub fn read(roster: &Path) -> Result<Self> {
        let signature_path = Self::signature_path(roster);
        let bytes = std::fs::read(roster).map_err(|e| {
            Error::new(format!(
                "read the machine roster {}: {e} — `machine_roster` in the \
                 release-credentials profile names it, so the cut cannot proceed without it",
                roster.display()
            ))
        })?;
        let signature = std::fs::read(&signature_path).map_err(|e| {
            Error::new(format!(
                "read the machine roster's master signature {}: {e} — it must sit beside \
                 the roster (`atpkg-keys setup`/`join` writes both)",
                signature_path.display()
            ))
        })?;
        Ok(Self { bytes, signature })
    }
}

/// How much of the freshness window a cut must still have in front of it.
///
/// The client checks `valid_until` at a strictly LATER wall clock than the producer
/// does — always, by the length of the cut plus however long a client waits to poll. A
/// producer that admits a roster with one second left therefore publishes a head the
/// entire fleet refuses (`Stale`) almost immediately, and `select_authoritative_release`
/// has no fallback to an older release, so the fleet stops updating until someone cuts
/// again under a re-signed roster.
///
/// Six hours is the honest floor for that gap: a universal build + notarization + upload
/// runs the better part of an hour, and the deployed staging window is six hours (see the
/// cut's own DONE line, "fleet stages within 6h"). A roster with less than that left is
/// one the fleet will refuse before it has finished taking the release, so refusing
/// PRE-CLAIM — while refusing is free — is strictly the better place to find out.
pub const MIN_REMAINING_WINDOW_SECS: i64 = 6 * 60 * 60;


/// THE CUT-TIME GATE. Prove that `signing_pubkey` belongs to a machine the roster
/// authorizes, and return the attribution the manifest will carry.
///
/// This runs the CLIENT's verifier over the exact bytes that will be published, which is
/// the only check worth having: a cut that fails here would have been refused by the
/// entire fleet after the fact.
///
/// The order mirrors the client's exactly — master signature, parse, freshness, deny-list,
/// identity — because a producer-side gate that checked a DIFFERENT set of conditions
/// would be worse than none: it would pass releases the fleet rejects, and reject releases
/// the fleet would take.
///
/// # What this function deliberately does NOT decide
///
/// The client's `admit` takes TWO arguments, and only one of them is knowable here. The
/// freshness window is a property of the roster document, so it is judged below. The
/// REPLAY FLOOR is not: it is the highest `roster_seq` the channel has already published,
/// which is remote state a local file cannot see. Passing 0 for it here is not a weakening
/// — it is a statement that this function does not own that question, exactly as
/// `ledger::next_build` does not own the channel's `min_build` floor. The owner is
/// [`crate::publish::roster_floor_covered`], which reads the floor out of the published
/// channel head and is called pre-claim and again under the lease at lock, selfcheck,
/// preflip and flip — the same four places `channel_floor_covered` guards `min_build`.
/// A producer-side gate that read a floor of 0 and claimed to be the whole client chain
/// would be the more dangerous arrangement, because the claim would be false.
///
/// `now_unix` is injected for the same reason it is on the client: the freshness gate is
/// pure, and every expiry case is testable without waiting for one.
/// THE FLEET'S ROSTER FLOOR, read from where the fleet reads it: the `roster_seq` of
/// the master-admitted `aterm-machines.toml` on the channel's LATEST release, or `None`
/// when that release carries no roster at all (a channel from before the tier). This is
/// the generation every client that has checked for updates has ratcheted its floor to
/// (`Floor::bump_and_write` ratchets on OBSERVATION of the asset), so it is the number a
/// cut's attribution must reach — and it is NOT necessarily the number inside the head
/// manifest: MEASURED 2026-08-18, a machine that joined the roster (seq 3) attached the
/// new pair to the already-published v0.23.0/v0.24.0 releases, whose manifests still
/// said `roster_seq = 2`; the pre-claim ratchet read the manifest, passed 2 ≥ 2, and the
/// cut shipped an attribution every client refused with `SeqMismatch` for hours.
///
/// Anonymous (`https://github.com/<slug>/releases/latest/download/…`, the same seed
/// path `provision` uses — no token, no API rate limit) and admitted under the committed
/// paper master, so an unsigned or forged asset cannot ratchet the producer. Errors are
/// transport or verification failures, returned as such — the caller decides whether
/// "cannot tell" fails the gate (it does: a wrong answer here burns a build number).
/// The master-admitted roster on the public channel's latest release: its generation
/// AND its verified bytes. The bytes matter at EQUAL generation: two machines that
/// each seeded from the same channel pair and each minted generation N+1 (both
/// master-signed, each listing only itself) agree on the number and disagree on the
/// document — a LINEAGE FORK that the number alone admits (2026-08-19 round-2 audit).
/// [`roster_lineage_agrees`] compares the documents where the generations tie.
pub(crate) fn channel_roster_document(
    slug: &str,
) -> std::result::Result<Option<(u64, Vec<u8>)>, String> {
    let url = |asset: &str| format!("https://github.com/{slug}/releases/latest/download/{asset}");
    let bytes = match anonymous_fetch(&url(aterm_update_core::roster::ROSTER_ASSET), 65_536) {
        Ok(b) => b,
        // No roster on the latest release: no floor. curl -f reports the HTTP status in
        // its stderr; anything that is not a clean 404 is "cannot tell", never "none".
        Err(e) if e.contains("returned error: 404") => return Ok(None),
        Err(e) => return Err(e),
    };
    let sig = anonymous_fetch(&url(aterm_update_core::roster::ROSTER_SIG_ASSET), 4_096)?;
    let verified = verify_roster(aterm_update_core::pins::PAPER_MASTER_PUBKEYS, bytes, &sig)
        .map_err(|e| format!("the channel roster did not verify under the committed paper master ({e:?})"))?;
    let parsed = Roster::parse(&verified)
        .map_err(|e| format!("the channel roster verified but did not parse ({e:?})"))?;
    Ok(Some((parsed.roster_seq, verified.as_slice().to_vec())))
}

/// At EQUAL generation the cut's roster (the pair in `dist/`) and the channel's must be
/// the same document; otherwise two lineages exist at one number and whichever flips
/// later strands every client that ratcheted on the other (and `provision` on either
/// machine later hard-stops on the fork). `None` channel ⇒ nothing to disagree with.
pub(crate) fn roster_lineage_agrees(
    local_roster: &[u8],
    carried_seq: Option<u64>,
    channel: Option<&(u64, Vec<u8>)>,
) -> std::result::Result<(), String> {
    match (carried_seq, channel) {
        (Some(carried), Some((observed, bytes))) if carried == *observed && local_roster != bytes.as_slice() => {
            Err(format!(
                "LINEAGE FORK: this cut carries machine-roster generation {carried} and the public \
                 channel's head carries generation {observed} too, but the two documents differ \
                 — two machines minted the same generation from the same seed. Publishing \
                 either over the other strands every client that ratcheted on the first. Stop; \
                 re-join from the machine holding the channel's document (never start a second \
                 roster)"
            ))
        }
        _ => Ok(()),
    }
}

/// One bounded anonymous download (the client's own roster limits: 64 KiB body, 4 KiB
/// signature — a floor read must never accept more than the fleet would).
fn anonymous_fetch(url: &str, cap: usize) -> std::result::Result<Vec<u8>, String> {
    let cap_s = cap.to_string();
    let out = std::process::Command::new("curl")
        .args([
            "-fsSL",
            "--proto",
            "=https",
            "--proto-redir",
            "=https",
            "--max-time",
            "60",
            "--max-filesize",
            &cap_s,
            url,
        ])
        .output()
        .map_err(|e| format!("failed to run curl: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!("curl {url}: {}", err.trim()));
    }
    if out.stdout.len() > cap {
        return Err(format!("{url}: exceeded the {cap}-byte cap"));
    }
    if out.stdout.is_empty() {
        return Err(format!("{url}: zero bytes"));
    }
    Ok(out.stdout)
}

pub fn authorize_cut(
    master_pubkeys: &[&str],
    roster_bytes: Vec<u8>,
    roster_sig: &[u8],
    signing_pubkey: &str,
    now_unix: i64,
) -> Result<Attribution> {
    let verified = verify_roster(master_pubkeys, roster_bytes, roster_sig).map_err(|e| {
        Error::new(format!(
            "the machine roster does not verify under the pinned paper master ({e:?}); \
             refusing a cut no client would accept"
        ))
    })?;
    let roster = Roster::parse(&verified)
        .map_err(|e| Error::new(format!("the machine roster is unusable ({e:?})")))?;
    // The floor is 0 for the reason stated in this function's doc: the channel's replay
    // floor is remote state, and `publish::roster_floor_covered` owns it. What is judged
    // here is freshness — and it is judged at the HORIZON, `now + MIN_REMAINING_WINDOW`,
    // not at `now`. Every client checks the same bound at a strictly later clock than the
    // producer, so admitting a roster that is merely still-valid publishes a head the
    // fleet refuses as soon as the window closes.
    roster
        .admit(0, now_unix.saturating_add(MIN_REMAINING_WINDOW_SECS))
        .map_err(|e| {
            Error::new(format!(
                "the machine roster is not usable for a cut right now ({e:?}) — it must \
                 still be valid {MIN_REMAINING_WINDOW_SECS} seconds from now, because \
                 every client checks its window at a later clock than this one. Re-sign \
                 it with the paper master (`atpkg-keys join`/`machine-revoke` \
                 refresh the window)"
            ))
        })?;

    // Compared as base64 TEXT, and that is exactly as strict as the client: `material` has
    // been through `canonical_update_pubkey` (decode + re-encode), and the decoder both
    // sides use refuses non-canonical padding and non-zero trailing bits — so every
    // spelling a client would verify under is already the canonical one. A byte-wise
    // comparison here would be the same comparison spelled longer.
    let me = roster
        .machines
        .iter()
        .find(|m| m.pubkey == signing_pubkey)
        .ok_or_else(|| {
            Error::new(format!(
                "the configured signing key's public identity {signing_pubkey} is not on \
                 the machine roster. Mint this machine a key with `atpkg-keys \
                 join --id <id>`, or cut from a machine that is listed"
            ))
        })?;
    // Revocation is checked through the same lookup the client uses, so a machine that has
    // been cut off cannot publish one last release from a stale local roster.
    let machine = roster.machine(&me.id, now_unix).map_err(|e| {
        Error::new(format!(
            "this machine ({}) may not sign: {e:?}. If it was revoked, that is the \
             roster working as intended",
            me.id
        ))
    })?;
    Ok(Attribution {
        machine_id: machine.id.clone(),
        pubkey_b64: machine.pubkey.clone(),
        roster_seq: roster.roster_seq,
    })
}

/// Which key the roster maps `id` to, if any — for REFUSAL MESSAGES only.
///
/// It exists because a remedy that names an action is a recommendation, and a
/// recommendation whose consequence the program can compute but does not is worse than
/// no recommendation at all. The mismatch refusal in
/// [`crate::publish::channel_signature_policy`] used to offer "or cut with the key that
/// belongs to <id>" unconditionally; on the one machine where the safe path actually
/// runs — the bootstrap box, whose `~/.aterm/machine.toml` names ITSELF while the cut
/// must go out under the incumbent head's key — that sentence points straight at the
/// key that strands the installed base. So the message asks this first.
///
/// Returns `None` for any input it cannot answer confidently: a roster that does not
/// verify, does not parse, or does not name the id. A refusal is already being emitted,
/// so a missing answer costs a clause, never a wrong verdict. Nothing here authorizes
/// anything — [`authorize_cut`] is the only function that does, it re-runs the whole
/// chain itself, and this deliberately shares none of its state.
#[must_use]
pub fn roster_pubkey_for(
    master_pubkeys: &[&str],
    roster_bytes: Vec<u8>,
    roster_sig: &[u8],
    id: &str,
) -> Option<String> {
    let verified = verify_roster(master_pubkeys, roster_bytes, roster_sig).ok()?;
    let roster = Roster::parse(&verified).ok()?;
    roster
        .machines
        .iter()
        .find(|m| m.id == id)
        .map(|m| m.pubkey.clone())
}

/// Prove that a roster downloaded from an ALREADY PUBLISHED release is the document that
/// release's manifest is attributed under.
///
/// Recovery reconstructs `dist/` from remotely validated bytes so the mirror can serve the
/// public channel exactly what `verify` proved live on the private repo. The two roster
/// assets are part of that set on the armed path, and they are the one part with no
/// SHA-256 in the manifest to check them against — the manifest's signature does not cover
/// them, the MASTER's does. So they are bound cryptographically instead, which is
/// strictly stronger than a digest: the master signature proves authorship, and the two
/// fields below prove it is THIS release's roster and not some other release's.
///
/// # What is deliberately not re-judged
///
/// Neither freshness nor the deny-list. These bytes are already published; they were
/// admitted by [`authorize_cut`] when the cut was made, and no local verdict can change
/// them now. Re-judging them would make recovery — the path taken when something has
/// ALREADY gone wrong — fail for a condition it cannot fix, and the only alternative
/// available to it would be to mirror DIFFERENT bytes, which is the one thing the mirror
/// step exists to prevent. The client will judge freshness and revocation for itself, as
/// it always does, against exactly these bytes.
pub fn verify_published_roster(
    master_pubkeys: &[&str],
    roster_bytes: Vec<u8>,
    roster_sig: &[u8],
    machine_id: &str,
    roster_seq: Option<u64>,
) -> Result<u64> {
    let verified = verify_roster(master_pubkeys, roster_bytes, roster_sig).map_err(|e| {
        Error::new(format!(
            "the published release's machine roster does not verify under this tree's \
             pinned paper master ({e:?}). A release that carries an attribution can only \
             be recovered by a tree that can check it — arm PAPER_MASTER_PUBKEYS with the \
             master that signed it, or finish this release by hand"
        ))
    })?;
    let roster = Roster::parse(&verified)
        .map_err(|e| Error::new(format!("the published machine roster is unusable ({e:?})")))?;
    // The pair (`machine_id`, `roster_seq`) is inside the manifest's SIGNED bytes. The
    // roster asset beside a published release may legitimately be NEWER than the
    // manifest's attribution — a machine joining the roster attaches the new pair to
    // releases that already shipped — and every v0.25+ client admits exactly that
    // (`Attribution::bind`: manifest.roster_seq <= roster.roster_seq). Recovery mirrors
    // the client: an OLDER asset than the manifest names, or no attribution at all,
    // is a roster the signed manifest does not name and is refused; a newer one is
    // the channel's steady state after a join (2026-08-19 audit — equality here
    // wedged `recover-lost` behind a lease forever once a join had re-dressed the
    // origin release).
    match roster_seq {
        Some(claimed) if claimed <= roster.roster_seq => {}
        _ => {
            return Err(Error::new(format!(
                "the published release's manifest claims roster_seq {roster_seq:?} but the \
                 roster asset carries {}; refusing to reconstruct a roster the signed \
                 manifest does not name (a newer asset over an older attribution is \
                 admissible; this is not that)",
                roster.roster_seq
            )));
        }
    }
    if !roster.machines.iter().any(|m| m.id == machine_id) {
        return Err(Error::new(format!(
            "the published release is attributed to machine {machine_id:?}, which the \
             roster asset beside it does not list; refusing to reconstruct a roster that \
             contradicts the signed manifest"
        )));
    }
    // The generation actually carried by the asset — what a recovery WRITES — which
    // may be newer than the manifest's attribution.
    Ok(roster.roster_seq)
}

/// Stamp attribution INTO the manifest, before it is serialized and signed.
///
/// Both keys must land inside the signed bytes or they are worth nothing: `machine_id` is
/// what makes a signature unrelabellable, and `roster_seq` is what stops an old roster
/// being paired with this release. Stamping after signing would produce a manifest whose
/// attribution any attacker could rewrite.
pub fn attribute(manifest: &mut Manifest, who: &Attribution) {
    manifest.machine_id = Some(who.machine_id.clone());
    manifest.roster_seq = Some(who.roster_seq);
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD as B64;
    use ring::signature::{Ed25519KeyPair, KeyPair};

    // Obviously synthetic seeds; they appear nowhere but here.
    const MASTER: [u8; 32] = [0x51; 32];
    const M3: [u8; 32] = [0x52; 32];
    const M11: [u8; 32] = [0x53; 32];
    const NOW: i64 = 1_785_801_600; // 2026-08-04T00:00:00Z

    fn kp(seed: &[u8; 32]) -> Ed25519KeyPair {
        Ed25519KeyPair::from_seed_unchecked(seed).unwrap()
    }

    fn pk(seed: &[u8; 32]) -> String {
        B64.encode(kp(seed).public_key().as_ref())
    }

    /// A roster listing m3 and m11, with `revoked` under the caller's control.
    fn roster(revoked: &[&str]) -> (Vec<u8>, Vec<u8>, String) {
        let r = Roster {
            schema: 1,
            roster_seq: 6,
            valid_until: "2027-02-01T00:00:00Z".into(),
            machines: vec![
                aterm_update_core::roster::Machine {
                    id: "m3".into(),
                    pubkey: pk(&M3),
                    added_at: "2026-08-04T00:00:00Z".into(),
                    not_after: None,
                },
                aterm_update_core::roster::Machine {
                    id: "m11".into(),
                    pubkey: pk(&M11),
                    added_at: "2026-08-04T00:00:00Z".into(),
                    not_after: None,
                },
            ],
            revoked: revoked.iter().map(|s| (*s).to_string()).collect(),
        };
        let bytes = r.to_toml().unwrap().into_bytes();
        let sig = kp(&MASTER).sign(&bytes).as_ref().to_vec();
        (bytes, sig, pk(&MASTER))
    }

    /// A LISTED MACHINE may cut, and the attribution it gets back names it correctly.
    #[test]
    fn a_listed_machine_is_authorized_and_attributed() {
        let (bytes, sig, master) = roster(&[]);
        let who = authorize_cut(&[&master], bytes, &sig, &pk(&M3), NOW).unwrap();
        assert_eq!(who.machine_id, "m3");
        assert_eq!(who.pubkey_b64, pk(&M3));
        assert_eq!(who.roster_seq, 6);
    }

    /// AN UNLISTED KEY may not cut. This is the check that replaces the old
    /// "must equal UPDATE_CHANNEL_PUBKEYS[0]" equality — same refusal, wider allowance.
    #[test]
    fn a_key_not_on_the_roster_may_not_cut() {
        let (bytes, sig, master) = roster(&[]);
        let stranger = pk(&[0x5F; 32]);
        let err = authorize_cut(&[&master], bytes, &sig, &stranger, NOW).unwrap_err();
        assert!(
            err.to_string().contains("not on the machine roster"),
            "{err}"
        );
    }

    /// A REVOKED MACHINE may not cut, even from its own laptop with its own key and its
    /// own copy of the roster — because the roster that lists it also denies it.
    #[test]
    fn a_revoked_machine_may_not_cut() {
        let (bytes, sig, master) = roster(&["m11"]);
        let err = authorize_cut(&[&master], bytes, &sig, &pk(&M11), NOW).unwrap_err();
        assert!(err.to_string().contains("may not sign"), "{err}");
        // m3, on the same roster, still cuts — the refusal is targeted.
        let (bytes, sig, master) = roster(&["m11"]);
        assert!(authorize_cut(&[&master], bytes, &sig, &pk(&M3), NOW).is_ok());
    }

    /// THE WRONG MASTER, and NO master. Both refuse; neither is a fallthrough.
    #[test]
    fn a_roster_under_the_wrong_or_absent_master_refuses_the_cut() {
        let (bytes, sig, _) = roster(&[]);
        let other_master = pk(&[0x5E; 32]);
        assert!(authorize_cut(&[&other_master], bytes.clone(), &sig, &pk(&M3), NOW).is_err());
        let err = authorize_cut(&[], bytes, &sig, &pk(&M3), NOW).unwrap_err();
        assert!(err.to_string().contains("does not verify"), "{err}");
    }


    /// A LAPSED ROSTER refuses the cut. Publishing under it would produce a release every
    /// client refuses, so failing at the cutter is strictly the better place to find out.
    #[test]
    fn a_lapsed_roster_refuses_the_cut_before_anything_is_published() {
        let (bytes, sig, master) = roster(&[]);
        let long_after = 1_900_000_000;
        let err = authorize_cut(&[&master], bytes, &sig, &pk(&M3), long_after).unwrap_err();
        assert!(err.to_string().contains("not usable for a cut"), "{err}");
    }

    /// The fixture roster's `valid_until`, 2027-02-01T00:00:00Z, as unix seconds. Stated
    /// as a constant so the margin tests below name the boundary rather than guess at it.
    const FIXTURE_VALID_UNTIL: i64 = 1_801_440_000;

    /// A roster that is STILL VALID but about to lapse may not start a cut.
    ///
    /// The producer is the only party that can see this coming: it checks the window at a
    /// strictly earlier clock than every client, so "valid now" and "valid when the fleet
    /// looks" are different questions and only the second one matters.
    ///
    /// Kills the mutation "admit at `now` rather than at the horizon": the first case
    /// below then succeeds, and a cut would publish a head the fleet refuses within the
    /// hour.
    #[test]
    fn a_roster_about_to_lapse_may_not_start_a_cut() {
        let one_second_left = FIXTURE_VALID_UNTIL - 1;
        let (bytes, sig, master) = roster(&[]);
        let err = authorize_cut(&[&master], bytes, &sig, &pk(&M3), one_second_left).unwrap_err();
        assert!(err.to_string().contains("not usable for a cut"), "{err}");
        // Precondition, so the refusal above is not vacuous: the CLIENT's own rule — no
        // margin at all — still admits this very roster at this very instant. The producer
        // is refusing something the client would take, which is the whole point.
        let (bytes, sig, master) = roster(&[]);
        let verified = verify_roster(&[&master], bytes, &sig).unwrap();
        Roster::parse(&verified)
            .unwrap()
            .admit(0, one_second_left)
            .expect("the client's bound is still open — only the producer looks ahead");

        // One second past the horizon is the last refusal; one second before it is the
        // first acceptance. Naming both is what makes this a boundary and not a mood.
        let just_short = FIXTURE_VALID_UNTIL - MIN_REMAINING_WINDOW_SECS;
        let (bytes, sig, master) = roster(&[]);
        assert!(authorize_cut(&[&master], bytes, &sig, &pk(&M3), just_short).is_err());
        let (bytes, sig, master) = roster(&[]);
        assert!(authorize_cut(&[&master], bytes, &sig, &pk(&M3), just_short - 1).is_ok());
    }

    /// Two master-signed documents at the SAME generation are a lineage fork; the
    /// number admits it, so the bytes decide. Different generations, or no channel
    /// roster at all, are not a fork (they are what the ratchet judges).
    #[test]
    fn an_equal_generation_with_a_different_document_is_a_lineage_fork() {
        let ours = b"roster_seq = 4\n[[machine]]\nid = \"m3\"\n".to_vec();
        let theirs = b"roster_seq = 4\n[[machine]]\nid = \"m19\"\n".to_vec();
        assert!(roster_lineage_agrees(&ours, Some(4), Some(&(4, ours.clone()))).is_ok());
        let err = roster_lineage_agrees(&ours, Some(4), Some(&(4, theirs.clone()))).unwrap_err();
        assert!(err.contains("LINEAGE FORK"), "{err}");
        assert!(roster_lineage_agrees(&ours, Some(4), Some(&(3, theirs.clone()))).is_ok());
        assert!(roster_lineage_agrees(&ours, Some(3), Some(&(4, theirs))).is_ok());
        assert!(roster_lineage_agrees(&ours, Some(4), None).is_ok());
        assert!(roster_lineage_agrees(&ours, None, Some(&(4, ours.clone()))).is_ok());
    }

    /// A RECOVERED roster must be the one the published manifest names — and nothing about
    /// freshness or revocation, which recovery cannot change.
    #[test]
    fn a_recovered_roster_is_bound_to_the_manifest_that_names_it() {
        let (bytes, sig, master) = roster(&[]);
        verify_published_roster(&[&master], bytes.clone(), &sig, "m3", Some(6))
            .expect("the roster this manifest names");
        // A LAPSED roster still recovers: the bytes already shipped, and mirroring
        // different ones is the failure this whole step exists to prevent.
        let lapsed = Roster::parse(&verify_roster(&[&master], bytes.clone(), &sig).unwrap())
            .unwrap()
            .admit(0, 1_900_000_000);
        assert!(lapsed.is_err(), "precondition: these bytes are lapsed");
        verify_published_roster(&[&master], bytes.clone(), &sig, "m3", Some(6))
            .expect("recovery does not re-judge a window it cannot change");

        // The bindings that DO refuse: a manifest claiming a NEWER generation than the
        // asset (7 over 6), or none at all. An asset NEWER than the attribution (a
        // join re-dressed the release) is the channel's steady state and recovers.
        let err =
            verify_published_roster(&[&master], bytes.clone(), &sig, "m3", Some(7)).unwrap_err();
        assert!(err.to_string().contains("roster_seq"), "{err}");
        let err =
            verify_published_roster(&[&master], bytes.clone(), &sig, "m3", None).unwrap_err();
        assert!(err.to_string().contains("roster_seq"), "{err}");
        verify_published_roster(&[&master], bytes.clone(), &sig, "m3", Some(5))
            .expect("a newer roster asset over an older attribution is what a join produces");
        let err =
            verify_published_roster(&[&master], bytes.clone(), &sig, "m99", Some(6)).unwrap_err();
        assert!(err.to_string().contains("m99"), "{err}");
        // An UNARMED tree cannot check a rostered release at all, and says so rather than
        // reconstructing bytes it has no authority over.
        let err = verify_published_roster(&[], bytes, &sig, "m3", Some(6)).unwrap_err();
        assert!(err.to_string().contains("pinned paper master"), "{err}");
    }

    /// The stamp lands INSIDE the manifest, and survives the emit/parse round trip through
    /// the client's own parser — which is what puts it inside the signed bytes.
    #[test]
    fn attribution_is_stamped_into_the_signed_manifest_bytes() {
        let mut m = crate::manifest_out::build(&crate::manifest_out::ManifestInputs {
            version: "0.99.0",
            build_number: 990,
            commit: &"a".repeat(40),
            dmg_name: "aterm-0.99.0.dmg",
            dmg_sha256: &"ab".repeat(32),
            zip_name: "aterm-0.99.0-mac.zip",
            zip_sha256: &"cd".repeat(32),
            dmg_x86_64_name: None,
            dmg_x86_64_sha256: None,
            repo_slug: "owner/repo",
            min_os: "11.0",
            team_id: "",
            pub_date: "2026-08-04T00:00:00Z",
            min_build: None,
            changelog: "### Added\n- a thing\n",
        });
        // Before the stamp the keys are absent, which is exactly what an unarmed tier
        // publishes — and the negative control that makes the assertion below mean
        // something.
        assert_eq!(m.machine_id, None);
        assert_eq!(m.roster_seq, None);
        let text = m.to_toml().unwrap();
        assert!(!text.contains("machine_id"), "{text}");

        attribute(
            &mut m,
            &Attribution {
                machine_id: "m3".into(),
                pubkey_b64: pk(&M3),
                roster_seq: 6,
            },
        );
        let text = m.to_toml().unwrap();
        assert!(text.contains("machine_id = \"m3\""), "{text}");
        assert!(text.contains("roster_seq = 6"), "{text}");
        // The bytes the client will verify are the bytes carrying the attribution.
        let back = Manifest::parse(&text).unwrap();
        assert_eq!(back.machine_id.as_deref(), Some("m3"));
        assert_eq!(back.roster_seq, Some(6));
        assert_eq!(
            back, m,
            "the stamp must survive the publish round-trip exactly"
        );
    }

    /// A machine with no identity file is not an error — most machines never publish.
    /// A machine with a BROKEN one is, because guessing an identity is how attribution
    /// silently becomes wrong.
    #[test]
    fn a_missing_identity_is_none_and_a_broken_one_is_an_error() {
        let dir = std::env::temp_dir().join(format!("aterm-machine-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("machine.toml");
        assert_eq!(MachineIdentity::read(&path).unwrap(), None);

        std::fs::write(&path, "id = \"m3\"\npubkey = \"abc\"\nminted_at = \"x\"\n").unwrap();
        let me = MachineIdentity::read(&path).unwrap().unwrap();
        assert_eq!(me.id, "m3");
        assert_eq!(me.pubkey, "abc");

        std::fs::write(&path, "id = \"\"\npubkey = \"abc\"\n").unwrap();
        assert!(
            MachineIdentity::read(&path).is_err(),
            "an empty id is not an identity"
        );

        std::fs::write(&path, "this is not toml = = =").unwrap();
        assert!(MachineIdentity::read(&path).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
