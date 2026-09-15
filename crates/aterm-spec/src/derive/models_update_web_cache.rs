// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The web-head shortcut depends on the original release judgment and its stage.

use super::*;

/// A cached tag may avoid a fresh release fetch only for its authorizing build
/// and source, with a publishable covering stage when the release is newer.
/// Status writes carry that authorization; they do not mint a new one. Tier-1
/// exercises the real status writer/reader, stage files, and web acquisition.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_update_web_cache_model() -> Model {
    crate::ty_model! {
        NativeUpdateWebCache {
            const Buggy = 0;
            const AuthorizedBuild = 1;
            var running = 1;
            var author = 1;
            var release = 2;
            var stage = 2;
            var same_source = 1;
            var recorded = 0;
            var skipped = 0;

            action LoseStage when (stage > 0) {
                stage = 0;
                skipped = 0;
            }
            action RestoreStage when (stage == 0) {
                stage = 2;
                skipped = 0;
            }
            action ChangeBuild when (running == 1) {
                running = 0;
                skipped = 0;
            }
            action ChangeSource when (same_source == 1) {
                same_source = 0;
                skipped = 0;
            }
            action JudgeAlreadyCurrent when (release == 2 && running == 1) {
                release = 1;
                skipped = 0;
            }
            action RecordStatus when (recorded == 0) {
                author = if Buggy == 1 { running } else { author };
                recorded = 1;
                skipped = 0;
            }
            action UseCache when (
                skipped == 0 && author == running && same_source == 1 &&
                (release <= running || release <= stage || Buggy == 1)
            ) {
                skipped = 1;
            }
            invariant NoSkippedRecovery:
                skipped == 0 ||
                (running == AuthorizedBuild && same_source == 1 &&
                 (release <= running || release <= stage));
        }
    }
}
