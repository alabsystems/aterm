// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Serialized, versioned preference transaction core for native Settings.
//!
//! The reducer is pure and deterministic. A host worker reads/writes the file and feeds
//! whole snapshots here; this service performs OCC, per-key stale rebase, conditional
//! undo, and one `aterm-toml` document transform per accepted patch. The UI never writes config
//! bytes directly.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::Path;
use std::sync::Arc;

const UNDO_LIMIT: usize = 64;
/// One admission budget shared by startup, watcher reloads, and Manual's
/// semantic analysis. Keeping these equal prevents startup from accepting a
/// generation the watcher can never fingerprint or the editor can never
/// validate.
pub(crate) const MAX_CONFIG_FILE_BYTES: usize = 512 * 1024;

#[derive(Clone, PartialEq)]
pub(crate) struct ConfigSnapshot {
    pub(crate) revision: u64,
    /// Generation of the external environment against which Manual diagnostics
    /// were last requested. A stable file observation advances this even when
    /// `aterm.toml` bytes are identical, because referenced assets and installed
    /// fonts may have changed independently of the document text.
    pub(crate) analysis_generation: u64,
    pub(crate) text: Arc<str>,
    /// Parsed semantic projection produced before admission. Native Settings
    /// and process-wide policy edges clone this Arc instead of reparsing the
    /// bounded TOML document on the event loop.
    pub(crate) config: Arc<crate::app_config::Config>,
    pub(crate) semantic_values: Arc<BTreeMap<String, String>>,
    /// Exact immutable non-text assets admitted with `text` at `revision`.
    /// Cloning a snapshot clones this one outer Arc; Trail manifests and the
    /// custom kitty sprite are never independently re-resolved by consumers.
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
            .field("assets", &self.assets)
            .finish_non_exhaustive()
    }
}

impl ConfigSnapshot {
    /// Semantic top-level values exactly as the transaction service compares
    /// them for optimistic concurrency.  Settings uses this projection for
    /// "Modified" and edit expectations: an effective default is presentation,
    /// not evidence that the key exists in `aterm.toml`.
    pub(crate) fn values(&self) -> Result<BTreeMap<String, String>, String> {
        Ok((*self.semantic_values).clone())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ExpectedValue {
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

/// Immutable handoff from the optimistic reducer to the sole durable config
/// worker. `expected_text` and `baseline` are the last synchronized disk
/// generation, while `snapshot` is the candidate to publish. Keeping all three
/// together prevents a later service mutation from changing a queued commit's
/// compare-and-swap authority.
#[derive(Clone, Debug)]
pub(crate) struct ConfigPersistencePlan {
    pub(crate) snapshot: ConfigSnapshot,
    pub(crate) expected_text: Arc<str>,
    pub(crate) logical_path: Option<std::path::PathBuf>,
    pub(crate) baseline: Option<crate::native_document_host::AtomicFileBaseline>,
}

/// One stable UTF-8 disk read plus the exact logical-target/file generation
/// that supplied it. The reload host carries this same value through Manual
/// refresh, strict parse, and config-service admission; no consumer performs an
/// independent second content read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ConfigDiskObservation {
    pub(crate) text: String,
    pub(crate) baseline: crate::native_document_host::AtomicFileBaseline,
}

/// Exact disk bytes plus every filesystem-backed asset and effect-feed runtime
/// needed to admit those bytes on the event loop. Construction belongs to a
/// host worker; admission and the downstream font worker perform only bounded
/// computation over this immutable generation.
#[derive(Clone)]
pub(crate) struct PreparedConfigObservation {
    pub(crate) observation: ConfigDiskObservation,
    pub(crate) config: crate::app_config::Config,
    pub(crate) values: BTreeMap<String, String>,
    pub(crate) assets: Arc<crate::app_config::ConfigAssetCatalog>,
    pub(crate) path_feeds: crate::app_config::PreparedPathFeedGeneration,
}

impl std::fmt::Debug for PreparedConfigObservation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PreparedConfigObservation")
            .field("observation", &self.observation)
            .field("semantic_value_count", &self.values.len())
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
    config: Arc<crate::app_config::Config>,
    durable_text: String,
    durable_values: BTreeMap<String, String>,
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
    // bench-support: `headless_for_test` (which the frame-latency bench builds
    // through) constructs its config service with this deterministic ctor.
    #[cfg(any(test, feature = "bench-support"))]
    pub(crate) fn new(text: String) -> Result<Self, String> {
        Self::new_with_themes(text, crate::app_config::ThemeCatalog::empty())
    }

    /// Runtime constructor used only before presentation. Theme/rainbow kitty discovery
    /// is bounded and parsed once here; Trail/Sparkle feeds remain explicitly
    /// pending for the parallel startup worker. Tests use the deterministic
    /// test constructor and its complete synchronous catalog.
    pub(crate) fn new_runtime(text: String) -> Result<Self, String> {
        Self::new_with_themes_mode(text, crate::app_config::ThemeCatalog::discover(), true)
    }

    #[cfg(any(test, feature = "bench-support"))]
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
            durable_config: Arc::clone(&config),
            durable_assets: Arc::clone(&assets),
            text,
            values,
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

    /// Seed the process service from the same user config path watched by the
    /// rest of the app. Missing config is the valid empty document; unreadable
    /// or malformed input is reported so startup can degrade without clobbering
    /// the user's file.
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

    /// Resolve one worker-observed generation completely before it crosses the
    /// event-loop boundary. The caller must run this on a host worker because
    /// Trail manifests and a custom kitty sprite may require bounded file I/O.
    pub(crate) fn prepare_observation(
        observation: ConfigDiskObservation,
        themes: Arc<crate::app_config::ThemeCatalog>,
    ) -> Result<PreparedConfigObservation, String> {
        let values = parse_values(&observation.text)?;
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
            assets: Arc::clone(&self.assets),
        }
    }

    /// Install the exact path-backed effect generation prepared for startup
    /// before any view can observe the service. This does not advance the
    /// revision: it completes revision 1's preliminary asset catalog rather
    /// than admitting a new text generation.
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

    /// Exact disk generation from which the current startup/runtime service was
    /// initialized. The watcher uses this only to close the load→watch handoff:
    /// a first observation that differs is a real edge, even before polling.
    pub(crate) fn observed_disk_baseline(
        &self,
    ) -> Option<&crate::native_document_host::AtomicFileBaseline> {
        self.disk_baseline.as_ref()
    }

    /// A writer reported an unknown or externally-won disk generation. Until a
    /// stable observation is admitted, persistence plans must not reuse the old
    /// baseline as if it still proved write authority.
    pub(crate) fn mark_reconciliation_required(&mut self) {
        self.disk_text_admitted = false;
        self.write_reconciliation_required = true;
    }

    pub(crate) fn reconciliation_required(&self) -> bool {
        self.write_reconciliation_required
    }

    /// Retain path/generation authority when startup could read UTF-8 bytes but
    /// could not admit malformed TOML into live Config. Opening Manual supplies
    /// the already-minted config grant here; the current service text remains
    /// unchanged until a later valid durable Manual observation is admitted.
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

    /// Immediately import the live durable file while retaining monotonic
    /// revision/key stamps. Manual editor completion calls this directly; it
    /// never waits for the polling watcher.
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
            self.config = Arc::new(config);
        } else if assets != self.assets {
            self.revision = self.revision.saturating_add(1);
        }
        self.durable_text.clone_from(&self.text);
        self.durable_values.clone_from(&self.values);
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
            assets,
            path_feeds: _,
        } = prepared;
        self.synchronize_observation_prepared(observation, config, values, assets)
    }

    /// Roll an optimistic in-memory candidate back to the last bytes proven
    /// durable when its worker commit fails and the current disk generation is
    /// not admissible (for example malformed external TOML). Revision/key stamps
    /// remain monotonic through the same external-replacement reducer.
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
            self.config = Arc::clone(&self.durable_config);
            self.assets = Arc::clone(&self.durable_assets);
        }
        Ok(self.snapshot())
    }

    #[cfg(test)]
    pub(crate) fn value(&self, key: &str) -> Result<Option<String>, String> {
        Ok(self.values.get(key).cloned())
    }

    /// Apply one all-or-nothing patch. A stale patch is accepted only when every
    /// touched key is unchanged since its base revision and every supplied expectation
    /// still matches the current semantic TOML value.
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

        // Validate the complete typed config before advancing any service
        // state. Host-backed source keys were rejected above and are edited in
        // Manual, whose worker admits text plus decoded assets atomically.
        // Ordinary structured toggles/theme/font changes therefore reuse the
        // admitted Trail/rainbow kitty/theme assets with no event-thread file I/O. A
        // sparkle-table change below rewraps those assets with a typed pending
        // consumer projection until its worker generation is ready.
        let next_config = match parse_config(&next) {
            Ok(config) => config,
            Err(message) => return self.rejected(message),
        };

        // The admitted consumer projection belongs to the exact parsed
        // `[sparkle_words]` generation that produced it. A structured Settings
        // patch is intentionally filesystem-free, so it cannot synchronously
        // rebuild lexicon/Toy Pack capabilities on the event thread. Publish a
        // typed preliminary state for a changed sparkle table instead of
        // carrying the previous generation's authoritative `Some(...)` verdict
        // across the revision. The config/font worker replaces `None` with the
        // exact empty-or-nonempty projection from the durable generation.
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
        // WALLPAPER: unlike the manual-only asset-source keys (which a worker
        // must co-admit with the text), the wallpaper key IS structured-writable
        // — the Settings file picker writes it — so this patch lane re-resolves
        // the image inline; without this the catalog would keep serving the
        // previous picture (or none) under the new path.
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
        // A watcher/durable observation is also an environment observation.
        // A byte-equal explicit observation remains the documented refresh
        // signal for a touched manifest/sprite. When config text itself changed,
        // however, an unrelated external edit reuses the exact immutable asset
        // Arc and performs no Trail/rainbow kitty I/O on the event thread; only an actual
        // source projection change resolves a new catalog. Manual diagnostics
        // use the independent generation to recheck fonts and other host state.
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
        self.config = after_config;
        self.durable_text.clone_from(&self.text);
        self.durable_values.clone_from(&self.values);
        self.durable_config = Arc::clone(&self.config);
        self.assets = assets;
        self.durable_assets = Arc::clone(&self.assets);
        Ok(self.snapshot())
    }

    /// Admit a catalog parsed by the background theme-directory watcher. Text,
    /// Trail Packs, and rainbow kitty retain their current values; publishing a changed
    /// theme catalog advances the single outer snapshot revision atomically.
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
    aterm_toml::from_str::<crate::app_config::Config>(text)
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
        .map_err(|error| format!("{} unreadable ({error})", path.display()))
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
        // FULL-DEPTH dotted-key projection, mirroring the nested writer
        // (`prefs::apply_prefs_edits` walks a dotted path of ANY depth): every
        // item inside a table tree is ALSO exposed under its full dotted path,
        // so dotted editable keys at any depth (`matrix_rain.enabled`,
        // `sparkle_words.profanity.enabled`) get exact per-key semantics —
        // `is_explicit`, the Modified page, and OCC expected-value comparison
        // all address the leaf, never a table's opaque serialization. Each
        // intermediate table keeps its opaque whole-table entry too (its
        // consumers predate the dotted model, and an external edit anywhere in
        // the table must still stamp its revision).
        project_item(&mut values, &join_config_key_path("", key), item);
    }
    Ok(values)
}

/// Append one semantic TOML key segment to the stable identity used by
/// `ConfigSnapshot::values`.
///
/// TOML's `packages.include` is a two-segment path, while
/// `"packages.include"` is one literal segment. Flattening both to the same
/// bytes lets an authored forward-compatible value impersonate a registered
/// setting. `aterm_toml::edit::Key` supplies the canonical, escaped TOML spelling for
/// a segment: bare-safe names stay compact and names containing `.` remain
/// quoted. The result is therefore both human-readable and injective over
/// segment sequences.
pub(crate) fn join_config_key_path(parent: &str, segment: &str) -> String {
    let segment = aterm_toml::edit::Key::new(segment).to_string();
    if parent.is_empty() {
        segment
    } else {
        format!("{parent}.{segment}")
    }
}

/// Insert `item`'s semantic value at `path` and, when the item is table-like
/// (both `[a.b]` header tables and `b = { … }` inline tables), recurse into
/// every child under `path.child`. Arrays of tables stay one opaque entry,
/// matching the nested writer, which never addresses into them.
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
        // Mint from a regular path, then replace that spelling with a symlink.
        // Symlinks are rejected at mint time now; this sequence retains the
        // more important regression coverage that an already-issued plan also
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
    fn rejected_sensitive_control_material_is_redacted_from_service_result() {
        for (key, pasted, expected) in [
            (
                crate::prefs::EDIT_TITLE_SUMMARY_TOKEN_FILE,
                "Bearer service-result-must-not-echo-this-secret",
                "expected a file path",
            ),
            (
                crate::prefs::EDIT_TITLE_SUMMARY_CA_FILE,
                concat!("-----BEGIN ", "PRIVATE KEY-----service-result-secret"),
                "expected a file path",
            ),
            (
                crate::prefs::EDIT_TITLE_SUMMARY_ENDPOINT,
                "https://models.example.test/chat?api_key=service-result-secret",
                "without a query or fragment",
            ),
        ] {
            let mut service = VersionedConfigService::new(String::new()).unwrap();
            let before = service.snapshot();
            let result = service.patch(ConfigPatchRequest {
                base_revision: before.revision,
                edits: vec![ConfigKeyEdit {
                    key: key.into(),
                    expected: ExpectedValue::Any,
                    value: Some(pasted.into()),
                }],
            });
            let ConfigPatchResult::Rejected { message, .. } = result else {
                panic!("pasted sensitive material should be rejected for {key}");
            };
            assert!(!message.contains(pasted), "material leaked: {message}");
            assert!(message.contains(expected), "{message}");
            assert_eq!(service.snapshot(), before);
        }
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

        // Make a re-read observable: the already-admitted source is now
        // invalid. A patch that cannot change either asset source must not
        // touch it and must retain the exact immutable catalog allocation.
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

        // A structured source-key patch is rejected before mutation: resolving
        // filesystem assets here would put I/O on the event loop, while reusing
        // the old Arc would publish a split generation.
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

    /// FULL-DEPTH dotted projection: every item in a table tree is exposed
    /// under its full dotted path with exact per-key semantics (the
    /// `is_explicit` / OCC face for `matrix_rain.enabled`), while each table's
    /// legacy whole-table opaque entry survives for its pre-dotted consumers.
    /// An absent child is simply absent — an effective default is
    /// presentation, not evidence.
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

    /// The dotted key is a first-class OCC citizen: a patch expecting the OLD
    /// child value conflicts once the file moved underneath it, exactly like a
    /// flat key — and the winning write + blank-revert both go through the one
    /// `apply_prefs_edits` transform (an emptied table is RETAINED: its header
    /// and comments survive, and empty parses to the same defaults).
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

        // Blank = revert: the child (the table's last key) goes; the emptied
        // [matrix_rain] header is RETAINED (it may carry user comments, and
        // empty parses to the same defaults).
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
            snapshot.text.contains("[matrix_rain]"),
            "an emptied table is RETAINED (the nested writer's contract — its \
             header may carry user comments, and empty parses to the defaults): {:?}",
            snapshot.text
        );
        assert_eq!(service.value("matrix_rain.enabled").unwrap(), None);
    }

    /// DEPTH-2 dotted keys are projected and transacted exactly like depth-1:
    /// a hand-set `sparkle_words.profanity.enabled` gets its own per-key entry
    /// (the `is_explicit` face), a stale exact expectation CONFLICTS while the
    /// current one APPLIES, and blank-revert removes only the leaf — the
    /// emptied tables at BOTH depths are retained.
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

        // Blank = revert: only the leaf goes; the emptied tables at BOTH
        // depths are RETAINED (headers/comments survive, and empty parses to
        // the same defaults).
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
            snapshot.text.contains("[sparkle_words]")
                && snapshot.text.contains("[sparkle_words.profanity]"),
            "emptied tables are RETAINED at both depths: {:?}",
            snapshot.text
        );
        assert_eq!(
            service.value("sparkle_words.profanity.enabled").unwrap(),
            None
        );
    }
}
