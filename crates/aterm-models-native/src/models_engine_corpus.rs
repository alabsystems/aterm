// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! The aterm-models-native models aterm-spec's own unit tests run the engine on
//! (`config_catalog_snapshot_model`), re-exported from this crate's root like every other.

use aterm_spec::derive::*;

/// Every admitted config revision publishes text, its resolved trail-style cache
/// (`TrailStyleCache`), resolved kitty sprite, parsed custom-theme catalog, and exact inline + Toy
/// Pack consumer projection as one immutable snapshot.
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
            action RefreshAssets when (revision == 0) {
                revision = revision + 1;
                text_generation = revision + 1;
                trail_generation = revision + 1;
                kitty_generation = revision + 1;
                theme_generation = revision + 1;
                sparkle_generation = revision + 1;
                asset_refresh = 1;
            }
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
            bound RevisionBounded: revision <= MaxRevision && asset_refresh <= 2;
        }
    }
}
