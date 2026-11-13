// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Release-side appcast manifest (`aterm-appcast.toml`) and local stage-ready marker (`ready.toml`)
//! codecs.

use std::path::Path;

use aterm_durable::Flush;
use serde::{Deserialize, Serialize};

#[cfg(test)]
std::thread_local! {
    static FAIL_NEXT_DURABLE_WRITE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
pub(crate) fn fail_next_durable_write() {
    FAIL_NEXT_DURABLE_WRITE.with(|fail| fail.set(true));
}

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
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let parent = match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir,
        _ => Path::new("."),
    };
    let tmp = path.with_extension(format!(
        "toml.{}-{}.tmp",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let write = (|| -> std::io::Result<()> {
        #[cfg(test)]
        if FAIL_NEXT_DURABLE_WRITE.with(|fail| fail.replace(false)) {
            return Err(std::io::Error::other("injected durable write failure"));
        }
        use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
            .open(&tmp)?;
        let meta = file.metadata()?;
        if !meta.is_file() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "refusing to write non-regular file",
            ));
        }
        if meta.len() != 0 {
            file.set_len(0)?;
        }
        if meta.permissions().mode() & 0o7777 != 0o600 {
            let _ = file.set_permissions(std::fs::Permissions::from_mode(0o600));
        }
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
            && crate::hold::held(
                staging,
                self.build_number,
                &self.dmg_sha256,
                crate::install::unix_now_secs(),
            )
            .is_none()
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
    let text = match crate::read_ledger_text_checked(&plist) {
        Ok(Some(text)) => text,
        Ok(None) => match std::fs::symlink_metadata(&plist) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
            _ => return Some(false),
        },
        Err(_) => return Some(false),
    };
    let Some(build) =
        xml_plist_string(&text, "<key>CFBundleVersion</key>").and_then(crate::parse_uint::<u64>)
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

pub(crate) fn records_unreadable(error: String) -> String {
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

    pub(crate) fn observe_reporting(
        path: &Path,
        observation: &FloorObservation<'_>,
    ) -> Result<(), String> {
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

/// The boot trial's artifact (`trial.toml`): which `(build_number, sha256)` the armed trial
/// swapped in, and the install root (`…/aterm.app`) it belongs to. Transaction state of one
/// trial, empty outside it; the verdict a trial ends in goes to the update record.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Trial {
    #[serde(default)]
    pub build_number: u64,
    #[serde(default)]
    pub sha256: String,
    /// The install root (`…/aterm.app`) the armed trial belongs to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub install_root: Option<String>,
}

impl Trial {
    /// Read the marker, or `None` if absent/unparseable.
    pub fn read(path: &Path) -> Option<Self> {
        let parsed: Self = aterm_toml::from_str(&crate::read_ledger_text(path)?).ok()?;
        (parsed.build_number != 0 || !parsed.sha256.is_empty()).then_some(parsed)
    }

    /// Whether this marker names the given artifact (same build and same DMG hash).
    pub fn matches(&self, build_number: u64, sha256: &str) -> bool {
        self.build_number == build_number && self.sha256.eq_ignore_ascii_case(sha256)
    }

    /// Record `(build_number, sha256)` as the in-flight trial artifact.
    #[cfg(test)]
    pub fn record(path: &Path, build_number: u64, sha256: &str) {
        let _ = Self::record_required(path, build_number, sha256, None);
    }

    /// Atomically record `(build_number, sha256)`, reporting persistence failure.
    pub(crate) fn record_required(
        path: &Path,
        build_number: u64,
        sha256: &str,
        install_root: Option<&Path>,
    ) -> Result<(), String> {
        let m = Self {
            build_number,
            sha256: sha256.to_ascii_lowercase(),
            install_root: install_root.map(|root| root.to_string_lossy().into_owned()),
        };
        let text = aterm_toml::to_string(&m)
            .map_err(|error| format!("serialize artifact marker: {error}"))?;
        let flush = match (install_root, path.parent()) {
            (Some(root), Some(dir)) => Flush::ordered_if_same_device(dir, root),
            _ => Flush::Durable,
        };
        write_durable(path, &text, "artifact marker", flush)
    }

    /// Clear the marker (the trial ended).
    pub fn clear(path: &Path) {
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(test)]
#[path = "manifest_tests.rs"]
mod tests;
