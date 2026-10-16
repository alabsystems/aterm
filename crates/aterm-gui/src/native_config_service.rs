// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Serialized, versioned preference transaction core for native Settings.
//!
//! The reducer is pure and deterministic. A host worker reads/writes the file and
//! feeds whole snapshots here; the service does OCC, per-key stale rebase, conditional
//! undo and one `aterm-toml` document transform per accepted patch. The UI never
//! writes config bytes directly.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::Path;
use std::sync::Arc;

const UNDO_LIMIT: usize = 64;
/// Admission budget shared by startup, watcher reloads and Manual's semantic
/// analysis, so startup never accepts a generation the watcher cannot fingerprint or
/// the editor cannot validate.
pub(crate) const MAX_CONFIG_FILE_BYTES: usize = 512 * 1024;

#[derive(Clone, PartialEq)]
pub(crate) struct ConfigSnapshot {
    pub(crate) revision: u64,
    /// Environment generation Manual diagnostics were last requested against. A stable
    /// file observation advances it even when `aterm.toml` is byte-identical, since
    /// referenced assets and installed fonts can change independently.
    pub(crate) analysis_generation: u64,
    pub(crate) text: Arc<str>,
    /// Parsed semantic projection produced before admission; Settings and process-wide
    /// policy clone this Arc instead of reparsing on the event loop.
    pub(crate) config: Arc<crate::app_config::Config>,
    pub(crate) semantic_values: Arc<BTreeMap<String, String>>,
    /// The values of `text` the harness's `[disk]` reader does not read as written
    /// ([`parse_disk_faults`]), parsed with `semantic_values`: Settings ▸ Modified asks
    /// them before it calls an authored key one with no effect.
    pub(crate) disk_faults: DiskFaults,
    /// Immutable non-text assets admitted with `text` at `revision`, shared as one outer
    /// Arc; consumers never re-resolve Trail manifests or the custom kitty sprite.
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

/// What the harness's `[disk]` reader does not read as written in one text
/// (`aterm_agent::harness::cli::disk_faults`).
pub(crate) type DiskFaults = Arc<[aterm_agent::harness::cli::DiskFault]>;

/// [`DiskFaults`] of `text`, computed with the text's semantic values
/// ([`parse_values`]) wherever they are: at load, on the host worker for a disk
/// observation, and on the reducer for a patch.
fn parse_disk_faults(text: &str) -> DiskFaults {
    aterm_agent::harness::cli::disk_faults(text).into()
}

impl ConfigSnapshot {
    /// Semantic top-level values as the service compares them for optimistic
    /// concurrency. Settings uses them for "Modified" and edit expectations; an
    /// effective default is presentation, not evidence the key exists in `aterm.toml`.
    pub(crate) fn values(&self) -> Result<BTreeMap<String, String>, String> {
        Ok((*self.semantic_values).clone())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ExpectedValue {
    /// No expectation beyond the patch's base revision. Settings always says
    /// what it expects; only tests write without an expectation.
    #[cfg(test)]
    Any,
    Exact(Option<String>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ConfigKeyEdit {
    pub(crate) key: String,
    pub(crate) expected: ExpectedValue,
    /// Raw semantic value. `None` removes the key and restores its default.
    pub(crate) value: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ConfigPatchRequest {
    pub(crate) base_revision: u64,
    pub(crate) edits: Vec<ConfigKeyEdit>,
}

/// Immutable handoff from the optimistic reducer to the sole durable config worker:
/// `expected_text` and `baseline` are the last synchronized disk generation, and
/// `snapshot` is the candidate to publish. Bundled so later service mutations cannot
/// change a queued commit's compare-and-swap authority.
#[derive(Clone, Debug)]
pub(crate) struct ConfigPersistencePlan {
    pub(crate) snapshot: ConfigSnapshot,
    pub(crate) expected_text: Arc<str>,
    pub(crate) logical_path: Option<std::path::PathBuf>,
    pub(crate) baseline: Option<crate::native_document_host::AtomicFileBaseline>,
}

/// One stable UTF-8 disk read plus the logical-target/file generation it came from,
/// carried through Manual refresh, strict parse and config-service admission so no
/// consumer reads the content a second time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ConfigDiskObservation {
    pub(crate) text: String,
    pub(crate) baseline: crate::native_document_host::AtomicFileBaseline,
}

/// Disk bytes plus every filesystem-backed asset and effect-feed runtime needed to
/// admit them on the event loop. Built on a host worker; admission and the font
/// worker only compute over this immutable generation.
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
}

/// Single-writer reducer. It is intended to live behind the process-global Config
/// service queue; callers may clone only immutable snapshots.
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
    #[cfg(test)]
    pub(crate) fn new(text: String) -> Result<Self, String> {
        Self::new_with_themes(text, crate::app_config::ThemeCatalog::empty())
    }

    /// Runtime constructor used before presentation. Theme/rainbow kitty discovery is
    /// bounded and parsed once here; Trail/Sparkle feeds stay pending for the startup
    /// worker. Tests use the deterministic test constructor instead.
    pub(crate) fn new_runtime(text: String) -> Result<Self, String> {
        Self::new_with_themes_mode(text, crate::app_config::ThemeCatalog::discover(), true)
    }

    #[cfg(test)]
    fn new_with_themes(
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

    /// Seed the process service from the watched user config path. A missing file is
    /// the valid empty document; unreadable or malformed input is reported so startup
    /// degrades without clobbering the file.
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
        let contents = read_config_file(path, allow_missing)?;
        let text = decode_config_bytes(&contents.bytes, path)?;
        Ok(ConfigDiskObservation {
            text,
            baseline: contents.baseline,
        })
    }

    /// Resolve one worker-observed generation before it crosses to the event loop. Runs
    /// on a host worker, since Trail manifests and a custom kitty sprite need file I/O.
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
            trail_packs: Arc::clone(&path_feeds.trail_packs),
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

    /// Install the path-backed effect generation prepared for startup before any view
    /// observes the service. Completes revision 1's preliminary asset catalog without
    /// advancing the revision.
    pub(crate) fn complete_startup_path_generation(
        &mut self,
        consumers: aterm_effects::spec::SpecConsumerCapabilities,
        trail_packs: Arc<crate::app_config::TrailPackCatalog>,
    ) {
        debug_assert_eq!(self.revision, 1);
        let assets = Arc::new(crate::app_config::ConfigAssetCatalog {
            trail_packs,
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

    /// Disk generation the service was initialized from. The watcher uses it to close
    /// the load→watch handoff: a differing first observation is a real edge.
    pub(crate) fn observed_disk_baseline(
        &self,
    ) -> Option<&crate::native_document_host::AtomicFileBaseline> {
        self.disk_baseline.as_ref()
    }

    /// A writer reported an unknown or externally won disk generation; until a stable
    /// observation is admitted, persistence plans must not reuse the old baseline.
    pub(crate) fn mark_reconciliation_required(&mut self) {
        self.disk_text_admitted = false;
        self.write_reconciliation_required = true;
    }

    pub(crate) fn reconciliation_required(&self) -> bool {
        self.write_reconciliation_required
    }

    /// Keep path/generation authority when startup read UTF-8 bytes but could not admit
    /// malformed TOML. Opening Manual supplies its config grant here; the service text
    /// stays unchanged until a valid durable Manual observation is admitted.
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

    /// Import the live durable file immediately, keeping revision/key stamps monotonic.
    /// Manual editor completion calls this directly instead of waiting for the watcher.
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

    /// [`synchronize_observation`] with an immutable asset catalog already
    /// resolved by the config/font worker from the same text generation.
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
        } else if assets != self.assets {
            self.revision = self.revision.saturating_add(1);
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

    /// Roll an optimistic candidate back to the last durable bytes when its worker
    /// commit fails and the current disk generation is not admissible (e.g. malformed
    /// external TOML). Stamps stay monotonic through the external-replacement reducer.
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

    /// Apply one all-or-nothing patch. A stale patch is accepted only when every touched
    /// key is unchanged since its base revision and every expectation still matches.
    pub(crate) fn patch(&mut self, request: ConfigPatchRequest) -> ConfigPatchResult {
        if request.base_revision == 0 || request.base_revision > self.revision {
            return self.rejected("invalid base revision");
        }
        if request.edits.is_empty() {
            return ConfigPatchResult::Unchanged {
                snapshot: self.snapshot(),
            };
        }
        let mut keys = BTreeSet::new();
        if request
            .edits
            .iter()
            .any(|edit| edit.key.trim().is_empty() || !keys.insert(edit.key.clone()))
        {
            return self.rejected("empty or duplicate preference key");
        }
        if let Some(edit) = request
            .edits
            .iter()
            .find(|edit| crate::prefs::config_asset_source_key(&edit.key))
        {
            return self.rejected(format!(
                "{} names filesystem-backed assets; edit it in Manual so a worker can validate and admit text plus assets atomically",
                edit.key
            ));
        }

        let current = self.values.clone();
        let mut conflicts = Vec::new();
        for edit in &request.edits {
            let changed_after_base = self
                .key_revision
                .get(&edit.key)
                .is_some_and(|revision| *revision > request.base_revision);
            let expectation_failed = match &edit.expected {
                #[cfg(test)]
                ExpectedValue::Any => false,
                ExpectedValue::Exact(expected) => current.get(&edit.key).cloned() != *expected,
            };
            if changed_after_base || expectation_failed {
                conflicts.push(edit.key.clone());
            }
        }
        if !conflicts.is_empty() {
            return ConfigPatchResult::Conflict {
                snapshot: self.snapshot(),
                keys: conflicts,
            };
        }

        let before = request
            .edits
            .iter()
            .map(|edit| (edit.key.clone(), current.get(&edit.key).cloned()))
            .collect::<Vec<_>>();
        let borrowed = request
            .edits
            .iter()
            .map(|edit| (edit.key.as_str(), edit.value.clone()))
            .collect::<Vec<_>>();
        let next = match crate::prefs::apply_prefs_edits(&self.text, &borrowed) {
            Ok(next) => next,
            Err(error) => return self.rejected(error.to_string()),
        };
        if next == self.text {
            return ConfigPatchResult::Unchanged {
                snapshot: self.snapshot(),
            };
        }

        // Validate the full typed config before advancing any state. Host-backed
        // source keys were rejected above (Manual's worker admits text and decoded
        // assets together), so structured toggle/theme/font changes reuse the admitted
        // Trail/rainbow kitty/theme assets with no event-thread I/O. A sparkle-table
        // change below rewraps them with a pending consumer projection.
        let next_config = match parse_config(&next) {
            Ok(config) => config,
            Err(message) => return self.rejected(message),
        };

        // The consumer projection belongs to the `[sparkle_words]` generation that
        // produced it, and a structured patch cannot rebuild lexicon/Toy Pack
        // capabilities on the event thread. A changed sparkle table therefore
        // publishes `None` (preliminary); the config/font worker fills in the
        // projection from the durable generation.
        if self.config.sparkle_words != next_config.sparkle_words
            && self.assets.sparkle_spec_consumers.is_some()
        {
            self.assets = Arc::new(crate::app_config::ConfigAssetCatalog {
                trail_packs: Arc::clone(&self.assets.trail_packs),
                kitty_sprite: self.assets.kitty_sprite.clone(),
                wallpaper: self.assets.wallpaper.clone(),
                themes: Arc::clone(&self.assets.themes),
                sparkle_spec_consumers: None,
            });
        }
        // Wallpaper is structured-writable (the Settings file picker writes it),
        // unlike the manual-only asset-source keys, so this lane re-resolves the image
        // inline; otherwise the catalog would serve the previous picture.
        if self.config.wallpaper != next_config.wallpaper {
            self.assets = Arc::new(crate::app_config::ConfigAssetCatalog {
                trail_packs: Arc::clone(&self.assets.trail_packs),
                kitty_sprite: self.assets.kitty_sprite.clone(),
                wallpaper: crate::app_config::resolve_wallpaper_asset(
                    next_config.wallpaper.as_deref(),
                ),
                themes: Arc::clone(&self.assets.themes),
                sparkle_spec_consumers: self.assets.sparkle_spec_consumers.clone(),
            });
        }

        self.revision = self.revision.saturating_add(1);
        self.text = next;
        self.config = Arc::new(next_config);
        for key in &keys {
            self.key_revision.insert(key.clone(), self.revision);
        }
        let after_values = match parse_values(&self.text) {
            Ok(values) => values,
            Err(message) => return self.rejected(message),
        };
        self.values.clone_from(&after_values);
        self.disk_faults = parse_disk_faults(&self.text);
        let after = request
            .edits
            .iter()
            .map(|edit| (edit.key.clone(), after_values.get(&edit.key).cloned()))
            .collect();
        let token = UndoToken(self.next_undo.max(1));
        self.next_undo = self.next_undo.saturating_add(1);
        self.undo.push_back(UndoRecord {
            token,
            before,
            after,
        });
        while self.undo.len() > UNDO_LIMIT {
            self.undo.pop_front();
        }
        ConfigPatchResult::Applied {
            snapshot: self.snapshot(),
            undo: token,
        }
    }

    /// Reset a complete named set in one transform/revision. Consumers pass the
    /// editable schema keys; unrelated hand-authored configuration survives.
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

    /// Conditional undo: it succeeds only if every value written by the target patch
    /// is still current. Later unrelated changes are preserved.
    pub(crate) fn undo(&mut self, token: UndoToken) -> ConfigPatchResult {
        let Some(record) = self
            .undo
            .iter()
            .find(|record| record.token == token)
            .cloned()
        else {
            return self.rejected("undo token expired");
        };
        let edits = record
            .before
            .iter()
            .zip(&record.after)
            .map(|((key, before), (after_key, after))| {
                debug_assert_eq!(key, after_key);
                ConfigKeyEdit {
                    key: key.clone(),
                    expected: ExpectedValue::Exact(after.clone()),
                    value: before.clone(),
                }
            })
            .collect();
        self.patch(ConfigPatchRequest {
            base_revision: self.revision,
            edits,
        })
    }

    /// Accept a whole watcher/file snapshot. Changed keys are revision-stamped so a
    /// queued stale UI patch can rebase across unrelated external changes only.
    #[cfg(test)]
    pub(crate) fn replace_external(&mut self, text: String) -> Result<ConfigSnapshot, String> {
        let before = self.values.clone();
        let after = parse_values(&text)?;
        let before_config = Arc::clone(&self.config);
        let after_config = Arc::new(parse_config(&text)?);
        // A watcher/durable observation is also an environment observation, and a
        // byte-equal one is the refresh signal for a touched manifest/sprite. When the
        // text changed, an unrelated edit reuses the asset Arc with no event-thread
        // I/O; only a source projection change resolves a new catalog. Manual
        // diagnostics recheck fonts and host state via the separate generation.
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

    /// Admit a catalog parsed by the background theme-directory watcher. Text, Trail
    /// Packs and rainbow kitty keep their values; a changed catalog advances the one
    /// outer snapshot revision.
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
            trail_packs: Arc::clone(&self.assets.trail_packs),
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
    before.cursor_trail_packs != after.cursor_trail_packs
        || normalized_kitty_sprite(before.cursor_nyan_sprite.as_deref())
            != normalized_kitty_sprite(after.cursor_nyan_sprite.as_deref())
}

fn read_config_file(
    path: &Path,
    allow_missing: bool,
) -> Result<crate::native_document_host::AtomicFileContents, String> {
    crate::native_document_host::read_config_atomic_file(path, MAX_CONFIG_FILE_BYTES, allow_missing)
        .map_err(|error| config_read_error(path, error))
}

/// Why `aterm.toml` at `path` could not be read, naming the file once: the host's
/// sentence without its internal stage tag. Every config-file reader reports this way.
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
        // Full-depth dotted-key projection, mirroring `prefs::apply_prefs_edits`:
        // every item in a table tree is also exposed under its full dotted path, so
        // `is_explicit`, the Modified page and OCC expectations address the leaf
        // (`matrix_rain.enabled`, `sparkle_words.profanity.enabled`). Each
        // intermediate table keeps its opaque whole-table entry too, so an external
        // edit anywhere in it still stamps its revision.
        project_item(&mut values, &join_config_key_path("", key), item);
    }
    Ok(values)
}

/// Append one semantic TOML key segment to the identity used by
/// `ConfigSnapshot::values`.
///
/// `packages.include` (two segments) and `"packages.include"` (one) must not collide,
/// or an unknown value could impersonate a registered setting.
/// `aterm_toml::edit::Key` supplies the escaped spelling (bare-safe names stay
/// compact, names with `.` stay quoted), so the result is readable and injective.
pub(crate) fn join_config_key_path(parent: &str, segment: &str) -> String {
    let segment = aterm_toml::edit::Key::new(segment).to_string();
    if parent.is_empty() {
        segment
    } else {
        format!("{parent}.{segment}")
    }
}

/// Insert `item`'s value at `path` and, for header or inline tables, recurse into
/// each child under `path.child`. Arrays of tables stay one opaque entry, as in the
/// nested writer.
fn project_item(values: &mut BTreeMap<String, String>, path: &str, item: &aterm_toml::edit::Item) {
    if let Some(table) = item.as_table_like() {
        for (child, child_item) in table.iter() {
            project_item(values, &join_config_key_path(path, child), child_item);
        }
    }
    values.insert(path.to_string(), semantic_item(item));
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

    #[test]
    fn byte_equal_external_observation_refreshes_assets_and_analysis_generation() {
        let dir = unique_config_dir("equal-text-assets");
        let manifest = dir.join("trail-pack.toml");
        std::fs::write(
            &manifest,
            include_str!("../../aterm-effects/assets/trail-packs/synthwave.toml"),
        )
        .unwrap();
        let text = format!(
            "cursor_trail_packs = [{:?}]\ncursor_trail_style = \"pack:synthwave\"\n",
            manifest.to_string_lossy()
        );
        let mut service = VersionedConfigService::new(text.clone()).unwrap();
        let initial = service.snapshot();
        assert_eq!(initial.assets.trail_packs.ids, ["synthwave"]);

        std::fs::write(
            &manifest,
            include_str!("../../aterm-effects/assets/trail-packs/emberfall.toml"),
        )
        .unwrap();
        let refreshed = service.replace_external(text.clone()).unwrap();
        assert_eq!(refreshed.text, initial.text, "config bytes stay identical");
        assert!(refreshed.revision > initial.revision);
        assert!(refreshed.analysis_generation > initial.analysis_generation);
        assert_eq!(refreshed.assets.trail_packs.ids, ["emberfall"]);

        let unchanged = service.replace_external(text).unwrap();
        assert_eq!(unchanged.revision, refreshed.revision);
        assert!(unchanged.analysis_generation > refreshed.analysis_generation);
        assert_eq!(unchanged.assets, refreshed.assets);
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
        service.complete_startup_path_generation(exact, prepared.trail_packs);
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
        service.complete_startup_path_generation(exact, prepared.trail_packs);
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
            &sparkle.assets.trail_packs,
            &exact_assets.trail_packs
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

        // If the external-edit path accidentally resolves assets, corrupting
        // the admitted source makes the regression fail visibly.
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

    /// A config the OS will not open says so once: the file, the OS error, no
    /// internal stage tag and no second "unreadable" wrapped around the first.
    #[cfg(unix)]
    #[test]
    fn an_unopenable_config_names_the_file_once() {
        use std::os::unix::fs::PermissionsExt;
        let dir = unique_config_dir("unopenable");
        let path = dir.join("aterm.toml");
        std::fs::write(&path, "font_px = 14.0\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
        let result = VersionedConfigService::observe_path(&path, false);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        // A process the permission bits do not stop (root) has nothing to say.
        let Err(error) = result else {
            return;
        };
        let shown = path.display().to_string();
        assert_eq!(error.matches(&shown).count(), 1, "{error}");
        assert!(error.starts_with("could not open "), "{error}");
        assert!(
            !error.contains("Preflight") && !error.contains("unreadable"),
            "{error}"
        );
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
        // Mint from a regular path, then replace it with a symlink: an issued plan
        // cannot be redirected after its baseline was captured.
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

    #[test]
    fn stale_patch_rebases_only_across_unchanged_keys() {
        let mut service = VersionedConfigService::new(
            "theme = \"Nord\"\nfont_px = 14.0\n# keep me\ncustom = 7\n".into(),
        )
        .unwrap();
        let base = service.snapshot().revision;
        assert!(matches!(
            service.patch(edit(base, "theme", ExpectedValue::Any, Some("Dracula"))),
            ConfigPatchResult::Applied { .. }
        ));
        assert!(matches!(
            service.patch(edit(base, "font_px", ExpectedValue::Any, Some("16"))),
            ConfigPatchResult::Applied { .. }
        ));
        assert!(matches!(
            service.patch(edit(base, "theme", ExpectedValue::Any, Some("Tokyo Night"))),
            ConfigPatchResult::Conflict { keys, .. } if keys == ["theme"]
        ));
        assert!(service.snapshot().text.contains("# keep me"));
        assert!(service.snapshot().text.contains("custom = 7"));
    }

    #[test]
    fn same_key_external_edit_blocks_stale_write() {
        let mut service = VersionedConfigService::new("theme = \"Nord\"\n".into()).unwrap();
        let base = service.snapshot().revision;
        service
            .replace_external("theme = \"Catppuccin Mocha\"\n".into())
            .unwrap();
        assert!(matches!(
            service.patch(edit(base, "theme", ExpectedValue::Any, Some("Dracula"))),
            ConfigPatchResult::Conflict { .. }
        ));
        assert_eq!(
            service.value("theme").unwrap().as_deref(),
            Some("Catppuccin Mocha")
        );
    }

    #[test]
    fn reset_all_is_one_revision_and_preserves_unknown_keys() {
        let mut service = VersionedConfigService::new(
            "theme = \"Nord\"\nfont_px = 15.0\ncustom = \"stay\"\n".into(),
        )
        .unwrap();
        let before = service.snapshot().revision;
        let ConfigPatchResult::Applied { snapshot, .. } =
            service.reset_all(before, ["theme".to_string(), "font_px".to_string()])
        else {
            panic!("reset should commit");
        };
        assert_eq!(snapshot.revision, before + 1);
        assert!(!snapshot.text.contains("theme ="));
        assert!(!snapshot.text.contains("font_px ="));
        assert!(snapshot.text.contains("custom = \"stay\""));
    }

    #[test]
    fn conditional_undo_preserves_unrelated_change_and_rejects_same_key_change() {
        let mut service =
            VersionedConfigService::new("theme = \"Nord\"\nfont_px = 14.0\n".into()).unwrap();
        let base = service.snapshot().revision;
        let ConfigPatchResult::Applied { undo, .. } =
            service.patch(edit(base, "theme", ExpectedValue::Any, Some("Dracula")))
        else {
            panic!("patch should commit");
        };
        let current = service.snapshot().revision;
        service.patch(edit(current, "font_px", ExpectedValue::Any, Some("18")));
        assert!(matches!(
            service.undo(undo),
            ConfigPatchResult::Applied { .. }
        ));
        assert_eq!(service.value("theme").unwrap().as_deref(), Some("Nord"));
        assert_eq!(service.value("font_px").unwrap().as_deref(), Some("18"));

        let base = service.snapshot().revision;
        let ConfigPatchResult::Applied { undo, .. } =
            service.patch(edit(base, "theme", ExpectedValue::Any, Some("Dracula")))
        else {
            panic!("patch should commit");
        };
        service
            .replace_external("theme = \"External\"\nfont_px = 18.0\n".into())
            .unwrap();
        assert!(matches!(
            service.undo(undo),
            ConfigPatchResult::Conflict { keys, .. } if keys == ["theme"]
        ));
        assert_eq!(service.value("theme").unwrap().as_deref(), Some("External"));
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

        // Make a re-read observable by invalidating the admitted source: a patch that
        // changes no asset source must keep the exact catalog allocation.
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

        // A structured source-key patch is rejected before mutation: resolving assets
        // here would put I/O on the event loop, and reusing the old Arc would publish
        // a split generation.
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

        // Manual/watcher supplies an exact observation. Simulate its worker
        // preparation explicitly, then admit the typed text+asset generation.
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

    /// An authored empty collection is still an authored value. Clearing it
    /// and applying the actual Undo token restores its typed empty collection,
    /// rather than refusing or silently turning it into an absent setting.
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
    }

    /// The value the service reports for a cleared palette (the slot table in
    /// either spelling, the `palette` array) is what Undo writes back, and it
    /// lands as the same TOML, so the file still loads. Control: a slot table
    /// with an out-of-range slot or a non-colour is refused, never written.
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

    /// Full-depth dotted projection: each item in a table tree is exposed under its
    /// dotted path with per-key semantics, while each table keeps its whole-table
    /// opaque entry. An absent child is absent; an effective default is presentation.
    #[test]
    fn parse_values_projects_dotted_keys() {
        let values =
            parse_values("font_px = 13.0\n[matrix_rain]\nenabled = true\nfps = 24\n").unwrap();
        assert_eq!(
            values.get("matrix_rain.enabled").map(String::as_str),
            Some("true")
        );
        assert_eq!(
            values.get("matrix_rain.fps").map(String::as_str),
            Some("24")
        );
        assert!(
            values.contains_key("matrix_rain"),
            "the whole-table opaque entry survives"
        );
        assert_eq!(values.get("font_px").map(String::as_str), Some("13"));
        let none = parse_values("font_px = 13.0\n").unwrap();
        assert!(
            !none.keys().any(|k| k.starts_with("matrix_rain")),
            "absent table projects nothing (not explicit)"
        );
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

    /// Dotted keys take part in OCC: a patch expecting the old child value conflicts
    /// once the file moved, like a flat key; the winning write and blank-revert both go
    /// through `apply_prefs_edits`, and a table the revert leaves bare goes with it.
    #[test]
    fn dotted_key_occ_conflict_write_and_blank_revert() {
        let mut service =
            VersionedConfigService::new("[matrix_rain]\nenabled = false\n".into()).unwrap();
        let base = service.snapshot().revision;

        // An external edit flips the child; a stale exact-expectation patch
        // must CONFLICT on the dotted key, not silently clobber.
        service
            .replace_external("[matrix_rain]\nenabled = true\n".into())
            .unwrap();
        let stale = service.patch(edit(
            base,
            "matrix_rain.enabled",
            ExpectedValue::Exact(Some("false".into())),
            Some("false"),
        ));
        let ConfigPatchResult::Conflict { keys, .. } = stale else {
            panic!("stale dotted expectation must conflict, got {stale:?}");
        };
        assert_eq!(keys, vec!["matrix_rain.enabled".to_string()]);

        // A current-revision write lands typed (bool, not string).
        let rev = service.snapshot().revision;
        let applied = service.patch(edit(
            rev,
            "matrix_rain.enabled",
            ExpectedValue::Exact(Some("true".into())),
            Some("false"),
        ));
        let ConfigPatchResult::Applied { snapshot, .. } = applied else {
            panic!("current-revision dotted write applies, got {applied:?}");
        };
        assert!(snapshot.text.contains("enabled = false"));
        assert_eq!(
            service.value("matrix_rain.enabled").unwrap().as_deref(),
            Some("false")
        );

        // Blank reverts: the table's last key goes, and the bare [matrix_rain]
        // header with it (no comment of its own; empty parses to defaults).
        let rev = service.snapshot().revision;
        let reverted = service.patch(edit(
            rev,
            "matrix_rain.enabled",
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
            !snapshot.text.contains("[matrix_rain]"),
            "a bare table goes with its last key: {:?}",
            snapshot.text
        );
        assert_eq!(service.value("matrix_rain.enabled").unwrap(), None);
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

        // An external edit flips the depth-2 child; a stale exact-expectation
        // patch must CONFLICT on the full dotted path, not silently clobber.
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

        // The current expectation applies, typed (bool, not string).
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

        // Blank reverts the leaf, and the tables it left bare at both depths.
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
