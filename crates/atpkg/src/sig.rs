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
#[path = "sig_testkit_tests.rs"]
pub(crate) mod testkit;

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
            Ok(text) => {
                let s = text.trim();
                match (!s.is_empty()
                    && (s == "0" || !s.starts_with('0'))
                    && s.bytes().all(|b| b.is_ascii_digit()))
                .then(|| s.parse::<u64>().ok())
                .flatten()
                {
                    Some(value) => (value, false),
                    None => (0, true),
                }
            }
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

        static FLOOR_TMP_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let seq = FLOOR_TMP_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let tmp = self.path.with_file_name(format!(
            "{}.{}-{seq}.tmp",
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
        let _flushed_or_unavailable = aterm_durable::Flush::Durable.dir_best_effort(parent)?;
        Ok(())
    }

    /// Stage `value` into `tmp` (`0600`, durably synced), publish it `0644`, and `rename` it
    /// over the floor.
    fn stage_and_publish(&self, tmp: &Path, value: u64) -> io::Result<()> {
        use std::io::Write as _;

        {
            let mut f = crate::platform::open_create_new_write(tmp, 0o600)?;
            f.write_all(value.to_string().as_bytes())?;
            crate::platform::sync_durable(&f)?;
            crate::platform::set_mode_on(&f, 0o644)?;
        }
        std::fs::rename(tmp, &self.path)
    }
}

#[cfg(test)]
#[path = "sig_tests.rs"]
mod tests;
