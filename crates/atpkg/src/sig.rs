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
//! constructor: the only way to mint one is through [`verify_index`] /
//! [`verify_index_with`] / [`verify_pkg`]. The post-verify parse entry points
//! ([`crate::manifest::parse_index`] / [`crate::manifest::parse_pkg`]) consume
//! `&VerifiedBytes`, so calling them on unverified bytes does not even type-check. A
//! runtime test in `crate::manifest` additionally proves the parser never runs after a
//! failed verification.
//!
//! # Two-tier delegation
//!
//! The [`crate::PINNED_PKG_ROOTKEY`] (offline root) verifies `index.toml`; the index
//! names a rotatable [`Delegation`] (release key) that verifies each `pkg-*.toml`. A
//! [`Delegation::revoked_release_keys`] deny-list refuses a named-but-revoked key
//! before any crypto runs.
//!
//! # Cheapest-first reject ordering
//!
//! Every gate fails CLOSED; any error is a [`Reject`]. The signature primitive checks,
//! in order: empty-pin (free) → base64-decode + 32-byte key length (cheap, local) →
//! 64-byte signature length (cheap, local) → the Ed25519 verification (the expensive
//! crypto, last). [`check_freshness`] (gate 2, §8) and the [`Floor`] high-water mark
//! (gate 3, §8) are pure/durable functions the caller sequences after the signature.

use std::io;
use std::path::{Path, PathBuf};

use crate::platform::FileLock;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use ring::signature::{ED25519, UnparsedPublicKey};

const MAX_FLOOR_BYTES: usize = 128;

/// A blob of bytes that has **passed signature verification**. The inner `Vec<u8>` is
/// private and there is no public constructor, so the only way to obtain a
/// `VerifiedBytes` is via [`verify_index`], [`verify_index_with`], or [`verify_pkg`].
/// Parsers take `&VerifiedBytes`, which makes "parse only after verify" a *compile-time*
/// guarantee rather than a convention.
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reject {
    /// No root/release key was supplied (e.g. an unpinned build). Fail-closed default,
    /// returned *before* any crypto.
    Disabled,
    /// The public key did not base64-decode, or was not exactly 32 bytes.
    BadKey,
    /// The detached signature was not exactly 64 bytes.
    BadSig,
    /// The Ed25519 signature did not verify against the key over these bytes.
    Verify,
    /// The named release key is on the index's `revoked_release_keys` deny-list.
    Revoked,
    /// A signature verified under a key the current index does not delegate. (Reserved
    /// for the discovery layer, which knows the full delegated key set; the primitive
    /// surfaces a non-delegated key as [`Reject::Verify`].)
    NotDelegated,
    /// The index's freshness window has lapsed (`now >= valid_until`).
    Stale,
    /// An index whose `index_build` is below the durable high-water floor — an attempted
    /// rollback to an older, once-valid signed index.
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
}

/// The cheapest-first detached-signature primitive: verify `sig` is a valid Ed25519
/// signature by `pubkey_b64` over `msg`. Ordering matters — each step is a fail-closed
/// gate and the expensive crypto runs last (mirrors `aterm-update`'s `verify.rs`
/// cheapest/most-local-first ordering):
///
/// 1. empty key ⇒ [`Reject::Disabled`] (free; fail-closed on an unpinned anchor);
/// 2. base64-decode the key and require exactly 32 bytes ⇒ [`Reject::BadKey`] (cheap, local);
/// 3. require a 64-byte signature ⇒ [`Reject::BadSig`] (cheap, local);
/// 4. the Ed25519 verification itself ⇒ [`Reject::Verify`] on any failure (the crypto, last).
///
/// `ring`'s verify error is opaque (`ring::error::Unspecified`), so every crypto
/// failure collapses to one [`Reject::Verify`] — no per-reason oracle.
fn verify_detached(pubkey_b64: &str, msg: &[u8], sig: &[u8]) -> Result<(), Reject> {
    // STEP 1 — free: an empty pin means there is no anchor; refuse before doing work.
    if pubkey_b64.is_empty() {
        return Err(Reject::Disabled);
    }
    // STEP 2 — cheap, local: decode + length-check the public key.
    let pk = STANDARD.decode(pubkey_b64).map_err(|_| Reject::BadKey)?;
    if pk.len() != 32 {
        return Err(Reject::BadKey);
    }
    // STEP 3 — cheap, local: an Ed25519 detached signature is exactly 64 bytes.
    if sig.len() != 64 {
        return Err(Reject::BadSig);
    }
    // STEP 4 — the actual crypto, last and only if everything above held.
    UnparsedPublicKey::new(&ED25519, &pk)
        .verify(msg, sig)
        .map_err(|_| Reject::Verify)
}

/// Verify `index.toml`'s raw bytes against the compile-time-pinned root key
/// ([`crate::PINNED_PKG_ROOTKEY`]). On success the *same* bytes become
/// [`VerifiedBytes`], ready for the post-verify parser. With no pin, returns
/// [`Reject::Disabled`] (the manager is inert).
pub fn verify_index(raw: Vec<u8>, sig: &[u8]) -> Result<VerifiedBytes, Reject> {
    verify_index_with(crate::PINNED_PKG_ROOTKEY, raw, sig)
}

/// Like [`verify_index`] but with an explicit root public key. This is the seam the
/// account-bound, out-of-band root override plugs into (`[packages].root_pubkey`, §8
/// — a *different* owner ships their own root key via config), and it is what tests
/// drive so they need neither the compile-time pin nor any env.
pub fn verify_index_with(
    root_pubkey_b64: &str,
    raw: Vec<u8>,
    sig: &[u8],
) -> Result<VerifiedBytes, Reject> {
    verify_detached(root_pubkey_b64, &raw, sig)?;
    Ok(VerifiedBytes(raw))
}

/// The release-key delegation extracted from a (root-verified) index: the named,
/// rotatable key that signs each `pkg-*.toml`, plus the revocation deny-list. Produced
/// by [`crate::manifest::Index::delegation`] (the real `toml` parser, run only on
/// [`VerifiedBytes`]); consumed by [`verify_pkg`].
pub struct Delegation {
    /// The index's `[keys].release_key_id` — the rotatable key's identifier.
    pub release_key_id: String,
    /// The index's `[keys].release_key_pubkey` (base64 Ed25519, 32 raw bytes).
    pub release_key_pubkey_b64: String,
    /// The index's `[keys].revoked_release_keys` deny-list (belt-and-suspenders over
    /// rotation): a key whose id appears here is refused before any crypto.
    pub revoked_release_keys: Vec<String>,
}

/// Verify a `pkg-*.toml`'s raw bytes under the release key the (already root-verified)
/// index delegates. Revocation is checked **first** — a cheap string compare before any
/// crypto: a release key id on the deny-list yields [`Reject::Revoked`] even if the
/// signature would otherwise verify. A `pkg.toml` signed by any key other than the
/// delegated one fails the Ed25519 check ([`Reject::Verify`]).
pub fn verify_pkg(raw: Vec<u8>, sig: &[u8], d: &Delegation) -> Result<VerifiedBytes, Reject> {
    // Cheapest gate first: a revoked release key is refused before signature math.
    if d.revoked_release_keys
        .iter()
        .any(|k| k == &d.release_key_id)
    {
        return Err(Reject::Revoked);
    }
    verify_detached(&d.release_key_pubkey_b64, &raw, sig)?;
    Ok(VerifiedBytes(raw))
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
    /// decision was made against the value read *before* the write).
    pub fn check_and_record(&self, index_build: u64) -> Result<(), Reject> {
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
            return Err(Reject::Rollback);
        }
        // Durable advance under the lock; never downgrades the recorded value.
        let _ = self.write(index_build.max(recorded));
        Ok(())
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
    fn read_floor(&self) -> u64 {
        crate::metadata_io::read_bounded_regular_utf8(&self.path, MAX_FLOOR_BYTES)
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
            .unwrap_or(0)
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
        }
        // Force 0600 even if the temp pre-existed with looser bits, then publish.
        crate::platform::harden_file(&tmp)?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::signature::{Ed25519KeyPair, KeyPair};
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    // A fixed 32-byte seed → a deterministic root keypair. The whole matrix is
    // reproducible and needs no RNG, no env, and not the compile-time const.
    const ROOT_SEED: [u8; 32] = [7u8; 32];
    const RELEASE_SEED: [u8; 32] = [1u8; 32];
    const ATTACKER_SEED: [u8; 32] = [2u8; 32];

    const MANIFEST: &[u8] = b"schema = 1\nindex_build = 41\n";

    fn keypair(seed: &[u8; 32]) -> Ed25519KeyPair {
        Ed25519KeyPair::from_seed_unchecked(seed).expect("valid 32-byte seed")
    }

    fn pubkey_b64(kp: &Ed25519KeyPair) -> String {
        STANDARD.encode(kp.public_key().as_ref())
    }

    fn sign(kp: &Ed25519KeyPair, msg: &[u8]) -> Vec<u8> {
        kp.sign(msg).as_ref().to_vec()
    }

    fn private_tmp_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("atpkg-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        #[cfg(unix)]
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        dir
    }

    // (1) A valid detached signature over the exact bytes ACCEPTS.
    #[test]
    fn valid_detached_sig_accepts() {
        let kp = keypair(&ROOT_SEED);
        let pk = pubkey_b64(&kp);
        let sig = sign(&kp, MANIFEST);
        assert_eq!(verify_detached(&pk, MANIFEST, &sig), Ok(()));
    }

    // (2) A single-byte flip ANYWHERE in the message REJECTS.
    #[test]
    fn single_byte_flip_rejects() {
        let kp = keypair(&ROOT_SEED);
        let pk = pubkey_b64(&kp);
        let sig = sign(&kp, MANIFEST);
        for i in 0..MANIFEST.len() {
            let mut tampered = MANIFEST.to_vec();
            tampered[i] ^= 0x01;
            assert_eq!(
                verify_detached(&pk, &tampered, &sig),
                Err(Reject::Verify),
                "flip at byte {i} must reject"
            );
        }
    }

    // (3) An appended trailing newline/whitespace REJECTS (no normalization).
    #[test]
    fn appended_trailing_whitespace_rejects() {
        let kp = keypair(&ROOT_SEED);
        let pk = pubkey_b64(&kp);
        let sig = sign(&kp, MANIFEST);
        let mut tampered = MANIFEST.to_vec();
        tampered.push(b'\n');
        assert_eq!(verify_detached(&pk, &tampered, &sig), Err(Reject::Verify));
        let mut spaced = MANIFEST.to_vec();
        spaced.push(b' ');
        assert_eq!(verify_detached(&pk, &spaced, &sig), Err(Reject::Verify));
    }

    // (4) A truncated message REJECTS.
    #[test]
    fn truncated_message_rejects() {
        let kp = keypair(&ROOT_SEED);
        let pk = pubkey_b64(&kp);
        let sig = sign(&kp, MANIFEST);
        let truncated = &MANIFEST[..MANIFEST.len() - 1];
        assert_eq!(verify_detached(&pk, truncated, &sig), Err(Reject::Verify));
    }

    // (5) An invalid-UTF-8 byte on the SIGNED path is handled as raw bytes — a
    // genuine signature over bytes containing 0xFF ACCEPTS (a verifier that did
    // from_utf8_lossy first would substitute U+FFFD, change the bytes, and wrongly
    // reject), and flipping that same byte still REJECTS.
    #[test]
    fn raw_non_utf8_bytes_verify_without_lossy_conversion() {
        let kp = keypair(&ROOT_SEED);
        let pk = pubkey_b64(&kp);
        let mut msg = b"a = 1\n".to_vec();
        let bad_idx = msg.len();
        msg.push(0xFF);
        msg.extend_from_slice(b"\nb = 2\n");
        let sig = sign(&kp, &msg);
        assert_eq!(verify_detached(&pk, &msg, &sig), Ok(()));
        let mut tampered = msg.clone();
        tampered[bad_idx] ^= 0x01;
        assert_eq!(verify_detached(&pk, &tampered, &sig), Err(Reject::Verify));
    }

    // (6) Empty pin ⇒ enabled()==false AND verify_detached("",..)==Err(Disabled),
    // before any crypto — the fail-closed, inert default of an unsigned build.
    #[test]
    fn empty_pin_is_disabled_and_inert() {
        let kp = keypair(&ROOT_SEED);
        let sig = sign(&kp, MANIFEST);
        assert_eq!(
            verify_detached("", MANIFEST, &sig),
            Err(Reject::Disabled),
            "empty key short-circuits before crypto"
        );
        // A plain build leaves the pin empty ⇒ the manager is inert.
        assert!(crate::PINNED_PKG_ROOTKEY.is_empty());
        assert!(!crate::enabled());
        // verify_index (which uses the const pin) is therefore Disabled even for a
        // byte-perfect, correctly-signed index.
        assert_eq!(verify_index(MANIFEST.to_vec(), &sig), Err(Reject::Disabled));
    }

    // (7) A pkg signed by a key the index does NOT delegate REJECTS under verify_pkg,
    // while the genuinely delegated key verifies.
    #[test]
    fn pkg_signed_by_non_delegated_key_rejects() {
        let release = keypair(&RELEASE_SEED);
        let attacker = keypair(&ATTACKER_SEED);
        let d = Delegation {
            release_key_id: "rk-2026-06".into(),
            release_key_pubkey_b64: pubkey_b64(&release),
            revoked_release_keys: vec![],
        };
        let pkg = b"name = \"ay\"\nbuild = 18\n";
        let forged = sign(&attacker, pkg);
        assert_eq!(verify_pkg(pkg.to_vec(), &forged, &d), Err(Reject::Verify));
        let good = sign(&release, pkg);
        let vb = verify_pkg(pkg.to_vec(), &good, &d).expect("delegated key verifies");
        assert_eq!(vb.as_slice(), pkg);
    }

    // (8) A release key id on the revoked deny-list REJECTS (Revoked) BEFORE any
    // crypto — even a perfectly valid signature by that key is refused.
    #[test]
    fn revoked_release_key_rejected_before_crypto() {
        let release = keypair(&RELEASE_SEED);
        let d = Delegation {
            release_key_id: "rk-2026-05".into(),
            release_key_pubkey_b64: pubkey_b64(&release),
            revoked_release_keys: vec!["rk-2026-05".into()],
        };
        let pkg = b"name = \"ay\"\n";
        let valid = sign(&release, pkg);
        assert_eq!(verify_pkg(pkg.to_vec(), &valid, &d), Err(Reject::Revoked));
    }

    // (9) now >= valid_until ⇒ check_freshness Err(Stale); now < valid_until ⇒ Ok.
    #[test]
    fn freshness_gate_rejects_at_or_after_valid_until() {
        assert_eq!(check_freshness(100, 200), Ok(()));
        assert_eq!(check_freshness(199, 200), Ok(()));
        assert_eq!(check_freshness(200, 200), Err(Reject::Stale));
        assert_eq!(check_freshness(201, 200), Err(Reject::Stale));
    }

    // (10) index_build below the recorded Floor ⇒ Err(Rollback); a higher build
    // advances the floor durably (and the file is 0600).
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

    // The post-verify parsers live in `crate::manifest` now (the real `toml` parser
    // replacing the Phase-1 line-scanner); the "parser never runs on unverified bytes"
    // guarantee, the table-scoping/duplicate-key/multiline cases, and the delegation
    // extraction are tested there (`manifest::tests`) against genuine `VerifiedBytes`.

    // Extra: malformed keys/signatures map to the cheap, local rejects.
    #[test]
    fn malformed_key_and_sig_lengths_reject() {
        let kp = keypair(&ROOT_SEED);
        let pk = pubkey_b64(&kp);
        let sig = sign(&kp, MANIFEST);
        // Not base64.
        assert_eq!(
            verify_detached("!!!not-base64!!!", MANIFEST, &sig),
            Err(Reject::BadKey)
        );
        // Valid base64 but wrong length (16 bytes).
        let short_key = STANDARD.encode([0u8; 16]);
        assert_eq!(
            verify_detached(&short_key, MANIFEST, &sig),
            Err(Reject::BadKey)
        );
        // Right key, wrong signature length.
        assert_eq!(
            verify_detached(&pk, MANIFEST, &[0u8; 63]),
            Err(Reject::BadSig)
        );
    }
}
