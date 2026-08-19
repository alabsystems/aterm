// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The two TOML records the updater reads/writes: the release-side **manifest**
//! (`aterm-appcast.toml`, an attached release asset, emitted by the ship tool —
//! `cargo ship cut`, crate aterm-release) and the local **ready marker**
//! (`ready.toml`, written last when staging completes — its presence is the sole
//! "ready" signal).

use std::path::Path;

use serde::{Deserialize, Serialize};

/// Write `text` to `path` atomically AND DURABLY: temp file, `sync_all`, rename,
/// best-effort directory sync — the shape `aterm_update_core::sentinel` already
/// uses for the boot sentinel.
///
/// WHY THESE TWO FILES NEED IT (2026-08-19 round-4 audit). The sentinel is the
/// commit marker for a swap and is `F_FULLFSYNC`'d; `trial.toml` (the trialed
/// artifact's identity) and `installed-receipt.toml` (the proof the installed
/// bundle is the artifact a ticket authorized) were written with a plain
/// `write` + `rename`. APFS commits the rename in a metadata transaction that a
/// later fsync can force, while the file's DATA pages stay dirty for up to ~30 s —
/// so a KERNEL panic or power loss in the trial window could leave the sentinel
/// armed beside a ZERO-LENGTH trial marker. (A userspace panic cannot: `write` and
/// `rename` are complete syscalls and the page cache is coherent for every later
/// reader. The dangerous window is the one the hardware can interrupt.) Every launch then counts, fails
/// `ensure_current_trial_receipt` (no identity to prove), and on the third one the
/// updater takes the "budget exhausted AND the rollback is unprovable" branch:
/// it DISARMS instead of reverting, so a crash-looping build stays installed with
/// its verified rollback sitting unused beside it. The two records must be as
/// durable as the marker that points at them.
///
/// An "unsupported" sync (network home, some FUSE volumes — Apple's `sync_all` is
/// a bare `F_FULLFSYNC` with no fallback) degrades to the previous non-durable
/// behaviour rather than failing the apply; a real I/O error still aborts.
pub(crate) fn write_durable(path: &Path, text: &str, what: &str) -> Result<(), String> {
    use std::io::Write as _;
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let tmp = path.with_extension(format!("toml.{}.tmp", std::process::id()));
    let write = (|| -> std::io::Result<()> {
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(text.as_bytes())?;
        match file.sync_all() {
            Ok(()) => Ok(()),
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::Unsupported | std::io::ErrorKind::InvalidInput
                ) =>
            {
                Ok(())
            }
            Err(e) => Err(e),
        }
    })();
    if let Err(error) = write {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("write {what}: {error}"));
    }
    if let Err(error) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("commit {what}: {error}"));
    }
    let _ = std::fs::File::open(parent).and_then(|d| d.sync_all());
    Ok(())
}

/// The client's view of the release manifest attached to a GitHub Release as
/// `aterm-appcast.toml`: just the fields the updater consumes, filled by
/// [`Manifest::parse`] from the shared `aterm_update_core::manifest::Manifest`
/// wire type (the same type the ship tool's emitter serializes), so the wire
/// schema and its validation live in exactly one place.
#[derive(Debug, Clone)]
pub struct Manifest {
    /// Manifest format version (the ship tool emits `schema = 1`). Absent ⇒ 0.
    ///
    /// Carried for tests only. The authoritative copy — and the sole decision
    /// this build makes with it, the "newer than supported" rejection — lives in
    /// `aterm_update_core::manifest::Manifest`, which [`Manifest::parse`] runs
    /// before this view is built; nothing downstream of the parse reads it.
    #[cfg(test)]
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
    pub commit: Option<String>,
    /// SHA-256 (lowercase hex) of the DMG asset.
    pub sha256: String,
    /// The DMG asset's file name within the release, e.g. `"aterm-0.2.0.dmg"`.
    pub dmg: String,
    /// The updater container's file name, e.g. `"aterm-0.2.0-mac.zip"` — the same
    /// signed bundle as the DMG, packed with `ditto` instead of `hdiutil`. PREFERRED
    /// for staging when present (see [`crate::install::stage_from_zip`]): after a
    /// seamless overlap update the surviving process is an orphan whose launchd job
    /// exited, and `hdiutil attach` fails ENXIO there because DiskImages needs a live
    /// bootstrap context. Absent ⇒ None (a manifest cut before zip staging), and the
    /// client falls back to the DMG.
    pub zip: Option<String>,
    /// SHA-256 (lowercase hex) of the zip asset. Absent ⇒ None. A zip without a
    /// digest is never staged from — there would be nothing to check the bytes
    /// against — so the client falls back to the DMG.
    pub zip_sha256: Option<String>,
    /// Optional operator **apply floor**: clients refuse to stage/apply ANY build
    /// whose `build_number` is below this, even a genuine signed one. Lets the owner
    /// retire a bad-but-genuine release after the fact — a "yank" a silent updater can
    /// honor without a signed channel. Ratcheted monotonically client-side (see
    /// [`Floor`]); the release cutter carries a raised channel floor into every
    /// successor manifest. Absent ⇒ 0 (no floor). (F5)
    pub min_build: Option<u64>,
    /// Optional human-readable "what changed" notes (the hand-written CHANGELOG.md
    /// section for this release, verbatim — the same text as the GitHub release
    /// notes). Surfaced by the in-app updater's status query + the "Check for
    /// Updates" menu so the user sees what a staged update brings. Absent ⇒ None.
    pub changelog: Option<String>,
    /// ATTRIBUTION: which MACHINE signed this release (`"m3"`). Carried through from the
    /// wire type because the roster chain binds it: the id sits inside the signed appcast
    /// bytes, so a genuine signature by one machine cannot be relabelled as another, and
    /// the roster maps id to public key so a stolen key cannot claim someone else's id.
    /// Absent ⇒ None, which is every release cut before the roster existed and every
    /// release seen by a build with no paper master pinned.
    pub machine_id: Option<String>,
    /// The `roster_seq` that authorized [`Self::machine_id`] — the cross-check that stops
    /// an old roster being paired with a new release after a machine is revoked.
    /// Absent ⇒ None.
    pub roster_seq: Option<u64>,
}

impl Manifest {
    /// Parse a manifest from TOML text via the shared `aterm-update-core`
    /// validator (schema ceiling, `min_build <= build_number` floor sanity),
    /// so publisher and client can never drift.
    pub fn parse(text: &str) -> Result<Self, String> {
        let m = aterm_update_core::manifest::Manifest::parse(text)?;
        Ok(Self {
            #[cfg(test)]
            schema: m.schema,
            version: m.version,
            build_number: m.build_number,
            commit: m.commit,
            sha256: m.sha256,
            dmg: m.dmg,
            zip: m.zip,
            zip_sha256: m.zip_sha256,
            min_build: m.min_build,
            changelog: m.changelog,
            machine_id: m.machine_id,
            roster_seq: m.roster_seq,
        })
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
    /// ATTRIBUTION recorded at stage time: which MACHINE's key authorized this bundle,
    /// and under which roster generation.
    ///
    /// Without these a stage was a promise nothing could withdraw. A revocation reaches
    /// a client as a NEW roster generation, which the check lane ratchets into
    /// [`Floor::roster_seq`] and enforces via `roster_authority_superseded` — but the
    /// apply lane re-read only `min_build`, so a bundle staged at 10:00 by a machine
    /// revoked at 10:30 installed at the next launch anyway, and only a separate
    /// `min_build` yank could have stopped a withdrawn machine's artifact. Recording
    /// the generation here is what lets the apply gate ask the question the stage gate
    /// already asks.
    ///
    /// Absent ⇒ None: a marker written before these fields existed, or a release with
    /// no attribution at all (every pre-roster cut). `None` means UNKNOWN, never
    /// "exempt" — see the apply gate for what that costs and why.
    #[serde(default)]
    pub machine_id: Option<String>,
    /// See [`Self::machine_id`].
    #[serde(default)]
    pub roster_seq: Option<u64>,
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
pub(crate) fn xml_plist_string<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    let after_key = text.find(key)? + key.len();
    // The value element must immediately follow the key (modulo whitespace).
    // Searching arbitrarily far forward would let a malformed/intervening key's
    // string be mis-bound to the release identity we are authenticating.
    let value_tail = text[after_key..].trim_start().strip_prefix("<string>")?;
    let string_end = value_tail.find("</string>")?;
    let value = value_tail[..string_end].trim();
    (!value.is_empty()).then_some(value)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct InstalledReceipt {
    pub(crate) build_number: u64,
    pub(crate) git_commit: String,
    pub(crate) dmg_sha256: String,
}

impl InstalledReceipt {
    fn canonical(build_number: u64, git_commit: &str, dmg_sha256: &str) -> Option<Self> {
        let git_commit = git_commit.trim();
        let dmg_sha256 = dmg_sha256.trim();
        (build_number != 0
            && git_commit.len() == 40
            && git_commit.bytes().all(|byte| byte.is_ascii_hexdigit())
            && dmg_sha256.len() == 64
            && dmg_sha256.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then(|| Self {
            build_number,
            git_commit: git_commit.to_ascii_lowercase(),
            dmg_sha256: dmg_sha256.to_ascii_lowercase(),
        })
    }

    /// Read a well-formed receipt. Missing, oversized, malformed, or non-canonical
    /// data fails closed as no proof.
    pub(crate) fn read(path: &Path) -> Option<Self> {
        let parsed: Self = toml::from_str(&crate::read_ledger_text(path)?).ok()?;
        Self::canonical(parsed.build_number, &parsed.git_commit, &parsed.dmg_sha256)
    }

    /// Atomically replace the prior installed receipt after a successful swap.
    /// Failure is returned so the caller can keep the install transaction honest.
    pub(crate) fn record(
        path: &Path,
        build_number: u64,
        git_commit: &str,
        dmg_sha256: &str,
    ) -> Result<(), String> {
        Self::write_atomic(path, build_number, git_commit, dmg_sha256)
    }

    fn write_atomic(
        path: &Path,
        build_number: u64,
        git_commit: &str,
        dmg_sha256: &str,
    ) -> Result<(), String> {
        let receipt = Self::canonical(build_number, git_commit, dmg_sha256)
            .ok_or_else(|| "installed receipt identity is malformed".to_string())?;
        let text = toml::to_string(&receipt)
            .map_err(|error| format!("serialize installed receipt: {error}"))?;
        write_durable(path, &text, "installed receipt")
    }

    /// Re-record a previously parsed receipt verbatim. Used when an exec-failure
    /// inverse swap restores OLD and its receipt must come back unchanged.
    pub(crate) fn record_preserving_kind(&self, path: &Path) -> Result<(), String> {
        Self::write_atomic(path, self.build_number, &self.git_commit, &self.dmg_sha256)
    }

    /// Bind this unsigned local receipt to the codesign-sealed identity of the
    /// bundle currently at the canonical install path.
    pub(crate) fn matches_sealed(&self, build_number: u64, git_commit: &str) -> bool {
        self.build_number == build_number && crate::commit_matches(&self.git_commit, git_commit)
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
/// NOTE: the first two fields are *unsigned* floors, so they cannot protect a brand-new
/// client that has never seen a higher build (it has no high-water yet). `roster_seq` is
/// the signed-channel answer to exactly that gap — see its own doc — though the residual
/// it leaves is different rather than absent. All three DO stop replay against any client
/// that has already advanced, and the first gives the operator a working yank.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Floor {
    /// Operator apply floor (max `min_build` ever seen). Absent ⇒ 0.
    #[serde(default)]
    pub min_build: u64,
    /// Highest build ever successfully staged by this client. Absent ⇒ 0.
    #[serde(default)]
    pub high_water: u64,
    /// Highest `roster_seq` from any machine roster this client has ever accepted — THE
    /// replay defence for the master-signed roster tier.
    ///
    /// The threat it answers is specific: the master revokes a stolen machine, but the
    /// attacker still holds that machine's key AND a copy of the roster generation that
    /// listed it. That old roster's master signature is valid FOREVER — signatures do not
    /// expire, documents do — so nothing about the crypto stops it being served again.
    /// This field does: a client that has durably seen sequence `n` refuses `n-1`
    /// permanently.
    ///
    /// Its limit is the honest one and belongs here rather than in a design doc: it is
    /// worth exactly nothing to a FRESH INSTALL, which has no recorded sequence and so
    /// accepts whatever it is first shown. Nor is anything else covering that client:
    /// the roster's `valid_until` was once a 180-day freshness window, but every roster
    /// this tooling mints now stamps 9999-12-31 (`atpkg_keys::roster_ops::
    /// VALID_UNTIL_FOREVER`), so the freshness gate always passes and first contact has
    /// NO replay defence at all. That is a recorded owner decision, not an oversight —
    /// the window's residual protection did not pay for a mandatory twice-yearly
    /// re-sign plus a fleet-wide fail-closed outage if the date ever lapsed unattended,
    /// and revocation is the answer to a stolen key either way. It is written down here
    /// because the previous wording called that window "a deliberate number", which
    /// reads as a protection still being relied on; the only thing this ratchet
    /// actually gives a fresh install is protection from the SECOND roster onwards.
    ///
    /// Absent ⇒ 0, the permissive first-contact value, matching the other two.
    #[serde(default)]
    pub roster_seq: u64,
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
    ///
    /// Best-effort, but no longer SILENT. Every step here used to discard its error,
    /// so a full disk, a read-only remount, or a floor file this uid cannot replace
    /// froze `min_build`, `high_water` and `roster_seq` at their recorded values
    /// indefinitely — a replayed pre-revocation roster generation and an operator-
    /// yanked build are then re-accepted every single cycle — while `update status`
    /// went on describing a perfectly healthy machine, so the one party who could fix
    /// the disk was never told. `atpkg`'s sibling ratchet (`atpkg::sig`,
    /// `Floor::write` / `read_floor`) makes exactly this call in the other direction
    /// and documents why: report the failure, then carry on under the old value,
    /// because refusing outright would let anyone who can wedge the file wedge the
    /// updater. Same shape here.
    pub fn bump_and_write(
        path: &Path,
        seen_min_build: u64,
        staged_build: u64,
        seen_roster_seq: u64,
    ) {
        if let Err(error) =
            Self::bump_and_write_reporting(path, seen_min_build, staged_build, seen_roster_seq)
        {
            crate::warn(&format!(
                "{error} — replay/rollback protection stays frozen at the values \
                 already on disk"
            ));
        }
    }

    /// [`Self::bump_and_write`]'s fallible body: the same ratchet, but handing the
    /// caller the failure instead of only warning about it, so a test can pin BOTH
    /// halves of a failed commit — the error is reported rather than swallowed, and the
    /// temp file is gone afterwards.
    fn bump_and_write_reporting(
        path: &Path,
        seen_min_build: u64,
        staged_build: u64,
        seen_roster_seq: u64,
    ) -> Result<(), String> {
        Self::commit_bump(path, seen_min_build, staged_build, seen_roster_seq).map_err(|error| {
            // NAME THE ADVANCE THAT WAS LOST, not just the step that failed. The raw
            // step error reads "commit /…/floor.toml: Is a directory", which leaves the
            // reader to work out on their own that replay and rollback protection just
            // stopped moving. Attaching the floor here makes that consequence legible at
            // EVERY call site instead of only at whichever one remembers to add it.
            format!(
                "could not raise the update floor at {} to (min_build {seen_min_build}, \
                 high_water {staged_build}, roster_seq {seen_roster_seq}): {error}",
                path.display()
            )
        })
    }

    /// The locked read/max/write transaction itself, reporting the STEP that failed.
    fn commit_bump(
        path: &Path,
        seen_min_build: u64,
        staged_build: u64,
        seen_roster_seq: u64,
    ) -> Result<(), String> {
        let lock_path = path.with_extension("toml.lock");
        // `_lock`, never `_`: the guard must live until this function returns, since
        // it covers the whole read/max/write transaction.
        let _lock = aterm_update_core::FileLock::acquire(&lock_path)
            .map_err(|error| format!("lock {}: {error}", lock_path.display()))?;
        let cur = Self::read(path);
        let next = Self {
            min_build: cur.min_build.max(seen_min_build),
            high_water: cur.high_water.max(staged_build),
            roster_seq: cur.roster_seq.max(seen_roster_seq),
        };
        if next == cur {
            return Ok(());
        }
        let text = toml::to_string(&next).map_err(|error| format!("encode floor: {error}"))?;
        let tmp = path.with_extension(format!("toml.{}.tmp", std::process::id()));
        // The temp is swept on EVERY failing path, not just a failed write. The rename
        // arm used to leak it, and a failing rename is precisely the case that repeats
        // forever (a full disk, a read-only remount): one `floor.toml.<pid>.tmp` per
        // failing cycle piling up in the 0700 Updates root, which nothing sweeps.
        let committed = std::fs::write(&tmp, text)
            .map_err(|error| format!("write {}: {error}", tmp.display()))
            .and_then(|()| {
                std::fs::rename(&tmp, path)
                    .map_err(|error| format!("commit {}: {error}", path.display()))
            });
        if committed.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        committed
    }
}

/// A memo of the last candidate build that was downloaded but FAILED to stage
/// (verification / mount / extract). Persisted under `Updates/failed.toml` so the
/// periodic loop doesn't re-download the same (up to 512 MB) DMG every interval when
/// the newest release is a build we already proved unstageable (F17). Keyed by
/// `(build_number, sha256)` so a re-published fix under the same build is retried.
///
/// # Why there is a retry budget and not a permanent memo
///
/// This memo used to be permanent for a given `(build, sha256)`: once recorded,
/// the only escapes were a NEWER build or a re-publish under a different digest.
/// That is right for a genuinely corrupt artifact and WRONG for everything else,
/// because staging fails for plenty of transient reasons that say nothing about
/// the bytes — a full disk, a `codesign`/`spctl` invocation that lost a race with
/// Gatekeeper's cache, a DMG mount refused under memory pressure, a machine put
/// to sleep mid-extract. One such blip permanently pinned that machine to its
/// current build with no user-visible cause and no recovery short of deleting
/// updater state by hand.
///
/// So the memo now carries an attempt count and a `retry_after` deadline, and it
/// SUPPRESSES rather than FORBIDS: the same candidate is retried on a widening
/// schedule ([`RETRY_BACKOFF_SECS`], capped at its last entry) instead of never.
/// A corrupt artifact therefore costs one download per backoff period rather than
/// one per check — the bandwidth F17 was protecting — while a transient failure
/// heals on its own.
///
/// The crash-loop TRIAL marker (`Updates/trial.toml`) is a different use of this
/// same type and deliberately keeps the old permanent semantics: poisoning an
/// artifact that crashed on boot must be deterministic, so those paths call
/// [`FailedMark::record`]/[`FailedMark::matches`] and never consult the deadline.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FailedMark {
    #[serde(default)]
    pub build_number: u64,
    #[serde(default)]
    pub sha256: String,
    /// How many times this exact `(build_number, sha256)` has failed. Absent in
    /// markers written before the retry budget existed ⇒ 0, which is treated as
    /// "one failure" by [`FailedMark::next_attempt`] so an old file does not get
    /// a longer backoff than a fresh one.
    #[serde(default)]
    pub attempts: u32,
    /// Unix seconds before which this candidate is skipped. Absent ⇒ 0, i.e. an
    /// old permanent-style marker becomes immediately retryable, which is the
    /// safe direction: the worst case is one extra download.
    #[serde(default)]
    pub retry_after: u64,
    /// QUARANTINE: this artifact was swapped in and then failed to confirm boot
    /// health `MAX_BOOT_ATTEMPTS` times, and the updater reverted it. Unlike
    /// `retry_after` this never lapses — the build proved itself bad ON THIS
    /// MACHINE, and the only honest escapes are a newer build or a re-publish
    /// under a different digest, both of which change the memo's key.
    ///
    /// It is a FIELD rather than a `retry_after` sentinel because the two states
    /// are genuinely different and were previously conflated: the crash-loop path
    /// wrote `retry_after = 0` meaning "forever", while [`Self::suppresses`] reads
    /// `retry_after = 0` as "the deadline has passed, retry now" — which is also
    /// exactly what a pre-budget legacy marker means. The poison was therefore
    /// written and then ignored, and the crash-looping build was re-downloaded and
    /// re-applied on the very next check. Absent ⇒ `false`, so a legacy marker
    /// keeps its (safe) retryable reading.
    #[serde(default)]
    pub quarantined: bool,
    /// TRIAL markers only: the install root (`…/aterm.app`) the armed trial belongs
    /// to. The sentinel and this marker are per USER, while a build can sit at
    /// several paths at once (a dev machine's `dist/aterm.app` beside
    /// `/Applications/aterm.app`, a duplicate copy of a release): a same-build
    /// process launched from a DIFFERENT bundle used to count launches against, or
    /// disarm, a trial it did not own — three sibling launches could revert and
    /// poison a build that never crashed. Absent in markers written before this
    /// field existed ⇒ unbound (the historical behaviour, for one trial's lifetime).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub install_root: Option<String>,
}

/// The widening retry schedule for a candidate that failed to stage, in seconds:
/// 15 min, 1 h, 4 h, then 24 h forever. The first entry is already longer than
/// the authenticated check interval (75 s), so a failing build cannot cost more
/// than one download per 15 minutes even at its most aggressive; the 24 h ceiling
/// bounds a permanently-corrupt artifact at one download a day, against the
/// unbounded "never retry" this replaced.
pub const RETRY_BACKOFF_SECS: [u64; 4] = [15 * 60, 60 * 60, 4 * 60 * 60, 24 * 60 * 60];

impl FailedMark {
    /// Read the memo, or `None` if absent/unparseable.
    /// A marker that parses to ALL DEFAULTS is not a marker: every field carries
    /// `#[serde(default)]`, so a zero-length file — a rename committed over data
    /// extents a kernel panic never wrote — deserialized into `build_number: 0` and
    /// answered "not this artifact" instead of failing closed, which would let a
    /// just-quarantined build be re-downloaded and re-applied (2026-08-19 round-4
    /// skeptics). Treated as ABSENT.
    pub fn read(path: &Path) -> Option<Self> {
        let parsed: Self = toml::from_str(&crate::read_ledger_text(path)?).ok()?;
        (parsed.build_number != 0 || !parsed.sha256.is_empty()).then_some(parsed)
    }

    /// Whether this memo matches the given candidate (same build AND same DMG hash).
    ///
    /// Pure identity, with no notion of time — this is what the crash-loop TRIAL
    /// marker wants. The download/stage memo must use [`FailedMark::suppresses`]
    /// instead, or a transient failure becomes permanent.
    pub fn matches(&self, build_number: u64, sha256: &str) -> bool {
        self.build_number == build_number && self.sha256.eq_ignore_ascii_case(sha256)
    }

    /// Whether this memo should SKIP the given candidate right now: it names the
    /// same artifact AND either it is QUARANTINED (crash-looped — never retried) or
    /// its backoff deadline has not yet passed. Once the deadline passes a
    /// non-quarantined candidate is retried (and, if it fails again, recorded with
    /// the next-wider backoff).
    ///
    /// `now` is unix seconds. A marker with no `retry_after` and no quarantine flag
    /// (written before the retry budget existed) never suppresses.
    #[must_use]
    pub fn suppresses(&self, build_number: u64, sha256: &str, now: u64) -> bool {
        self.matches(build_number, sha256) && (self.quarantined || now < self.retry_after)
    }

    /// Whether this memo is the never-lapsing crash-loop quarantine rather than a
    /// timed backoff. Callers report the two differently: one ends by itself, the
    /// other ends only when the channel offers a different artifact.
    #[must_use]
    pub fn is_quarantine(&self) -> bool {
        self.quarantined
    }

    /// Seconds until this memo stops suppressing, or 0 if it already has. Meaningless
    /// for a quarantine — check [`Self::is_quarantine`] first.
    #[must_use]
    pub fn retry_in_secs(&self, now: u64) -> u64 {
        self.retry_after.saturating_sub(now)
    }

    /// The attempt count and backoff a fresh failure of `(build_number, sha256)`
    /// should be recorded with, given this memo as the prior state. Pure, so the
    /// widening schedule is unit-testable without a filesystem or a clock.
    ///
    /// A failure of a DIFFERENT artifact resets the budget — the previous one is
    /// no longer the candidate and its history says nothing about this one.
    #[must_use]
    pub fn next_attempt(&self, build_number: u64, sha256: &str) -> u32 {
        if self.matches(build_number, sha256) {
            self.attempts.saturating_add(1).max(2)
        } else {
            1
        }
    }

    /// The backoff for the `n`-th consecutive failure (1-based), saturating at the
    /// last entry of [`RETRY_BACKOFF_SECS`].
    #[must_use]
    pub fn backoff_secs(attempts: u32) -> u64 {
        let idx = (attempts.max(1) as usize - 1).min(RETRY_BACKOFF_SECS.len() - 1);
        RETRY_BACKOFF_SECS[idx]
    }

    /// Record `(build_number, sha256)` as the IN-FLIGHT trial artifact, discarding the
    /// persistence error. TEST-ONLY: production arms the trial through
    /// [`Self::record_required`], because a trial marker that failed to persist must abort
    /// the apply rather than swap in a build whose crash-loop recovery cannot name it.
    ///
    /// Identity only, no verdict: the trial marker exists so a machine that comes back
    /// up after a swap can tell WHICH artifact it is trialing, and its readers use
    /// [`Self::matches`], never the deadline. The download/stage memo wants
    /// [`Self::record_stage_failure`]; the crash-loop verdict wants
    /// [`Self::record_quarantine`] — which is what the one production caller this
    /// convenience used to have was changed to, leaving it to the fixtures.
    #[cfg(test)]
    pub fn record(path: &Path, build_number: u64, sha256: &str) {
        let _ = Self::record_required(path, build_number, sha256, None);
    }

    /// QUARANTINE `(build_number, sha256)`: it was swapped in, failed to confirm boot
    /// health `MAX_BOOT_ATTEMPTS` times, and was reverted. [`Self::suppresses`] then
    /// skips it forever — until the channel offers a different build or a re-publish
    /// under a different digest, either of which changes the key and so no longer
    /// matches.
    ///
    /// This exists because [`Self::record`] could not express it. That path wrote
    /// `retry_after = 0` intending "never retry", but `suppresses` reads a zero
    /// deadline as "already elapsed" — so the memo was written and then ignored, and
    /// the very next check re-downloaded and re-applied the build that had just
    /// crash-looped, straight back into the crash/revert loop the poison existed to
    /// break.
    /// DURABLE, like the trial identity it supersedes: this memo is the only thing
    /// standing between a build that just crash-looped and the next check
    /// re-downloading and re-applying it, and every other record that could
    /// re-derive the verdict (trial, rollback, stage) is deleted moments later
    /// (2026-08-19 round-4 skeptics).
    pub fn record_quarantine(path: &Path, build_number: u64, sha256: &str) {
        let m = FailedMark {
            build_number,
            sha256: sha256.to_ascii_lowercase(),
            attempts: 0,
            retry_after: 0,
            quarantined: true,
            install_root: None,
        };
        let Ok(text) = toml::to_string(&m) else {
            return;
        };
        let tmp = path.with_extension(format!("toml.{}.tmp", std::process::id()));
        if std::fs::write(&tmp, text).is_ok() {
            let _ = std::fs::rename(&tmp, path);
        } else {
            let _ = std::fs::remove_file(&tmp);
        }
    }

    /// Record a download/stage failure of `(build_number, sha256)`, widening the
    /// retry backoff when this is a repeat of the same artifact and resetting it
    /// when the candidate changed. Best-effort: a memo we fail to persist only
    /// costs a redundant download next cycle, never correctness.
    ///
    /// `now` is unix seconds, threaded in rather than read here so the widening
    /// schedule is testable.
    pub fn record_stage_failure(path: &Path, build_number: u64, sha256: &str, now: u64) {
        let prior = Self::read(path).unwrap_or_default();
        let attempts = prior.next_attempt(build_number, sha256);
        let m = FailedMark {
            build_number,
            sha256: sha256.to_ascii_lowercase(),
            attempts,
            retry_after: now.saturating_add(Self::backoff_secs(attempts)),
            // A timed backoff, never a quarantine: this failure is about fetching or
            // staging the artifact, not about the artifact having proved itself bad.
            quarantined: false,
            install_root: None,
        };
        let Ok(text) = toml::to_string(&m) else {
            return;
        };
        let _ = write_durable(path, &text, "artifact quarantine");
    }

    /// Atomically record `(build_number, sha256)`, reporting persistence failure.
    /// Apply uses this stricter form before swap because crash-loop recovery must be
    /// able to poison the exact trial artifact deterministically.
    pub(crate) fn record_required(
        path: &Path,
        build_number: u64,
        sha256: &str,
        install_root: Option<&Path>,
    ) -> Result<(), String> {
        let m = FailedMark {
            build_number,
            sha256: sha256.to_ascii_lowercase(),
            // Identity only — no deadline and no verdict; see `record`.
            attempts: 0,
            retry_after: 0,
            quarantined: false,
            install_root: install_root.map(|root| root.to_string_lossy().into_owned()),
        };
        let text =
            toml::to_string(&m).map_err(|error| format!("serialize artifact marker: {error}"))?;
        write_durable(path, &text, "artifact marker")
    }

    /// Clear the memo (called once a stage finally succeeds).
    pub fn clear(path: &Path) {
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A zero-length marker — a rename committed over data extents a kernel panic
    /// never wrote — must read as ABSENT, not as an all-defaults memo that answers
    /// "not this artifact" and lets a just-quarantined build be re-applied.
    #[test]
    fn a_vacuous_marker_reads_as_absent_not_as_an_empty_memo() {
        let dir = std::env::temp_dir().join(format!("aterm-vacuous-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("failed.toml");

        std::fs::write(&path, "").unwrap();
        assert!(FailedMark::read(&path).is_none(), "zero length is absent");
        std::fs::write(&path, "build_number = 0\nsha256 = \"\"\n").unwrap();
        assert!(FailedMark::read(&path).is_none(), "all-defaults is absent");

        FailedMark::record_quarantine(&path, 42, &"ab".repeat(32));
        let read = FailedMark::read(&path).expect("a real memo reads back");
        assert_eq!(read.build_number, 42);
        assert!(read.quarantined, "and keeps its verdict");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The regression this budget exists for: a stage failure must SUPPRESS the
    /// candidate for a while, then let it through again. The old marker had no
    /// deadline and stranded the machine on its current build forever.
    #[test]
    fn a_stage_failure_suppresses_then_expires_instead_of_being_permanent() {
        let root =
            std::env::temp_dir().join(format!("aterm-failedmark-ttl-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("failed.toml");
        const BUILD: u64 = 1785125098;
        const SHA: &str = "62605231daafe06037584df32414f02120f0f040b2b20ba77bdaff214ba028d6";

        FailedMark::record_stage_failure(&path, BUILD, SHA, 1_000);
        let m = FailedMark::read(&path).expect("marker persisted");
        assert_eq!(m.attempts, 1);
        assert_eq!(m.retry_after, 1_000 + RETRY_BACKOFF_SECS[0]);

        // Inside the window: skipped.
        assert!(m.suppresses(BUILD, SHA, 1_000));
        assert!(m.suppresses(BUILD, SHA, 1_000 + RETRY_BACKOFF_SECS[0] - 1));
        // Past the window: retried. THIS is what the old permanent memo never did.
        assert!(!m.suppresses(BUILD, SHA, 1_000 + RETRY_BACKOFF_SECS[0]));

        // A different artifact is never suppressed by this memo.
        assert!(!m.suppresses(BUILD + 1, SHA, 1_000));
        assert!(!m.suppresses(BUILD, &"a".repeat(64), 1_000));

        let _ = std::fs::remove_dir_all(&root);
    }

    /// Repeats of the SAME artifact widen the backoff and saturate; a different
    /// artifact resets it.
    #[test]
    fn the_retry_budget_widens_saturates_and_resets_on_a_new_artifact() {
        let root =
            std::env::temp_dir().join(format!("aterm-failedmark-widen-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("failed.toml");
        const SHA: &str = "deadbeef";

        for (n, expected) in RETRY_BACKOFF_SECS.iter().enumerate() {
            FailedMark::record_stage_failure(&path, 7, SHA, 0);
            let m = FailedMark::read(&path).unwrap();
            assert_eq!(m.attempts as usize, n + 1, "attempt {n} count");
            assert_eq!(m.retry_after, *expected, "attempt {n} backoff");
        }
        // Saturated: one more failure keeps the ceiling, does not grow past it.
        FailedMark::record_stage_failure(&path, 7, SHA, 0);
        let m = FailedMark::read(&path).unwrap();
        assert_eq!(m.retry_after, *RETRY_BACKOFF_SECS.last().unwrap());

        // A different build resets to the first rung.
        FailedMark::record_stage_failure(&path, 8, SHA, 0);
        let m = FailedMark::read(&path).unwrap();
        assert_eq!(m.attempts, 1);
        assert_eq!(m.retry_after, RETRY_BACKOFF_SECS[0]);

        let _ = std::fs::remove_dir_all(&root);
    }

    /// The crash-loop TRIAL marker keeps its deterministic permanent semantics:
    /// `record` writes no deadline, so `suppresses` never fires and `matches`
    /// still identifies the poisoned artifact exactly.
    #[test]
    fn the_permanent_record_form_keeps_crash_loop_poisoning_deterministic() {
        let root =
            std::env::temp_dir().join(format!("aterm-failedmark-perm-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("trial.toml");

        FailedMark::record(&path, 42, "ABCDEF");
        let m = FailedMark::read(&path).unwrap();
        assert_eq!(m.retry_after, 0);
        assert!(m.matches(42, "abcdef"), "identity is case-insensitive");
        assert!(
            !m.suppresses(42, "abcdef", u64::MAX),
            "a permanent poison marker must not participate in the retry budget"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// A marker written before the retry budget existed has no `attempts` /
    /// `retry_after` keys. It must parse, and must be immediately retryable
    /// rather than inheriting the old forever-skip.
    #[test]
    fn a_pre_budget_marker_parses_and_becomes_retryable() {
        let m: FailedMark = toml::from_str("build_number = 9\nsha256 = \"ff\"\n").unwrap();
        assert_eq!(m.attempts, 0);
        assert_eq!(m.retry_after, 0);
        assert!(m.matches(9, "ff"));
        assert!(
            !m.suppresses(9, "ff", 0),
            "old markers must not strand a machine"
        );
        // And the next failure of that same artifact starts at rung 2, not 1 —
        // it is genuinely a repeat, so it should not get the shortest backoff.
        assert_eq!(m.next_attempt(9, "ff"), 2);
        assert_eq!(m.next_attempt(10, "ff"), 1);
        // A legacy marker is never mistaken for a quarantine: the flag is absent, and
        // absent must mean "no", or every pre-budget machine strands itself on upgrade.
        assert!(!m.is_quarantine());
    }

    /// THE CRASH-LOOP QUARANTINE, and why it needed a field of its own.
    ///
    /// The revert path wrote its poison as `retry_after = 0` MEANING "forever", while
    /// [`FailedMark::suppresses`] reads a zero deadline as "already elapsed" — which is
    /// also exactly what the pre-budget legacy marker above means. The two states were
    /// indistinguishable, so the poison was written and then ignored, and the build that
    /// had just crash-looped was re-downloaded and re-applied on the next check.
    #[test]
    fn a_quarantine_suppresses_forever_while_the_shape_it_used_to_take_does_not() {
        let root = std::env::temp_dir().join(format!("aterm-quarantine-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("failed.toml");
        let sha = "ab".repeat(32);

        // The OLD poison shape (identity only) — inert, at any time. This is the bug.
        FailedMark::record(&path, 42, &sha);
        let old = FailedMark::read(&path).expect("parses");
        assert!(old.matches(42, &sha), "it names the right artifact…");
        assert!(!old.is_quarantine());
        assert!(!old.suppresses(42, &sha, 0), "…and suppresses nothing");

        // The quarantine: same key, but it holds.
        FailedMark::record_quarantine(&path, 42, &sha);
        let q = FailedMark::read(&path).expect("parses");
        assert!(q.is_quarantine());
        assert!(q.suppresses(42, &sha, 0));
        assert!(
            q.suppresses(42, &sha, u64::MAX),
            "no clock lapses a quarantine"
        );
        // Case-insensitive on the digest, like every other comparison here.
        assert!(q.suppresses(42, &sha.to_ascii_uppercase(), 0));

        // THE ESCAPES — a newer build, or a re-publish under a different digest.
        assert!(!q.suppresses(43, &sha, 0));
        assert!(!q.suppresses(42, &"cd".repeat(32), 0));

        // A timed stage failure recorded afterwards REPLACES the quarantine with a
        // lapsing window: the artifact identity is the key, and the last write wins.
        FailedMark::record_stage_failure(&path, 42, &sha, 100);
        let timed = FailedMark::read(&path).expect("parses");
        assert!(!timed.is_quarantine());
        assert!(timed.suppresses(42, &sha, 100));
        assert!(!timed.suppresses(42, &sha, 100 + RETRY_BACKOFF_SECS[3]));

        let _ = std::fs::remove_dir_all(&root);
    }

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
    fn concurrent_floor_writers_cannot_lose_independent_maxima() {
        let root =
            std::env::temp_dir().join(format!("aterm-floor-transaction-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("floor.toml");
        let start = std::sync::Arc::new(std::sync::Barrier::new(3));

        let writer = |seen_min_build, staged_build, seen_roster_seq| {
            let path = path.clone();
            let start = std::sync::Arc::clone(&start);
            std::thread::spawn(move || {
                start.wait();
                Floor::bump_and_write(&path, seen_min_build, staged_build, seen_roster_seq);
            })
        };
        // Each writer carries the maximum of exactly one coordinate. If the locked
        // transaction were not doing its job, the loser's write would clobber the
        // winner's — including, now, the roster sequence, whose regression would make a
        // replayed pre-revocation roster acceptable again.
        let min_writer = writer(1_000, 1, 1);
        let high_writer = writer(1, 2_000, 9);
        start.wait();
        min_writer.join().unwrap();
        high_writer.join().unwrap();

        assert_eq!(
            Floor::read(&path),
            Floor {
                min_build: 1_000,
                high_water: 2_000,
                roster_seq: 9,
            },
            "read/max/write is one locked transaction; no coordinate may regress"
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

    /// The zip container is optional in BOTH directions. A manifest that carries
    /// one must surface it (that is what lets the client stage without `hdiutil`),
    /// and every already-published manifest — which has no such keys — must keep
    /// parsing exactly as before, or shipping zip staging would strand the fleet
    /// on the release that introduced it.
    #[test]
    fn zip_container_is_parsed_when_present_and_optional_when_not() {
        let with_zip = Manifest::parse(
            r#"schema = 1
               version = "0.10.0"
               build_number = 1234
               sha256 = "abc123"
               dmg = "aterm-0.10.0.dmg"
               zip = "aterm-0.10.0-mac.zip"
               zip_sha256 = "def456""#,
        )
        .unwrap();
        assert_eq!(with_zip.zip.as_deref(), Some("aterm-0.10.0-mac.zip"));
        assert_eq!(with_zip.zip_sha256.as_deref(), Some("def456"));

        let without_zip = Manifest::parse(
            r#"schema = 1
               version = "0.10.0"
               build_number = 1234
               sha256 = "abc123"
               dmg = "aterm-0.10.0.dmg""#,
        )
        .unwrap();
        assert_eq!(without_zip.zip, None);
        assert_eq!(without_zip.zip_sha256, None);
        assert_eq!(without_zip.dmg, "aterm-0.10.0.dmg");
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
            machine_id: Some("m3".into()),
            roster_seq: Some(2),
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
            machine_id: None,
            roster_seq: None,
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

    /// This is the live parser used by the updater scan (now delegating to the
    /// shared aterm-update-core validator). Keep an explicit test here so a
    /// future refactor cannot accidentally leave the client path permissive.
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
        assert_eq!(Floor::read(&p).roster_seq, 0);
        // Bump up.
        Floor::bump_and_write(&p, 5, 12, 3);
        let f = Floor::read(&p);
        assert_eq!((f.min_build, f.high_water, f.roster_seq), (5, 12, 3));
        // Lower observations must NOT lower the floor (monotonic). The roster sequence
        // ratchets on exactly the same rule — that is what makes a replayed
        // pre-revocation roster permanently unusable once a newer one has been seen.
        Floor::bump_and_write(&p, 3, 8, 2);
        let f = Floor::read(&p);
        assert_eq!((f.min_build, f.high_water, f.roster_seq), (5, 12, 3));
        // Higher observations raise each independently.
        Floor::bump_and_write(&p, 9, 12, 3);
        let f = Floor::read(&p);
        assert_eq!((f.min_build, f.high_water), (9, 12));
        Floor::bump_and_write(&p, 0, 0, 7);
        assert_eq!(Floor::read(&p).roster_seq, 7);
        let _ = std::fs::remove_file(&p);
    }

    /// THE LEAK AND THE SILENCE, TOGETHER. The ratchet removed its temp file only when
    /// the WRITE failed, so a failed RENAME left `floor.toml.<pid>.tmp` in the Updates
    /// root forever — and it returned `()` either way, so a frozen floor (replayed
    /// roster generations and yanked builds accepted again on every single check) was
    /// indistinguishable from a healthy one to every observer the machine has.
    ///
    /// MUTATION: move the `remove_file` back into a write-failure-only arm, or discard
    /// the commit error, and one of the two assertions below fails.
    #[test]
    fn a_floor_that_cannot_be_committed_is_reported_and_leaves_no_temp_file() {
        let root =
            std::env::temp_dir().join(format!("aterm-floor-commit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        // A DIRECTORY where the floor file belongs: the temp write succeeds and the
        // rename over it cannot, which is exactly the arm that used to leak.
        let path = root.join("floor.toml");
        std::fs::create_dir(&path).unwrap();
        let tmp = path.with_extension(format!("toml.{}.tmp", std::process::id()));

        let error = Floor::bump_and_write_reporting(&path, 5, 12, 3)
            .expect_err("a floor that cannot be committed must not report success");
        assert!(
            error.contains("roster_seq 3"),
            "the report must name the advance that was lost: {error}"
        );
        assert!(
            !tmp.exists(),
            "the temp file must never outlive a failed commit"
        );
        // The consequence the report exists to explain: the floor is still all-zero, so
        // the same roster generation is accepted again on the next check.
        assert_eq!(Floor::read(&path), Floor::default());

        // NEGATIVE CONTROL: on an ordinary path the same advance commits, reports
        // nothing, and also leaves no temp file behind.
        let ok_path = root.join("ok.toml");
        assert_eq!(Floor::bump_and_write_reporting(&ok_path, 5, 12, 3), Ok(()));
        assert_eq!(Floor::read(&ok_path).roster_seq, 3);
        let ok_tmp = ok_path.with_extension(format!("toml.{}.tmp", std::process::id()));
        assert!(!ok_tmp.exists(), "a successful commit leaves no temp file");
        let _ = std::fs::remove_dir_all(&root);
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

    /// The floor used to fail in the worst possible way: silently, AND leaking. A
    /// commit that cannot land (full disk, read-only remount, a floor file this uid
    /// may not replace) freezes the replay/rollback ratchet while status still reads
    /// healthy, and the old code's `remove_file` sat only in the write-failure arm, so
    /// each failing cycle also dropped one `floor.toml.<pid>.tmp` into the 0700
    /// Updates root that nothing ever collects.
    #[test]
    fn an_uncommittable_floor_is_reported_and_sweeps_its_own_temp_file() {
        let root =
            std::env::temp_dir().join(format!("aterm-floor-uncommittable-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        // A directory standing where the floor file belongs is the cheapest portable
        // way to make the RENAME fail while the temp write still succeeds — i.e. the
        // exact arm that used to leak.
        let path = root.join("floor.toml");
        std::fs::create_dir(&path).unwrap();

        let error = Floor::bump_and_write_reporting(&path, 5, 12, 3)
            .expect_err("a floor cannot be committed over a directory");
        assert!(
            error.contains("commit"),
            "the reported error must name the step that failed: {error}"
        );

        let leaked: Vec<String> = std::fs::read_dir(&root)
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".tmp"))
            .collect();
        assert!(
            leaked.is_empty(),
            "a failed commit must leave no temp behind: {leaked:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
