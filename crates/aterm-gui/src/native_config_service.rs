// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Serialized, versioned preference transaction core for native Settings.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::Path;
use std::sync::Arc;

use aterm_agent::supervise::config::OwnerKeys;

const UNDO_LIMIT: usize = 64;

/// Why a write this service admits (Settings, Manual, `ctl settings`, an undo of one) that
/// would move the owner's keys from the file's is refused ([`VersionedConfigService::patch`]).
const OWNER_KEYS_REFUSED: &str = "aterm.toml's [harness] occupancy keys are the owner's: \
     Settings never writes them; edit them in aterm.toml at the window yourself";

/// Admission budget shared by startup, watcher reloads and Manual's semantic analysis.
pub(crate) const MAX_CONFIG_FILE_BYTES: usize = atpkg::config::MAX_PACKAGES_CONFIG_BYTES;

#[derive(Clone, PartialEq)]
pub(crate) struct ConfigSnapshot {
    pub(crate) revision: u64,
    /// Environment generation Manual diagnostics were last requested against.
    pub(crate) analysis_generation: u64,
    pub(crate) text: Arc<str>,
    /// Parsed semantic projection produced before admission.
    pub(crate) config: Arc<crate::app_config::Config>,
    pub(crate) semantic_values: Arc<BTreeMap<String, String>>,
    /// The values of `text` the harness's `[disk]` reader does not read as written.
    pub(crate) disk_faults: DiskFaults,
    /// Immutable non-text assets admitted with `text` at `revision`, shared as one outer Arc.
    pub(crate) assets: Arc<crate::app_config::ConfigAssetCatalog>,
}

impl std::fmt::Debug for ConfigSnapshot {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ConfigSnapshot")
            .field("revision", &self.revision)
            .field("analysis_generation", &self.analysis_generation)
            .field("text_len", &self.text.len())
            .field("semantic_value_count", &self.semantic_values.len())
            .field("disk_fault_count", &self.disk_faults.len())
            .field("assets", &self.assets)
            .finish_non_exhaustive()
    }
}

/// What the harness's `[disk]` reader does not read as written in one text.
pub(crate) type DiskFaults = Arc<[aterm_agent::harness::cli::DiskFault]>;

fn parse_disk_faults(text: &str) -> DiskFaults {
    aterm_agent::harness::cli::disk_faults(text).into()
}

impl ConfigSnapshot {
    /// Semantic values (every key and dotted table path) the service compares for OCC.
    pub(crate) fn values(&self) -> Result<BTreeMap<String, String>, String> {
        Ok((*self.semantic_values).clone())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ExpectedValue {
    /// No expectation beyond the patch's base revision.
    #[cfg(test)]
    Any,
    Exact(Option<String>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ConfigKeyEdit {
    pub(crate) key: String,
    pub(crate) expected: ExpectedValue,
    /// Raw semantic value.
    pub(crate) value: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ConfigPatchRequest {
    pub(crate) base_revision: u64,
    pub(crate) edits: Vec<ConfigKeyEdit>,
}

/// Immutable handoff from the optimistic reducer to the sole durable config worker.
#[derive(Clone, Debug)]
pub(crate) struct ConfigPersistencePlan {
    pub(crate) snapshot: ConfigSnapshot,
    pub(crate) expected_text: Arc<str>,
    pub(crate) logical_path: Option<std::path::PathBuf>,
    pub(crate) baseline: Option<crate::native_document_host::AtomicFileBaseline>,
}

/// One stable UTF-8 disk read plus the logical-target/file generation it came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ConfigDiskObservation {
    pub(crate) text: String,
    pub(crate) baseline: crate::native_document_host::AtomicFileBaseline,
}

/// The record that the one-time cursor migration ([`crate::prefs::migrate_cursor_settings`])
/// ran for one aterm.toml: a file in aterm's data folder named by the config file's path.
/// It is state, not a reading of the config's content: after it exists the file is never
/// migrated again, so a retired name typed into it later is refused and not rewritten.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CursorMigrationStamp {
    /// `None` where aterm has no data folder: nothing is recorded, and each read migrates.
    path: Option<std::path::PathBuf>,
}

impl CursorMigrationStamp {
    /// The stamp of the aterm.toml at `config`.
    pub(crate) fn for_config(config: &Path) -> Self {
        // FNV-1a over the path's bytes: a name that is the same in every build.
        let mut hash = 0xcbf2_9ce4_8422_2325_u64;
        for byte in config.as_os_str().as_encoded_bytes() {
            hash = (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3);
        }
        Self {
            path: aterm_types::dirs::aterm_data_dir().map(|dir| {
                dir.join("config-migrations")
                    .join(format!("cursor-settings-{hash:016x}"))
            }),
        }
    }

    /// A stamp kept at `path`.
    #[cfg(all(test, unix))]
    pub(crate) fn at(path: std::path::PathBuf) -> Self {
        Self { path: Some(path) }
    }

    /// Whether the migration ran.
    pub(crate) fn ran(&self) -> bool {
        self.path.as_deref().is_some_and(Path::exists)
    }

    /// Record that the migration ran. A record that cannot be written leaves the next read to
    /// migrate again, which changes nothing in a file already migrated.
    fn record(&self) {
        let Some(path) = self.path.as_deref() else {
            return;
        };
        if let Some(dir) = path.parent()
            && let Err(error) = std::fs::create_dir_all(dir)
        {
            aterm_log::warn!("config: the cursor migration's record was not kept: {error}");
            return;
        }
        if let Err(error) = std::fs::write(
            path,
            "aterm migrated this aterm.toml's cursor settings once; old style names are refused now\n",
        ) {
            aterm_log::warn!("config: the cursor migration's record was not kept: {error}");
        }
    }
}

/// The text of the last aterm.toml whose migration could not be written, hashed, so the one
/// line that says so is said once for that text and not at every read of it.
static CURSOR_MIGRATION_UNWRITTEN: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

/// Read an aterm.toml through its one-time cursor migration: `ran` is read from `stamp`
/// before `observe` reads the file, so a second aterm that migrated the file between the two
/// reads is seen as a migrated file and not as an old name typed later.
pub(crate) fn observe_through_cursor_migration<E>(
    stamp: &CursorMigrationStamp,
    observe: impl FnOnce() -> Result<ConfigDiskObservation, E>,
) -> Result<ConfigDiskObservation, E> {
    let ran = stamp.ran();
    let observation = observe()?;
    Ok(if ran {
        observation
    } else {
        admit_cursor_migration(observation, stamp)
    })
}

/// THE ONE-TIME MIGRATION OF AN ATERM.TOML'S CURSOR SETTINGS, at the first read of the file
/// by a build that has the pet as its own setting: an observed aterm.toml the migration
/// changes ([`crate::prefs::migrate_cursor_settings`]) is written back, the observation
/// returned is the file as written, and `stamp` records that it ran. It says so once: a line
/// in the log and one record in Messages, which draws no row. A file it changes nothing in is
/// stamped the same way. The caller asks only while the stamp is absent
/// ([`observe_through_cursor_migration`]); afterwards a retired name in the file is refused
/// by what it became (`Config::cursor_trail_style_warning`).
///
/// A write that does not land (a read-only or managed aterm.toml, a locked file, another
/// aterm writing it first) keeps the person's choice all the same: the observation returned
/// carries the migrated text, so this generation draws what the file meant, and the stamp is
/// left absent so the next read tries again. A file that cannot be written says so once, with
/// the lines to write by hand.
pub(crate) fn admit_cursor_migration(
    observation: ConfigDiskObservation,
    stamp: &CursorMigrationStamp,
) -> ConfigDiskObservation {
    use crate::native_document_host::{
        AtomicCommitResult, AtomicFileBaseline, commit_atomic_bytes,
    };
    let Some(migration) = crate::prefs::migrate_cursor_settings(&observation.text) else {
        // Nothing to change, or a file that does not parse: its reader reports that, and
        // the migration waits for a file it can read.
        if observation
            .text
            .parse::<aterm_toml::edit::DocumentMut>()
            .is_ok()
        {
            stamp.record();
        }
        return observation;
    };
    let said = || {
        for sentence in migration.sentences() {
            aterm_log::info!("config: {sentence}; aterm.toml rewritten once");
        }
        crate::message_inbox::queue_message(crate::message_reporters::cursor_names_renamed(
            &migration,
        ));
    };
    let ConfigDiskObservation { baseline, .. } = observation;
    match commit_atomic_bytes(&baseline, migration.text.as_bytes()) {
        AtomicCommitResult::Committed(proof) => {
            said();
            stamp.record();
            ConfigDiskObservation {
                text: migration.text,
                baseline: AtomicFileBaseline {
                    target: baseline.target,
                    observed: proof.observed,
                },
            }
        }
        // The file is replaced; the next read of it holds the new names.
        AtomicCommitResult::PublishedUnverified { .. } => {
            said();
            stamp.record();
            ConfigDiskObservation {
                text: migration.text,
                baseline,
            }
        }
        // Another writer reached the file first: its next read is migrated or is the other
        // aterm's migration. This generation draws the migrated text.
        AtomicCommitResult::Conflict { message, .. } => {
            aterm_log::info!(
                "config: aterm.toml changed while its cursor settings were migrated \
                 ({message}); the next read migrates it"
            );
            ConfigDiskObservation {
                text: migration.text,
                baseline,
            }
        }
        AtomicCommitResult::Failed { message, .. } => {
            let mut hash = aterm_hash::FxHasher::default();
            std::hash::Hash::hash(&migration.text, &mut hash);
            let hash = std::hash::Hasher::finish(&hash) | 1;
            if CURSOR_MIGRATION_UNWRITTEN.swap(hash, std::sync::atomic::Ordering::Relaxed) != hash {
                aterm_log::warn!(
                    "config: aterm.toml's cursor settings were migrated in memory; the file \
                     could not be written ({message})"
                );
                crate::message_inbox::queue_message(
                    crate::message_reporters::cursor_names_unwritten(&migration, &message),
                );
            }
            ConfigDiskObservation {
                text: migration.text,
                baseline,
            }
        }
    }
}

/// Disk bytes plus every filesystem-backed asset and effect-feed runtime needed on the event loop.
#[derive(Clone)]
pub(crate) struct PreparedConfigObservation {
    pub(crate) observation: ConfigDiskObservation,
    pub(crate) config: crate::app_config::Config,
    pub(crate) values: BTreeMap<String, String>,
    pub(crate) disk_faults: DiskFaults,
    pub(crate) assets: Arc<crate::app_config::ConfigAssetCatalog>,
    pub(crate) path_feeds: crate::app_config::PreparedPathFeedGeneration,
}

impl std::fmt::Debug for PreparedConfigObservation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PreparedConfigObservation")
            .field("observation", &self.observation)
            .field("semantic_value_count", &self.values.len())
            .field("disk_fault_count", &self.disk_faults.len())
            .field("assets", &self.assets)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct UndoToken(u64);

impl UndoToken {
    pub(crate) const fn get(self) -> u64 {
        self.0
    }

    pub(crate) const fn from_stored(raw: u64) -> Self {
        Self(raw)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ConfigPatchResult {
    Applied {
        snapshot: ConfigSnapshot,
        undo: UndoToken,
    },
    Unchanged {
        snapshot: ConfigSnapshot,
    },
    Conflict {
        snapshot: ConfigSnapshot,
        keys: Vec<String>,
    },
    Rejected {
        snapshot: ConfigSnapshot,
        message: String,
    },
}

#[derive(Clone, Debug)]
struct UndoRecord {
    token: UndoToken,
    before: Vec<(String, Option<String>)>,
    after: Vec<(String, Option<String>)>,
    /// The admitted text just before the patch and just after it. Undo
    /// restores the DOCUMENT, not the values: while the file still holds
    /// `after_text` it goes back to `before_text` byte for byte (every
    /// line where it stood, every comment with the key it described, the
    /// value as it was spelled); once the file moved on it reverses only
    /// the patch's own lines ([`merge_reverting`]).
    before_text: Arc<str>,
    after_text: Arc<str>,
}

/// Single-writer reducer owned by the `App`; callers clone only immutable snapshots.
pub(crate) struct VersionedConfigService {
    text: String,
    values: BTreeMap<String, String>,
    disk_faults: DiskFaults,
    config: Arc<crate::app_config::Config>,
    durable_text: String,
    durable_values: BTreeMap<String, String>,
    durable_disk_faults: DiskFaults,
    durable_config: Arc<crate::app_config::Config>,
    durable_assets: Arc<crate::app_config::ConfigAssetCatalog>,
    disk_baseline: Option<crate::native_document_host::AtomicFileBaseline>,
    disk_text_admitted: bool,
    write_reconciliation_required: bool,
    revision: u64,
    analysis_generation: u64,
    assets: Arc<crate::app_config::ConfigAssetCatalog>,
    key_revision: BTreeMap<String, u64>,
    next_undo: u64,
    undo: VecDeque<UndoRecord>,
}

impl VersionedConfigService {
    #[cfg(any(test, feature = "bench-support"))]
    pub(crate) fn new(text: String) -> Result<Self, String> {
        Self::new_with_themes(text, crate::app_config::ThemeCatalog::empty())
    }

    /// Runtime constructor used before presentation.
    pub(crate) fn new_runtime(text: String) -> Result<Self, String> {
        Self::new_with_themes_mode(text, crate::app_config::ThemeCatalog::discover(), true)
    }

    #[cfg(any(test, feature = "bench-support"))]
    pub(crate) fn new_with_themes(
        text: String,
        themes: Arc<crate::app_config::ThemeCatalog>,
    ) -> Result<Self, String> {
        Self::new_with_themes_mode(text, themes, false)
    }

    fn new_with_themes_mode(
        text: String,
        themes: Arc<crate::app_config::ThemeCatalog>,
        path_feeds_pending: bool,
    ) -> Result<Self, String> {
        // Said, never logged: launch prints it to stderr (gui.log).
        let values = parse_values(&text).map_err(ConfigFailure::into_said)?;
        let disk_faults = parse_disk_faults(&text);
        let config = parse_config(&text).map_err(ConfigFailure::into_said)?;
        let assets = if path_feeds_pending {
            config.resolve_preliminary_asset_catalog_with_themes(themes)
        } else {
            config.resolve_asset_catalog_with_themes(themes)
        };
        let config = Arc::new(config);
        Ok(Self {
            durable_text: text.clone(),
            durable_values: values.clone(),
            durable_disk_faults: Arc::clone(&disk_faults),
            durable_config: Arc::clone(&config),
            durable_assets: Arc::clone(&assets),
            text,
            values,
            disk_faults,
            config,
            disk_baseline: None,
            disk_text_admitted: false,
            write_reconciliation_required: false,
            revision: 1,
            analysis_generation: 1,
            assets,
            key_revision: BTreeMap::new(),
            next_undo: 1,
            undo: VecDeque::new(),
        })
    }

    /// Seed the process service from the watched user config path.
    pub(crate) fn load_current() -> Result<Self, String> {
        let Some(path) = crate::app_config::config_path() else {
            return Self::new_runtime(String::new());
        };
        Self::load_path(&path)
    }

    pub(crate) fn load_path(path: &Path) -> Result<Self, String> {
        let observation =
            observe_through_cursor_migration(&CursorMigrationStamp::for_config(path), || {
                Self::observe_path(path, true)
            })?;
        let mut service = Self::new_runtime(observation.text)?;
        service.disk_baseline = Some(observation.baseline);
        service.disk_text_admitted = true;
        Ok(service)
    }

    pub(crate) fn observe_path(
        path: &Path,
        allow_missing: bool,
    ) -> Result<ConfigDiskObservation, String> {
        Self::observe_path_for_a_person(path, allow_missing).map_err(|failure| match failure {
            ObserveFailure::Refused(error) => config_read_error(path, error),
            ObserveFailure::Text(message) => message,
        })
    }

    /// [`Self::observe_path`] for a caller that words a refused read for a person.
    pub(crate) fn observe_path_for_a_person(
        path: &Path,
        allow_missing: bool,
    ) -> Result<ConfigDiskObservation, ObserveFailure> {
        let contents = crate::native_document_host::read_config_atomic_file(
            path,
            MAX_CONFIG_FILE_BYTES,
            allow_missing,
        )
        .map_err(ObserveFailure::Refused)?;
        let text = decode_config_bytes(&contents.bytes, path).map_err(ObserveFailure::Text)?;
        Ok(ConfigDiskObservation {
            text,
            baseline: contents.baseline,
        })
    }

    /// Resolve one worker-observed generation before it crosses to the event loop.
    pub(crate) fn prepare_observation(
        observation: ConfigDiskObservation,
        themes: Arc<crate::app_config::ThemeCatalog>,
    ) -> Result<PreparedConfigObservation, ConfigFailure> {
        let values = parse_values(&observation.text)?;
        let disk_faults = parse_disk_faults(&observation.text);
        let config = parse_config(&observation.text)?;
        let path_feeds = config.prepare_path_feed_generation();
        let preliminary = config.resolve_preliminary_asset_catalog_with_themes(themes);
        let assets = Arc::new(crate::app_config::ConfigAssetCatalog {
            trail_style: Arc::clone(&preliminary.trail_style),
            kitty_sprite: preliminary.kitty_sprite.clone(),
            wallpaper: preliminary.wallpaper.clone(),
            themes: Arc::clone(&preliminary.themes),
            sparkle_spec_consumers: Some(Arc::new(path_feeds.sparkle.consumer_capabilities())),
        });
        Ok(PreparedConfigObservation {
            observation,
            config,
            values,
            disk_faults,
            assets,
            path_feeds,
        })
    }

    pub(crate) fn snapshot(&self) -> ConfigSnapshot {
        ConfigSnapshot {
            revision: self.revision,
            analysis_generation: self.analysis_generation,
            text: Arc::from(self.text.clone()),
            config: Arc::clone(&self.config),
            semantic_values: Arc::new(self.values.clone()),
            disk_faults: Arc::clone(&self.disk_faults),
            assets: Arc::clone(&self.assets),
        }
    }

    /// Install the path-backed effect generation prepared for startup before any view observes it.
    pub(crate) fn complete_startup_path_generation(
        &mut self,
        consumers: aterm_effects::spec::SpecConsumerCapabilities,
    ) {
        debug_assert_eq!(self.revision, 1);
        let assets = Arc::new(crate::app_config::ConfigAssetCatalog {
            trail_style: Arc::clone(&self.assets.trail_style),
            kitty_sprite: self.assets.kitty_sprite.clone(),
            wallpaper: self.assets.wallpaper.clone(),
            themes: Arc::clone(&self.assets.themes),
            sparkle_spec_consumers: Some(Arc::new(consumers)),
        });
        self.assets = Arc::clone(&assets);
        self.durable_assets = assets;
    }

    pub(crate) const fn analysis_generation(&self) -> u64 {
        self.analysis_generation
    }

    pub(crate) fn persistence_plan(&self, snapshot: ConfigSnapshot) -> ConfigPersistencePlan {
        debug_assert_eq!(snapshot.revision, self.revision);
        debug_assert_eq!(snapshot.text.as_ref(), self.text);
        ConfigPersistencePlan {
            snapshot,
            expected_text: Arc::from(self.durable_text.clone()),
            logical_path: self.bound_logical_path().map(Path::to_path_buf),
            baseline: if self.disk_text_admitted {
                self.disk_baseline.clone()
            } else {
                None
            },
        }
    }

    pub(crate) fn bound_logical_path(&self) -> Option<&Path> {
        self.disk_baseline
            .as_ref()
            .map(|baseline| baseline.target.logical_path())
    }

    /// Disk generation the service was initialized from.
    pub(crate) const fn observed_disk_baseline(
        &self,
    ) -> Option<&crate::native_document_host::AtomicFileBaseline> {
        self.disk_baseline.as_ref()
    }

    /// A writer reported an unknown or externally won disk generation.
    pub(crate) const fn mark_reconciliation_required(&mut self) {
        self.disk_text_admitted = false;
        self.write_reconciliation_required = true;
    }

    pub(crate) const fn reconciliation_required(&self) -> bool {
        self.write_reconciliation_required
    }

    /// The admitted disk observation, when the service holds no optimistic edit ahead of disk.
    pub(crate) fn admitted_observation(&self) -> Option<ConfigDiskObservation> {
        (self.disk_text_admitted
            && !self.write_reconciliation_required
            && self.text == self.durable_text)
            .then(|| self.disk_baseline.clone())
            .flatten()
            .map(|baseline| ConfigDiskObservation {
                text: self.text.clone(),
                baseline,
            })
    }

    /// Keep path/generation authority when startup read UTF-8 bytes with malformed TOML.
    pub(crate) fn bind_unparsed_disk_baseline(
        &mut self,
        baseline: crate::native_document_host::AtomicFileBaseline,
    ) -> Result<(), String> {
        if let Some(current) = self.disk_baseline.as_ref() {
            if current.target != baseline.target {
                return Err("config service is already bound to a different file".to_string());
            }
            return Ok(());
        }
        self.disk_baseline = Some(baseline);
        self.disk_text_admitted = false;
        self.write_reconciliation_required = true;
        Ok(())
    }

    /// Re-read the bound (or default) config path and admit it.
    #[cfg(test)]
    pub(crate) fn synchronize_from_disk(&mut self) -> Result<ConfigSnapshot, String> {
        let path = self
            .bound_logical_path()
            .map(Path::to_path_buf)
            .or_else(crate::app_config::config_path)
            .ok_or_else(|| "no config path (HOME/XDG unset)".to_string())?;
        self.synchronize_from_path(&path)
    }

    #[cfg(test)]
    pub(crate) fn synchronize_from_path(&mut self, path: &Path) -> Result<ConfigSnapshot, String> {
        let observation = Self::observe_path(path, true)?;
        self.synchronize_observation(observation)
    }

    #[cfg(test)]
    pub(crate) fn synchronize_observation(
        &mut self,
        observation: ConfigDiskObservation,
    ) -> Result<ConfigSnapshot, String> {
        let themes = Arc::clone(&self.assets.themes);
        let prepared =
            Self::prepare_observation(observation, themes).map_err(ConfigFailure::into_said)?;
        self.synchronize_prepared_observation(prepared)
    }

    /// Admit the config and assets resolved by the worker.
    pub(crate) fn synchronize_observation_prepared(
        &mut self,
        observation: ConfigDiskObservation,
        config: crate::app_config::Config,
        values: BTreeMap<String, String>,
        disk_faults: DiskFaults,
        assets: Arc<crate::app_config::ConfigAssetCatalog>,
    ) -> Result<ConfigSnapshot, String> {
        let ConfigDiskObservation { text, baseline } = observation;
        self.analysis_generation = self.analysis_generation.saturating_add(1);
        if text != self.text {
            self.revision = self.revision.saturating_add(1);
            for key in self
                .values
                .keys()
                .chain(values.keys())
                .cloned()
                .collect::<BTreeSet<_>>()
            {
                if self.values.get(&key) != values.get(&key) {
                    self.key_revision.insert(key, self.revision);
                }
            }
            self.text = text;
            self.values = values;
            self.disk_faults = disk_faults;
            self.config = Arc::new(config);
        } else if config.ai != self.config.ai
            || config.harness != self.config.harness
            || assets != self.assets
        {
            self.revision = self.revision.saturating_add(1);
            self.config = Arc::new(config);
        }
        self.durable_text.clone_from(&self.text);
        self.durable_values.clone_from(&self.values);
        self.durable_disk_faults = Arc::clone(&self.disk_faults);
        self.durable_config = Arc::clone(&self.config);
        self.durable_assets = Arc::clone(&assets);
        self.assets = assets;
        self.disk_baseline = Some(baseline);
        self.disk_text_admitted = true;
        self.write_reconciliation_required = false;
        Ok(self.snapshot())
    }

    pub(crate) fn synchronize_prepared_observation(
        &mut self,
        prepared: PreparedConfigObservation,
    ) -> Result<ConfigSnapshot, String> {
        let PreparedConfigObservation {
            observation,
            config,
            values,
            disk_faults,
            assets,
            path_feeds: _,
        } = prepared;
        self.synchronize_observation_prepared(observation, config, values, disk_faults, assets)
    }

    /// Roll an optimistic candidate back to the last durable bytes when its worker commit fails.
    pub(crate) fn restore_durable_snapshot(&mut self) -> Result<ConfigSnapshot, String> {
        let changed = self.text != self.durable_text
            || self.values != self.durable_values
            || self.config != self.durable_config
            || self.assets != self.durable_assets;
        self.analysis_generation = self.analysis_generation.saturating_add(1);
        if changed {
            self.revision = self.revision.saturating_add(1);
            for key in self
                .values
                .keys()
                .chain(self.durable_values.keys())
                .cloned()
                .collect::<BTreeSet<_>>()
            {
                if self.values.get(&key) != self.durable_values.get(&key) {
                    self.key_revision.insert(key, self.revision);
                }
            }
            self.text.clone_from(&self.durable_text);
            self.values.clone_from(&self.durable_values);
            self.disk_faults = Arc::clone(&self.durable_disk_faults);
            self.config = Arc::clone(&self.durable_config);
            self.assets = Arc::clone(&self.durable_assets);
        }
        Ok(self.snapshot())
    }

    #[cfg(test)]
    pub(crate) fn value(&self, key: &str) -> Result<Option<String>, String> {
        Ok(self.values.get(key).cloned())
    }

    /// Apply one all-or-nothing patch.
    pub(crate) fn patch(&mut self, request: ConfigPatchRequest) -> ConfigPatchResult {
        if let Err(result) = self.check_patch(&request) {
            return result;
        }
        let borrowed = request
            .edits
            .iter()
            .map(|edit| (edit.key.as_str(), edit.value.clone()))
            .collect::<Vec<_>>();
        let (borrowed, next) = match planned_write(&self.text, &borrowed, &self.assets.themes) {
            Ok(Some(planned)) => (planned.edits, planned.text),
            Ok(None) => {
                return ConfigPatchResult::Unchanged {
                    snapshot: self.snapshot(),
                };
            }
            Err(message) => return self.rejected(message),
        };
        let mut keys = borrowed
            .iter()
            .map(|(key, _)| (*key).to_string())
            .collect::<BTreeSet<_>>();
        let before = match leaf_values(&self.text) {
            Ok(values) => values,
            Err(message) => return self.rejected(message),
        };
        let after = match leaf_values(&next) {
            Ok(values) => values,
            Err(message) => return self.rejected(message),
        };
        let implicit = before
            .keys()
            .chain(after.keys())
            .filter(|key| before.get(*key) != after.get(*key))
            .filter(|key| {
                !key.match_indices('.')
                    .any(|(at, _)| keys.contains(&key[..at]))
            })
            .cloned()
            .collect::<BTreeSet<_>>();
        let conflicts = implicit
            .iter()
            .filter(|key| {
                self.key_revision
                    .get(*key)
                    .is_some_and(|revision| *revision > request.base_revision)
            })
            .cloned()
            .collect::<Vec<_>>();
        if !conflicts.is_empty() {
            return ConfigPatchResult::Conflict {
                snapshot: self.snapshot(),
                keys: conflicts,
            };
        }
        keys.extend(implicit);
        self.admit_text(next, &keys)
    }

    fn check_patch(&self, request: &ConfigPatchRequest) -> Result<(), ConfigPatchResult> {
        if request.base_revision == 0 || request.base_revision > self.revision {
            return Err(self.rejected("invalid base revision"));
        }
        if request.edits.is_empty() {
            return Err(ConfigPatchResult::Unchanged {
                snapshot: self.snapshot(),
            });
        }
        let mut keys = BTreeSet::new();
        if request
            .edits
            .iter()
            .any(|edit| edit.key.trim().is_empty() || !keys.insert(edit.key.clone()))
        {
            return Err(self.rejected("empty or duplicate preference key"));
        }
        if let Some(edit) = request
            .edits
            .iter()
            .find(|edit| crate::prefs::config_asset_source_key(&edit.key))
        {
            return Err(self.rejected(format!(
                "{} names filesystem-backed assets; edit it in Manual so a worker can validate and admit text plus assets atomically",
                edit.key
            )));
        }
        let mut conflicts = Vec::new();
        for edit in &request.edits {
            let changed_after_base = self
                .key_revision
                .get(&edit.key)
                .is_some_and(|revision| *revision > request.base_revision);
            let expectation_failed = match &edit.expected {
                #[cfg(test)]
                ExpectedValue::Any => false,
                ExpectedValue::Exact(expected) => {
                    !holds(&edit.key, self.values.get(&edit.key), expected.as_deref())
                }
            };
            if changed_after_base || expectation_failed {
                conflicts.push(edit.key.clone());
            }
        }
        if !conflicts.is_empty() {
            return Err(ConfigPatchResult::Conflict {
                snapshot: self.snapshot(),
                keys: conflicts,
            });
        }
        Ok(())
    }

    fn admit_text(&mut self, next: String, touched: &BTreeSet<String>) -> ConfigPatchResult {
        if next == self.text {
            return ConfigPatchResult::Unchanged {
                snapshot: self.snapshot(),
            };
        }
        // The owner's keys change only by a person at the window, in the file itself
        // (`owner_keys_save`): no write admitted here moves them from the file's.
        if OwnerKeys::of(&next) != OwnerKeys::of(&self.durable_text) {
            return self.rejected(OWNER_KEYS_REFUSED);
        }

        // A rejected patch answers its writer (Settings, Manual, `ctl settings`); no log keeps it.
        let next_config = match parse_config(&next) {
            Ok(config) => config,
            Err(failure) => return self.rejected(failure.said),
        };
        let after_values = match parse_values(&next) {
            Ok(values) => values,
            Err(failure) => return self.rejected(failure.said),
        };

        if self.config.sparkle_words != next_config.sparkle_words
            && self.assets.sparkle_spec_consumers.is_some()
        {
            self.assets = Arc::new(crate::app_config::ConfigAssetCatalog {
                trail_style: Arc::clone(&self.assets.trail_style),
                kitty_sprite: self.assets.kitty_sprite.clone(),
                wallpaper: self.assets.wallpaper.clone(),
                themes: Arc::clone(&self.assets.themes),
                sparkle_spec_consumers: None,
            });
        }
        if self.config.wallpaper != next_config.wallpaper {
            self.assets = Arc::new(crate::app_config::ConfigAssetCatalog {
                trail_style: Arc::clone(&self.assets.trail_style),
                kitty_sprite: self.assets.kitty_sprite.clone(),
                wallpaper: crate::app_config::resolve_wallpaper_asset(
                    next_config.wallpaper.as_deref(),
                ),
                themes: Arc::clone(&self.assets.themes),
                sparkle_spec_consumers: self.assets.sparkle_spec_consumers.clone(),
            });
        }

        let before = touched
            .iter()
            .map(|key| (key.clone(), self.values.get(key).cloned()))
            .collect::<Vec<_>>();
        let after = touched
            .iter()
            .map(|key| (key.clone(), after_values.get(key).cloned()))
            .collect::<Vec<_>>();
        self.revision = self.revision.saturating_add(1);
        let moved = self
            .values
            .keys()
            .chain(after_values.keys())
            .filter(|key| self.values.get(*key) != after_values.get(*key))
            .cloned()
            .collect::<BTreeSet<_>>();
        for key in touched.iter().chain(&moved) {
            self.key_revision.insert(key.clone(), self.revision);
        }
        let before_text = match self.undo.back() {
            Some(previous) if *previous.after_text == *self.text => {
                Arc::clone(&previous.after_text)
            }
            _ => Arc::from(self.text.as_str()),
        };
        let after_text: Arc<str> = Arc::from(next.as_str());
        self.text = next;
        self.config = Arc::new(next_config);
        self.values = after_values;
        self.disk_faults = parse_disk_faults(&self.text);
        let token = UndoToken(self.next_undo.max(1));
        self.next_undo = self.next_undo.saturating_add(1);
        self.undo.push_back(UndoRecord {
            token,
            before,
            after,
            before_text,
            after_text,
        });
        while self.undo.len() > UNDO_LIMIT {
            self.undo.pop_front();
        }
        ConfigPatchResult::Applied {
            snapshot: self.snapshot(),
            undo: token,
        }
    }

    /// Reset a complete named set in one transform/revision.
    #[cfg(test)]
    pub(crate) fn reset_all(
        &mut self,
        base_revision: u64,
        keys: impl IntoIterator<Item = String>,
    ) -> ConfigPatchResult {
        self.patch(ConfigPatchRequest {
            base_revision,
            edits: keys
                .into_iter()
                .map(|key| ConfigKeyEdit {
                    key,
                    expected: ExpectedValue::Any,
                    value: None,
                })
                .collect(),
        })
    }

    /// Conditional undo: succeeds only if every value written by the target patch is still current.
    pub(crate) fn undo(&mut self, token: UndoToken) -> ConfigPatchResult {
        let Some(record) = self
            .undo
            .iter()
            .find(|record| record.token == token)
            .cloned()
        else {
            return self.rejected("undo token expired");
        };
        let conflicts = record
            .after
            .iter()
            .filter(|(key, after)| self.values.get(key) != after.as_ref())
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        if !conflicts.is_empty() {
            return ConfigPatchResult::Conflict {
                snapshot: self.snapshot(),
                keys: conflicts,
            };
        }
        let next = if *self.text == *record.after_text {
            record.before_text.to_string()
        } else {
            let merged = merge_reverting(&record.after_text, &record.before_text, &self.text)
                .filter(|merged| {
                    parse_config(merged).is_ok()
                        && reads_as_reverted(
                            merged,
                            &record.after_text,
                            &record.before_text,
                            &self.text,
                        )
                });
            match merged {
                Some(merged) => merged,
                None => {
                    let inverse = record
                        .before
                        .iter()
                        .map(|(key, before)| (key.as_str(), before.clone()))
                        .collect::<Vec<_>>();
                    match crate::prefs::apply_prefs_edits(&self.text, &inverse) {
                        Ok(replay) => replay,
                        Err(error) => return self.rejected(error.to_string()),
                    }
                }
            }
        };
        let touched = record.before.iter().map(|(key, _)| key.clone()).collect();
        self.admit_text(next, &touched)
    }

    /// Accept a whole watcher/file snapshot.
    #[cfg(test)]
    pub(crate) fn replace_external(&mut self, text: String) -> Result<ConfigSnapshot, String> {
        let before = self.values.clone();
        let after = parse_values(&text).map_err(ConfigFailure::into_said)?;
        let before_config = Arc::clone(&self.config);
        let after_config = Arc::new(parse_config(&text).map_err(ConfigFailure::into_said)?);
        let refresh_assets =
            text == self.text || asset_sources_changed(&before_config, &after_config);
        let assets = if refresh_assets {
            after_config.resolve_asset_catalog_with_themes(Arc::clone(&self.assets.themes))
        } else {
            Arc::clone(&self.assets)
        };
        self.analysis_generation = self.analysis_generation.saturating_add(1);
        if text == self.text {
            self.durable_text = text;
            self.durable_values.clone_from(&self.values);
            self.durable_disk_faults = Arc::clone(&self.disk_faults);
            self.durable_config = Arc::clone(&self.config);
            if assets != self.assets {
                self.revision = self.revision.saturating_add(1);
                self.assets = assets;
            }
            self.durable_assets = Arc::clone(&self.assets);
            return Ok(self.snapshot());
        }
        self.revision = self.revision.saturating_add(1);
        let keys = before
            .keys()
            .chain(after.keys())
            .cloned()
            .collect::<BTreeSet<_>>();
        for key in keys {
            if before.get(&key) != after.get(&key) {
                self.key_revision.insert(key, self.revision);
            }
        }
        self.text = text;
        self.values = after;
        self.disk_faults = parse_disk_faults(&self.text);
        self.config = after_config;
        self.durable_text.clone_from(&self.text);
        self.durable_values.clone_from(&self.values);
        self.durable_disk_faults = Arc::clone(&self.disk_faults);
        self.durable_config = Arc::clone(&self.config);
        self.assets = assets;
        self.durable_assets = Arc::clone(&self.assets);
        Ok(self.snapshot())
    }

    /// Admit a catalog parsed by the background theme-directory watcher.
    pub(crate) fn replace_theme_catalog(
        &mut self,
        themes: Arc<crate::app_config::ThemeCatalog>,
    ) -> ConfigSnapshot {
        self.analysis_generation = self.analysis_generation.saturating_add(1);
        if *themes == *self.assets.themes {
            return self.snapshot();
        }
        self.revision = self.revision.saturating_add(1);
        self.assets = Arc::new(crate::app_config::ConfigAssetCatalog {
            trail_style: Arc::clone(&self.assets.trail_style),
            kitty_sprite: self.assets.kitty_sprite.clone(),
            wallpaper: self.assets.wallpaper.clone(),
            themes,
            sparkle_spec_consumers: self.assets.sparkle_spec_consumers.clone(),
        });
        self.durable_assets = Arc::clone(&self.assets);
        self.snapshot()
    }

    fn rejected(&self, message: impl Into<String>) -> ConfigPatchResult {
        ConfigPatchResult::Rejected {
            snapshot: self.snapshot(),
            message: message.into(),
        }
    }
}

fn parse_config(text: &str) -> Result<crate::app_config::Config, ConfigFailure> {
    crate::app_config::Config::parse(text)
        .map_err(|error| ConfigFailure::toml("aterm.toml is not a valid aterm config: ", &error))
}

/// A native-config failure, as a person reads it (Manual's diagnostics, the Messages row, a
/// rejected request's answer) and as aterm.log keeps it. aterm.toml's own text (the offending
/// line, the value a serde message quotes: [`aterm_toml::Error`]'s `Display`) is said and never
/// logged, where the log names its position ([`aterm_toml::Error::log_words`], DESIGN §3.6);
/// aterm's own words (a path, a worker's state) are both. It has no `Display`, so no `{}` can
/// log the said form by accident.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ConfigFailure {
    /// What a person reads.
    pub(crate) said: String,
    /// What aterm.log keeps.
    pub(crate) log: String,
}

impl ConfigFailure {
    /// aterm.toml's text that did not parse: `head` and the error said, `head` and its position
    /// logged.
    pub(crate) fn toml(head: &str, error: &aterm_toml::Error) -> Self {
        Self {
            said: format!("{head}{error}"),
            log: format!("{head}{}", error.log_words()),
        }
    }

    /// What a person reads, for a caller that only shows it (a patch's answer, launch's stderr
    /// line): a call by name, so every flow that drops the log form is one a reader can find.
    pub(crate) fn into_said(self) -> String {
        self.said
    }

    /// `head` before both forms.
    #[must_use]
    pub(crate) fn headed(self, head: &str) -> Self {
        Self {
            said: format!("{head}{}", self.said),
            log: format!("{head}{}", self.log),
        }
    }
}

/// A failure in aterm's own words, said and logged alike.
impl From<String> for ConfigFailure {
    fn from(words: String) -> Self {
        Self {
            log: words.clone(),
            said: words,
        }
    }
}

/// A failure in aterm's own words, borrowed.
impl From<&str> for ConfigFailure {
    fn from(words: &str) -> Self {
        Self::from(words.to_string())
    }
}

#[cfg(test)]
fn asset_sources_changed(
    before: &crate::app_config::Config,
    after: &crate::app_config::Config,
) -> bool {
    fn normalized_kitty_sprite(value: Option<&str>) -> Option<&str> {
        value.map(str::trim).filter(|value| !value.is_empty())
    }
    normalized_kitty_sprite(before.cursor_nyan_sprite.as_deref())
        != normalized_kitty_sprite(after.cursor_nyan_sprite.as_deref())
}

/// Why an observation of aterm.toml failed: the host refused the read, or what it read is not text.
#[derive(Debug)]
pub(crate) enum ObserveFailure {
    Refused(crate::native_document_host::DocumentHostError),
    Text(String),
}

/// Why `aterm.toml` at `path` could not be read, naming the file once.
pub(crate) fn config_read_error(
    path: &Path,
    error: crate::native_document_host::DocumentHostError,
) -> String {
    use crate::native_document_host::DocumentHostError;
    match error {
        DocumentHostError::Io { message, .. } if message.contains(&*path.to_string_lossy()) => {
            message
        }
        DocumentHostError::Io { message, .. } => format!("{}: {message}", path.display()),
        other => format!("{}: {other}", path.display()),
    }
}

fn decode_config_bytes(bytes: &[u8], path: &Path) -> Result<String, String> {
    let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes);
    std::str::from_utf8(bytes)
        .map(str::to_owned)
        .map_err(|_| format!("{} is not valid UTF-8", path.display()))
}

fn parse_values(text: &str) -> Result<BTreeMap<String, String>, ConfigFailure> {
    let document = text
        .parse::<aterm_toml::edit::DocumentMut>()
        .map_err(|error| ConfigFailure::toml("existing aterm.toml is not valid TOML: ", &error))?;
    let mut values = BTreeMap::new();
    for (key, item) in document.iter() {
        project_item(&mut values, &join_config_key_path("", key), item);
    }
    Ok(values)
}

fn reads_as_reverted(merged: &str, after: &str, before: &str, current: &str) -> bool {
    let (Ok(merged), Ok(after), Ok(before), Ok(current)) = (
        leaf_values(merged),
        leaf_values(after),
        leaf_values(before),
        leaf_values(current),
    ) else {
        return false;
    };
    merged
        .keys()
        .chain(after.keys())
        .chain(before.keys())
        .chain(current.keys())
        .all(|key| {
            let wanted = if after.get(key) == before.get(key) {
                current.get(key)
            } else {
                before.get(key)
            };
            merged.get(key) == wanted
        })
}

fn leaf_values(text: &str) -> Result<BTreeMap<String, String>, String> {
    fn walk(values: &mut BTreeMap<String, String>, path: &str, item: &aterm_toml::edit::Item) {
        if let Some(table) = item.as_table_like() {
            for (child, child_item) in table.iter() {
                walk(values, &join_config_key_path(path, child), child_item);
            }
        } else {
            values.insert(path.to_string(), semantic_item(item));
        }
    }
    let document = text
        .parse::<aterm_toml::edit::DocumentMut>()
        .map_err(|error| format!("existing aterm.toml is not valid TOML: {error}"))?;
    let mut values = BTreeMap::new();
    for (key, item) in document.iter() {
        walk(&mut values, &join_config_key_path("", key), item);
    }
    Ok(values)
}

fn merge_reverting(after: &str, before: &str, current: &str) -> Option<String> {
    let base: Vec<&str> = after.split_inclusive('\n').collect();
    let wanted: Vec<&str> = before.split_inclusive('\n').collect();
    let theirs: Vec<&str> = current.split_inclusive('\n').collect();
    let revert = line_hunks(&base, &wanted)?;
    let later = line_hunks(&base, &theirs)?;
    let clash = |a: &LineHunk, b: &LineHunk| {
        (a.start < b.end && b.start < a.end)
            || (a.start == a.end && b.start == b.end && a.start == b.start)
    };
    if revert.iter().any(|a| later.iter().any(|b| clash(a, b))) {
        return None;
    }
    let mut merged = String::with_capacity(current.len().max(before.len()));
    let mut at = 0usize;
    let mut revert = revert.iter().peekable();
    let mut later = later.iter().peekable();
    loop {
        let next = match (revert.peek(), later.peek()) {
            (Some(a), Some(b)) if (a.start, a.end) <= (b.start, b.end) => (revert.next()?, &wanted),
            (Some(_), Some(_)) | (None, Some(_)) => (later.next()?, &theirs),
            (Some(_), None) => (revert.next()?, &wanted),
            (None, None) => break,
        };
        let (hunk, lines) = next;
        merged.extend(base[at..hunk.start].iter().copied());
        merged.extend(lines[hunk.other_start..hunk.other_end].iter().copied());
        at = hunk.end;
    }
    merged.extend(base[at..].iter().copied());
    Some(merged)
}

/// One difference between two line lists: `base[start..end]` became
/// `other[other_start..other_end]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LineHunk {
    start: usize,
    end: usize,
    other_start: usize,
    other_end: usize,
}

fn line_hunks(base: &[&str], other: &[&str]) -> Option<Vec<LineHunk>> {
    const MAX_DIFFERENCES: usize = 512;
    let (n, m) = (base.len(), other.len());
    let max = (n + m).min(MAX_DIFFERENCES);
    let offset = max + 1;
    let mut frontier = vec![0usize; 2 * max + 3];
    let mut trace: Vec<Vec<usize>> = Vec::new();
    let mut found = None;
    'search: for d in 0..=max {
        trace.push(frontier.clone());
        let d = d as isize;
        let mut k = -d;
        while k <= d {
            let index = (k + offset as isize) as usize;
            let mut x = if k == -d || (k != d && frontier[index - 1] < frontier[index + 1]) {
                frontier[index + 1]
            } else {
                frontier[index - 1] + 1
            };
            let mut y = (x as isize - k) as usize;
            while x < n && y < m && base[x] == other[y] {
                x += 1;
                y += 1;
            }
            frontier[index] = x;
            if x >= n && y >= m {
                found = Some(d as usize);
                break 'search;
            }
            k += 2;
        }
    }
    let differences = found?;
    let mut matched: Vec<(usize, usize)> = Vec::new();
    let (mut x, mut y) = (n, m);
    for d in (1..=differences).rev() {
        let row = &trace[d];
        let d = d as isize;
        let k = x as isize - y as isize;
        let index = |k: isize| (k + offset as isize) as usize;
        let previous_k = if k == -d || (k != d && row[index(k - 1)] < row[index(k + 1)]) {
            k + 1
        } else {
            k - 1
        };
        let previous_x = row[index(previous_k)];
        let previous_y = (previous_x as isize - previous_k) as usize;
        let (start_x, start_y) = if previous_k == k + 1 {
            (previous_x, previous_y + 1)
        } else {
            (previous_x + 1, previous_y)
        };
        while x > start_x && y > start_y {
            x -= 1;
            y -= 1;
            matched.push((x, y));
        }
        x = previous_x;
        y = previous_y;
    }
    while x > 0 && y > 0 {
        x -= 1;
        y -= 1;
        matched.push((x, y));
    }
    matched.reverse();
    let mut hunks = Vec::new();
    let (mut at_base, mut at_other) = (0usize, 0usize);
    for (base_line, other_line) in matched.into_iter().chain(std::iter::once((n, m))) {
        if base_line > at_base || other_line > at_other {
            hunks.push(LineHunk {
                start: at_base,
                end: base_line,
                other_start: at_other,
                other_end: other_line,
            });
        }
        at_base = base_line + 1;
        at_other = other_line + 1;
    }
    Some(hunks)
}

/// Append one semantic TOML key segment to the identity used by `ConfigSnapshot::values`.
pub(crate) fn join_config_key_path(parent: &str, segment: &str) -> String {
    let segment = aterm_toml::edit::Key::new(segment).to_string();
    if parent.is_empty() {
        segment
    } else {
        format!("{parent}.{segment}")
    }
}

fn project_item(values: &mut BTreeMap<String, String>, path: &str, item: &aterm_toml::edit::Item) {
    if let Some(table) = item.as_table_like() {
        for (child, child_item) in table.iter() {
            project_item(values, &join_config_key_path(path, child), child_item);
        }
    }
    values.insert(path.to_string(), semantic_item(item));
}

fn holds(key: &str, held: Option<&String>, expected: Option<&str>) -> bool {
    match (held, expected) {
        (None, None) => true,
        (Some(held), Some(expected)) => {
            held == expected || written_value(key, expected).as_ref() == Some(held)
        }
        (None, Some(_)) | (Some(_), None) => false,
    }
}

/// The write a patch makes ([`planned_write`]).
pub(crate) struct PlannedWrite<'a> {
    pub(crate) edits: Vec<(&'a str, Option<String>)>,
    pub(crate) text: String,
}

/// THE WRITE A PATCH MAKES, the one judgement every writer shares.
pub(crate) fn planned_write<'a>(
    text: &str,
    edits: &[(&'a str, Option<String>)],
    themes: &crate::app_config::ThemeCatalog,
) -> Result<Option<PlannedWrite<'a>>, String> {
    let held = parse_values(text).map_err(ConfigFailure::into_said)?;
    let elide = crate::prefs::unset_equivalent_edits(text, edits, themes);
    let edits = edits
        .iter()
        .zip(elide)
        .map(|((key, value), elide)| (*key, if elide { None } else { value.clone() }))
        .filter(|(key, value)| {
            value.as_deref().is_none_or(|value| {
                written_value(key, value).is_none_or(|written| held.get(*key) != Some(&written))
            })
        })
        .collect::<Vec<_>>();
    if edits.is_empty() {
        return Ok(None);
    }
    let next = crate::prefs::apply_prefs_edits(text, &edits).map_err(|error| error.to_string())?;
    Ok((next != text).then_some(PlannedWrite { edits, text: next }))
}

pub(crate) fn written_value(key: &str, raw: &str) -> Option<String> {
    crate::prefs::typed_toml_item(key, raw)
        .ok()
        .map(|item| semantic_item(&item))
}

fn semantic_item(item: &aterm_toml::edit::Item) -> String {
    match item {
        aterm_toml::edit::Item::Value(value) => {
            if let Some(value) = value.as_str() {
                value.to_owned()
            } else if let Some(value) = value.as_integer() {
                value.to_string()
            } else if let Some(value) = value.as_float() {
                value.to_string()
            } else if let Some(value) = value.as_bool() {
                value.to_string()
            } else {
                value.to_string().trim().to_owned()
            }
        }
        aterm_toml::edit::Item::Table(table) if aterm_toml::edit::TableLike::is_empty(table) => {
            "{}".to_owned()
        }
        aterm_toml::edit::Item::Table(table) => table.to_string().trim().to_owned(),
        aterm_toml::edit::Item::ArrayOfTables(tables) => tables.to_string().trim().to_owned(),
        aterm_toml::edit::Item::None => String::new(),
    }
}

#[cfg(test)]
#[path = "native_config_service_tests.rs"]
mod tests;
