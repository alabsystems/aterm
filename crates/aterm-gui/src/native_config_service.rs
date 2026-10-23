// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Serialized, versioned preference transaction core for native Settings.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::Path;
use std::sync::Arc;

const UNDO_LIMIT: usize = 64;
/// Admission budget shared by startup, watcher reloads and Manual's semantic analysis, so startup never accepts a generation the watcher cannot fingerprint or the editor cannot validate.
pub(crate) const MAX_CONFIG_FILE_BYTES: usize = 512 * 1024;

#[derive(Clone, PartialEq)]
pub(crate) struct ConfigSnapshot {
    pub(crate) revision: u64,
    /// Environment generation Manual diagnostics were last requested against.
    pub(crate) analysis_generation: u64,
    pub(crate) text: Arc<str>,
    /// Parsed semantic projection produced before admission.
    pub(crate) config: Arc<crate::app_config::Config>,
    pub(crate) semantic_values: Arc<BTreeMap<String, String>>,
    /// The values of `text` the harness's `[disk]` reader does not read as written ([`parse_disk_faults`]).
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

/// What the harness's `[disk]` reader does not read as written in one text (`aterm_agent::harness::cli::disk_faults`).
pub(crate) type DiskFaults = Arc<[aterm_agent::harness::cli::DiskFault]>;

/// [`DiskFaults`] of `text`, computed wherever [`parse_values`] runs on it: at load, on the
/// host worker for a disk observation, and in the reducer for a patch.
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

/// One stable UTF-8 disk read plus the logical-target/file generation it came from, carried through Manual refresh, strict parse and config-service admission so no consumer reads the content a second time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ConfigDiskObservation {
    pub(crate) text: String,
    pub(crate) baseline: crate::native_document_host::AtomicFileBaseline,
}

/// Disk bytes plus every filesystem-backed asset and effect-feed runtime needed to admit them on the event loop.
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
    /// The admitted text just before the patch and just after it.
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
        let values = parse_values(&text)?;
        let disk_faults = parse_disk_faults(&text);
        let config = parse_config(&text)?;
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
        let observation = Self::observe_path(path, true)?;
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

    /// [`Self::observe_path`] for a caller that words a refused read for a person ([`crate::save_words`]).
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
    ) -> Result<PreparedConfigObservation, String> {
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

    /// Install the path-backed effect generation prepared for startup before any view observes the service.
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
    pub(crate) fn observed_disk_baseline(
        &self,
    ) -> Option<&crate::native_document_host::AtomicFileBaseline> {
        self.disk_baseline.as_ref()
    }

    /// A writer reported an unknown or externally won disk generation.
    pub(crate) fn mark_reconciliation_required(&mut self) {
        self.disk_text_admitted = false;
        self.write_reconciliation_required = true;
    }

    pub(crate) fn reconciliation_required(&self) -> bool {
        self.write_reconciliation_required
    }

    /// The admitted disk observation, when the service holds no optimistic edit ahead of disk (DTP §6.4's retained observation).
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

    /// Keep path/generation authority when startup read UTF-8 bytes but could not admit malformed TOML.
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

    /// Re-read the bound (or default) config path and admit it; revision and key stamps stay monotonic.
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
        let prepared = Self::prepare_observation(observation, themes)?;
        self.synchronize_prepared_observation(prepared)
    }

    /// Admit the config and assets resolved by the worker, including path-fed policy changes with identical text.
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

    /// Roll an optimistic candidate back to the last durable bytes when its worker commit fails and the current disk generation is not admissible (e.g. malformed external TOML).
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

    /// The patch's own validation and optimistic concurrency: a known base
    /// revision, unique non-empty keys, no asset-source key, and every touched
    /// key unchanged since its base revision with its expectation current.
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

    /// Admit `next` as the new text, the one revision bump a patch or an undo
    /// makes: validate the whole typed config before any state moves, rewrap
    /// the sparkle and wallpaper assets it changed, stamp the revision on
    /// every key in `touched` and every key whose value moved, and record the
    /// undo of exactly this transition.
    fn admit_text(&mut self, next: String, touched: &BTreeSet<String>) -> ConfigPatchResult {
        if next == self.text {
            return ConfigPatchResult::Unchanged {
                snapshot: self.snapshot(),
            };
        }

        let next_config = match parse_config(&next) {
            Ok(config) => config,
            Err(message) => return self.rejected(message),
        };
        let after_values = match parse_values(&next) {
            Ok(values) => values,
            Err(message) => return self.rejected(message),
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

    /// Conditional undo: it succeeds only if every value written by the target patch is still current.
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
            // Still the patch's result: the text before it, exactly, whatever
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
        let after = parse_values(&text)?;
        let before_config = Arc::clone(&self.config);
        let after_config = Arc::new(parse_config(&text)?);
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

fn parse_config(text: &str) -> Result<crate::app_config::Config, String> {
    crate::app_config::Config::parse(text)
        .map_err(|error| format!("aterm.toml is not a valid aterm config: {error}"))
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

/// Why `aterm.toml` at `path` could not be read, naming the file once: the host's sentence without its internal stage tag.
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

fn parse_values(text: &str) -> Result<BTreeMap<String, String>, String> {
    let document = text
        .parse::<aterm_toml::edit::DocumentMut>()
        .map_err(|error| format!("existing aterm.toml is not valid TOML: {error}"))?;
    let mut values = BTreeMap::new();
    for (key, item) in document.iter() {
        // Full-depth dotted projection, mirroring `prefs::apply_prefs_edits`, so Settings
        project_item(&mut values, &join_config_key_path("", key), item);
    }
    Ok(values)
}

/// Whether `merged` reads as `current` with the patch from `before` to
/// `after` taken back out: every leaf the patch moved as `before` held it,
/// every other leaf as `current` holds it ([`leaf_values`]).
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

/// The values of `text`'s leaves — every key whose value is not a table —
/// under their dotted paths, as [`parse_values`] names them.
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

/// `current` with the change from `before` to `after` taken back out, line
/// for line: the three-way merge of `after` (the base both texts grew from),
/// `before` (what the undo wants there) and `current` (the file now, with a
/// later change of its own).
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

/// The shortest edit from `base` to `other` as hunks in base order (Myers'
/// O((N+M)D) greedy diff over whole lines).
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

/// Insert `item`'s value at `path` and, for header or inline tables, recurse into
/// each child under `path.child`.
fn project_item(values: &mut BTreeMap<String, String>, path: &str, item: &aterm_toml::edit::Item) {
    if let Some(table) = item.as_table_like() {
        for (child, child_item) in table.iter() {
            project_item(values, &join_config_key_path(path, child), child_item);
        }
    }
    values.insert(path.to_string(), semantic_item(item));
}

/// How `raw`, written for `key`, reads among [`ConfigSnapshot::values`] —
/// the projection a patch's expectation is compared in (`12` for a typed
/// `12.0`) — or `None` where the writer refuses it.
fn holds(key: &str, held: Option<&String>, expected: Option<&str>) -> bool {
    match (held, expected) {
        (None, None) => true,
        (Some(held), Some(expected)) => {
            held == expected || written_value(key, expected).as_ref() == Some(held)
        }
        (None, Some(_)) | (Some(_), None) => false,
    }
}

/// The write a patch makes ([`planned_write`]): the edits the writer takes, and the text it makes of them.
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
    let held = parse_values(text)?;
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
mod tests {
    use super::*;

    fn unique_config_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "aterm-native-config-service-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn edit(
        base_revision: u64,
        key: &str,
        expected: ExpectedValue,
        value: Option<&str>,
    ) -> ConfigPatchRequest {
        ConfigPatchRequest {
            base_revision,
            edits: vec![ConfigKeyEdit {
                key: key.into(),
                expected,
                value: value.map(str::to_owned),
            }],
        }
    }

    /// A patch whose base was never published (zero, or the revision the write would
    /// produce) is refused before anything moves.
    #[test]
    fn a_base_revision_the_service_never_published_is_refused_without_a_write() {
        let mut service =
            VersionedConfigService::new("theme = \"Nord\"\n".into()).expect("valid config");
        let before = service.snapshot();
        for base in [0, before.revision + 1] {
            assert!(matches!(
                service.patch(edit(
                    base,
                    "theme",
                    ExpectedValue::Exact(Some("Nord".into())),
                    None,
                )),
                ConfigPatchResult::Rejected { message, .. } if message == "invalid base revision"
            ));
            assert_eq!(service.snapshot(), before);
        }
        assert!(matches!(
            service.patch(edit(
                before.revision,
                "theme",
                ExpectedValue::Exact(Some("Nord".into())),
                None,
            )),
            ConfigPatchResult::Applied { .. }
        ));
    }

    /// `aterm ctl settings set cursor_trail_style …` and a Settings re-commit
    /// reduce through this service: an alias released configs carry (`rainbow
    /// kitty`), a hidden canonical (`classic mono`) and the Settings label
    /// `lightning` all apply and save as their canonical name; a near miss is
    /// refused without a write.
    #[test]
    fn settings_set_accepts_trail_aliases() {
        for (input, written) in [
            ("nyan", "rainbow kitty flying"),
            ("nyan rainbow", "rainbow kitty flying"),
            ("lightning", "laser"),
            ("rainbow dog", "rainbow dog pet"),
            ("classic mono", "classic mono"),
            ("classic lumen", "classic mono"),
            ("rainbow kitty flat", "rainbow kitty flat"),
            ("beam", "beam"),
        ] {
            let mut service = VersionedConfigService::new(String::new()).unwrap();
            let base = service.snapshot().revision;
            let ConfigPatchResult::Applied { snapshot, .. } = service.patch(edit(
                base,
                crate::prefs::EDIT_CURSOR_TRAIL_STYLE,
                ExpectedValue::Any,
                Some(input),
            )) else {
                panic!("{input:?} must apply");
            };
            assert_eq!(
                snapshot.config.cursor_trail_style.as_deref(),
                Some(written),
                "{input:?}"
            );
        }
        let mut service = VersionedConfigService::new(String::new()).unwrap();
        let before = service.snapshot();
        assert!(matches!(
            service.patch(edit(
                before.revision,
                crate::prefs::EDIT_CURSOR_TRAIL_STYLE,
                ExpectedValue::Any,
                Some("lightnin"),
            )),
            ConfigPatchResult::Rejected { .. }
        ));
        assert_eq!(service.snapshot(), before);
    }

    #[test]
    fn byte_equal_external_observation_refreshes_assets_and_analysis_generation() {
        let dir = unique_config_dir("equal-text-assets");
        let sprite = dir.join("nyan.png");
        let png = |rgba: &[u8]| crate::app_introspect::encode_rgba8_png(rgba, 2, 1).unwrap();
        std::fs::write(
            &sprite,
            png(&[0x22, 0x66, 0xee, 0xff, 0xee, 0x66, 0x22, 0xff]),
        )
        .unwrap();
        let text = format!(
            "trail_sound_volume = nan\ncursor_nyan_sprite = {:?}\n",
            sprite.to_string_lossy()
        );
        let mut service = VersionedConfigService::new(text.clone()).unwrap();
        let initial = service.snapshot();
        let initial_fp = initial.assets.kitty_sprite.fingerprint();

        std::fs::write(
            &sprite,
            png(&[0xee, 0x22, 0x66, 0xff, 0x22, 0xee, 0x66, 0xff]),
        )
        .unwrap();
        let refreshed = service.replace_external(text.clone()).unwrap();
        assert_eq!(refreshed.text, initial.text, "config bytes stay identical");
        assert!(refreshed.revision > initial.revision);
        assert!(refreshed.analysis_generation > initial.analysis_generation);
        assert_ne!(refreshed.assets.kitty_sprite.fingerprint(), initial_fp);

        let unchanged = service.replace_external(text.clone()).unwrap();
        assert_eq!(unchanged.revision, refreshed.revision);
        assert!(unchanged.analysis_generation > refreshed.analysis_generation);
        assert_eq!(unchanged.assets, refreshed.assets);

        let config_path = dir.join("aterm.toml");
        let fleet_path = dir.join(aterm_agent::ai::config::FLEET_FILE);
        std::fs::write(&config_path, &text).unwrap();
        let observe = |service: &mut VersionedConfigService| {
            let observation = VersionedConfigService::observe_path(&config_path, false).unwrap();
            let mut prepared = VersionedConfigService::prepare_observation(
                observation,
                Arc::clone(&service.snapshot().assets.themes),
            )
            .unwrap();
            prepared.config.ai = crate::app_config::AiPolicy::read_beside(&text, Some(&dir));
            service.synchronize_prepared_observation(prepared).unwrap()
        };
        let absent = observe(&mut service);
        assert_eq!(absent.config.ai.settings().preferred.0.id, "claude");

        std::fs::write(&fleet_path, "schema = 1\npreferred = \"codex\"\n").unwrap();
        let resolved = observe(&mut service);
        assert_eq!(resolved.text, absent.text);
        assert_eq!(resolved.semantic_values, absent.semantic_values);
        assert_eq!(resolved.assets, absent.assets);
        assert_eq!(
            resolved.config.ai.settings().preferred.0.id,
            "codex",
            "same-text worker admission publishes the newly resolved policy"
        );
        assert_eq!(resolved.config.ai.fleet_paths.first(), Some(&fleet_path));
        assert_eq!(resolved.revision, absent.revision + 1);
        assert!(resolved.analysis_generation > absent.analysis_generation);

        let repeated = observe(&mut service);
        assert_eq!(repeated.revision, resolved.revision);
        assert!(repeated.analysis_generation > resolved.analysis_generation);
        assert!(repeated.config.trail_sound_volume.unwrap().is_nan());
        assert_eq!(repeated.config.ai, resolved.config.ai);
        assert_eq!(
            service.restore_durable_snapshot().unwrap().config.ai,
            resolved.config.ai,
            "rollback keeps the admitted resolved policy"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn startup_service_publishes_sparkle_consumers_only_after_exact_preparation() {
        let pack = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../aterm-effects/toy-packs/community/tiny-triumphs/pack.toml");
        let text = format!(
            "[sparkle_words]\ntoy_packs = [{:?}]\n",
            pack.to_string_lossy()
        );
        let mut service = VersionedConfigService::new(text).unwrap();
        let preliminary = service.snapshot();
        assert!(preliminary.assets.sparkle_spec_consumers.is_none());

        let prepared = preliminary.config.prepare_path_feed_generation();
        let exact = prepared.sparkle.consumer_capabilities();
        assert!(
            exact.sparkle_or_starburst_burst && exact.rainbow_ink && exact.twotone_ink,
            "negative control: the checked-in pack has shared-setting consumers"
        );
        service.complete_startup_path_generation(exact);
        let published = service.snapshot();
        assert_eq!(published.revision, preliminary.revision);
        assert_eq!(
            published.assets.sparkle_spec_consumers.as_deref(),
            Some(&exact)
        );

        let restored = service.restore_durable_snapshot().unwrap();
        assert_eq!(
            restored.assets.sparkle_spec_consumers.as_deref(),
            Some(&exact),
            "the completed startup generation is also the durable rollback generation"
        );
    }

    #[test]
    fn sparkle_patch_never_carries_an_authoritative_consumer_verdict_across_revisions() {
        let mut service = VersionedConfigService::new(
            "serious_mode = false\n[sparkle_words]\nenabled = false\n".to_string(),
        )
        .unwrap();
        let initial = service.snapshot();
        let prepared = initial.config.prepare_path_feed_generation();
        let exact = prepared.sparkle.consumer_capabilities();
        service.complete_startup_path_generation(exact);
        let exact_snapshot = service.snapshot();
        let exact_assets = Arc::clone(&exact_snapshot.assets);
        assert_eq!(
            exact_snapshot.assets.sparkle_spec_consumers.as_deref(),
            Some(&exact)
        );

        let unrelated = service.patch(edit(
            exact_snapshot.revision,
            "serious_mode",
            ExpectedValue::Exact(Some("false".to_string())),
            Some("true"),
        ));
        let ConfigPatchResult::Applied {
            snapshot: unrelated,
            ..
        } = unrelated
        else {
            panic!("unrelated patch must apply")
        };
        assert!(
            Arc::ptr_eq(&unrelated.assets, &exact_assets),
            "a non-sparkle patch preserves the exact admitted asset generation"
        );

        let sparkle = service.patch(edit(
            unrelated.revision,
            "sparkle_words.enabled",
            ExpectedValue::Exact(Some("false".to_string())),
            None,
        ));
        let ConfigPatchResult::Applied {
            snapshot: sparkle, ..
        } = sparkle
        else {
            panic!("sparkle patch must apply")
        };
        assert!(sparkle.assets.sparkle_spec_consumers.is_none());
        assert!(!Arc::ptr_eq(&sparkle.assets, &exact_assets));
        assert!(Arc::ptr_eq(
            &sparkle.assets.trail_style,
            &exact_assets.trail_style
        ));
        assert!(Arc::ptr_eq(&sparkle.assets.themes, &exact_assets.themes));
    }

    #[test]
    fn unrelated_external_edit_reuses_asset_arc_and_source_change_is_negative_control() {
        let dir = unique_config_dir("external-asset-reuse");
        let first = dir.join("first.png");
        let second = dir.join("second.png");
        let first_rgba = [0x22, 0x66, 0xee, 0xff, 0xee, 0x66, 0x22, 0xff];
        let first_png = crate::app_introspect::encode_rgba8_png(&first_rgba, 2, 1).unwrap();
        std::fs::write(&first, first_png).unwrap();
        let first_source = first.to_string_lossy().into_owned();
        let mut service = VersionedConfigService::new(format!(
            "theme = \"Nord\"\ncursor_nyan_sprite = {first_source:?}\n"
        ))
        .unwrap();
        let initial = service.snapshot();
        let initial_fp = initial.assets.kitty_sprite.fingerprint();

        std::fs::write(&first, b"not a PNG").unwrap();
        let unrelated = service
            .replace_external(format!(
                "theme = \"Dracula\"\ncursor_nyan_sprite = {first_source:?}\n"
            ))
            .unwrap();
        assert!(Arc::ptr_eq(&unrelated.assets, &initial.assets));
        assert_eq!(unrelated.assets.kitty_sprite.fingerprint(), initial_fp);

        let second_rgba = [0xee, 0x22, 0x66, 0xff, 0x22, 0xee, 0x66, 0xff];
        let second_png = crate::app_introspect::encode_rgba8_png(&second_rgba, 2, 1).unwrap();
        std::fs::write(&second, second_png).unwrap();
        let second_source = second.to_string_lossy().into_owned();
        let changed = service
            .replace_external(format!(
                "theme = \"Dracula\"\ncursor_nyan_sprite = {second_source:?}\n"
            ))
            .unwrap();
        assert!(!Arc::ptr_eq(&changed.assets, &unrelated.assets));
        assert_eq!(
            changed.assets.kitty_sprite.source_id(),
            Some(second_source.as_str())
        );
        assert_ne!(changed.assets.kitty_sprite.fingerprint(), initial_fp);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn background_theme_catalog_refresh_publishes_one_atomic_snapshot_revision() {
        let mut service = VersionedConfigService::new("theme = \"Work\"\n".into()).unwrap();
        let initial = service.snapshot();
        assert!(initial.assets.themes.resolve("Work").is_err());

        let scheme = aterm_types::scheme::builtin("Dracula").unwrap();
        let themes =
            crate::app_config::ThemeCatalog::from_schemes([("Work".to_string(), scheme.clone())]);
        let refreshed = service.replace_theme_catalog(themes);
        assert_eq!(refreshed.text, initial.text);
        assert_eq!(refreshed.revision, initial.revision + 1);
        assert!(!Arc::ptr_eq(&refreshed.assets, &initial.assets));
        assert_eq!(refreshed.assets.themes.resolve("Work"), Ok(scheme));

        let duplicate = service.replace_theme_catalog(Arc::new((*refreshed.assets.themes).clone()));
        assert_eq!(duplicate.revision, refreshed.revision);
        assert!(Arc::ptr_eq(&duplicate.assets, &refreshed.assets));
        assert!(duplicate.analysis_generation > refreshed.analysis_generation);
    }

    #[test]
    fn observation_rejects_non_utf8_oversize_and_non_file_inputs() {
        let dir = unique_config_dir("observation-errors");
        let path = dir.join("aterm.toml");
        std::fs::write(&path, [0xff]).unwrap();
        let utf8_error = VersionedConfigService::observe_path(&path, false).unwrap_err();
        assert!(utf8_error.contains("not valid UTF-8"), "{utf8_error}");

        let file = std::fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&path)
            .unwrap();
        file.set_len(MAX_CONFIG_FILE_BYTES as u64 + 1).unwrap();
        let oversize = VersionedConfigService::observe_path(&path, false).unwrap_err();
        assert!(oversize.contains("exceeds"), "{oversize}");
        let startup = match VersionedConfigService::load_path(&path) {
            Ok(_) => panic!("startup must reject a config beyond the shared cap"),
            Err(error) => error,
        };
        assert!(startup.contains("exceeds"), "{startup}");

        let not_file = VersionedConfigService::observe_path(&dir, false).unwrap_err();
        assert!(not_file.contains("not a regular file"), "{not_file}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn reconciliation_gate_stays_closed_until_a_stable_valid_disk_generation_is_admitted() {
        let dir = unique_config_dir("reconciliation-gate");
        let path = dir.join("aterm.toml");
        std::fs::write(&path, "serious_mode = false\n").unwrap();
        let mut service = VersionedConfigService::load_path(&path).unwrap();
        service.mark_reconciliation_required();
        assert!(service.reconciliation_required());

        std::fs::write(&path, "serious_mode = [\n").unwrap();
        assert!(service.synchronize_from_disk().is_err());
        assert!(
            service.reconciliation_required(),
            "a malformed observation cannot reopen structured persistence"
        );

        std::fs::write(&path, "serious_mode = true\n").unwrap();
        let admitted = service.synchronize_from_disk().unwrap();
        assert!(!service.reconciliation_required());
        assert_eq!(
            admitted
                .values()
                .unwrap()
                .get(crate::prefs::EDIT_SERIOUS_MODE)
                .map(String::as_str),
            Some("true")
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn unparsed_startup_binding_cannot_authorize_a_structured_overwrite() {
        let dir = unique_config_dir("unparsed-binding");
        let path = dir.join("aterm.toml");
        let malformed = "theme = [\n";
        std::fs::write(&path, malformed).unwrap();
        let observation = VersionedConfigService::observe_path(&path, false).unwrap();
        let mut service = VersionedConfigService::new(String::new()).unwrap();
        service
            .bind_unparsed_disk_baseline(observation.baseline)
            .unwrap();
        let base = service.snapshot().revision;
        let ConfigPatchResult::Applied { snapshot, .. } = service.patch(edit(
            base,
            "theme",
            ExpectedValue::Exact(None),
            Some("Nord"),
        )) else {
            panic!("valid candidate should reduce before durable preflight");
        };
        let plan = service.persistence_plan(snapshot);
        assert!(plan.baseline.is_none());
        assert_eq!(plan.logical_path.as_deref(), Some(path.as_path()));

        assert!(matches!(
            crate::prefs::save_prefs_snapshot(&plan),
            crate::prefs::SaveOutcome::Conflict { message, .. }
                if message.contains("changed on disk")
        ));
        service.restore_durable_snapshot().unwrap();
        assert_eq!(service.snapshot().text.as_ref(), "");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), malformed);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn structured_snapshot_refuses_a_retargeted_config_symlink() {
        use std::os::unix::fs::symlink;

        let dir = unique_config_dir("structured-retarget");
        let first = dir.join("first.toml");
        let second = dir.join("second.toml");
        let logical = dir.join("aterm.toml");
        std::fs::write(&logical, "theme = \"Default\"\n").unwrap();
        std::fs::write(&second, "theme = \"Dracula\"\n").unwrap();
        let mut service = VersionedConfigService::load_path(&logical).unwrap();
        let base = service.snapshot().revision;
        let result = service.patch(edit(
            base,
            "theme",
            ExpectedValue::Exact(Some("Default".to_string())),
            Some("Nord"),
        ));
        let ConfigPatchResult::Applied { snapshot, .. } = result else {
            panic!("valid patch must apply before persistence");
        };
        let plan = service.persistence_plan(snapshot);
        std::fs::rename(&logical, &first).unwrap();
        symlink(&second, &logical).unwrap();

        assert!(matches!(
            crate::prefs::save_prefs_snapshot(&plan),
            crate::prefs::SaveOutcome::Conflict { message, .. }
                if message.contains("changed while saving")
        ));
        assert_eq!(
            std::fs::read_to_string(&first).unwrap(),
            "theme = \"Default\"\n"
        );
        assert_eq!(
            std::fs::read_to_string(&second).unwrap(),
            "theme = \"Dracula\"\n"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A TYPE VALUE THE THEME SETS OTHERWISE IS KEPT, EVEN AT ATERM'S DEFAULT:
    /// under a theme that sets the line height (1.15), `line_height = 1.0`
    /// is what keeps the theme's off, so it stays written, whether the theme
    /// is already in aterm.toml or comes in the same patch (Keep colours
    /// only writes the theme and its pins together).
    #[test]
    fn a_default_over_a_themes_type_value_stays_written() {
        let catalog = |extra: &str| {
            let text = format!("foreground = #e0e0e0\nbackground = #101418\n{extra}");
            let parsed = aterm_types::scheme::parse_theme_str(&text).expect("a theme");
            crate::app_config::ThemeCatalog::from_parsed([("Studio".to_string(), parsed)])
        };
        let default = crate::prefs::editable_fields(&crate::app_config::Config::default())
            .into_iter()
            .find(|field| field.key == crate::prefs::EDIT_LINE_HEIGHT)
            .and_then(|field| crate::prefs::unset_literal(&field))
            .expect("line height has a default");
        for (source, with_theme, extra, kept) in [
            ("theme = \"Studio\"\n", false, "line_height = 1.15\n", true),
            ("", true, "line_height = 1.15\n", true),
            ("", false, "line_height = 1.15\n", false),
            ("theme = \"Studio\"\n", false, "", false),
        ] {
            let mut service =
                VersionedConfigService::new_with_themes(source.into(), catalog(extra)).unwrap();
            let mut edits = vec![ConfigKeyEdit {
                key: crate::prefs::EDIT_LINE_HEIGHT.into(),
                expected: ExpectedValue::Any,
                value: Some(default.clone()),
            }];
            if with_theme {
                edits.push(ConfigKeyEdit {
                    key: crate::prefs::EDIT_THEME.into(),
                    expected: ExpectedValue::Any,
                    value: Some("Studio".into()),
                });
            }
            service.patch(ConfigPatchRequest {
                base_revision: service.snapshot().revision,
                edits,
            });
            assert_eq!(
                service
                    .value(crate::prefs::EDIT_LINE_HEIGHT)
                    .unwrap()
                    .is_some(),
                kept,
                "line_height = {default} over {source:?} (theme in the patch: {with_theme}, theme sets {extra:?})"
            );
        }
    }

    /// AN EXPECTATION NAMES A VALUE, NOT ITS SPELLING: a patch expecting
    /// `line_height` at `1.0` lands over the file's `1` (the writer's
    /// spelling of the `1.0` a press on its way chose), as does one
    /// expecting the exact text.
    #[test]
    fn an_expectation_names_a_value_not_its_spelling() {
        let patch = |source: &str, expected: Option<&str>| {
            let mut service = VersionedConfigService::new(source.into()).unwrap();
            let base = service.snapshot().revision;
            service.patch(edit(
                base,
                crate::prefs::EDIT_LINE_HEIGHT,
                ExpectedValue::Exact(expected.map(str::to_owned)),
                Some("1.3"),
            ))
        };
        for expected in ["1.0", "1"] {
            assert!(
                matches!(
                    patch("line_height = 1\n", Some(expected)),
                    ConfigPatchResult::Applied { .. }
                ),
                "expecting {expected:?}"
            );
        }
        for (source, expected) in [("line_height = 1\n", Some("1.2")), ("", Some("1.0"))] {
            assert!(
                matches!(patch(source, expected), ConfigPatchResult::Conflict { .. }),
                "{source:?} expecting {expected:?}"
            );
        }
    }

    /// A VALUE EQUAL TO ITS DEFAULT IS WRITTEN AS A REMOVAL, by every writer
    /// the service serves: for every Settings key with an unset value,
    /// writing that value to an empty file changes nothing, and writing it
    /// over another value removes the key.
    #[test]
    fn a_default_value_is_written_as_a_removal() {
        let fields = crate::prefs::editable_fields(&crate::app_config::Config::default());
        let mut checked = 0;
        for field in &fields {
            if crate::prefs::config_asset_source_key(field.key) {
                continue;
            }
            let Some(literal) = crate::prefs::unset_literal(field) else {
                continue;
            };
            let mut service = VersionedConfigService::new(String::new()).unwrap();
            let base = service.snapshot().revision;
            let result = service.patch(edit(base, field.key, ExpectedValue::Any, Some(&literal)));
            assert!(
                matches!(result, ConfigPatchResult::Unchanged { .. }),
                "{} = {literal:?} on an empty file: {result:?}",
                field.key
            );
            assert_eq!(&*service.snapshot().text, "", "{}", field.key);
            checked += 1;
        }
        assert!(checked > 50, "only {checked} keys have an unset value");

        let mut service =
            VersionedConfigService::new("window_theme = \"dark\"\nbidi = \"disabled\"\n".into())
                .unwrap();
        let base = service.snapshot().revision;
        assert!(matches!(
            service.patch(edit(base, "window_theme", ExpectedValue::Any, Some("Auto"))),
            ConfigPatchResult::Applied { .. }
        ));
        assert_eq!(&*service.snapshot().text, "bidi = \"disabled\"\n");

        let mut service = VersionedConfigService::new(String::new()).unwrap();
        let base = service.snapshot().revision;
        service.patch(edit(base, "theme", ExpectedValue::Any, Some("Default")));
        assert_eq!(service.value("theme").unwrap().as_deref(), Some("Default"));
        let base = service.snapshot().revision;
        service.patch(edit(base, "window_theme", ExpectedValue::Any, Some("dark")));
        assert_eq!(
            service.value("window_theme").unwrap().as_deref(),
            Some("dark")
        );

        let arcade = {
            let text = "foreground = #e0e0e0\nbackground = #101418\nfont_family = display:pixel\n";
            let parsed = aterm_types::scheme::parse_theme_str(text).expect("a theme");
            crate::app_config::ThemeCatalog::from_parsed([("Arcade".to_string(), parsed)])
        };
        for (source, kept) in [
            ("theme = \"Arcade\"\n", true),
            ("font_family = \"display:pixel\"\n", true),
            ("theme = \"Arcade\"\nfont_family = \"Menlo\"\n", false),
            ("", false),
        ] {
            let mut service =
                VersionedConfigService::new_with_themes(source.into(), Arc::clone(&arcade))
                    .unwrap();
            let base = service.snapshot().revision;
            service.patch(edit(
                base,
                crate::prefs::EDIT_DISPLAY_FONT,
                ExpectedValue::Any,
                Some("off"),
            ));
            assert_eq!(
                service
                    .value(crate::prefs::EDIT_DISPLAY_FONT)
                    .unwrap()
                    .is_some(),
                kept,
                "display_font = \"off\" over {source:?}"
            );
        }

        let laser = crate::app_config::Config::bloom_radius_style_default(
            crate::app_config::resolve_trail_style("laser"),
        );
        let kitty = crate::app_config::Config::bloom_radius_style_default(
            crate::app_config::resolve_trail_style(crate::prefs::DEFAULT_CURSOR_TRAIL_STYLE),
        );
        assert_ne!(laser, kitty, "the styles' own bloom sizes differ");
        let mut service =
            VersionedConfigService::new("cursor_trail_style = \"laser\"\n".into()).unwrap();
        let base = service.snapshot().revision;
        service.patch(edit(
            base,
            crate::prefs::EDIT_CURSOR_TRAIL_BLOOM_RADIUS,
            ExpectedValue::Any,
            Some(&kitty.to_string()),
        ));
        assert!(
            service
                .value(crate::prefs::EDIT_CURSOR_TRAIL_BLOOM_RADIUS)
                .unwrap()
                .is_some(),
            "the kitty's bloom size is not Laser's default"
        );
        let base = service.snapshot().revision;
        service.patch(edit(
            base,
            crate::prefs::EDIT_CURSOR_TRAIL_BLOOM_RADIUS,
            ExpectedValue::Any,
            Some(&laser.to_string()),
        ));
        assert_eq!(
            service
                .value(crate::prefs::EDIT_CURSOR_TRAIL_BLOOM_RADIUS)
                .unwrap(),
            None,
            "Laser's own bloom size is its default"
        );
    }

    /// AN ALIAS OF THE DEFAULT IS THE DEFAULT: the writer stores `kitty` as
    /// `rainbow kitty pet`, so the rule that writes a default as a removal
    /// judges the stored spelling, not the typed one.
    #[test]
    fn an_alias_of_the_default_style_is_written_as_a_removal() {
        let default = crate::app_config::default_cursor_trail_style();
        let aliases: Vec<&str> = aterm_effects::cursor_glow::spellings::TRAIL_STYLE_ALIASES
            .iter()
            .filter(|(_, target)| *target == default)
            .map(|(alias, _)| *alias)
            .collect();
        assert!(aliases.len() > 3, "{default} has aliases: {aliases:?}");
        for alias in aliases.iter().copied().chain(["Kitty", " rainbow kitty "]) {
            let mut service = VersionedConfigService::new(
                "cursor_trail_style = \"comet\"\nfont_px = 14\n".into(),
            )
            .unwrap();
            let base = service.snapshot().revision;
            service.patch(edit(
                base,
                crate::prefs::EDIT_CURSOR_TRAIL_STYLE,
                ExpectedValue::Any,
                Some(alias),
            ));
            assert_eq!(
                &*service.snapshot().text,
                "font_px = 14\n",
                "{alias:?} is the default style"
            );
            let mut service = VersionedConfigService::new(String::new()).unwrap();
            let base = service.snapshot().revision;
            let result = service.patch(edit(
                base,
                crate::prefs::EDIT_CURSOR_TRAIL_STYLE,
                ExpectedValue::Any,
                Some(alias),
            ));
            assert!(
                matches!(result, ConfigPatchResult::Unchanged { .. }),
                "{alias:?} over no style: {result:?}"
            );
        }
        let mut service = VersionedConfigService::new(String::new()).unwrap();
        let base = service.snapshot().revision;
        service.patch(edit(
            base,
            crate::prefs::EDIT_CURSOR_TRAIL_STYLE,
            ExpectedValue::Any,
            Some("nyan"),
        ));
        assert_eq!(
            service
                .value(crate::prefs::EDIT_CURSOR_TRAIL_STYLE)
                .unwrap()
                .as_deref(),
            Some("rainbow kitty flying")
        );
    }

    /// A VALUE THE KEY ALREADY HOLDS IS NO WRITE, whoever sends it (`aterm
    /// ctl settings set font_px 14` over the file's `font_px = 14`): the
    /// patch is Unchanged, no revision or Undo is minted, and the person's
    /// line keeps its spelling and its comment.
    #[test]
    fn a_value_the_key_already_holds_is_no_write() {
        let source = "font_px = 14  # projector\nbidi = \"disabled\"\n";
        let mut service = VersionedConfigService::new(source.into()).unwrap();
        let base = service.snapshot().revision;
        let result = service.patch(edit(base, "font_px", ExpectedValue::Any, Some("14")));
        assert!(
            matches!(result, ConfigPatchResult::Unchanged { .. }),
            "{result:?}"
        );
        assert_eq!(&*service.snapshot().text, source);
        assert_eq!(service.snapshot().revision, base);

        let result = service.patch(ConfigPatchRequest {
            base_revision: base,
            edits: vec![
                ConfigKeyEdit {
                    key: "font_px".into(),
                    expected: ExpectedValue::Any,
                    value: Some("14.0".into()),
                },
                ConfigKeyEdit {
                    key: "bidi".into(),
                    expected: ExpectedValue::Any,
                    value: Some("implicit".into()),
                },
            ],
        });
        let ConfigPatchResult::Applied { snapshot, .. } = result else {
            panic!("the bidi change applies: {result:?}");
        };
        assert!(
            snapshot.text.contains("font_px = 14  # projector\n"),
            "{:?}",
            snapshot.text
        );
        assert!(!snapshot.text.contains("bidi"), "{:?}", snapshot.text);

        let base = service.snapshot().revision;
        let result = service.patch(edit(base, "font_px", ExpectedValue::Any, Some("15")));
        assert!(
            matches!(result, ConfigPatchResult::Applied { .. }),
            "{result:?}"
        );
        assert_eq!(service.value("font_px").unwrap().as_deref(), Some("15"));
    }

    /// The file a reset-and-undo test starts from: root keys with a comment
    /// above one and an inline comment on another, tables in an authored
    /// order, a comment above a table's key and one under a table's last key.
    const AUTHORED: &str = "\
# note
font_px = 14  # projector
cursor_blink = false
scrollback_lines = 5000

[update]
enabled = false

[packages]
enabled = false

[machine]
universal_control = \"leave\"

[sparkle_words.feline]
# keep kitties quiet at work
enabled = false

[harness]
# supervision off on this laptop
enabled = false
";

    /// UNDO RESTORES THE FILE, NOT THE VALUES: a reset of several keys, then
    /// Undo, gives back the exact bytes (every key where it stood, every
    /// comment with the key it described, `14` still spelled `14`); Undo of
    /// that Undo gives back the reset text, and once more the original.
    #[test]
    fn undo_of_a_reset_restores_the_file_byte_for_byte() {
        let mut service = VersionedConfigService::new(AUTHORED.into()).unwrap();
        let base = service.snapshot().revision;
        let ConfigPatchResult::Applied { undo, .. } = service.reset_all(
            base,
            [
                "font_px",
                "cursor_blink",
                "update.enabled",
                "packages.enabled",
                "sparkle_words.feline.enabled",
                "harness.enabled",
            ]
            .map(str::to_string),
        ) else {
            panic!("the reset applies");
        };
        let reset = service.snapshot().text.to_string();
        assert!(
            !reset.contains("font_px") && !reset.contains("[update]"),
            "{reset}"
        );
        let ConfigPatchResult::Applied { undo: redo, .. } = service.undo(undo) else {
            panic!("the undo applies");
        };
        assert_eq!(&*service.snapshot().text, AUTHORED);
        let ConfigPatchResult::Applied { undo: again, .. } = service.undo(redo) else {
            panic!("the redo applies");
        };
        assert_eq!(&*service.snapshot().text, reset);
        assert!(matches!(
            service.undo(again),
            ConfigPatchResult::Applied { .. }
        ));
        assert_eq!(&*service.snapshot().text, AUTHORED);
    }

    /// After an unrelated change, Undo takes back only its own lines: the
    /// reset `font_px` returns where it stood with the comment above it and
    /// its inline comment, and a hand edit made since (a changed value and a
    /// new comment elsewhere) keeps its bytes.
    #[test]
    fn undo_after_an_unrelated_edit_reverts_only_its_own_lines() {
        let mut service = VersionedConfigService::new(AUTHORED.into()).unwrap();
        let base = service.snapshot().revision;
        let ConfigPatchResult::Applied { undo, .. } =
            service.patch(edit(base, "font_px", ExpectedValue::Any, None))
        else {
            panic!("the reset applies");
        };
        let reset = service.snapshot().text.to_string();
        let hand = reset.replace(
            "cursor_blink = false\n",
            "cursor_blink = true   # blinking for the demo\n",
        ) + "# my own note\n";
        service.replace_external(hand.clone()).unwrap();
        assert!(matches!(
            service.undo(undo),
            ConfigPatchResult::Applied { .. }
        ));
        let expected = AUTHORED.replace(
            "cursor_blink = false\n",
            "cursor_blink = true   # blinking for the demo\n",
        ) + "# my own note\n";
        assert_eq!(&*service.snapshot().text, expected);

        let base = service.snapshot().revision;
        let ConfigPatchResult::Applied { undo, .. } =
            service.patch(edit(base, "font_px", ExpectedValue::Any, None))
        else {
            panic!("the reset applies");
        };
        let edited = format!("font_px = 20\n{}", service.snapshot().text);
        service.replace_external(edited).unwrap();
        assert!(matches!(
            service.undo(undo),
            ConfigPatchResult::Conflict { keys, .. } if keys == ["font_px"]
        ));
    }

    /// The line merge Undo uses: the later change and the reverted change
    /// both stand where they touch different lines, and lines both touched
    /// are refused (the caller writes the values back instead).
    #[test]
    fn the_reverting_merge_keeps_both_changes_or_refuses() {
        let before = "a\nb\nc\nd\n";
        let after = "a\nB\nc\nd\n";
        assert_eq!(
            merge_reverting(after, before, "a\nB\nc\nD\n").as_deref(),
            Some("a\nb\nc\nD\n")
        );
        assert_eq!(
            merge_reverting(after, before, "x\na\nB\nc\nd\ny\n").as_deref(),
            Some("x\na\nb\nc\nd\ny\n")
        );
        assert_eq!(merge_reverting(after, before, "a\nBB\nc\nd\n"), None);
        assert_eq!(
            merge_reverting(after, before, after).as_deref(),
            Some(before)
        );
        assert_eq!(
            line_hunks(&["a\n", "b\n"], &["a\n", "b\n"]),
            Some(Vec::new())
        );
    }

    #[test]
    fn invalid_multi_key_patch_changes_nothing() {
        let mut service = VersionedConfigService::new("font_px = 14.0\n".into()).unwrap();
        let before = service.snapshot();
        let result = service.patch(ConfigPatchRequest {
            base_revision: before.revision,
            edits: vec![
                ConfigKeyEdit {
                    key: "font_px".into(),
                    expected: ExpectedValue::Any,
                    value: Some("16".into()),
                },
                ConfigKeyEdit {
                    key: "cursor_blink".into(),
                    expected: ExpectedValue::Any,
                    value: Some("definitely".into()),
                },
            ],
        });
        assert!(matches!(result, ConfigPatchResult::Rejected { .. }));
        assert_eq!(service.snapshot(), before);
    }

    #[test]
    fn unrelated_patch_reuses_asset_arc_and_source_changes_require_worker_admission() {
        use std::sync::Arc;

        let path = std::env::temp_dir().join(format!(
            "aterm-config-service-nyan-{}.png",
            std::process::id()
        ));
        let second_path = path.with_extension("second.png");
        let rgba = [0x22, 0x66, 0xee, 0xff, 0xee, 0x66, 0x22, 0xff];
        let png = crate::app_introspect::encode_rgba8_png(&rgba, 2, 1).unwrap();
        std::fs::write(&path, png).unwrap();
        let source = path.to_string_lossy().into_owned();
        let mut service = VersionedConfigService::new(format!(
            "theme = \"Nord\"\ncursor_nyan_sprite = {source:?}\n"
        ))
        .unwrap();
        let initial = service.snapshot();
        assert!(matches!(
            initial.assets.kitty_sprite,
            crate::app_config::KittySpriteAsset::Ready { .. }
        ));
        let initial_fp = initial.assets.kitty_sprite.fingerprint();

        std::fs::write(&path, b"not a PNG").unwrap();

        let ConfigPatchResult::Applied {
            snapshot: patched,
            undo,
        } = service.patch(edit(
            initial.revision,
            "theme",
            ExpectedValue::Exact(Some("Nord".to_string())),
            Some("Dracula"),
        ))
        else {
            panic!("patch must apply");
        };
        assert!(Arc::ptr_eq(&initial.assets, &patched.assets));
        assert!(matches!(
            patched.assets.kitty_sprite,
            crate::app_config::KittySpriteAsset::Ready { .. }
        ));

        let ConfigPatchResult::Applied {
            snapshot: undone, ..
        } = service.undo(undo)
        else {
            panic!("undo must apply");
        };
        assert!(Arc::ptr_eq(&patched.assets, &undone.assets));
        assert_eq!(service.value("theme").unwrap().as_deref(), Some("Nord"));

        let second_rgba = [0xee, 0x22, 0x66, 0xff, 0x22, 0xee, 0x66, 0xff];
        let second_png = crate::app_introspect::encode_rgba8_png(&second_rgba, 2, 1).unwrap();
        std::fs::write(&second_path, second_png).unwrap();
        let second_source = second_path.to_string_lossy().into_owned();
        let rejected = service.patch(edit(
            undone.revision,
            crate::prefs::EDIT_CURSOR_NYAN_SPRITE,
            ExpectedValue::Exact(Some(source.clone())),
            Some(&second_source),
        ));
        assert!(matches!(rejected, ConfigPatchResult::Rejected { .. }));
        assert_eq!(service.snapshot(), undone);

        let config_path = path.with_extension("toml");
        std::fs::write(
            &config_path,
            format!("theme = \"Nord\"\ncursor_nyan_sprite = {second_source:?}\n"),
        )
        .unwrap();
        let observation = VersionedConfigService::observe_path(&config_path, false).unwrap();
        let prepared = VersionedConfigService::prepare_observation(
            observation,
            Arc::clone(&undone.assets.themes),
        )
        .unwrap();
        let source_changed = service.synchronize_prepared_observation(prepared).unwrap();
        assert!(!Arc::ptr_eq(&undone.assets, &source_changed.assets));
        assert_eq!(
            source_changed.assets.kitty_sprite.source_id(),
            Some(second_source.as_str())
        );
        assert_ne!(source_changed.assets.kitty_sprite.fingerprint(), initial_fp);

        let rejected_reset = service.reset_all(
            source_changed.revision,
            [crate::prefs::EDIT_CURSOR_NYAN_SPRITE.to_string()],
        );
        assert!(matches!(rejected_reset, ConfigPatchResult::Rejected { .. }));
        assert_eq!(service.snapshot(), source_changed);

        std::fs::write(&config_path, "theme = \"Nord\"\n").unwrap();
        let observation = VersionedConfigService::observe_path(&config_path, false).unwrap();
        let prepared = VersionedConfigService::prepare_observation(
            observation,
            Arc::clone(&source_changed.assets.themes),
        )
        .unwrap();
        let reset = service.synchronize_prepared_observation(prepared).unwrap();
        assert!(!Arc::ptr_eq(&source_changed.assets, &reset.assets));
        assert!(matches!(
            reset.assets.kitty_sprite,
            crate::app_config::KittySpriteAsset::BuiltIn
        ));

        let missing = path.with_extension("missing.png");
        let external = service
            .replace_external(format!(
                "theme = \"External\"\ncursor_nyan_sprite = {:?}\n",
                missing.to_string_lossy()
            ))
            .expect("a bad optional sprite does not reject unrelated config");
        assert!(matches!(
            external.assets.kitty_sprite,
            crate::app_config::KittySpriteAsset::Invalid { .. }
        ));
        assert_eq!(service.value("theme").unwrap().as_deref(), Some("External"));
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(second_path);
        let _ = std::fs::remove_file(config_path);
    }

    /// An authored empty collection is still an authored value.
    #[test]
    fn undo_restores_authored_empty_collections() {
        for (key, authored) in [
            ("palette", "palette = []\n"),
            ("palette_overrides", "palette_overrides = {}\n"),
            ("palette_overrides", "[palette_overrides]\n"),
            (
                "palette_overrides",
                "[palette_overrides]\n# An authored empty section.\n",
            ),
            ("shell_args", "shell_args = []\n"),
            ("font_features", "font_features = []\n"),
        ] {
            let text = format!("font_px = 13.0\n{authored}");
            let expected: aterm_toml::Value = aterm_toml::from_str(&text).unwrap();
            let mut service = VersionedConfigService::new(text).unwrap();
            assert!(service.value(key).unwrap().is_some());
            let ConfigPatchResult::Applied { snapshot, undo } =
                service.reset_all(service.snapshot().revision, [key.to_string()])
            else {
                panic!("{authored}: clear must apply");
            };
            assert!(service.value(key).unwrap().is_none(), "{authored}");
            assert!(snapshot.text.contains("font_px = 13.0"));
            let ConfigPatchResult::Applied { snapshot, .. } = service.undo(undo) else {
                panic!("{authored}: Undo must restore the authored empty collection");
            };
            assert!(service.value(key).unwrap().is_some(), "{authored}");
            let restored: aterm_toml::Value = aterm_toml::from_str(&snapshot.text).unwrap();
            assert_eq!(
                restored, expected,
                "{authored}: typed value and unrelated setting survive"
            );
            let _: crate::app_config::Config = aterm_toml::from_str(&snapshot.text).unwrap();
        }
        for master in [false, true] {
            let original = format!("font_px = 13.0\ncursor_trail = {master}\n");
            let mut service = VersionedConfigService::new(original.clone()).unwrap();
            let ConfigPatchResult::Applied { snapshot, undo } = service.patch(edit(
                service.snapshot().revision,
                "cursor_trail_style",
                ExpectedValue::Exact(None),
                Some("comet"),
            )) else {
                panic!("style choice");
            };
            let chosen = snapshot.text.to_string();
            assert!(!snapshot.semantic_values.contains_key("cursor_trail"));
            let ConfigPatchResult::Applied { .. } = service.patch(edit(
                service.snapshot().revision,
                "font_px",
                ExpectedValue::Exact(Some("13".into())),
                Some("20"),
            )) else {
                panic!("unrelated later write");
            };
            let ConfigPatchResult::Applied {
                snapshot,
                undo: redo,
            } = service.undo(undo)
            else {
                panic!("the full inverse must survive an unrelated edit");
            };
            let expected = original.replace("font_px = 13.0", "font_px = 20");
            assert_eq!(
                parse_values(&snapshot.text).unwrap(),
                parse_values(&expected).unwrap()
            );
            let ConfigPatchResult::Applied { snapshot, .. } = service.undo(redo) else {
                panic!("Redo");
            };
            let expected = chosen.replace("font_px = 13.0", "font_px = 20");
            assert_eq!(
                parse_values(&snapshot.text).unwrap(),
                parse_values(&expected).unwrap()
            );
        }

        let source = "cursor_trail = false\ncursor_momentum_glow = true\n";
        let mut service = VersionedConfigService::new(source.into()).unwrap();
        let ConfigPatchResult::Applied { snapshot, undo } = service.patch(edit(
            service.snapshot().revision,
            "cursor_trail_style",
            ExpectedValue::Exact(None),
            Some("comet"),
        )) else {
            panic!("style choice");
        };
        let newer = service
            .replace_external(snapshot.text.replace(
                "cursor_momentum_glow = false",
                "cursor_momentum_glow = true",
            ))
            .unwrap();
        assert!(
            matches!(service.undo(undo), ConfigPatchResult::Conflict { keys, .. }
            if keys.contains(&"cursor_momentum_glow".to_string()))
        );
        assert_eq!(service.snapshot(), newer);

        let source = "cursor_trail = false\ncursor_trail_style = \"laser\"\n";
        let mut service = VersionedConfigService::new(source.into()).unwrap();
        let base = service.snapshot().revision;
        let newer = service
            .replace_external(format!("{source}cursor_momentum_glow = true\n"))
            .unwrap();
        assert!(matches!(service.patch(edit(base, "cursor_trail_style",
            ExpectedValue::Exact(Some("laser".into())), Some("comet"))),
            ConfigPatchResult::Conflict { keys, .. } if keys.contains(&"cursor_momentum_glow".to_string())));
        assert_eq!(service.snapshot(), newer);

        let mut service =
            VersionedConfigService::new("[matrix_rain]\nenabled = false\nfps = 37\n".into())
                .unwrap();
        let base = service.snapshot().revision;
        let ConfigPatchResult::Applied { undo, .. } = service.patch(edit(
            base,
            "matrix_rain.enabled",
            ExpectedValue::Exact(Some("false".into())),
            Some("true"),
        )) else {
            panic!("nested patch");
        };
        let ConfigPatchResult::Applied { .. } = service.patch(edit(
            base,
            "matrix_rain.fps",
            ExpectedValue::Exact(Some("37".into())),
            Some("41"),
        )) else {
            panic!("stale unrelated child rebases");
        };
        let ConfigPatchResult::Applied { snapshot, .. } = service.undo(undo) else {
            panic!("conditional Undo");
        };
        assert_eq!(
            snapshot
                .semantic_values
                .get("matrix_rain.enabled")
                .map(String::as_str),
            Some("false")
        );
        assert_eq!(
            snapshot
                .semantic_values
                .get("matrix_rain.fps")
                .map(String::as_str),
            Some("41")
        );
    }

    /// The value the service reports for a cleared palette (the slot table in
    /// either spelling, the `palette` array) is what Undo writes back, and it
    /// lands as the same TOML, so the file still loads.
    #[test]
    fn a_reported_palette_value_writes_back_as_toml() {
        for text in [
            "palette_overrides = { 12 = \"#88c0d0\", 3 = \"#112233\" }\n",
            "[palette_overrides]\n12 = \"#88c0d0\"\n3 = \"#112233\"\n",
        ] {
            let raw = parse_values(text).unwrap()["palette_overrides"].clone();
            let out = crate::prefs::apply_prefs_edits("", &[("palette_overrides", Some(raw))])
                .unwrap_or_else(|error| panic!("{text}: {error}"));
            let config: crate::app_config::Config = aterm_toml::from_str(&out).expect(&out);
            let slots = config.palette_overrides.expect(&out);
            assert_eq!(
                slots.get("12").map(String::as_str),
                Some("#88c0d0"),
                "{out}"
            );
            assert_eq!(slots.get("3").map(String::as_str), Some("#112233"), "{out}");
        }
        let raw =
            parse_values("palette = [\"#1d1f21\", \"#cc6666\"]\n").unwrap()["palette"].clone();
        let out = crate::prefs::apply_prefs_edits("", &[("palette", Some(raw))]).unwrap();
        let config: crate::app_config::Config = aterm_toml::from_str(&out).expect(&out);
        assert_eq!(
            config.palette,
            Some(vec!["#1d1f21".to_string(), "#cc6666".to_string()])
        );
        for bad in [
            "{ 300 = \"#88c0d0\" }",
            "{ 12 = \"nope\" }",
            "[]",
            "#88c0d0",
        ] {
            assert!(
                crate::prefs::apply_prefs_edits(
                    "",
                    &[("palette_overrides", Some(bad.to_string()))]
                )
                .is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn snapshot_key_identities_keep_literal_dots_distinct_from_nested_paths() {
        let source = r#""packages.include" = ["literal"]

[packages]
include = ["nested"]
"#;
        let values = parse_values(source).unwrap();
        assert_eq!(
            values.get(r#""packages.include""#).map(String::as_str),
            Some(r#"["literal"]"#),
            "one literal segment keeps its canonical quoted identity"
        );
        assert_eq!(
            values.get("packages.include").map(String::as_str),
            Some(r#"["nested"]"#),
            "the registered two-segment path remains independently addressable"
        );
        assert_eq!(
            join_config_key_path("", "packages.include"),
            r#""packages.include""#
        );
        assert_eq!(
            join_config_key_path("packages", "include"),
            "packages.include"
        );

        let mut service = VersionedConfigService::new(source.to_string()).unwrap();
        let revision = service.snapshot().revision;
        let result = service.patch(edit(
            revision,
            "packages.include",
            ExpectedValue::Exact(Some(r#"["nested"]"#.into())),
            None,
        ));
        let ConfigPatchResult::Applied { snapshot, .. } = result else {
            panic!("nested reset must apply without touching the literal key: {result:?}");
        };
        let after = snapshot.values().unwrap();
        assert!(!after.contains_key("packages.include"));
        assert_eq!(
            after.get(r#""packages.include""#).map(String::as_str),
            Some(r#"["literal"]"#),
            "a nested reset is lossless for the lookalike literal key"
        );
        assert!(
            snapshot
                .text
                .contains(r#""packages.include" = ["literal"]"#)
        );

        let mut service = VersionedConfigService::new(
            r#""packages.include" = ["literal-one"]
"#
            .to_string(),
        )
        .unwrap();
        let base = service.snapshot().revision;
        service
            .replace_external(
                r#""packages.include" = ["literal-two"]
"#
                .to_string(),
            )
            .unwrap();
        let result = service.patch(edit(
            base,
            "packages.include",
            ExpectedValue::Exact(None),
            None,
        ));
        let ConfigPatchResult::Unchanged { snapshot } = result else {
            panic!(
                "a literal-key change must not OCC-conflict with an absent nested reset: {result:?}"
            );
        };
        let values = snapshot.values().unwrap();
        assert_eq!(
            values.get(r#""packages.include""#).map(String::as_str),
            Some(r#"["literal-two"]"#)
        );
        assert!(!values.contains_key("packages.include"));
    }

    /// Depth-2 dotted keys behave like depth-1: `sparkle_words.profanity.enabled` gets
    /// its own entry, a stale expectation conflicts while the current one applies, and
    /// blank-revert removes the leaf and the tables it left bare at both depths.
    #[test]
    fn depth_two_dotted_key_projection_occ_and_blank_revert() {
        let values = parse_values(
            "[sparkle_words]\nenabled = true\n\n[sparkle_words.profanity]\nenabled = false\n",
        )
        .unwrap();
        assert_eq!(
            values
                .get("sparkle_words.profanity.enabled")
                .map(String::as_str),
            Some("false"),
            "a hand-set depth-2 leaf projects as an exact per-key entry"
        );
        assert_eq!(
            values.get("sparkle_words.enabled").map(String::as_str),
            Some("true")
        );
        assert!(
            values.contains_key("sparkle_words") && values.contains_key("sparkle_words.profanity"),
            "every intermediate table keeps its opaque whole-table entry"
        );
        let none = parse_values("font_px = 13.0\n").unwrap();
        assert!(
            !none.keys().any(|k| k.starts_with("sparkle_words")),
            "an absent tree projects nothing (not explicit)"
        );

        let mut service = VersionedConfigService::new(
            "[sparkle_words]\n\n[sparkle_words.profanity]\nenabled = false\n".into(),
        )
        .unwrap();
        let base = service.snapshot().revision;

        service
            .replace_external(
                "[sparkle_words]\n\n[sparkle_words.profanity]\nenabled = true\n".into(),
            )
            .unwrap();
        let stale = service.patch(edit(
            base,
            "sparkle_words.profanity.enabled",
            ExpectedValue::Exact(Some("false".into())),
            Some("false"),
        ));
        let ConfigPatchResult::Conflict { keys, .. } = stale else {
            panic!("stale depth-2 expectation must conflict, got {stale:?}");
        };
        assert_eq!(keys, vec!["sparkle_words.profanity.enabled".to_string()]);

        let rev = service.snapshot().revision;
        let applied = service.patch(edit(
            rev,
            "sparkle_words.profanity.enabled",
            ExpectedValue::Exact(Some("true".into())),
            Some("false"),
        ));
        let ConfigPatchResult::Applied { snapshot, .. } = applied else {
            panic!("current-revision depth-2 write applies, got {applied:?}");
        };
        assert!(snapshot.text.contains("enabled = false"));
        assert_eq!(
            service
                .value("sparkle_words.profanity.enabled")
                .unwrap()
                .as_deref(),
            Some("false")
        );

        let rev = service.snapshot().revision;
        let reverted = service.patch(edit(
            rev,
            "sparkle_words.profanity.enabled",
            ExpectedValue::Exact(Some("false".into())),
            None,
        ));
        let ConfigPatchResult::Applied { snapshot, .. } = reverted else {
            panic!("blank revert applies, got {reverted:?}");
        };
        assert!(
            !snapshot.text.contains("enabled"),
            "the leaf is removed: {:?}",
            snapshot.text
        );
        assert!(
            !snapshot.text.contains("[sparkle_words"),
            "bare tables go at both depths: {:?}",
            snapshot.text
        );
        assert_eq!(
            service.value("sparkle_words.profanity.enabled").unwrap(),
            None
        );
    }
}
