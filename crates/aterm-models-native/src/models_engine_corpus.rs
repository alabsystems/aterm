// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! The aterm-models-native models aterm-spec's own unit tests run the engine on (`config_catalog_snapshot_model`),
//! re-exported from this crate's root like every other. aterm-spec `#[path]`-mounts this file
//! (`src/test_models.rs`), so it holds only these and names only `aterm_spec::…` paths: an edit
//! to the rest of this crate's models then rebuilds no aterm-spec test and does not seed
//! aterm-spec, and with it every crate that depends on aterm-spec, into `verify`'s change cone.

use aterm_spec::derive::*;

/// Every admitted config revision publishes text, its resolved trail-style
/// cache (`TrailStyleCache`), resolved kitty sprite, parsed custom-theme
/// catalog, and exact inline + Toy Pack consumer projection as one immutable
/// snapshot. Settings, the live host, and capture may observe a generation only
/// after every payload reaches it. Explicit mutant actions
/// independently retain each stale asset generation, proving no member is
/// accidentally protected only by another. One more, `PublishLiveUnadmitted`,
/// installs a worker-prepared generation on the live host before the service
/// admits it — `finish_native_config_write` scheduling its runtime observation
/// ahead of `synchronize_prepared_observation` instead of on its `Ok` arm — so
/// when admission fails the host runs a config the service never published
/// (`ViewsNeverAhead`).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn config_catalog_snapshot_model() -> Model {
    aterm_spec::ty_model! {
        ConfigCatalogSnapshot {
            const Buggy = 0;
            const MaxRevision = 2;
            var revision = 0;
            var text_generation = 0;
            var trail_generation = 0;
            var kitty_generation = 0;
            var theme_generation = 0;
            var sparkle_generation = 0;
            // Distinguishes a byte-identical path-asset refresh from an
            // ordinary text patch in Tier-1 traces without weakening the
            // shared atomic-generation invariant.
            var asset_refresh = 0;
            var view_one_generation = 0;
            var view_two_generation = 0;
            var live_generation = 0;
            var capture_generation = 0;
            action AdmitPatch when (revision == 0) {
                revision = revision + 1;
                text_generation = revision + 1;
                trail_generation = revision + 1;
                kitty_generation = revision + 1;
                theme_generation = revision + 1;
                sparkle_generation = revision + 1;
                asset_refresh = 0;
            }
            action AdmitExternal when (revision == 1) {
                revision = revision + 1;
                text_generation = revision + 1;
                trail_generation = revision + 1;
                kitty_generation = revision + 1;
                theme_generation = revision + 1;
                sparkle_generation = revision + 1;
                asset_refresh = 0;
            }
            // A byte-identical external observation may still resolve a new
            // path-backed asset generation. The text is re-admitted as part of
            // the same immutable snapshot identity; consumers never combine
            // the retained text Arc with an independently refreshed catalog.
            action RefreshAssets when (revision == 0) {
                revision = revision + 1;
                text_generation = revision + 1;
                trail_generation = revision + 1;
                kitty_generation = revision + 1;
                theme_generation = revision + 1;
                sparkle_generation = revision + 1;
                asset_refresh = 1;
            }
            // The theme-directory watcher parses off-thread and republishes a
            // complete outer snapshot even when config text is byte-identical.
            action RefreshThemes when (revision == 0) {
                revision = revision + 1;
                text_generation = revision + 1;
                trail_generation = revision + 1;
                kitty_generation = revision + 1;
                theme_generation = revision + 1;
                sparkle_generation = revision + 1;
                asset_refresh = 2;
            }
            action AdmitStaleTrail when (Buggy == 1 && revision == 0) {
                revision = revision + 1;
                text_generation = revision + 1;
                trail_generation = trail_generation;
                kitty_generation = revision + 1;
                theme_generation = revision + 1;
                sparkle_generation = revision + 1;
                asset_refresh = 0;
            }
            action AdmitStaleKitty when (Buggy == 1 && revision == 0) {
                revision = revision + 1;
                text_generation = revision + 1;
                trail_generation = revision + 1;
                kitty_generation = kitty_generation;
                theme_generation = revision + 1;
                sparkle_generation = revision + 1;
                asset_refresh = 0;
            }
            action AdmitStaleTheme when (Buggy == 1 && revision == 0) {
                revision = revision + 1;
                text_generation = revision + 1;
                trail_generation = revision + 1;
                kitty_generation = revision + 1;
                theme_generation = theme_generation;
                sparkle_generation = revision + 1;
                asset_refresh = 0;
            }
            action AdmitStaleSparkle when (Buggy == 1 && revision == 0) {
                revision = revision + 1;
                text_generation = revision + 1;
                trail_generation = revision + 1;
                kitty_generation = revision + 1;
                theme_generation = revision + 1;
                sparkle_generation = sparkle_generation;
                asset_refresh = 0;
            }
            action PublishLiveUnadmitted when (
                Buggy == 1 && revision <= MaxRevision - 1
            ) {
                live_generation = revision + 1;
            }
            action PublishOne when (
                text_generation == revision && trail_generation == revision &&
                kitty_generation == revision && theme_generation == revision &&
                sparkle_generation == revision
            ) {
                view_one_generation = revision;
            }
            action PublishTwo when (
                text_generation == revision && trail_generation == revision &&
                kitty_generation == revision && theme_generation == revision &&
                sparkle_generation == revision
            ) {
                view_two_generation = revision;
            }
            action PublishLive when (
                text_generation == revision && trail_generation == revision &&
                kitty_generation == revision && theme_generation == revision &&
                sparkle_generation == revision
            ) {
                live_generation = revision;
            }
            action PublishCapture when (
                text_generation == revision && trail_generation == revision &&
                kitty_generation == revision && theme_generation == revision &&
                sparkle_generation == revision
            ) {
                capture_generation = revision;
            }
            invariant SnapshotAtomic:
                text_generation == revision && trail_generation == revision &&
                kitty_generation == revision && theme_generation == revision &&
                sparkle_generation == revision;
            invariant ViewsNeverAhead:
                view_one_generation <= revision && view_two_generation <= revision &&
                live_generation <= revision && capture_generation <= revision;
            // Consumers receive the one immutable snapshot `Arc` — text and every
            // asset together — so a consumer's generation is complete whenever the
            // snapshot is (`SnapshotAtomic`) and not ahead of it (`ViewsNeverAhead`).
            bound RevisionBounded: revision <= MaxRevision && asset_refresh <= 2;
        }
    }
}
