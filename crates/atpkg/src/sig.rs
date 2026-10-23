// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Detached Ed25519 verification over the exact raw bytes of a manifest asset, enforced before any parse by construction.

use std::io;
use std::path::{Path, PathBuf};

use crate::platform::FileLock;
use aterm_update_core::roster::{Attribution, Roster, RosterReject, VerifiedRoster, verify_roster};

const MAX_FLOOR_BYTES: usize = 128;

/// Bytes that passed signature verification under a machine the pinned master's roster authorizes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedBytes(Vec<u8>);

impl VerifiedBytes {
    /// The verified raw bytes — the *same* bytes the signature was checked over, with no normalization or lossy conversion applied.
    #[inline]
    #[must_use]
    pub fn as_slice(&self) -> &[u8] {
        &self.0
    }
}

/// The single opaque rejection set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reject {
    /// No master is pinned (an unarmed build): the fail-closed default, returned before any crypto.
    Disabled,
    /// The public key did not base64-decode, or was not exactly 32 bytes.
    BadKey,
    /// The detached signature was not exactly 64 bytes.
    BadSig,
    /// The Ed25519 signature did not verify against the key over these bytes.
    Verify,
    /// The signing machine is on the roster's deny-list.
    Revoked,
    /// The roster authorizes no machine that could have signed this.
    NotAuthorized,
    /// The signing machine's own `not_after` has lapsed.
    Expired,
    /// The document carries no `machine_id`, so it cannot be attributed.
    Unattributed,
    /// The document's `roster_seq` is newer than the authorizing roster's, or absent: an old roster paired with a new index.
    SeqMismatch,
    /// The index's freshness window has lapsed (`now >= valid_until`), or the roster's has.
    Stale,
    /// A document below a durable high-water floor.
    Rollback,
    /// Bytes that passed signature verification but are not valid UTF-8 or TOML, or miss a required field or repeat a key.
    Malformed,
    /// A document whose `schema` is newer than this build understands ([`crate::manifest::SUPPORTED_SCHEMA`]).
    Schema,
    /// A `pkg-*.toml` row spelled with a retired `kind` (today only `vendor-fetch`, split into `kind` and `protocol` before any client saw it).
    RetiredKind(&'static str),
    /// A `pkg-*.toml` whose `shim_env` list breaks a rule a shim can honour ([`crate::shim_env::ShimEnv::admit`]).
    ShimEnv(String),
}

/// Translate the roster tier's verdict into atpkg's: one-to-one and total, with no catch-all,
/// so a new [`RosterReject`] variant is a compile error here.
#[inline]
const fn from_roster(e: RosterReject) -> Reject {
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
#[derive(Debug, Clone)]
pub struct Anchor {
    masters: Vec<String>,
    /// The durable `roster_seq` high-water this client has already accepted.
    pub roster_floor: u64,
}

impl Anchor {
    /// The anchor this binary ships with: the committed paper-master keyset plus the caller's durable roster floor.
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
    #[inline]
    #[must_use]
    pub const fn of(masters: Vec<String>, roster_floor: u64) -> Self {
        Self {
            masters,
            roster_floor,
        }
    }

    /// Whether any master is pinned.
    #[inline]
    #[must_use]
    pub const fn is_armed(&self) -> bool {
        !self.masters.is_empty()
    }

    /// The keyset as `verify_roster` wants it.
    fn keys(&self) -> Vec<&str> {
        self.masters.iter().map(String::as_str).collect()
    }
}

/// A roster generation that verified under the pinned master and passed admission: proof steps 1–7 ran.
#[derive(Debug, Clone)]
pub struct TrustedRoster {
    roster: Roster,
    now_unix: i64,
}

/// Steps 1–7: verify the roster's exact bytes under the pinned master, parse it only from `VerifiedRoster`, and admit it against the replay floor and its freshness window.
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
    let verified: VerifiedRoster = verify_roster(&anchor.keys(), raw, sig).map_err(from_roster)?;
    let roster = Roster::parse(&verified).map_err(from_roster)?;
    roster
        .admit(anchor.roster_floor, now_unix)
        .map_err(from_roster)?;
    Ok(TrustedRoster { roster, now_unix })
}

impl TrustedRoster {
    /// This generation's `roster_seq` — what the caller ratchets the durable floor to.
    #[inline]
    #[must_use]
    pub const fn seq(&self) -> u64 {
        self.roster.roster_seq
    }

    /// Steps 8–9: verify one artifact's exact bytes under this roster's live machines;
    /// revoked and expired ones leave the set before any crypto.
    fn authorize(&self, bytes: &[u8], sig: &[u8]) -> Result<Attribution, Reject> {
        self.roster
            .authorize_appcast(bytes, sig, self.now_unix)
            .map_err(from_roster)
    }

    /// [`Self::authorize`], minting the [`VerifiedBytes`] the post-verify parsers consume.
    pub fn authorize_bytes(
        &self,
        raw: Vec<u8>,
        sig: &[u8],
    ) -> Result<(VerifiedBytes, Attribution), Reject> {
        let who = self.authorize(&raw, sig)?;
        Ok((VerifiedBytes(raw), who))
    }

    /// Steps 8–10 for `index.toml`.
    pub fn authorize_index(
        &self,
        raw: Vec<u8>,
        sig: &[u8],
    ) -> Result<(TrustedIndex, VerifiedBytes), Reject> {
        let (verified, who) = self.authorize_bytes(raw, sig)?;
        let index = crate::manifest::parse_index(&verified)?;
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

/// An `index.toml` a master-signed roster generation authorized, paired with that generation.
#[derive(Debug, Clone)]
pub struct TrustedIndex {
    index: crate::manifest::Index,
    roster: TrustedRoster,
    attribution: Attribution,
}

impl std::ops::Deref for TrustedIndex {
    type Target = crate::manifest::Index;
    #[inline]
    fn deref(&self) -> &Self::Target {
        &self.index
    }
}

impl TrustedIndex {
    /// Which machine signed this index, proved by its key rather than claimed by a label.
    #[inline]
    #[must_use]
    pub const fn attribution(&self) -> &Attribution {
        &self.attribution
    }

    /// The `roster_seq` that authorized it — what the caller ratchets the durable roster floor to once the index has actually been used.
    #[inline]
    #[must_use]
    pub const fn roster_seq(&self) -> u64 {
        self.roster.seq()
    }

    /// The generation this index declares inside its own signed bytes.
    #[inline]
    #[must_use]
    pub fn authorizing_seq(&self) -> u64 {
        self.index.roster_seq.unwrap_or(0)
    }

    /// Verify a `pkg-<program>-<build>.toml`'s raw bytes under the roster generation that authorized this index.
    pub fn verify_pkg(&self, raw: Vec<u8>, sig: &[u8]) -> Result<VerifiedBytes, Reject> {
        self.roster.authorize_bytes(raw, sig).map(|(v, _)| v)
    }
}

/// Genuinely signed [`VerifiedBytes`] for other modules' tests.
#[cfg(test)]
pub(crate) mod testkit {
    use super::{Anchor, TrustedRoster, VerifiedBytes, admit_roster};
    use aterm_update_core::roster::{Machine, Roster};
    use ring::signature::{Ed25519KeyPair, KeyPair};

    /// Obviously synthetic, and shared with `sig`'s own tests so one fixture describes the whole crate's notion of "the owner".
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

    /// The index `raw`, signed by the test machine and authorized under the test roster re-issued at generation `seq`.
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

    /// `raw`, machine-signed and taken through the real authorization path.
    pub(crate) fn machine_signed(raw: Vec<u8>) -> VerifiedBytes {
        let sig = sign(&MACHINE_SEED, &raw);
        trusted_roster()
            .authorize_bytes(raw, &sig)
            .expect("the roster's own machine signed these bytes")
            .0
    }
}

/// The durable anti-rollback floor over `index.toml`, and the roster generation that set it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BuildFloor {
    /// The highest `index_build` durably recorded (`<prefix>/floor`).
    pub index_build: u64,
    /// The roster generation the index above was authorized by (`<prefix>/floor.gen`).
    pub roster_seq: u64,
}

impl BuildFloor {
    /// A first-contact floor: nothing recorded, belonging to no generation.
    #[inline]
    #[must_use]
    pub const fn none() -> Self {
        Self {
            index_build: 0,
            roster_seq: 0,
        }
    }

    /// Whether an index at `index_build`, authorized by roster generation `roster_seq`, clears this floor.
    #[inline]
    #[must_use]
    pub const fn admits(&self, roster_seq: u64, index_build: u64) -> bool {
        if roster_seq > self.roster_seq {
            return true;
        }
        index_build >= self.index_build
    }
}

/// Freshness gate: an index is fresh iff `now_unix < valid_until_unix`.
#[inline]
pub const fn check_freshness(now_unix: i64, valid_until_unix: i64) -> Result<(), Reject> {
    if now_unix >= valid_until_unix {
        Err(Reject::Stale)
    } else {
        Ok(())
    }
}

/// A durable, anti-rollback high-water mark over the index's monotonic `index_build`.
pub struct Floor {
    path: PathBuf,
}

impl Floor {
    /// A floor backed by `path`.
    #[inline]
    #[must_use]
    pub const fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// The recorded high-water floor (`0` if none or unreadable), for a caller that filters on it before advancing it with [`Self::check_and_record`].
    #[inline]
    #[must_use]
    pub fn current(&self) -> u64 {
        self.read_floor()
    }

    /// Reject any `index_build` below the recorded floor ([`Reject::Rollback`]).
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

    /// [`Self::check_and_record`] with the persist failure returned instead of printed, so tests can observe durability apart from the decision.
    pub(crate) fn check_and_record_classified(
        &self,
        index_build: u64,
    ) -> (Result<(), Reject>, Option<String>) {
        let _lock = self.acquire_file_lock();
        let (recorded, corrupt) = self.read_floor_reported();
        #[cfg(test)]
        tests::floor_window(&self.path);
        if index_build < recorded {
            return (Err(Reject::Rollback), None);
        }
        if !corrupt && index_build <= recorded && self.is_published() {
            return (Ok(()), None);
        }
        let persist_failure = self
            .write(index_build.max(recorded))
            .err()
            .map(|e| e.to_string());
        (Ok(()), persist_failure)
    }

    /// Write `value` unconditionally, including downward: the one non-monotonic operation.
    pub fn rebase(&self, value: u64) {
        let _lock = self.acquire_file_lock();
        #[cfg(test)]
        tests::floor_window(&self.path);
        if let Err(error) = self.write(value) {
            eprintln!(
                "atpkg: could not re-base the floor at {} to {value}: {error}",
                self.path.display()
            );
        }
    }

    /// The advisory lock over [`Self::check_and_record`]'s critical section: `LOCK_EX` on a
    /// sibling `<floor>.lock` (created `0600`), released on drop.
    fn acquire_file_lock(&self) -> Option<FileLock> {
        let name = self
            .path
            .file_name()
            .and_then(std::ffi::OsStr::to_str)
            .unwrap_or("floor");
        let lockpath = self.path.with_file_name(format!("{name}.lock"));
        FileLock::acquire(&lockpath).ok()
    }

    /// The recorded floor, or `0` if the file is missing or unparseable (fail-open only for first contact).
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
    /// (`0644` — see [`Self::write`]).
    fn is_published(&self) -> bool {
        match std::fs::symlink_metadata(&self.path) {
            Ok(meta) => {
                !crate::platform::HAS_POSIX_MODES
                    || crate::platform::permission_mode(&meta) & 0o777 == 0o644
            }
            Err(_) => false,
        }
    }

    /// `(recorded floor, corrupt)`.
    pub(crate) fn read_floor_classified(&self) -> (u64, bool) {
        match crate::metadata_io::read_bounded_regular_utf8(&self.path, MAX_FLOOR_BYTES) {
            Ok(text) => match text.trim().parse::<u64>() {
                Ok(value) => (value, false),
                Err(_) => (0, true),
            },
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
        if let Err(error) = self.stage_and_publish(&tmp, value) {
            let _ = std::fs::remove_file(&tmp);
            return Err(error);
        }
        crate::store::sync_dir(parent);
        Ok(())
    }

    /// Stage `value` into `tmp` (`0600`, durably synced), publish it `0644`, and `rename` it
    /// over the floor.
    fn stage_and_publish(&self, tmp: &Path, value: u64) -> io::Result<()> {
        use std::io::Write as _;

        {
            let mut f = crate::platform::open_create_write(tmp, 0o600)?;
            f.write_all(value.to_string().as_bytes())?;
            crate::platform::sync_durable(&f)?;
        }
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

    /// An unarmed anchor authorizes nothing, not even a roster genuinely signed by a real
    /// paper master, and refuses before any crypto.
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
        assert!(admit_roster(&armed(0), bytes, &sig, NOW).is_ok());
    }

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
        assert!(
            roster
                .authorize_index(raw, &sign(&M3_SEED, &index_body("m3", 3, 41)))
                .is_ok()
        );
    }

    /// A revoked machine is refused with its signature never checked: it leaves the candidate
    /// set in `live()`, so the verdict is `Verify`, not `Revoked`.
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
        assert_eq!(
            revoking
                .authorize_bytes(PKG.to_vec(), &sign(&M11_SEED, PKG))
                .err(),
            Some(Reject::Verify)
        );

        let mut same_but_live = roster();
        same_but_live.roster_seq = 4;
        let permissive = admit(&anchor, &same_but_live).unwrap();
        assert!(permissive.authorize_index(raw, &sig).is_ok());

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
        assert!(admit_roster(&armed(0), bytes, &sig, NOW).is_ok());
    }

    /// A rolled-back roster is refused once a newer generation is durably seen, though its
    /// master signature still verifies.
    #[test]
    fn a_rolled_back_roster_is_refused_by_the_durable_floor() {
        let old = roster();
        assert_eq!(
            admit(&armed(4), &old).err(),
            Some(Reject::Rollback),
            "a client that has seen seq 4 must never accept seq 3 again"
        );
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

        let raw = index_body("m3", 3, 41);
        assert_eq!(
            roster
                .authorize_index(raw, &sign(&M11_SEED, &index_body("m3", 3, 41)))
                .err(),
            Some(Reject::NotAuthorized),
            "a genuine signature must not be wearable under another machine's name"
        );

        let mut s = String::from("schema = 2\nindex_build = 41\n");
        s.push_str("valid_until = \"2027-02-01T00:00:00Z\"\n[programs.ay]\nrepo = \"ay\"\n");
        let bare = s.into_bytes();
        assert_eq!(
            roster
                .authorize_index(bare.clone(), &sign(&M3_SEED, &bare))
                .err(),
            Some(Reject::Unattributed)
        );

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
        assert_eq!(floor.check_and_record(41), Ok(()));
        assert_eq!(floor.check_and_record(40), Err(Reject::Rollback));
        assert_eq!(floor.check_and_record(41), Ok(()));
        assert_eq!(floor.check_and_record(50), Ok(()));
        assert_eq!(floor.check_and_record(45), Err(Reject::Rollback));
        assert_eq!(
            Floor::new(path.clone()).check_and_record(49),
            Err(Reject::Rollback)
        );
        #[cfg(unix)]
        {
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o644, "floor file must be readable (0644)");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// What a test runs inside the floor's locked window: between the read and the
    /// compare → write of an advance, or before a re-base's write.
    type FloorAct = Box<dyn FnOnce(&Path)>;

    thread_local! {
        static FLOOR_WINDOW: std::cell::RefCell<Option<FloorAct>> =
            const { std::cell::RefCell::new(None) };
    }

    /// The floor's locked window: run the act this thread armed, if any.
    pub(super) fn floor_window(path: &Path) {
        if let Some(act) = FLOOR_WINDOW.with(|window| window.borrow_mut().take()) {
            act(path);
        }
    }

    /// THE RATCHET IS READ AND ADVANCED UNDER ITS LOCK (CWE-362): inside the read →
    /// compare → write window, a second opener of `<floor>.lock` cannot take it.
    #[cfg(unix)]
    #[test]
    fn the_floor_is_read_and_advanced_under_its_lock() {
        let dir = private_tmp_dir("floor-lock");
        let floor = Floor::new(dir.join("index_build.floor"));
        let advance = || assert_eq!(floor.check_and_record(7), Ok(()));
        let rebase = || floor.rebase(3);
        for (what, touch) in [
            ("an advance", &advance as &dyn Fn()),
            ("a re-base", &rebase),
        ] {
            let lock = dir.join("index_build.floor.lock");
            let held = std::rc::Rc::new(std::cell::Cell::new(None));
            let probe = std::rc::Rc::clone(&held);
            FLOOR_WINDOW.with(|window| {
                *window.borrow_mut() = Some(Box::new(move |_: &Path| {
                    let file = std::fs::File::open(&lock).unwrap();
                    #[expect(
                        clippy::disallowed_methods,
                        reason = "a probe: released below on the path that takes it"
                    )]
                    let took = file.try_lock();
                    probe.set(Some(matches!(took, Err(std::fs::TryLockError::WouldBlock))));
                    if took.is_ok() {
                        let _ = file.unlock();
                    }
                }));
            });
            touch();
            assert_eq!(
                held.get(),
                Some(true),
                "{what} ran its window with the lock held"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A ratchet touch that advances nothing does not rewrite the file (on Apple targets a
    /// rewrite costs two full drive flushes).
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

        let recording = aterm_durable::seam::Recording::start();
        assert_eq!(floor.check_and_record(4), Ok(()));
        assert_eq!(recording.take(), [], "nothing to persist, nothing flushed");
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
        assert_eq!(floor.check_and_record_classified(4), (Ok(()), None));
        assert_eq!(ino(&path), ino(&witness));

        assert_eq!(floor.check_and_record(5), Ok(()));
        assert_eq!(recording.take(), [aterm_durable::Flush::Durable; 2]);
        assert_ne!(ino(&path), ino(&witness), "an advance must reach the disk");
        assert_eq!(Floor::new(path.clone()).current(), 5);

        for legacy in [0o600, 0o666] {
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(legacy)).unwrap();
            assert_eq!(floor.check_and_record(5), Ok(()));
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o644,
                "a {legacy:o} floor is re-published 0644 even at an unchanged value"
            );
        }
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
        let published = floor.is_published();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(!published, "a mode-000 floor is not published");

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
    /// that revokes it.
    #[test]
    fn the_build_floor_binds_within_its_generation_and_is_rebased_by_a_newer_one() {
        let floor = BuildFloor {
            index_build: 50,
            roster_seq: 7,
        };
        assert!(floor.admits(7, 50), "equal to the floor passes");
        assert!(floor.admits(7, 51));
        assert!(
            !floor.admits(7, 49),
            "below the floor, same generation ⇒ refused"
        );
        assert!(
            floor.admits(8, 1),
            "a newer master-signed generation is not bound by a floor an older one set"
        );
        assert!(
            !floor.admits(6, 49),
            "an OLDER generation must not waive the floor"
        );
        assert!(!floor.admits(0, 49), "nor an absent/unknown one");
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
        assert_eq!(floor.check_and_record(101), Err(Reject::Rollback));
        assert_eq!(floor.current(), u64::MAX, "and the refusal changed nothing");
        floor.rebase(101);
        assert_eq!(Floor::new(path).current(), 101);
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
        assert_eq!(floor.read_floor_classified(), (0, false));
        assert_eq!(floor.check_and_record(7), Ok(()));
        assert_eq!(floor.read_floor_classified(), (7, false));
        std::fs::write(&path, b" 7\n").unwrap();
        assert_eq!(floor.read_floor_classified(), (7, false));
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
        assert_eq!(floor.check_and_record(1), Ok(()));
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

        assert_eq!(Floor::new(path.clone()).current(), 7);
        assert_eq!(floor.check_and_record(7), Ok(()));

        let (decision, persist_failure) = floor.check_and_record_classified(8);
        assert_eq!(decision, Ok(()));
        assert_eq!(persist_failure, None);
        assert_eq!(Floor::new(path).current(), 8);

        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o777)).unwrap();
        let (decision, persist_failure) = floor.check_and_record_classified(9);
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(decision, Ok(()));
        assert!(
            persist_failure.is_some(),
            "a shared parent refuses the write"
        );
        assert_eq!(floor.current(), 8);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A persist that fails after the temp exists removes the temp: nothing else sweeps
    /// `<floor>.<pid>.tmp`, so a store that keeps refusing writes would collect one per pid.
    #[test]
    fn a_failed_floor_persist_removes_its_temp_file() {
        let dir = private_tmp_dir("floor-temp-litter");
        let path = dir.join("index_build.floor");
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
}
