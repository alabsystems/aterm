// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Release-side appcast manifest (`aterm-appcast.toml`) and local stage-ready marker (`ready.toml`)
//! codecs.

use std::path::Path;

use aterm_durable::Flush;
use serde::{Deserialize, Serialize};

/// Write `text` to `path` atomically, finishing its temporary and parent directory with
/// `flush`. A refused file or directory flush is accepted. A genuine directory flush
/// failure after the rename is logged: the record is already in place, but its entry
/// may not survive a power loss.
///
/// # Errors
/// The temporary could not be written or flushed, or its rename failed. The destination
/// remains as it was; a directory flush failure after the rename is no error here.
pub(crate) fn write_durable(
    path: &Path,
    text: &str,
    what: &str,
    flush: Flush,
) -> Result<(), String> {
    use std::io::Write as _;
    let parent = match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir,
        _ => Path::new("."),
    };
    let tmp = path.with_extension(format!("toml.{}.tmp", std::process::id()));
    let write = (|| -> std::io::Result<()> {
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(text.as_bytes())?;
        aterm_durable::refusal_accepted(crate::durable::finish(&file, flush))
    })();
    if let Err(error) = write {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("write {what}: {error}"));
    }
    if let Err(error) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("commit {what}: {error}"));
    }
    if let Err(error) = flush.dir_best_effort(parent) {
        // The receipt's caller would roll back an exchange if this already-landed write
        // returned an error. Keep its result separate from the entry's durability.
        crate::warn(&format!(
            "{what} is in place at {}, but flushing its folder failed, so a power loss may \
             take it back: {error}",
            path.display()
        ));
    }
    Ok(())
}

/// The client's view of the release manifest (`aterm-appcast.toml`).
#[derive(Debug, Clone)]
pub struct Manifest {
    /// Manifest format version (the ship tool emits `schema = 1`).
    #[cfg(test)]
    pub schema: u32,
    /// Human semver, e.g. `"0.2.0"`.
    pub version: String,
    /// Monotonic build number, unique per published build.
    pub build_number: u64,
    /// Full git commit hash of the source the release was built from, binding the build number to
    /// an exact commit.
    pub commit: Option<String>,
    /// SHA-256 (lowercase hex) of the DMG asset.
    pub sha256: String,
    /// The DMG asset's file name within the release, e.g. `"aterm-0.2.0.dmg"`.
    pub dmg: String,
    /// The container's browser download URL as the publisher wrote it.
    pub url: Option<String>,
    /// The updater container's file name.
    pub zip: Option<String>,
    /// SHA-256 (lowercase hex) of the zip asset.
    pub zip_sha256: Option<String>,
    /// Optional operator **apply floor**.
    pub min_build: Option<u64>,
    /// Optional "what changed" notes.
    pub changelog: Option<String>,
    /// Attribution: which machine signed this release (`"m3"`).
    pub machine_id: Option<String>,
    /// The `roster_seq` that authorized [`Self::machine_id`].
    pub roster_seq: Option<u64>,
}

impl Manifest {
    /// Parse a manifest from TOML text via the shared `aterm-update-core` validator.
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
            url: m.url,
            zip: m.zip,
            zip_sha256: m.zip_sha256,
            min_build: m.min_build,
            changelog: m.changelog,
            machine_id: m.machine_id,
            roster_seq: m.roster_seq,
        })
    }
}

/// The local staging marker.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ready {
    /// Build number of the staged bundle (must exceed the running build to apply).
    pub build_number: u64,
    /// Human semver of the staged bundle (for log lines).
    pub version: String,
    /// Git commit of the staged bundle's source, copied from the manifest at stage time.
    #[serde(default)]
    pub commit: Option<String>,
    /// SHA-256 of the DMG the staged bundle came from (stage-time integrity record).
    pub dmg_sha256: String,
    /// Team ID recorded at stage time (re-checked against the signature at apply).
    pub team_id: String,
    /// RFC3339 UTC time the bundle was staged.
    pub staged_at: String,
    /// "What changed" notes copied from the manifest at stage time.
    #[serde(default)]
    pub changelog: Option<String>,
    /// Attribution recorded at stage time.
    #[serde(default)]
    pub machine_id: Option<String>,
    /// See [`Self::machine_id`].
    #[serde(default)]
    pub roster_seq: Option<u64>,
    /// RFC3339 UTC of when the last check that confirmed this stage's authorization read its
    /// machine roster ([`crate::install::confirm_roster_authorization`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub roster_confirmed_at: Option<String>,
}

/// How long a stage's roster confirmation ([`Ready::roster_confirmed_at`]) stays good for a swap.
pub(crate) const ROSTER_FRESHNESS_SECS: u64 = 24 * 60 * 60;

/// How old a confirmation may grow before the check re-reads the channel head's roster.
pub(crate) const ROSTER_RECONFIRM_AFTER_SECS: u64 = 60 * 60;

const ROSTER_CONFIRMATION_SKEW_SECS: u64 = 60;

/// The words every stale-confirmation refusal carries.
pub(crate) use crate::ROSTER_UNCONFIRMED_KEY;

impl Ready {
    /// Read + parse the marker, or `None` if absent or unparseable (non-UTF-8 included).
    pub fn read(path: &Path) -> Option<Self> {
        let text = crate::read_ledger_text(path)?;
        aterm_toml::from_str(&text).ok()
    }

    /// Serialize to TOML text.
    pub fn to_toml(&self) -> Result<String, String> {
        aterm_toml::to_string(self).map_err(|e| format!("serialize ready marker: {e}"))
    }

    /// Cheap structural identity gate shared by apply and status surfaces.
    pub(crate) fn has_canonical_identity(&self) -> bool {
        self.build_number != 0
            && self
                .commit
                .as_deref()
                .is_some_and(|commit| aterm_codec::hex::is_hex(commit.trim(), 40))
            && aterm_codec::hex::is_hex(self.dmg_sha256.trim(), 64)
    }

    /// Whether this marker names a genuinely published local stage.
    pub(crate) fn is_publishable(&self, staging: &crate::paths::Staging) -> bool {
        self.is_published_bundle(staging)
            && Floor::read_for_gate(&staging.floor()).is_ok_and(|floor| {
                floor
                    .withdraws(self.build_number, self.machine_id.as_deref())
                    .is_none()
            })
    }

    /// [`Self::is_publishable`] without the floor.
    pub(crate) fn is_published_bundle(&self, staging: &crate::paths::Staging) -> bool {
        self.names_published_bundle(staging)
            && artifact_quarantined_for_gate(staging, self.build_number, &self.dmg_sha256)
                == Ok(false)
    }

    /// [`Self::is_published_bundle`] without the records.
    pub(crate) fn names_published_bundle(&self, staging: &crate::paths::Staging) -> bool {
        self.has_canonical_identity()
            && std::fs::symlink_metadata(&staging.staged_app)
                .is_ok_and(|metadata| metadata.is_dir() && !metadata.file_type().is_symlink())
            && cheap_staged_identity_matches(self, &staging.staged_app) == Some(true)
    }

    /// Read only a marker that passes [`Self::is_publishable`].
    pub(crate) fn read_publishable(staging: &crate::paths::Staging) -> Option<Self> {
        Self::read(&staging.ready).filter(|ready| ready.is_publishable(staging))
    }

    /// Read a marker that passes [`Self::is_published_bundle`].
    pub(crate) fn read_published_bundle(staging: &crate::paths::Staging) -> Option<Self> {
        Self::read(&staging.ready).filter(|ready| ready.is_published_bundle(staging))
    }

    /// Seconds since [`Self::roster_confirmed_at`] at `now`.
    pub(crate) fn roster_confirmation_age(&self, now: u64) -> Option<u64> {
        let stamp = self.roster_confirmed_at.as_deref()?;
        let confirmed = u64::try_from(aterm_types::rfc3339::parse_utc(stamp)?).ok()?;
        if confirmed > now.saturating_add(ROSTER_CONFIRMATION_SKEW_SECS) {
            return None;
        }
        Some(now.saturating_sub(confirmed))
    }

    /// Why no lane may swap this stage in at `now`.
    pub(crate) fn roster_authorization_stale(&self, now: u64, armed: bool) -> Option<String> {
        if !armed {
            return None;
        }
        let build = self.build_number;
        let since = match self.roster_confirmation_age(now) {
            Some(age) if age < ROSTER_FRESHNESS_SECS => return None,
            Some(_) => format!(
                "since {}",
                self.roster_confirmed_at.as_deref().unwrap_or("?")
            ),
            None if self.roster_confirmed_at.is_some() => {
                format!(
                    "since {} (a time ahead of this Mac's clock)",
                    self.roster_confirmed_at.as_deref().unwrap_or("?")
                )
            }
            None => "on this Mac".to_string(),
        };
        Some(crate::verify::mark_passing(format!(
            "staged build {build} has not been {ROSTER_UNCONFIRMED_KEY} {since}, so a \
             revocation of its signer could have been missed; it installs once the next \
             update check confirms it"
        )))
    }

    /// Whether a check should re-read the channel head's machine roster for this stage.
    pub(crate) fn needs_roster_reconfirmation(&self, now: u64, armed: bool) -> bool {
        armed
            && self
                .roster_confirmation_age(now)
                .is_none_or(|age| age >= ROSTER_RECONFIRM_AFTER_SECS)
    }
}

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

/// Extract the string immediately following one exact XML plist key.
pub(crate) fn xml_plist_string<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    let after_key = text.find(key)? + key.len();
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
            && aterm_codec::hex::is_hex(git_commit, 40)
            && aterm_codec::hex::is_hex(dmg_sha256, 64))
        .then(|| Self {
            build_number,
            git_commit: git_commit.to_ascii_lowercase(),
            dmg_sha256: dmg_sha256.to_ascii_lowercase(),
        })
    }

    /// Read a well-formed receipt.
    pub(crate) fn read(path: &Path) -> Option<Self> {
        let parsed: Self = aterm_toml::from_str(&crate::read_ledger_text(path)?).ok()?;
        Self::canonical(parsed.build_number, &parsed.git_commit, &parsed.dmg_sha256)
    }

    /// Atomically replace the prior installed receipt after a successful swap.
    pub(crate) fn record(
        path: &Path,
        build_number: u64,
        git_commit: &str,
        dmg_sha256: &str,
        flush: Flush,
    ) -> Result<(), String> {
        Self::write_atomic(path, build_number, git_commit, dmg_sha256, flush)
    }

    fn write_atomic(
        path: &Path,
        build_number: u64,
        git_commit: &str,
        dmg_sha256: &str,
        flush: Flush,
    ) -> Result<(), String> {
        let receipt = Self::canonical(build_number, git_commit, dmg_sha256)
            .ok_or_else(|| "installed receipt identity is malformed".to_string())?;
        let text = aterm_toml::to_string(&receipt)
            .map_err(|error| format!("serialize installed receipt: {error}"))?;
        write_durable(path, &text, "installed receipt", flush)
    }

    /// Re-record a previously parsed receipt verbatim.
    pub(crate) fn record_preserving_kind(&self, path: &Path, flush: Flush) -> Result<(), String> {
        Self::write_atomic(
            path,
            self.build_number,
            &self.git_commit,
            &self.dmg_sha256,
            flush,
        )
    }

    /// Check whether this receipt matches the staged bundle's sealed build number and commit.
    pub(crate) fn matches_sealed(&self, build_number: u64, git_commit: &str) -> bool {
        self.build_number == build_number && crate::commit_matches(&self.git_commit, git_commit)
    }

    pub(crate) fn clear(path: &Path) {
        let _ = std::fs::remove_file(path);
    }
}

/// A persisted recency floor kept under `Updates/floor.toml`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Floor {
    /// Operator apply floor (max `min_build` ever seen from a machine still trusted).
    #[serde(default)]
    pub min_build: u64,
    /// Highest build ever successfully staged by this client (from a machine still trusted).
    #[serde(default)]
    pub high_water: u64,
    /// Highest `roster_seq` from any machine roster this client has ever accepted.
    #[serde(default)]
    pub roster_seq: u64,
    /// Who raised [`Self::min_build`].
    #[serde(default, skip_serializing_if = "FloorSources::is_empty")]
    pub min_build_sources: FloorSources,
    /// Who raised [`Self::high_water`].
    #[serde(default, skip_serializing_if = "FloorSources::is_empty")]
    pub high_water_sources: FloorSources,
    /// Every machine id an admitted, master-verified roster has revoked.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub revoked_machines: Vec<String>,
}

pub use crate::floor_sources::FloorSources;

/// One observation the floor is told about ([`Floor::observe_and_write`]).
#[derive(Debug, Clone, Copy, Default)]
pub struct FloorObservation<'a> {
    /// An observed manifest's `min_build` (0: none).
    pub min_build: u64,
    /// A build just staged (0: none).
    pub high_water: u64,
    /// An admitted roster's generation (0: none).
    pub roster_seq: u64,
    /// The machine whose signed manifest carried `min_build` / `high_water`.
    pub machine: Option<&'a str>,
    /// The machine ids an admitted roster revokes.
    pub revoked: &'a [String],
}

fn records_unreadable(error: String) -> String {
    crate::verify::mark_passing(format!(
        "aterm can't read its update records ({error}), so no update installs until it \
         can — if this lasts, make that file readable again (it should be yours) or \
         remove it"
    ))
}

impl Floor {
    /// Read the floor FOR display, defaulting to all-zero (no floor) when absent.
    pub fn read(path: &Path) -> Self {
        crate::read_ledger_text(path)
            .and_then(|t| aterm_toml::from_str(&t).ok())
            .unwrap_or_default()
    }

    fn read_for_replace(path: &Path) -> Result<Self, String> {
        let text = crate::read_ledger_text_checked(path)
            .map_err(|error| format!("read {}: {error}", path.display()))?;
        Ok(text
            .and_then(|t| aterm_toml::from_str(&t).ok())
            .unwrap_or_default())
    }

    /// [`Self::read`] for a gate that admits a build.
    pub(crate) fn read_for_gate(path: &Path) -> Result<Self, String> {
        Self::read_for_replace(path).map_err(records_unreadable)
    }

    /// Whether `machine` has been revoked by a roster this client admitted.
    #[must_use]
    pub fn revokes(&self, machine: Option<&str>) -> bool {
        machine.is_some_and(|id| self.revoked_machines.iter().any(|r| r == id))
    }

    /// Why this floor withdraws a build (`build` signed by `machine`).
    #[must_use]
    pub fn withdraws(&self, build: u64, machine: Option<&str>) -> Option<String> {
        if self.revokes(machine) {
            return Some(format!(
                "build {build} was signed by machine {}, which the machine roster has revoked",
                machine.unwrap_or("?")
            ));
        }
        (build < self.min_build).then(|| {
            format!(
                "build {build} is below the operator floor {} (yanked)",
                self.min_build
            )
        })
    }

    fn canonical(mut self) -> Self {
        (self.min_build, self.min_build_sources) =
            self.min_build_sources.step(self.min_build, &[], 0, None);
        (self.high_water, self.high_water_sources) =
            self.high_water_sources.step(self.high_water, &[], 0, None);
        self
    }

    /// [`Self::observe_and_write`] with no machine and no revocations.
    #[cfg(test)]
    pub fn bump_and_write(
        path: &Path,
        seen_min_build: u64,
        staged_build: u64,
        seen_roster_seq: u64,
    ) {
        Self::observe_and_write(
            path,
            &FloorObservation {
                min_build: seen_min_build,
                high_water: staged_build,
                roster_seq: seen_roster_seq,
                ..FloorObservation::default()
            },
        );
    }

    /// Persist atomically, raising each field monotonically to at least `observation`'s values.
    pub fn observe_and_write(path: &Path, observation: &FloorObservation<'_>) {
        if let Err(error) = Self::observe_reporting(path, observation) {
            crate::warn(&format!(
                "{error} — replay/rollback protection stays frozen at the values \
                 already on disk"
            ));
        }
    }

    /// [`Self::bump_and_write`] that returns the failure instead of warning.
    #[cfg(test)]
    fn bump_and_write_reporting(
        path: &Path,
        seen_min_build: u64,
        staged_build: u64,
        seen_roster_seq: u64,
    ) -> Result<(), String> {
        Self::observe_reporting(
            path,
            &FloorObservation {
                min_build: seen_min_build,
                high_water: staged_build,
                roster_seq: seen_roster_seq,
                ..FloorObservation::default()
            },
        )
    }

    fn observe_reporting(path: &Path, observation: &FloorObservation<'_>) -> Result<(), String> {
        Self::commit_observation(path, observation).map_err(|error| {
            format!(
                "could not raise the update floor at {} to (min_build {}, high_water {}, \
                 roster_seq {}): {error}",
                path.display(),
                observation.min_build,
                observation.high_water,
                observation.roster_seq
            )
        })
    }

    fn after(cur: Self, observation: &FloorObservation<'_>) -> Self {
        let mut next = cur;
        for id in observation.revoked {
            if !next.revoked_machines.iter().any(|known| known == id) {
                next.revoked_machines.push(id.clone());
            }
        }
        (next.min_build, next.min_build_sources) = next.min_build_sources.step(
            next.min_build,
            &next.revoked_machines,
            observation.min_build,
            observation.machine,
        );
        (next.high_water, next.high_water_sources) = next.high_water_sources.step(
            next.high_water,
            &next.revoked_machines,
            observation.high_water,
            observation.machine,
        );
        next.roster_seq = next.roster_seq.max(observation.roster_seq);
        next
    }

    fn commit_observation(path: &Path, observation: &FloorObservation<'_>) -> Result<(), String> {
        let lock_path = path.with_extension("toml.lock");
        let _lock = aterm_update_core::FileLock::acquire_within(
            &lock_path,
            crate::install::BACKGROUND_LOCK_WAIT,
        )
        .map_err(|error| format!("lock {}: {error}", lock_path.display()))?;
        let cur = Self::read_for_replace(path)?.canonical();
        let next = Self::after(cur.clone(), observation);
        if next == cur {
            return Ok(());
        }
        let text =
            aterm_toml::to_string(&next).map_err(|error| format!("encode floor: {error}"))?;
        write_durable(path, &text, "update floor", Flush::Durable)
    }
}

/// A memo of the last candidate build that was downloaded but failed to stage.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FailedMark {
    #[serde(default)]
    pub build_number: u64,
    #[serde(default)]
    pub sha256: String,
    /// How many times this exact `(build_number, sha256)` has failed.
    #[serde(default)]
    pub attempts: u32,
    /// Unix seconds before which this candidate is skipped.
    #[serde(default)]
    pub retry_after: u64,
    /// Quarantine: this artifact was swapped in, failed to confirm boot health
    /// `MAX_BOOT_ATTEMPTS` times, and was reverted.
    #[serde(default)]
    pub quarantined: bool,
    /// Trial markers only: the install root (`…/aterm.app`) the armed trial belongs to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub install_root: Option<String>,
    /// The latest failure booked here was a moment.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub moment: bool,
}

/// The widening retry schedule for a candidate that failed to stage, in seconds.
pub const RETRY_BACKOFF_SECS: [u64; 4] = [15 * 60, 60 * 60, 4 * 60 * 60, 24 * 60 * 60];

/// The retry schedule for a candidate whose staging could not finish (a moment).
pub const MOMENT_BACKOFF_SECS: [u64; 4] = [0, 15 * 60, 60 * 60, 4 * 60 * 60];

impl FailedMark {
    /// Read the memo, or `None` if absent/unparseable.
    pub fn read(path: &Path) -> Option<Self> {
        Self::parse(&crate::read_ledger_text(path)?)
    }

    fn read_for_replace(path: &Path) -> Result<Option<Self>, String> {
        let text = crate::read_ledger_text_checked(path)
            .map_err(|error| format!("read {}: {error}", path.display()))?;
        Ok(text.and_then(|text| Self::parse(&text)))
    }

    fn parse(text: &str) -> Option<Self> {
        let parsed: Self = aterm_toml::from_str(text).ok()?;
        (parsed.build_number != 0 || !parsed.sha256.is_empty()).then_some(parsed)
    }

    /// Whether this memo matches the given candidate (same build and same DMG hash).
    pub fn matches(&self, build_number: u64, sha256: &str) -> bool {
        self.build_number == build_number && self.sha256.eq_ignore_ascii_case(sha256)
    }

    /// Whether this memo should skip the given candidate right now.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NativeUpdateFailedMarkSuppression",
            action = "ProbeSuppresses",
            project = "aterm_update::failed_mark_suppression_projection"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "NativeUpdateFailedMarkSuppression",
            action = "PickSuppressionInputs",
            reason = "Bounded nondeterministic environment selection, not a shipping updater transition; Tier-1 exhaustively enumerates every projected input class before driving the record/probe pair."
        )
    )]
    #[must_use]
    pub fn suppresses(&self, build_number: u64, sha256: &str, now: u64) -> bool {
        self.matches(build_number, sha256) && (self.quarantined || now < self.retry_after)
    }

    /// Whether this memo is the never-lapsing crash-loop quarantine rather than a timed backoff.
    #[must_use]
    pub const fn is_quarantine(&self) -> bool {
        self.quarantined
    }

    /// Seconds until this memo stops suppressing, or 0 if it already has.
    #[must_use]
    pub const fn retry_in_secs(&self, now: u64) -> u64 {
        self.retry_after.saturating_sub(now)
    }

    /// The attempt count a fresh failure of `(build_number, sha256)` should be recorded with, given
    /// this memo as the prior state.
    #[must_use]
    pub fn next_attempt(&self, build_number: u64, sha256: &str) -> u32 {
        if self.matches(build_number, sha256) {
            self.attempts.saturating_add(1).max(2)
        } else {
            1
        }
    }

    /// The backoff for the `n`-th consecutive failure (1-based).
    #[must_use]
    pub fn backoff_secs(attempts: u32) -> u64 {
        let idx = (attempts.max(1) as usize - 1).min(RETRY_BACKOFF_SECS.len() - 1);
        RETRY_BACKOFF_SECS[idx]
    }

    /// The backoff for the `n`-th consecutive failure (1-based) when it is a moment.
    #[must_use]
    pub fn moment_backoff_secs(attempts: u32) -> u64 {
        let idx = (attempts.max(1) as usize - 1).min(MOMENT_BACKOFF_SECS.len() - 1);
        MOMENT_BACKOFF_SECS[idx]
    }

    /// Record `(build_number, sha256)` as the in-flight trial artifact.
    #[cfg(test)]
    pub fn record(path: &Path, build_number: u64, sha256: &str) {
        let _ = Self::record_required(path, build_number, sha256, None);
    }

    /// Quarantine `(build_number, sha256)`.
    pub fn record_quarantine(path: &Path, build_number: u64, sha256: &str, flush: Flush) {
        let m = Self::quarantine_mark(build_number, sha256);
        let Ok(text) = aterm_toml::to_string(&m) else {
            return;
        };
        let _ = write_durable(path, &text, "artifact quarantine", flush);
    }

    /// The in-memory quarantine verdict [`Self::record_quarantine`] persists.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NativeUpdateFailedMarkSuppression",
            action = "RecordQuarantine",
            project = "aterm_update::failed_mark_suppression_projection"
        )
    )]
    #[must_use]
    pub(crate) fn quarantine_mark(build_number: u64, sha256: &str) -> Self {
        FailedMark {
            build_number,
            sha256: sha256.to_ascii_lowercase(),
            attempts: 0,
            retry_after: 0,
            quarantined: true,
            install_root: None,
            moment: false,
        }
    }

    /// Record a download/stage failure of `(build_number, sha256)` at `now` (unix seconds).
    pub fn record_stage_failure(path: &Path, build_number: u64, sha256: &str, now: u64) {
        Self::record_timed(path, build_number, sha256, now, false);
    }

    /// Record a staging moment of `(build_number, sha256)`.
    pub fn record_stage_moment(path: &Path, build_number: u64, sha256: &str, now: u64) -> Self {
        Self::record_timed(path, build_number, sha256, now, true)
    }

    fn record_timed(path: &Path, build_number: u64, sha256: &str, now: u64, moment: bool) -> Self {
        let prior = match Self::read_for_replace(path) {
            Ok(prior) => prior.unwrap_or_default(),
            Err(error) => {
                crate::warn(&format!(
                    "did not write the backoff memo of build {build_number} over a memo this \
                     could not read: {error}"
                ));
                return Self::default().stage_failure_mark(build_number, sha256, now, moment);
            }
        };
        let m = prior.stage_failure_mark(build_number, sha256, now, moment);
        if prior.is_quarantine() {
            let ledger = path.with_file_name("quarantine.toml");
            if let Err(error) =
                Quarantine::record(&ledger, prior.build_number, &prior.sha256, Flush::Ordered)
            {
                crate::warn(&format!(
                    "kept the quarantine of build {} in place of a backoff memo: {error}",
                    prior.build_number
                ));
                return m;
            }
        }
        let Ok(text) = aterm_toml::to_string(&m) else {
            return m;
        };
        let tmp = path.with_extension(format!("toml.{}.tmp", std::process::id()));
        if std::fs::write(&tmp, text).is_ok() {
            let _ = std::fs::rename(&tmp, path);
        } else {
            let _ = std::fs::remove_file(&tmp);
        }
        m
    }

    /// The in-memory backoff memo [`Self::record_stage_failure`] persists.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NativeUpdateFailedMarkSuppression",
            action = "RecordStageFailure",
            project = "aterm_update::failed_mark_suppression_projection"
        )
    )]
    #[must_use]
    pub(crate) fn stage_failure_mark(
        &self,
        build_number: u64,
        sha256: &str,
        now: u64,
        moment: bool,
    ) -> Self {
        let attempts = self.next_attempt(build_number, sha256);
        let backoff = if moment {
            Self::moment_backoff_secs(attempts)
        } else {
            Self::backoff_secs(attempts)
        };
        FailedMark {
            build_number,
            sha256: sha256.to_ascii_lowercase(),
            attempts,
            retry_after: now.saturating_add(backoff),
            quarantined: false,
            install_root: None,
            moment,
        }
    }

    /// Atomically record `(build_number, sha256)`, reporting persistence failure.
    pub(crate) fn record_required(
        path: &Path,
        build_number: u64,
        sha256: &str,
        install_root: Option<&Path>,
    ) -> Result<(), String> {
        let m = FailedMark {
            build_number,
            sha256: sha256.to_ascii_lowercase(),
            attempts: 0,
            retry_after: 0,
            quarantined: false,
            install_root: install_root.map(|root| root.to_string_lossy().into_owned()),
            moment: false,
        };
        let text = aterm_toml::to_string(&m)
            .map_err(|error| format!("serialize artifact marker: {error}"))?;
        let flush = match (install_root, path.parent()) {
            (Some(root), Some(dir)) => Flush::ordered_if_same_device(dir, root),
            _ => Flush::Durable,
        };
        write_durable(path, &text, "artifact marker", flush)
    }

    /// Clear the memo (called once a stage finally succeeds).
    pub fn clear(path: &Path) {
        let _ = std::fs::remove_file(path);
    }

    /// Clear the download/stage backoff memo once a stage succeeds.
    pub fn clear_backoff(path: &Path) {
        match Self::read_for_replace(path) {
            Ok(Some(mark)) if mark.is_quarantine() => {}
            Ok(_) => Self::clear(path),
            Err(_) => {}
        }
    }
}

/// The crash-loop quarantine ledger (`quarantine.toml`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Quarantine {
    /// `"<build_number>:<sha256>"`, lowercase digest, oldest first.
    #[serde(default)]
    pub artifacts: Vec<String>,
}

impl Quarantine {
    /// How many verdicts the ledger keeps.
    pub const KEEP: usize = 64;

    fn entry(build_number: u64, sha256: &str) -> String {
        format!("{build_number}:{}", sha256.to_ascii_lowercase())
    }

    /// The ledger, or an empty one when it is absent or unreadable.
    #[must_use]
    pub fn read(path: &Path) -> Self {
        crate::read_ledger_text(path)
            .and_then(|text| aterm_toml::from_str(&text).ok())
            .unwrap_or_default()
    }

    fn read_for_replace(path: &Path) -> Result<Self, String> {
        let text = crate::read_ledger_text_checked(path)
            .map_err(|error| format!("read {}: {error}", path.display()))?;
        Ok(text
            .and_then(|text| aterm_toml::from_str(&text).ok())
            .unwrap_or_default())
    }

    /// Whether `(build_number, sha256)` is quarantined here.
    #[must_use]
    pub fn holds(&self, build_number: u64, sha256: &str) -> bool {
        let entry = Self::entry(build_number, sha256);
        self.artifacts.contains(&entry)
    }

    /// Whether any artifact of `build_number` is quarantined here.
    #[must_use]
    pub fn holds_build(&self, build_number: u64) -> bool {
        let prefix = format!("{build_number}:");
        self.artifacts.iter().any(|held| held.starts_with(&prefix))
    }

    /// Add `(build_number, sha256)`, durably.
    pub fn record(
        path: &Path,
        build_number: u64,
        sha256: &str,
        flush: Flush,
    ) -> Result<(), String> {
        let mut ledger = Self::read_for_replace(path)?;
        if ledger.holds(build_number, sha256) {
            return Ok(());
        }
        ledger.artifacts.push(Self::entry(build_number, sha256));
        let excess = ledger.artifacts.len().saturating_sub(Self::KEEP);
        ledger.artifacts.drain(..excess);
        let text = aterm_toml::to_string(&ledger)
            .map_err(|error| format!("serialize artifact quarantine: {error}"))?;
        write_durable(path, &text, "artifact quarantine ledger", flush)
    }
}

/// Quarantine `(build_number, sha256)` on this machine (the crash-loop revert's verdict).
pub fn quarantine_artifact(
    staging: &crate::paths::Staging,
    build_number: u64,
    sha256: &str,
    flush: Flush,
) {
    if let Err(error) = Quarantine::record(&staging.quarantine(), build_number, sha256, flush) {
        crate::warn(&format!(
            "could not record the quarantine of build {build_number}: {error}"
        ));
    }
    FailedMark::record_quarantine(&staging.failed(), build_number, sha256, flush);
}

/// Whether `(build_number, sha256)` is quarantined on this machine.
#[must_use]
pub fn artifact_quarantined(
    staging: &crate::paths::Staging,
    build_number: u64,
    sha256: &str,
) -> bool {
    Quarantine::read(&staging.quarantine()).holds(build_number, sha256)
        || FailedMark::read(&staging.failed())
            .is_some_and(|mark| mark.is_quarantine() && mark.matches(build_number, sha256))
}

/// [`artifact_quarantined`] for a gate that admits a build.
pub(crate) fn artifact_quarantined_for_gate(
    staging: &crate::paths::Staging,
    build_number: u64,
    sha256: &str,
) -> Result<bool, String> {
    if Quarantine::read_for_replace(&staging.quarantine())
        .map_err(records_unreadable)?
        .holds(build_number, sha256)
    {
        return Ok(true);
    }
    Ok(FailedMark::read_for_replace(&staging.failed())
        .map_err(records_unreadable)?
        .is_some_and(|mark| mark.is_quarantine() && mark.matches(build_number, sha256)))
}

/// One bounded machine state as both spec tiers exchange it — variable name to bounded value.
#[cfg(any(test, debug_assertions))]
type SpecState = std::collections::BTreeMap<&'static str, i64>;

/// Project one observed writer→reader round trip of [`FailedMark`] onto the bounded
/// `NativeUpdateFailedMarkSuppression` machine.
#[cfg(any(test, debug_assertions))]
#[doc(hidden)]
#[must_use]
pub fn failed_mark_suppression_projection(
    quarantine_writer: bool,
    probe_now_class: i64,
    candidate_matches: bool,
    recorded_quarantined: bool,
    recorded_deadline_class: i64,
    suppressed: bool,
) -> (SpecState, SpecState, SpecState) {
    let picked = std::collections::BTreeMap::from([
        ("phase", 1),
        ("writer_kind", if quarantine_writer { 1 } else { 2 }),
        ("probe_now", probe_now_class),
        ("candidate_matches", i64::from(candidate_matches)),
        ("quarantined", 0),
        ("deadline", 0),
        ("suppressed", 0),
    ]);
    let mut recorded = picked.clone();
    recorded.insert("phase", 2);
    recorded.insert("quarantined", i64::from(recorded_quarantined));
    recorded.insert("deadline", recorded_deadline_class);
    let mut probed = recorded.clone();
    probed.insert("phase", 3);
    probed.insert("suppressed", i64::from(suppressed));
    (picked, recorded, probed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_record_keeps_its_landed_result_and_reports_a_failed_directory_flush() {
        use aterm_durable::seam::FailingDirs;
        let staging = crate::paths::Staging::scratch("record-directory-flush");
        let path = staging.installed_receipt();
        let commit = "c".repeat(40);
        let digest = "ab".repeat(32);
        let warnings = || {
            crate::log_capture::take()
                .into_iter()
                .filter(|(level, _)| *level == aterm_log::Level::Warn)
                .count()
        };
        let _ = crate::log_capture::take();
        for flush in [Flush::Durable, Flush::Ordered] {
            {
                let _failure = FailingDirs::with(libc::EIO);
                InstalledReceipt::record(&path, 3, &commit, &digest, flush).unwrap();
            }
            assert_eq!(InstalledReceipt::read(&path).unwrap().build_number, 3);
            assert_eq!(warnings(), 1);
            {
                let _refused = FailingDirs::with(libc::ENOTSUP);
                InstalledReceipt::record(&path, 4, &commit, &digest, flush).unwrap();
            }
            assert_eq!(InstalledReceipt::read(&path).unwrap().build_number, 4);
            assert_eq!(warnings(), 0);
        }
    }

    #[test]
    fn a_failed_record_rename_keeps_the_destination_and_is_still_an_error() {
        use aterm_durable::seam::FailingDirs;
        let staging = crate::paths::Staging::scratch("record-rename-failure");
        let path = staging.root.join("occupied.toml");
        std::fs::create_dir(&path).unwrap();
        std::fs::write(path.join("witness"), b"keep").unwrap();
        let _ = crate::log_capture::take();
        let _refused = FailingDirs::with(libc::ENOTSUP);
        assert!(write_durable(&path, "new", "test record", Flush::Ordered).is_err());
        assert_eq!(std::fs::read(path.join("witness")).unwrap(), b"keep");
        assert!(
            !path
                .with_extension(format!("toml.{}.tmp", std::process::id()))
                .exists()
        );
        assert!(crate::log_capture::take().is_empty());
    }

    #[test]
    fn each_record_write_names_its_flush() {
        use aterm_durable::seam::Recording;
        const O: Flush = Flush::Ordered;
        const D: Flush = Flush::Durable;
        let staging = crate::paths::Staging::scratch("flush-tiers");
        let digest = "ab".repeat(32);
        let recording = Recording::start();
        Floor::bump_and_write(&staging.floor(), 3, 0, 0);
        assert_eq!(
            recording.take(),
            [D, D],
            "the floor: its file and its entry"
        );
        InstalledReceipt::record(&staging.installed_receipt(), 3, &"c".repeat(40), &digest, O)
            .expect("receipt");
        assert_eq!(recording.take(), [O, O], "an ordered receipt");
        FailedMark::record_required(&staging.trial(), 3, &digest, Some(&staging.root))
            .expect("trial");
        assert_eq!(
            recording.take(),
            [O, O],
            "a trial beside a root on its device"
        );
        FailedMark::record_required(&staging.trial(), 3, &digest, None).expect("trial");
        assert_eq!(recording.take(), [D, D], "a trial with no root");
        quarantine_artifact(&staging, 3, &digest, O);
        assert_eq!(
            recording.take(),
            [O, O, O, O],
            "the ledger, then failed.toml"
        );
        FailedMark::record_quarantine(&staging.failed(), 5, &"ef".repeat(32), O);
        assert_eq!(recording.take(), [O, O], "an older build's verdict alone");
        FailedMark::record_stage_failure(&staging.failed(), 6, &"cd".repeat(32), 100);
        assert_eq!(
            recording.take(),
            [O, O],
            "the quarantine carried into the ledger; the memo itself is not flushed"
        );
    }

    #[test]
    fn a_timed_memo_never_erases_a_quarantine() {
        let staging = crate::paths::Staging::scratch("timed-keeps-quarantine");
        let (x, y) = ("aa".repeat(32), "bb".repeat(32));
        let failed = staging.failed();
        FailedMark::record_quarantine(&failed, 7, &x, aterm_durable::Flush::Durable);
        assert!(
            !Quarantine::read(&staging.quarantine()).holds(7, &x),
            "PRECONDITION"
        );
        for at in 0..5 {
            FailedMark::record_stage_moment(&failed, 8, &y, 1_000 + at);
        }
        FailedMark::record_stage_failure(&failed, 8, &y, 2_000);
        assert!(artifact_quarantined(&staging, 7, &x), "still withdrawn");
        assert!(
            FailedMark::read(&failed).is_some_and(|m| m.matches(8, &y)),
            "the new artifact's memo is recorded"
        );

        let fresh = crate::paths::Staging::scratch("timed-plain");
        FailedMark::record_stage_failure(&fresh.failed(), 7, &x, 1_000);
        FailedMark::record_stage_moment(&fresh.failed(), 8, &y, 2_000);
        assert!(!artifact_quarantined(&fresh, 7, &x));
    }

    #[test]
    fn a_record_the_writer_could_not_read_survives() {
        use std::os::unix::fs::PermissionsExt;
        let staging = crate::paths::Staging::scratch("unreadable-keeps-quarantine");
        let (x, y, z) = ("aa".repeat(32), "bb".repeat(32), "cc".repeat(32));
        FailedMark::record_quarantine(&staging.failed(), 7, &x, aterm_durable::Flush::Durable);
        Quarantine::record(&staging.quarantine(), 9, &z, aterm_durable::Flush::Durable).unwrap();
        let mode = |path: &Path, bits| {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(bits))
        };
        for locked in [staging.failed(), staging.quarantine()] {
            mode(&locked, 0o000).unwrap();
            if std::fs::File::open(&locked).is_ok() {
                aterm_uds::host::could_not_run(
                    "a mode-0 file opened anyway: root, or a file system that ignores modes",
                );
            }
            FailedMark::record_stage_moment(&staging.failed(), 8, &y, 1_000);
            FailedMark::record_stage_failure(&staging.failed(), 8, &y, 2_000);
            FailedMark::clear_backoff(&staging.failed());
            let _ = mode(&locked, 0o600);
            let at = locked.display();
            assert!(artifact_quarantined(&staging, 7, &x), "{at}");
            assert!(artifact_quarantined(&staging, 9, &z), "{at}");
        }
    }

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

        FailedMark::record_quarantine(&path, 42, &"ab".repeat(32), aterm_durable::Flush::Durable);
        let read = FailedMark::read(&path).expect("a real memo reads back");
        assert_eq!(read.build_number, 42);
        assert!(read.quarantined, "and keeps its verdict");
        let strays: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| name.contains(".tmp"))
            .collect();
        assert!(
            strays.is_empty(),
            "durable write leaves no scratch: {strays:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

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

        assert!(m.suppresses(BUILD, SHA, 1_000));
        assert!(m.suppresses(BUILD, SHA, 1_000 + RETRY_BACKOFF_SECS[0] - 1));
        assert!(!m.suppresses(BUILD, SHA, 1_000 + RETRY_BACKOFF_SECS[0]));

        assert!(!m.suppresses(BUILD + 1, SHA, 1_000));
        assert!(!m.suppresses(BUILD, &"a".repeat(64), 1_000));

        let _ = std::fs::remove_dir_all(&root);
    }

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
        FailedMark::record_stage_failure(&path, 7, SHA, 0);
        let m = FailedMark::read(&path).unwrap();
        assert_eq!(m.retry_after, *RETRY_BACKOFF_SECS.last().unwrap());

        FailedMark::record_stage_failure(&path, 8, SHA, 0);
        let m = FailedMark::read(&path).unwrap();
        assert_eq!(m.attempts, 1);
        assert_eq!(m.retry_after, RETRY_BACKOFF_SECS[0]);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_pre_budget_marker_parses_and_becomes_retryable() {
        let m: FailedMark = aterm_toml::from_str("build_number = 9\nsha256 = \"ff\"\n").unwrap();
        assert_eq!(m.attempts, 0);
        assert_eq!(m.retry_after, 0);
        assert!(m.matches(9, "ff"));
        assert!(
            !m.suppresses(9, "ff", 0),
            "old markers must not strand a machine"
        );
        assert_eq!(m.next_attempt(9, "ff"), 2);
        assert_eq!(m.next_attempt(10, "ff"), 1);
        assert!(!m.is_quarantine());
    }

    #[test]
    fn a_quarantine_survives_a_later_stage_failure_of_another_build() {
        const NOW: u64 = 1_000_000;
        let staging = crate::paths::Staging::scratch("quarantine-survives");
        let (x, sx) = (41, "ab".repeat(32));
        let (y, sy) = (42, "cd".repeat(32));
        quarantine_artifact(&staging, x, &sx, aterm_durable::Flush::Durable);
        assert!(artifact_quarantined(&staging, x, &sx));

        FailedMark::record_stage_failure(&staging.failed(), y, &sy, NOW);
        assert!(
            artifact_quarantined(&staging, x, &sx),
            "another build's backoff must not erase the crash-loop verdict"
        );
        FailedMark::clear_backoff(&staging.failed());
        assert!(
            artifact_quarantined(&staging, x, &sx),
            "nor may a later successful stage"
        );
        quarantine_artifact(&staging, y, &sy, aterm_durable::Flush::Durable);
        assert!(artifact_quarantined(&staging, x, &sx));
        assert!(artifact_quarantined(&staging, y, &sy));
        assert!(!artifact_quarantined(&staging, x, &"ef".repeat(32)));
        assert!(!artifact_quarantined(&staging, x + 2, &sx));
        let _ = std::fs::remove_dir_all(&staging.root);
    }

    #[test]
    fn a_quarantine_suppresses_forever_while_the_shape_it_used_to_take_does_not() {
        let root = std::env::temp_dir().join(format!("aterm-quarantine-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("failed.toml");
        let sha = "ab".repeat(32);

        FailedMark::record(&path, 42, &sha);
        let old = FailedMark::read(&path).expect("parses");
        assert!(old.matches(42, &sha), "it names the right artifact…");
        assert!(!old.is_quarantine());
        assert!(!old.suppresses(42, &sha, 0), "…and suppresses nothing");

        FailedMark::record_quarantine(&path, 42, &sha, aterm_durable::Flush::Durable);
        let q = FailedMark::read(&path).expect("parses");
        assert!(q.is_quarantine());
        assert!(q.suppresses(42, &sha, 0));
        assert!(
            q.suppresses(42, &sha, u64::MAX),
            "no clock lapses a quarantine"
        );
        assert!(q.suppresses(42, &sha.to_ascii_uppercase(), 0));

        assert!(!q.suppresses(43, &sha, 0));
        assert!(!q.suppresses(42, &"cd".repeat(32), 0));

        FailedMark::record_stage_failure(&path, 42, &sha, 100);
        let timed = FailedMark::read(&path).expect("parses");
        assert!(!timed.is_quarantine());
        assert!(timed.suppresses(42, &sha, 100));
        assert!(!timed.suppresses(42, &sha, 100 + RETRY_BACKOFF_SECS[3]));

        let _ = std::fs::remove_dir_all(&root);
    }

    fn timeline_class(now: u64, deadline: u64) -> i64 {
        if deadline == 0 {
            i64::try_from(now.min(3)).expect("bounded class fits i64")
        } else if now == 0 {
            0
        } else if now < deadline {
            1
        } else if now == deadline {
            2
        } else {
            3
        }
    }

    #[test]
    fn failed_mark_suppression_exhaustively_refines_the_model() {
        use std::collections::BTreeSet;

        let model = aterm_models_update::native_update_failed_mark_suppression_model();
        let reachable_inputs: BTreeSet<_> = model
            .successors("PickSuppressionInputs", &model.init_state())
            .into_iter()
            .collect();
        assert_eq!(
            reachable_inputs.len(),
            16,
            "2 writers × 4 timeline classes × 2 identity classes"
        );

        const BUILD: u64 = 42;
        let sha = "ab".repeat(32);
        let wrong_sha = "cd".repeat(32);
        let candidates = [
            (BUILD, sha.clone(), true),
            (BUILD + 1, sha.clone(), false),
            (BUILD, wrong_sha, false),
        ];

        let quarantine = FailedMark::quarantine_mark(BUILD, &sha);
        assert!(quarantine.quarantined);
        assert_eq!(
            quarantine.retry_after, 0,
            "the verdict must NOT ride the deadline field — that was the bug"
        );

        const NOW0: u64 = 1_000;
        let backoff = FailedMark::default().stage_failure_mark(BUILD, &sha, NOW0, false);
        assert_eq!(backoff.retry_after, NOW0 + RETRY_BACKOFF_SECS[0]);
        assert_eq!(backoff.attempts, 1);
        assert!(!backoff.quarantined && !backoff.moment);
        let first_moment = FailedMark::default().stage_failure_mark(BUILD, &sha, NOW0, true);
        assert_eq!(first_moment.retry_after, NOW0 + MOMENT_BACKOFF_SECS[0]);
        assert_eq!(first_moment.retry_after, NOW0, "one moment costs nothing");
        let second_moment = first_moment.stage_failure_mark(BUILD, &sha, NOW0, true);
        assert_eq!(second_moment.retry_after, NOW0 + MOMENT_BACKOFF_SECS[1]);
        assert_eq!(second_moment.attempts, 2);
        assert!(second_moment.moment && !second_moment.quarantined);

        let mut projected_inputs = BTreeSet::new();
        let mut calls = 0usize;
        for (mark, quarantine_writer) in [
            (&quarantine, true),
            (&backoff, false),
            (&first_moment, false),
            (&second_moment, false),
        ] {
            let deadline = mark.retry_after;
            let probes = if quarantine_writer {
                [0, 1, 2, u64::MAX]
            } else {
                [0, deadline - 1, deadline, u64::MAX]
            };
            for now in probes {
                for (cand_build, cand_sha, matches) in &candidates {
                    let suppressed = mark.suppresses(*cand_build, cand_sha, now);
                    if quarantine_writer {
                        assert_eq!(
                            suppressed, *matches,
                            "quarantine must suppress its artifact at every now (now={now})"
                        );
                    } else {
                        assert_eq!(
                            suppressed,
                            *matches && now < deadline,
                            "backoff must suppress iff now < deadline (now={now})"
                        );
                    }

                    let (picked, recorded, probed) = failed_mark_suppression_projection(
                        quarantine_writer,
                        timeline_class(now, mark.retry_after),
                        *matches,
                        mark.quarantined,
                        if mark.retry_after == 0 { 0 } else { 2 },
                        suppressed,
                    );
                    assert!(
                        reachable_inputs.contains(&picked),
                        "real input projection is unreachable: {picked:?}"
                    );
                    projected_inputs.insert(picked.clone());

                    let label = format!(
                        "failed-mark suppression quarantine={quarantine_writer} now={now} matches={matches}"
                    );
                    let writer_action = if quarantine_writer {
                        "RecordQuarantine"
                    } else {
                        "RecordStageFailure"
                    };
                    let (admitted, why) = aterm_spec::verify::validate_transition_tiered(
                        &model,
                        &[],
                        &picked,
                        &recorded,
                        Some(writer_action),
                        &label,
                    );
                    assert!(admitted, "real writer transition rejected: {why}");
                    let (admitted, why) = aterm_spec::verify::validate_transition_tiered(
                        &model,
                        &[],
                        &recorded,
                        &probed,
                        Some("ProbeSuppresses"),
                        &label,
                    );
                    assert!(admitted, "real reader transition rejected: {why}");
                    for invariant in &model.invariants {
                        assert!(
                            model.check_invariant(invariant.name, &recorded),
                            "real writer violated {}: {recorded:?}",
                            invariant.name
                        );
                        assert!(
                            model.check_invariant(invariant.name, &probed),
                            "real round trip violated {}: {probed:?}",
                            invariant.name
                        );
                    }
                    calls += 1;
                }
            }
        }

        assert_eq!(
            calls, 48,
            "the concrete decision lattice must stay exhaustive"
        );
        assert_eq!(
            projected_inputs, reachable_inputs,
            "Tier-1 must cover every bounded PickSuppressionInputs class"
        );
    }

    #[test]
    fn failed_mark_suppression_model_negative_controls_are_non_vacuous() {
        let model = aterm_models_update::native_update_failed_mark_suppression_model();
        let buggy = aterm_spec::interp::with_buggy(&model, 1);

        let legacy_poison = FailedMark {
            build_number: 42,
            sha256: "ab".repeat(32),
            attempts: 0,
            retry_after: 0,
            quarantined: false,
            install_root: None,
            moment: false,
        };
        let suppressed = legacy_poison.suppresses(42, &"ab".repeat(32), 0);
        assert!(
            !suppressed,
            "the regression shape: the poison does not hold"
        );
        let (picked, leaked, retried) = failed_mark_suppression_projection(
            true,
            0,
            true,
            legacy_poison.quarantined,
            0,
            suppressed,
        );
        let (admitted, why) = aterm_spec::verify::validate_transition_tiered(
            &model,
            &[],
            &picked,
            &leaked,
            Some("RecordQuarantine"),
            "failed-mark suppression writer negative control",
        );
        assert!(
            !admitted,
            "healthy model admitted the quarantine-as-timed-memo writer: {why}"
        );
        assert!(
            buggy
                .successors("RecordQuarantine", &picked)
                .contains(&leaked),
            "Buggy=1 must reproduce the 5ffcc15d writer"
        );
        assert!(!buggy.check_invariant("QuarantineVerdictLandsInTheQuarantineField", &leaked));
        assert!(
            buggy
                .successors("ProbeSuppresses", &leaked)
                .contains(&retried),
            "Buggy=1 must complete the written-then-ignored round trip"
        );
        assert!(!buggy.check_invariant("QuarantineSuppressesAtEveryProbe", &retried));

        let (picked, recorded, flipped) =
            failed_mark_suppression_projection(false, 3, true, false, 2, true);
        let (admitted, why) = aterm_spec::verify::validate_transition_tiered(
            &model,
            &[],
            &picked,
            &recorded,
            Some("RecordStageFailure"),
            "failed-mark suppression reader negative control (prefix)",
        );
        assert!(admitted, "the recorded prefix must be healthy: {why}");
        let (admitted, why) = aterm_spec::verify::validate_transition_tiered(
            &model,
            &[],
            &recorded,
            &flipped,
            Some("ProbeSuppresses"),
            "failed-mark suppression reader negative control",
        );
        assert!(
            !admitted,
            "healthy model admitted a memo suppressing past its deadline: {why}"
        );
        assert!(
            buggy
                .successors("ProbeSuppresses", &recorded)
                .contains(&flipped),
            "Buggy=1 must reproduce the flipped comparison"
        );
        assert!(!buggy.check_invariant("BackoffSuppressesIffBeforeDeadline", &flipped));
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

        InstalledReceipt::record(&path, 53, commit, &digest, aterm_durable::Flush::Durable)
            .unwrap();
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
            assert!(
                InstalledReceipt::record(
                    &path,
                    54,
                    bad_commit,
                    bad_digest,
                    aterm_durable::Flush::Durable
                )
                .is_err()
            );
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
                ..Floor::default()
            },
            "read/max/write is one locked transaction; no coordinate may regress"
        );
        let _ = std::fs::remove_dir_all(root);
    }

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
        assert_eq!(
            m.min_build, None,
            "an appcast without the field still parses"
        );
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
            roster_confirmed_at: Some(crate::install::now_rfc3339()),
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
    fn an_unreadable_floor_publishes_nothing() {
        use std::os::unix::fs::PermissionsExt;
        let staging = crate::paths::Staging::scratch("publishable-floor-unreadable");
        let commit = "0123456789abcdef0123456789abcdef01234567";
        let ready = Ready {
            build_number: 54,
            version: "0.54.0".into(),
            commit: Some(commit.into()),
            dmg_sha256: "ab".repeat(32),
            team_id: "T".into(),
            staged_at: String::new(),
            changelog: None,
            machine_id: Some("m11".into()),
            roster_seq: None,
            roster_confirmed_at: Some(crate::install::now_rfc3339()),
        };
        std::fs::write(&staging.ready, ready.to_toml().unwrap()).unwrap();
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
        std::fs::write(staging.floor(), "roster_seq = 3\n").unwrap();
        assert!(Ready::read_publishable(&staging).is_some(), "PRECONDITION");
        std::fs::write(
            staging.floor(),
            "roster_seq = 4\nrevoked_machines = [\"m11\"]\n",
        )
        .unwrap();
        assert!(Ready::read_publishable(&staging).is_none(), "revoked");

        let mode =
            |bits| std::fs::set_permissions(staging.floor(), std::fs::Permissions::from_mode(bits));
        mode(0o000).unwrap();
        if std::fs::File::open(staging.floor()).is_ok() {
            aterm_uds::host::could_not_run(
                "a mode-0 file opened anyway: root, or a file system that ignores modes",
            );
        }
        let read = Ready::read_publishable(&staging);
        mode(0o600).unwrap();
        assert!(read.is_none(), "an unreadable floor admitted {read:?}");
    }

    #[test]
    fn manifest_rejects_min_build_above_its_own_build() {
        Manifest::parse(
            r#"version = "1.0.0"
               build_number = 10
               sha256 = "x"
               dmg = "a.dmg"
               min_build = 11"#,
        )
        .unwrap_err();

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
        let dir = aterm_tempfile::Builder::new()
            .prefix("aterm-floor-")
            .tempdir()
            .unwrap();
        let p = dir.path().join("floor.toml");
        assert_eq!(Floor::read(&p).min_build, 0);
        assert_eq!(Floor::read(&p).high_water, 0);
        assert_eq!(Floor::read(&p).roster_seq, 0);
        Floor::bump_and_write(&p, 5, 12, 3);
        let f = Floor::read(&p);
        assert_eq!((f.min_build, f.high_water, f.roster_seq), (5, 12, 3));
        Floor::bump_and_write(&p, 3, 8, 2);
        let f = Floor::read(&p);
        assert_eq!((f.min_build, f.high_water, f.roster_seq), (5, 12, 3));
        Floor::bump_and_write(&p, 9, 12, 3);
        let f = Floor::read(&p);
        assert_eq!((f.min_build, f.high_water), (9, 12));
        Floor::bump_and_write(&p, 0, 0, 7);
        assert_eq!(Floor::read(&p).roster_seq, 7);
    }

    #[test]
    fn a_floor_without_provenance_is_never_lowered_and_an_old_reader_sees_the_numbers() {
        #[derive(Deserialize)]
        struct OldFloor {
            #[serde(default)]
            min_build: u64,
            #[serde(default)]
            high_water: u64,
            #[serde(default)]
            roster_seq: u64,
        }
        let root = std::env::temp_dir().join(format!("aterm-floor-mixed-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("floor.toml");
        let revoked = vec!["m11".to_string()];
        Floor::observe_and_write(
            &path,
            &FloorObservation {
                min_build: 9_000,
                high_water: 9_500,
                roster_seq: 9,
                machine: Some("m11"),
                revoked: &[],
            },
        );
        let old: OldFloor = aterm_toml::from_str(&std::fs::read_to_string(&path).unwrap())
            .expect("an older build parses the file this one wrote");
        assert_eq!(
            (old.min_build, old.high_water, old.roster_seq),
            (9_000, 9_500, 9)
        );

        std::fs::write(
            &path,
            "min_build = 9000\nhigh_water = 9500\nroster_seq = 9\n",
        )
        .unwrap();
        Floor::observe_and_write(
            &path,
            &FloorObservation {
                roster_seq: 10,
                revoked: &revoked,
                ..FloorObservation::default()
            },
        );
        let floor = Floor::read(&path);
        assert_eq!(
            (floor.min_build, floor.high_water, floor.roster_seq),
            (9_000, 9_500, 10),
            "an unattributed floor is never lowered: {floor:?}"
        );
        assert!(floor.revokes(Some("m11")) && !floor.revokes(None));

        let _ = std::fs::remove_file(&path);
        Floor::observe_and_write(
            &path,
            &FloorObservation {
                min_build: 9_000,
                high_water: 9_500,
                roster_seq: 9,
                machine: Some("m11"),
                revoked: &[],
            },
        );
        Floor::observe_and_write(
            &path,
            &FloorObservation {
                roster_seq: 10,
                revoked: &revoked,
                ..FloorObservation::default()
            },
        );
        let floor = Floor::read(&path);
        assert_eq!((floor.min_build, floor.high_water), (0, 0), "{floor:?}");
        assert_eq!(
            floor
                .withdraws(5, Some("m11"))
                .map(|why| why.contains("revoked")),
            Some(true)
        );
        assert_eq!(floor.withdraws(5, Some("m3")), None);
        let yank = Floor {
            min_build: 5,
            ..Floor::default()
        };
        assert_eq!(yank.withdraws(5, None), None);
        assert!(yank.withdraws(4, None).is_some());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_floor_that_cannot_be_committed_is_reported_and_leaves_no_temp_file() {
        let root = std::env::temp_dir().join(format!("aterm-floor-commit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("floor.toml");
        std::fs::create_dir(&path).unwrap();
        let tmp = path.with_extension(format!("toml.{}.tmp", std::process::id()));

        Floor::bump_and_write_reporting(&path, 5, 12, 3)
            .expect_err("a floor that cannot be committed must not report success");
        assert!(
            !tmp.exists(),
            "the temp file must never outlive a failed commit"
        );
        let leaked: Vec<String> = std::fs::read_dir(&root)
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".tmp"))
            .collect();
        assert!(leaked.is_empty(), "a failed commit left temps: {leaked:?}");
        assert_eq!(Floor::read(&path), Floor::default());

        let ok_path = root.join("ok.toml");
        assert_eq!(Floor::bump_and_write_reporting(&ok_path, 5, 12, 3), Ok(()));
        assert_eq!(Floor::read(&ok_path).roster_seq, 3);
        let ok_tmp = ok_path.with_extension(format!("toml.{}.tmp", std::process::id()));
        assert!(!ok_tmp.exists(), "a successful commit leaves no temp file");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_floor_the_write_could_not_read_is_never_replaced_with_defaults() {
        use std::os::unix::fs::PermissionsExt;

        let dir = aterm_tempfile::Builder::new()
            .prefix("aterm-floor-unreadable-")
            .tempdir()
            .unwrap();
        let recorded = "min_build = 500\nhigh_water = 600\nroster_seq = 10\n\
                        revoked_machines = [\"m11\"]\n";
        let observation = FloorObservation {
            high_water: 700,
            machine: Some("m3"),
            ..FloorObservation::default()
        };
        let keeps_the_record = |floor: &Floor| {
            floor.roster_seq == 10 && floor.min_build == 500 && floor.revokes(Some("m11"))
        };

        let path = dir.path().join("floor.toml");
        std::fs::write(&path, recorded).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
        let outcome = Floor::observe_reporting(&path, &observation);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let floor = Floor::read(&path);
        assert!(
            keeps_the_record(&floor),
            "an unreadable floor was replaced with defaults: {floor:?}"
        );
        assert!(
            outcome.is_err(),
            "the refused advance must be reported: {outcome:?}"
        );

        let ok_path = dir.path().join("ok.toml");
        std::fs::write(&ok_path, recorded).unwrap();
        assert_eq!(Floor::observe_reporting(&ok_path, &observation), Ok(()));
        let floor = Floor::read(&ok_path);
        assert!(keeps_the_record(&floor), "{floor:?}");
        assert_eq!(floor.high_water, 700);
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

    fn confirmed_ready(confirmed: Option<u64>) -> Ready {
        Ready {
            build_number: 20,
            version: "0.0.20".into(),
            commit: Some("0123456789abcdef0123456789abcdef01234567".into()),
            dmg_sha256: "ab".repeat(32),
            team_id: "T".into(),
            staged_at: String::new(),
            changelog: None,
            machine_id: Some("m11".into()),
            roster_seq: Some(9),
            roster_confirmed_at: confirmed.map(aterm_types::rfc3339::format_rfc3339),
        }
    }

    #[test]
    fn a_stage_is_swapped_only_inside_a_day_of_its_roster_confirmation() {
        let now = 1_790_000_000;
        let day = ROSTER_FRESHNESS_SECS;
        for (confirmed, stale) in [
            (Some(now), false),
            (Some(now - 3_600), false),
            (Some(now - day + 1), false),
            (Some(now + ROSTER_CONFIRMATION_SKEW_SECS), false),
            (Some(now - day), true),
            (Some(now - day - 3_600), true),
            (Some(now + ROSTER_CONFIRMATION_SKEW_SECS + 1), true),
            (None, true),
        ] {
            let ready = confirmed_ready(confirmed);
            let refusal = ready.roster_authorization_stale(now, true);
            assert_eq!(refusal.is_some(), stale, "{confirmed:?}: {refusal:?}");
            if let Some(refusal) = refusal {
                assert!(crate::is_passing_refusal(&refusal), "{refusal}");
                assert!(
                    crate::is_roster_confirmation_wait(&refusal),
                    "the window's lane files it as the wait, not a failure: {refusal}"
                );
                assert!(
                    refusal.contains(ROSTER_UNCONFIRMED_KEY) && refusal.contains("20"),
                    "{refusal}"
                );
                if let Some(stamp) = ready.roster_confirmed_at.as_deref() {
                    assert!(refusal.contains(stamp), "names when: {refusal}");
                }
            }
            assert_eq!(ready.roster_authorization_stale(now, false), None);
        }
        let mut malformed = confirmed_ready(Some(now));
        malformed.roster_confirmed_at = Some("2026-10-02 09:00:00".into());
        assert!(malformed.roster_authorization_stale(now, true).is_some());
        assert!(!crate::is_roster_confirmation_wait(
            &crate::verify::mark_passing("could not take the apply lock within 10s".to_string())
        ));
        assert!(!crate::is_roster_confirmation_wait(ROSTER_UNCONFIRMED_KEY));

        let reread = ROSTER_RECONFIRM_AFTER_SECS;
        assert!(!confirmed_ready(Some(now - reread + 1)).needs_roster_reconfirmation(now, true));
        assert!(confirmed_ready(Some(now - reread)).needs_roster_reconfirmation(now, true));
        assert!(confirmed_ready(None).needs_roster_reconfirmation(now, true));
        assert!(!confirmed_ready(None).needs_roster_reconfirmation(now, false));
        let oldest_at_quit =
            reread + crate::cadence::INTERVAL_SECS * (100 + crate::cadence::JITTER_PCT) / 100;
        assert!(
            oldest_at_quit <= 2 * 60 * 60,
            "a running client's stamp is about an hour old at most: {oldest_at_quit} s"
        );
        let quit = now - 13 * 3_600 - 1_800;
        assert_eq!(
            confirmed_ready(Some(quit - oldest_at_quit)).roster_authorization_stale(now, true),
            None,
            "an overnight quit does not make the morning's launch wait"
        );
        assert!(
            confirmed_ready(Some(now - day - 1))
                .roster_authorization_stale(now, true)
                .is_some(),
            "a day with no check does"
        );
    }

    #[test]
    fn the_roster_confirmation_is_one_additive_key_both_ways() {
        #[derive(serde::Deserialize)]
        #[allow(dead_code)]
        struct ReadyV098 {
            build_number: u64,
            version: String,
            #[serde(default)]
            commit: Option<String>,
            dmg_sha256: String,
            team_id: String,
            staged_at: String,
            #[serde(default)]
            changelog: Option<String>,
            #[serde(default)]
            machine_id: Option<String>,
            #[serde(default)]
            roster_seq: Option<u64>,
        }
        let confirmed = confirmed_ready(Some(1_790_000_000));
        let text = confirmed.to_toml().unwrap();
        assert!(text.contains("roster_confirmed_at"), "{text}");
        let old: ReadyV098 = aterm_toml::from_str(&text).expect("v0.98.0 reads the new marker");
        assert_eq!((old.build_number, old.roster_seq), (20, Some(9)));

        let unconfirmed = confirmed_ready(None);
        let text = unconfirmed.to_toml().unwrap();
        assert!(
            !text.contains("roster_confirmed_at"),
            "skipped while empty: {text}"
        );
        let back: Ready = aterm_toml::from_str(&text).unwrap();
        assert_eq!(back.roster_confirmed_at, None);
        assert_eq!(
            aterm_toml::from_str::<Ready>(&confirmed.to_toml().unwrap())
                .unwrap()
                .roster_confirmed_at,
            confirmed.roster_confirmed_at
        );
    }
}
