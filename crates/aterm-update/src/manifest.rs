// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The two TOML records the updater reads/writes: the release-side **manifest**
//! (`aterm-appcast.toml`, an attached release asset, emitted by the ship tool —
//! `cargo ship cut`, crate aterm-release) and the local **ready marker**
//! (`ready.toml`, written last when staging completes — its presence is the sole
//! "ready" signal).

use std::path::Path;

use serde::{Deserialize, Serialize};

/// The highest manifest `schema` version this build understands. A manifest
/// declaring a higher schema is from a newer format we can't safely interpret, so
/// we reject it (the client stays on its current build) rather than misread it.
pub const SUPPORTED_SCHEMA: u32 = 1;

/// The release manifest attached to a GitHub Release as `aterm-appcast.toml`.
/// Field set is kept in lockstep with the ship tool's emitter (aterm-release
/// `manifest_out.rs`, which serializes the shared `aterm-update-core` type).
#[derive(Debug, Clone, Deserialize)]
pub struct Manifest {
    /// Manifest format version (the ship tool emits `schema = 1`). Absent ⇒ 0.
    #[serde(default)]
    pub schema: u32,
    /// Human semver, e.g. `"0.2.0"`.
    pub version: String,
    /// Monotonic build number, unique per published build: claimed from the
    /// append-only `RELEASES.ledger` as `max(last+1, unix-epoch-now)` at cut time
    /// (== the bundle's sealed `CFBundleVersion`). The downgrade gate.
    pub build_number: u64,
    /// Full git commit hash of the source the release was built from.
    /// Every ship-tool manifest carries it; clients used to drop it at parse.
    /// Binds the build number to an exact source commit, so a staged (and later
    /// running) build is checkable against the repo. Absent ⇒ None (a hand-written
    /// or pre-field manifest).
    #[serde(default)]
    pub commit: Option<String>,
    /// SHA-256 (lowercase hex) of the DMG asset.
    pub sha256: String,
    /// The DMG asset's file name within the release, e.g. `"aterm-0.2.0.dmg"`.
    pub dmg: String,
    /// Optional operator **apply floor**: clients refuse to stage/apply ANY build
    /// whose `build_number` is below this, even a genuine signed one. Lets the owner
    /// retire a bad-but-genuine release after the fact — a "yank" a silent updater can
    /// honor without a signed channel. Ratcheted monotonically client-side (see
    /// [`Floor`]); the release cutter carries a raised channel floor into every
    /// successor manifest. Absent ⇒ 0 (no floor). (F5)
    #[serde(default)]
    pub min_build: Option<u64>,
    /// Optional human-readable "what changed" notes (the hand-written CHANGELOG.md
    /// section for this release, verbatim — the same text as the GitHub release
    /// notes). Surfaced by the in-app updater's status query + the "Check for
    /// Updates" menu so the user sees what a staged update brings. Absent ⇒ None.
    #[serde(default)]
    pub changelog: Option<String>,
}

impl Manifest {
    /// Parse a manifest from TOML text, rejecting a schema this build is too old
    /// to understand.
    pub fn parse(text: &str) -> Result<Self, String> {
        let m: Manifest = toml::from_str(text).map_err(|e| format!("parse manifest: {e}"))?;
        if m.schema > SUPPORTED_SCHEMA {
            return Err(format!(
                "manifest schema {} is newer than supported ({SUPPORTED_SCHEMA}); upgrade aterm",
                m.schema
            ));
        }
        if let Some(min_build) = m.min_build
            && min_build > m.build_number
        {
            return Err(format!(
                "manifest min_build {min_build} exceeds its build_number {}; refusing an \
                 impossible update floor",
                m.build_number
            ));
        }
        Ok(m)
    }
}

/// The local staging marker. Written atomically (temp + rename) AFTER the staged
/// bundle is fully materialized and verified, so a reader never sees a
/// half-staged update.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ready {
    /// Build number of the staged bundle (must exceed the running build to apply).
    pub build_number: u64,
    /// Human semver of the staged bundle (for log lines).
    pub version: String,
    /// Git commit of the staged bundle's source, copied from the manifest at stage
    /// time (same rationale as `changelog`: queryable via status without
    /// re-fetching). Absent ⇒ None (staged before this field existed, or the
    /// manifest omitted it).
    #[serde(default)]
    pub commit: Option<String>,
    /// SHA-256 of the DMG the staged bundle came from (stage-time integrity record).
    pub dmg_sha256: String,
    /// Team ID recorded at stage time (re-checked against the signature at apply).
    pub team_id: String,
    /// RFC3339 UTC time the bundle was staged.
    pub staged_at: String,
    /// "What changed" notes copied from the manifest at stage time, so the version +
    /// changelog is queryable (status / menu) without re-fetching. Absent ⇒ None.
    #[serde(default)]
    pub changelog: Option<String>,
}

impl Ready {
    /// Read + parse the marker, or `None` if absent/unparseable. (Non-UTF-8 IS an
    /// unparseable marker; `.ok()?` maps it to `None`.)
    pub fn read(path: &Path) -> Option<Self> {
        let text = crate::read_ledger_text(path)?;
        toml::from_str(&text).ok()
    }

    /// Serialize to TOML text.
    pub fn to_toml(&self) -> Result<String, String> {
        toml::to_string(self).map_err(|e| format!("serialize ready marker: {e}"))
    }

    /// Cheap structural identity gate shared by apply and status surfaces. A
    /// merely parseable marker is not enough to advertise an update: shipping
    /// markers carry a full 40-hex source commit and 64-hex artifact digest.
    pub(crate) fn has_canonical_identity(&self) -> bool {
        self.build_number != 0
            && self.commit.as_deref().is_some_and(|commit| {
                let commit = commit.trim();
                commit.len() == 40 && commit.bytes().all(|byte| byte.is_ascii_hexdigit())
            })
            && {
                let digest = self.dmg_sha256.trim();
                digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
            }
    }

    /// Whether this marker names a genuinely published local stage. A parseable
    /// `ready.toml` is only metadata: the bundle directory is the capability the
    /// checker, status UI, and apply path must all agree exists. Never follow a
    /// staged-app symlink when granting that capability.
    ///
    /// Require the release bundle's XML Info.plist and cheaply bind the marker
    /// to its build and source stamp too. Full signature/policy verification
    /// remains the apply-time authority; this local read is an early stale/corrupt
    /// rejection that does not spawn a helper process on status/check paths.
    pub(crate) fn is_publishable(&self, staging: &crate::paths::Staging) -> bool {
        if !self.has_canonical_identity()
            || !std::fs::symlink_metadata(&staging.staged_app)
                .is_ok_and(|metadata| metadata.is_dir() && !metadata.file_type().is_symlink())
        {
            return false;
        }
        cheap_staged_identity_matches(self, &staging.staged_app) == Some(true)
    }

    /// Read only a marker that passes [`Self::is_publishable`]. This is the one
    /// shared staged-ready read used by background checks and status surfaces.
    pub(crate) fn read_publishable(staging: &crate::paths::Staging) -> Option<Self> {
        Self::read(&staging.ready).filter(|ready| ready.is_publishable(staging))
    }
}

/// Bind the required XML Info.plist. `None` means the cheap binding is
/// unavailable; it therefore grants no authority. `Some(false)` means a
/// present plist is malformed, unsafe, or disagrees with the marker.
fn cheap_staged_identity_matches(ready: &Ready, app: &Path) -> Option<bool> {
    let plist = app.join("Contents/Info.plist");
    let metadata = match std::fs::symlink_metadata(&plist) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(_) => return Some(false),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Some(false);
    }
    let Some(text) = crate::read_ledger_text(&plist) else {
        return Some(false);
    };
    let Some(build) = xml_plist_string(&text, "<key>CFBundleVersion</key>")
        .and_then(|value| value.parse::<u64>().ok())
    else {
        return Some(false);
    };
    let Some(commit) = xml_plist_string(&text, "<key>ATermGitCommit</key>") else {
        return Some(false);
    };
    Some(
        build == ready.build_number
            && ready
                .commit
                .as_deref()
                .is_some_and(|expected| crate::commit_matches(expected, commit)),
    )
}

/// Extract the string immediately following one exact XML plist key. Release
/// bundles are emitted as XML; missing/binary/malformed plists fail closed and
/// force a fresh verified stage.
fn xml_plist_string<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    let after_key = text.find(key)? + key.len();
    // The value element must immediately follow the key (modulo whitespace).
    // Searching arbitrarily far forward would let a malformed/intervening key's
    // string be mis-bound to the release identity we are authenticating.
    let value_tail = text[after_key..].trim_start().strip_prefix("<string>")?;
    let string_end = value_tail.find("</string>")?;
    let value = value_tail[..string_end].trim();
    (!value.is_empty()).then_some(value)
}

/// Durable proof of the exact artifact installed by the most recent successful
/// self-update swap. This is distinct from the boot-health trial marker: health
/// confirmation may clear crash-loop state, but an overlapping old process still
/// needs `(build, commit, DMG digest)` evidence to classify the swap as installed.
///
/// The receipt is useful only together with the codesign-sealed build and commit
/// read from the canonical installed bundle. [`Self::matches_sealed`] performs that
/// bind; an unsigned or stale receipt alone never proves an install.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ReceiptCommitKind {
    /// Normal updater receipt: the full release-manifest commit is available.
    #[default]
    ModernFull,
    /// One-time v0.52 recovery: only the exact 12-hex codesign-sealed plist/
    /// compiled stamp survived after that updater deleted `ready.toml`.
    LegacySealedShort,
}

impl ReceiptCommitKind {
    fn is_modern(&self) -> bool {
        *self == Self::ModernFull
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct InstalledReceipt {
    pub(crate) build_number: u64,
    pub(crate) git_commit: String,
    pub(crate) dmg_sha256: String,
    /// Missing on every pre-migration receipt, which remains the strict modern
    /// full-commit format. The short form is explicit so a truncated modern
    /// receipt can never be reinterpreted as legacy authority.
    #[serde(default, skip_serializing_if = "ReceiptCommitKind::is_modern")]
    commit_kind: ReceiptCommitKind,
}

impl InstalledReceipt {
    fn canonical(
        build_number: u64,
        git_commit: &str,
        dmg_sha256: &str,
        commit_kind: ReceiptCommitKind,
    ) -> Option<Self> {
        let git_commit = git_commit.trim();
        let dmg_sha256 = dmg_sha256.trim();
        let commit_len_is_canonical = match commit_kind {
            ReceiptCommitKind::ModernFull => git_commit.len() == 40,
            ReceiptCommitKind::LegacySealedShort => git_commit.len() == 12,
        };
        (build_number != 0
            && commit_len_is_canonical
            && git_commit.bytes().all(|byte| byte.is_ascii_hexdigit())
            && dmg_sha256.len() == 64
            && dmg_sha256.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then(|| Self {
            build_number,
            git_commit: git_commit.to_ascii_lowercase(),
            dmg_sha256: dmg_sha256.to_ascii_lowercase(),
            commit_kind,
        })
    }

    /// Read a well-formed receipt. Missing, oversized, malformed, or non-canonical
    /// data fails closed as no proof.
    pub(crate) fn read(path: &Path) -> Option<Self> {
        let parsed: Self = toml::from_str(&crate::read_ledger_text(path)?).ok()?;
        Self::canonical(
            parsed.build_number,
            &parsed.git_commit,
            &parsed.dmg_sha256,
            parsed.commit_kind,
        )
    }

    /// Atomically replace the prior installed receipt after a successful swap.
    /// Failure is returned so the caller can keep the install transaction honest.
    pub(crate) fn record(
        path: &Path,
        build_number: u64,
        git_commit: &str,
        dmg_sha256: &str,
    ) -> Result<(), String> {
        Self::record_with_kind(
            path,
            build_number,
            git_commit,
            dmg_sha256,
            ReceiptCommitKind::ModernFull,
        )
    }

    /// Atomically record the one-time v0.52 recovery form. Callers must first
    /// prove the legacy-only disk shape; this constructor merely prevents that
    /// path from weakening the normal full-commit receipt parser.
    pub(crate) fn record_legacy_sealed_short(
        path: &Path,
        build_number: u64,
        sealed_git_commit: &str,
        dmg_sha256: &str,
    ) -> Result<(), String> {
        Self::record_with_kind(
            path,
            build_number,
            sealed_git_commit,
            dmg_sha256,
            ReceiptCommitKind::LegacySealedShort,
        )
    }

    fn record_with_kind(
        path: &Path,
        build_number: u64,
        git_commit: &str,
        dmg_sha256: &str,
        commit_kind: ReceiptCommitKind,
    ) -> Result<(), String> {
        let receipt = Self::canonical(build_number, git_commit, dmg_sha256, commit_kind)
            .ok_or_else(|| "installed receipt identity is malformed".to_string())?;
        let text = toml::to_string(&receipt)
            .map_err(|error| format!("serialize installed receipt: {error}"))?;
        let tmp = path.with_extension(format!("toml.{}.tmp", std::process::id()));
        if let Err(error) = std::fs::write(&tmp, text) {
            let _ = std::fs::remove_file(&tmp);
            return Err(format!("write installed receipt: {error}"));
        }
        std::fs::rename(&tmp, path).map_err(|error| {
            let _ = std::fs::remove_file(&tmp);
            format!("commit installed receipt: {error}")
        })
    }

    /// Rewrite a previously parsed receipt without changing its provenance
    /// format. Used when an exec-failure inverse swap restores OLD: a legacy OLD
    /// must regain its exact tagged short receipt, never be coerced through the
    /// modern full-commit constructor.
    pub(crate) fn record_preserving_kind(&self, path: &Path) -> Result<(), String> {
        Self::record_with_kind(
            path,
            self.build_number,
            &self.git_commit,
            &self.dmg_sha256,
            self.commit_kind,
        )
    }

    /// Bind this unsigned local receipt to the codesign-sealed identity of the
    /// bundle currently at the canonical install path.
    pub(crate) fn matches_sealed(&self, build_number: u64, git_commit: &str) -> bool {
        self.build_number == build_number
            && match self.commit_kind {
                ReceiptCommitKind::ModernFull => {
                    crate::commit_matches(&self.git_commit, git_commit)
                }
                ReceiptCommitKind::LegacySealedShort => {
                    self.git_commit.eq_ignore_ascii_case(git_commit.trim())
                }
            }
    }

    pub(crate) fn clear(path: &Path) {
        let _ = std::fs::remove_file(path);
    }
}

/// A persisted, **monotonic** recency floor kept under `Updates/floor.toml`. Both
/// fields only ever ratchet UP, so the file can never be used to force a *downgrade*;
/// its purpose is to block replay/rollback and honor an operator yank (F5/F6):
///
/// * `min_build` — the highest `min_build` any observed manifest has declared; the
///   client refuses to stage/apply below it (operator-driven yank of a genuine build).
/// * `high_water` — the highest build the client has ever *successfully staged*; it
///   refuses to stage a "latest available" that is below this (an attacker who
///   re-points the newest release at an older genuine build can't roll a client back).
///
/// NOTE: this is an *unsigned* floor, so it cannot protect a brand-new client that has
/// never seen a higher build (it has no high-water yet); fully closing that requires a
/// signed channel file the client pins (see `docs/RELEASING.md`). It DOES stop replay
/// against any client that has already advanced, and gives the operator a working yank.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Floor {
    /// Operator apply floor (max `min_build` ever seen). Absent ⇒ 0.
    #[serde(default)]
    pub min_build: u64,
    /// Highest build ever successfully staged by this client. Absent ⇒ 0.
    #[serde(default)]
    pub high_water: u64,
}

impl Floor {
    /// Read the floor, defaulting to all-zero (no floor) when absent or unparseable —
    /// a corrupt floor must not permanently wedge updates, and zero is the safe
    /// (permissive) default since both fields are lower bounds.
    pub fn read(path: &Path) -> Self {
        crate::read_ledger_text(path)
            .and_then(|t| toml::from_str(&t).ok())
            .unwrap_or_default()
    }

    /// Persist atomically (temp + rename), raising each field monotonically to at
    /// least the given observed values. The per-floor lock covers the complete
    /// read/max/write transaction, preventing two processes from overwriting each
    /// other's independent maxima. A no-op write is skipped. Best-effort — a
    /// failure to persist the floor never blocks an update decision.
    pub fn bump_and_write(path: &Path, seen_min_build: u64, staged_build: u64) {
        let Ok(_lock) = aterm_update_core::FileLock::acquire(&path.with_extension("toml.lock"))
        else {
            return;
        };
        let cur = Self::read(path);
        let next = Self {
            min_build: cur.min_build.max(seen_min_build),
            high_water: cur.high_water.max(staged_build),
        };
        if next.min_build == cur.min_build && next.high_water == cur.high_water {
            return;
        }
        let Ok(text) = toml::to_string(&next) else {
            return;
        };
        let tmp = path.with_extension(format!("toml.{}.tmp", std::process::id()));
        if std::fs::write(&tmp, text).is_ok() {
            let _ = std::fs::rename(&tmp, path);
        } else {
            let _ = std::fs::remove_file(&tmp);
        }
    }
}

/// A memo of the last candidate build that was downloaded but FAILED to stage
/// (verification / mount / extract). Persisted under `Updates/failed.toml` so the
/// periodic loop doesn't re-download the same (up to 512 MB) DMG every interval when
/// the newest release is a build we already proved unstageable (F17). Keyed by
/// `(build_number, sha256)` so a re-published fix under the same build is retried.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FailedMark {
    #[serde(default)]
    pub build_number: u64,
    #[serde(default)]
    pub sha256: String,
}

impl FailedMark {
    /// Read the memo, or `None` if absent/unparseable.
    pub fn read(path: &Path) -> Option<Self> {
        toml::from_str(&crate::read_ledger_text(path)?).ok()
    }

    /// Whether this memo matches the given candidate (same build AND same DMG hash).
    pub fn matches(&self, build_number: u64, sha256: &str) -> bool {
        self.build_number == build_number && self.sha256.eq_ignore_ascii_case(sha256)
    }

    /// Record `(build_number, sha256)` as the last failed candidate (best-effort).
    pub fn record(path: &Path, build_number: u64, sha256: &str) {
        let _ = Self::record_required(path, build_number, sha256);
    }

    /// Atomically record `(build_number, sha256)`, reporting persistence failure.
    /// Apply uses this stricter form before swap because crash-loop recovery must be
    /// able to poison the exact trial artifact deterministically.
    pub(crate) fn record_required(
        path: &Path,
        build_number: u64,
        sha256: &str,
    ) -> Result<(), String> {
        let m = FailedMark {
            build_number,
            sha256: sha256.to_ascii_lowercase(),
        };
        let text =
            toml::to_string(&m).map_err(|error| format!("serialize artifact marker: {error}"))?;
        let tmp = path.with_extension(format!("toml.{}.tmp", std::process::id()));
        if let Err(error) = std::fs::write(&tmp, text) {
            let _ = std::fs::remove_file(&tmp);
            return Err(format!("write artifact marker: {error}"));
        }
        std::fs::rename(&tmp, path).map_err(|error| {
            let _ = std::fs::remove_file(&tmp);
            format!("commit artifact marker: {error}")
        })
    }

    /// Clear the memo (called once a stage finally succeeds).
    pub fn clear(path: &Path) {
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installed_receipt_is_exact_and_requires_sealed_bundle_identity() {
        let root =
            std::env::temp_dir().join(format!("aterm-installed-receipt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("installed.toml");
        let commit = "0123456789abcdef0123456789abcdef01234567";
        let digest = "ab".repeat(32);

        InstalledReceipt::record(&path, 53, commit, &digest).unwrap();
        let receipt = InstalledReceipt::read(&path).expect("valid receipt");
        assert!(receipt.matches_sealed(53, commit));
        assert!(receipt.matches_sealed(53, "0123456789ab"));
        assert!(!receipt.matches_sealed(52, commit));
        assert!(!receipt.matches_sealed(53, "fedcba9876543210fedcba9876543210fedcba98"));

        for (bad_commit, bad_digest) in [
            ("0123456789ab", digest.as_str()),
            (commit, "ab"),
            (
                "fedcba9876543210fedcba9876543210fedcba98-dirty",
                digest.as_str(),
            ),
        ] {
            assert!(InstalledReceipt::record(&path, 54, bad_commit, bad_digest).is_err());
            assert_eq!(
                InstalledReceipt::read(&path),
                Some(receipt.clone()),
                "a rejected replacement must preserve the prior valid receipt"
            );
        }

        InstalledReceipt::clear(&path);
        assert!(InstalledReceipt::read(&path).is_none());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn legacy_receipt_is_explicit_exact_twelve_hex_and_never_prefix_authority() {
        let root = std::env::temp_dir().join(format!(
            "aterm-legacy-installed-receipt-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("installed.toml");
        // Exact ATermGitCommit from the deployed v0.53 plist staged by v0.52.
        let sealed = "c16c6fd7955b";
        let digest = "cdfc51afab0d5212206e169047df2a23b97b396ecb22f38edaab7eb1e2c93e95";

        assert!(InstalledReceipt::record(&path, 1_784_433_247, sealed, digest).is_err());
        InstalledReceipt::record_legacy_sealed_short(&path, 1_784_433_247, sealed, digest).unwrap();
        let receipt = InstalledReceipt::read(&path).expect("tagged legacy receipt");
        assert!(receipt.matches_sealed(1_784_433_247, sealed));
        assert!(receipt.matches_sealed(1_784_433_247, "C16C6FD7955B"));
        assert!(
            !receipt.matches_sealed(1_784_433_247, "c16c6fd7955bf565fcdc6e700548b987acce31b9"),
            "the short sealed form is exact, never prefix-based"
        );
        let restored_path = root.join("restored-after-inverse-swap.toml");
        receipt.record_preserving_kind(&restored_path).unwrap();
        assert_eq!(
            InstalledReceipt::read(&restored_path),
            Some(receipt.clone()),
            "inverse-swap restoration preserves the explicit legacy format"
        );
        for malformed in [
            "c16c6fd7955",
            "c16c6fd7955bf",
            "c16c6fd7955g",
            "c16c6fd7955b-dirty",
        ] {
            assert!(
                InstalledReceipt::record_legacy_sealed_short(
                    &root.join(format!("{malformed}.toml")),
                    1_784_433_247,
                    malformed,
                    digest,
                )
                .is_err(),
                "legacy provenance must be exactly 12 clean hex: {malformed}"
            );
        }

        // An untagged/truncated modern wire is still invalid; only the dedicated
        // constructor can mint the legacy representation.
        std::fs::write(
            root.join("truncated-modern.toml"),
            format!(
                "build_number = 1784433247\ngit_commit = \"{sealed}\"\ndmg_sha256 = \"{digest}\"\n"
            ),
        )
        .unwrap();
        assert!(InstalledReceipt::read(&root.join("truncated-modern.toml")).is_none());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn concurrent_floor_writers_cannot_lose_independent_maxima() {
        let root =
            std::env::temp_dir().join(format!("aterm-floor-transaction-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("floor.toml");
        let start = std::sync::Arc::new(std::sync::Barrier::new(3));

        let writer = |seen_min_build, staged_build| {
            let path = path.clone();
            let start = std::sync::Arc::clone(&start);
            std::thread::spawn(move || {
                start.wait();
                Floor::bump_and_write(&path, seen_min_build, staged_build);
            })
        };
        let min_writer = writer(1_000, 1);
        let high_writer = writer(1, 2_000);
        start.wait();
        min_writer.join().unwrap();
        high_writer.join().unwrap();

        assert_eq!(
            Floor::read(&path),
            Floor {
                min_build: 1_000,
                high_water: 2_000,
            },
            "read/max/write is one locked transaction; neither coordinate may regress"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn parses_a_manifest() {
        let m = Manifest::parse(
            r#"
            schema = 1
            version = "0.2.0"
            build_number = 1234
            sha256 = "abc123"
            dmg = "aterm-0.2.0.dmg"
            team_id = "TEAMID1234"
            commit = "deadbeef"
        "#,
        )
        .unwrap();
        assert_eq!(m.version, "0.2.0");
        assert_eq!(m.build_number, 1234);
        assert_eq!(m.dmg, "aterm-0.2.0.dmg");
    }

    /// Lock the published wire shape (the historical gen-appcast.sh output, which
    /// the ship tool emits byte-compatibly): the exact field set (including the
    /// `'''` multiline `changelog` and extra keys) must parse, and the Manifest
    /// fields must read back correctly.
    #[test]
    fn parses_full_gen_appcast_shape() {
        let m = Manifest::parse(
            r#"
# Auto-generated by tools/gen-appcast.sh
schema = 1
version = "0.2.0"
build_number = 1234
commit = "deadbeefcafe"
dmg = "aterm-0.2.0.dmg"
sha256 = "ABC123"
min_os = "11.0"
team_id = "TEAMID1234"
pub_date = "2026-06-21T00:00:00Z"
url = "https://github.com/o/r/releases/download/v0.2.0/aterm-0.2.0.dmg"
changelog = '''
### Added
- a `thing` with "quotes" and # hashes
'''
"#,
        )
        .unwrap();
        assert_eq!(m.schema, 1);
        assert_eq!(m.version, "0.2.0");
        assert_eq!(m.build_number, 1234);
        assert_eq!(m.sha256, "ABC123");
        assert_eq!(m.dmg, "aterm-0.2.0.dmg");
        assert_eq!(
            m.commit.as_deref(),
            Some("deadbeefcafe"),
            "the appcast commit binds the build to a source commit — must be captured"
        );
    }

    /// A manifest from a newer format than this build understands is rejected
    /// (the client stays put) rather than silently misread.
    #[test]
    fn rejects_newer_schema() {
        let r = Manifest::parse(
            r#"
            schema = 99
            version = "9.0.0"
            build_number = 999999
            sha256 = "x"
            dmg = "aterm-9.0.0.dmg"
        "#,
        );
        assert!(r.is_err(), "a future schema must be rejected");
    }

    #[test]
    fn ready_round_trips() {
        let r = Ready {
            build_number: 9,
            version: "0.3.0".into(),
            commit: Some("deadbeefcafe".into()),
            dmg_sha256: "ff".into(),
            team_id: "T".into(),
            staged_at: "2026-06-21T00:00:00Z".into(),
            changelog: Some("### Fixes\n- a thing".into()),
        };
        let parsed: Ready = toml::from_str(&r.to_toml().unwrap()).unwrap();
        assert_eq!(parsed.build_number, 9);
        assert_eq!(parsed.version, "0.3.0");
        assert_eq!(parsed.commit.as_deref(), Some("deadbeefcafe"));
    }

    #[test]
    fn publishable_ready_requires_real_directory_and_binds_available_plist() {
        let root =
            std::env::temp_dir().join(format!("aterm-publishable-ready-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("download")).unwrap();
        let staging = crate::paths::Staging {
            apply_lock: root.join("apply.lock"),
            stage_lock: root.join("stage.lock"),
            download: root.join("download"),
            staged_app: root.join("staged/aterm.app"),
            ready: root.join("ready.toml"),
            status: root.join("status.toml"),
            root: root.clone(),
        };
        let commit = "0123456789abcdef0123456789abcdef01234567";
        let ready = Ready {
            build_number: 54,
            version: "0.54.1".into(),
            commit: Some(commit.into()),
            dmg_sha256: "ab".repeat(32),
            team_id: "T".into(),
            staged_at: String::new(),
            changelog: None,
        };
        std::fs::write(&staging.ready, ready.to_toml().unwrap()).unwrap();
        assert!(
            !ready.is_publishable(&staging),
            "marker alone is not a stage"
        );

        std::fs::create_dir_all(&staging.staged_app).unwrap();
        assert!(
            !ready.is_publishable(&staging),
            "an empty app directory cannot masquerade as a published release bundle"
        );

        let contents = staging.staged_app.join("Contents");
        std::fs::create_dir_all(&contents).unwrap();
        std::fs::write(
            contents.join("Info.plist"),
            format!(
                "<plist><dict><key>CFBundleVersion</key><string>54</string>\
                 <key>ATermGitCommit</key><string>{commit}</string></dict></plist>"
            ),
        )
        .unwrap();
        assert!(ready.is_publishable(&staging));

        std::fs::write(
            contents.join("Info.plist"),
            format!(
                "<plist><dict><key>CFBundleVersion</key><string>53</string>\
                 <key>ATermGitCommit</key><string>{commit}</string></dict></plist>"
            ),
        )
        .unwrap();
        assert!(
            !ready.is_publishable(&staging),
            "available staged build identity must agree with Ready"
        );

        std::fs::write(
            contents.join("Info.plist"),
            format!(
                "<plist><dict><key>CFBundleVersion</key><key>Other</key><string>54</string>\
                 <key>ATermGitCommit</key><string>{commit}</string></dict></plist>"
            ),
        )
        .unwrap();
        assert!(
            !ready.is_publishable(&staging),
            "an intervening plist key cannot lend its string to CFBundleVersion"
        );

        std::fs::remove_dir_all(&staging.staged_app).unwrap();
        let target = root.join("target.app");
        std::fs::create_dir_all(&target).unwrap();
        std::os::unix::fs::symlink(&target, &staging.staged_app).unwrap();
        assert!(
            !ready.is_publishable(&staging),
            "a staged-app symlink never grants publishable authority"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn manifest_min_build_optional_and_parsed() {
        // Absent ⇒ None (old-style appcast without the field still parses).
        let m = Manifest::parse(
            r#"version = "1.0.0"
               build_number = 10
               sha256 = "x"
               dmg = "a.dmg""#,
        )
        .unwrap();
        assert_eq!(m.min_build, None);
        // Present ⇒ Some(n).
        let m = Manifest::parse(
            r#"version = "1.0.0"
               build_number = 10
               sha256 = "x"
               dmg = "a.dmg"
               min_build = 7"#,
        )
        .unwrap();
        assert_eq!(m.min_build, Some(7));
    }

    /// This is the live parser used by the updater scan. Keep an explicit test
    /// here (in addition to aterm-update-core's shared-type proof) so a future
    /// refactor cannot accidentally leave the shipping duplicate permissive.
    #[test]
    fn manifest_rejects_min_build_above_its_own_build() {
        let err = Manifest::parse(
            r#"version = "1.0.0"
               build_number = 10
               sha256 = "x"
               dmg = "a.dmg"
               min_build = 11"#,
        )
        .unwrap_err();
        assert!(
            err.contains("min_build 11") && err.contains("build_number 10"),
            "{err}"
        );

        // Negative control for the strict boundary: equality remains valid.
        let m = Manifest::parse(
            r#"version = "1.0.0"
               build_number = 10
               sha256 = "x"
               dmg = "a.dmg"
               min_build = 10"#,
        )
        .unwrap();
        assert_eq!(m.min_build, Some(10));
    }

    #[test]
    fn floor_ratchets_monotonically_and_reads_default() {
        let p = std::env::temp_dir().join(format!("aterm-floor-{}.toml", std::process::id()));
        let _ = std::fs::remove_file(&p);
        // Absent ⇒ all-zero default (permissive).
        assert_eq!(Floor::read(&p).min_build, 0);
        assert_eq!(Floor::read(&p).high_water, 0);
        // Bump up.
        Floor::bump_and_write(&p, 5, 12);
        let f = Floor::read(&p);
        assert_eq!((f.min_build, f.high_water), (5, 12));
        // Lower observations must NOT lower the floor (monotonic).
        Floor::bump_and_write(&p, 3, 8);
        let f = Floor::read(&p);
        assert_eq!((f.min_build, f.high_water), (5, 12));
        // Higher observations raise each independently.
        Floor::bump_and_write(&p, 9, 12);
        let f = Floor::read(&p);
        assert_eq!((f.min_build, f.high_water), (9, 12));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn failed_mark_matches_on_build_and_hash_only() {
        let p = std::env::temp_dir().join(format!("aterm-failed-{}.toml", std::process::id()));
        let _ = std::fs::remove_file(&p);
        assert!(FailedMark::read(&p).is_none());
        FailedMark::record(&p, 42, "ABCdef");
        let m = FailedMark::read(&p).unwrap();
        assert!(m.matches(42, "abcdef"), "hash compare is case-insensitive");
        assert!(!m.matches(43, "abcdef"), "different build ⇒ retry");
        assert!(
            !m.matches(42, "999"),
            "different hash (re-published) ⇒ retry"
        );
        FailedMark::clear(&p);
        assert!(FailedMark::read(&p).is_none());
    }
}
