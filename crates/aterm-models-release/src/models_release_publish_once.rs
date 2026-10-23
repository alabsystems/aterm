// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The one publication a cut makes, and who holds the channel's `latest` pointer.

use aterm_spec::derive::*;

/// Publish once.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn release_publish_once_model() -> Model {
    aterm_spec::ty_model! {
        ReleasePublishOnce {
            const Buggy = 0;
            var phase = 0;
            var latest = 0;
            var prerelease = 0;
            var source = 0;
            var sig_up = 0;
            var toml_up = 0;
            var proved = 0;
            var checked = 0;
            var newer = 0;
            var joined = 0;
            var bound = 0;
            var headed = 0;
            var displaced = 0;
            var behind = 0;

            action PublishSource when (phase == 0) {
                phase = 1;
                prerelease = 1;
                source = 1;
            }
            action PublishSourceFull when (Buggy == 1 && phase == 0) {
                phase = 1;
                latest = 1;
                source = 1;
            }
            action NewerRelease when (phase == 1 && newer == 0) {
                newer = 1;
                latest = 2;
            }
            action RosterJoin when (phase > 0 && phase <= 3 && joined == 0) {
                joined = 1;
                checked = 0;
            }
            action AcquireLease when (phase == 1) {
                phase = 2;
            }
            action Ratchet when (phase == 2 && latest <= 1 && joined == 0) {
                checked = 1;
            }
            action RatchetBlind when (Buggy == 1 && phase == 2) {
                checked = 1;
            }
            action RefuseNewer when (phase == 2 && latest == 2) {
                phase = 4;
            }
            action RefuseFloor when (phase == 2 && joined == 1) {
                phase = 4;
            }
            action Bind when (phase == 2 && checked == 1 && bound == 0) {
                bound = 1;
            }
            action BindBlind when (Buggy == 1 && phase == 2 && checked == 0 && bound == 0) {
                bound = 1;
            }
            action UploadSig when (phase == 2 && bound == 1 && sig_up == 0) {
                sig_up = 1;
                proved = 0;
            }
            action UploadToml when (phase == 2 && sig_up == 1 && toml_up == 0) {
                toml_up = 1;
                proved = 0;
            }
            action UploadTomlFirst when (
                Buggy == 1 && phase == 2 && bound == 1 && sig_up == 0 && toml_up == 0
            ) {
                toml_up = 1;
                proved = 0;
            }
            action ProveAssets when (phase == 2 && toml_up == 1 && proved == 0) {
                proved = 1;
            }
            action ProveBeforeLastUpload when (
                Buggy == 1 && phase == 2 && bound == 1 && toml_up == 0 && proved == 0
            ) {
                proved = 1;
            }
            action MakeHead when (phase == 2 && checked == 1 && proved == 1) {
                displaced = if latest == 2 { 1 } else { displaced };
                behind = joined;
                latest = 1;
                prerelease = 0;
                headed = 1;
                phase = 3;
            }
            action MakeHeadEarly when (
                Buggy == 1 && phase == 2 && checked == 1 && bound == 1 && toml_up == 0
            ) {
                latest = 1;
                prerelease = 0;
                headed = 1;
                phase = 3;
            }
            action MakeHeadUnproved when (
                Buggy == 1 && phase == 2 && checked == 1 && toml_up == 1 && proved == 0
            ) {
                latest = 1;
                prerelease = 0;
                headed = 1;
                phase = 3;
            }
            action MakeHeadStaleFloors when (Buggy == 1 && phase == 2 && proved == 1) {
                behind = joined;
                latest = 1;
                prerelease = 0;
                headed = 1;
                phase = 3;
            }
            action FinishWithoutHead when (Buggy == 1 && phase == 2 && checked == 1 && proved == 1) {
                phase = 6;
            }
            action OtherTakesLatest when (phase == 3 && latest == 1) {
                latest = 0;
            }
            action ReassertHead when (phase == 3) {
                latest = 1;
            }
            action ProveHead when (phase == 3 && latest == 1) {
                phase = 6;
            }
            action ResumeRefusesItsOwnHead when (Buggy == 1 && phase == 3 && joined == 1) {
                phase = 4;
            }
            action Abandon when ((phase == 2 || phase == 4) && headed == 0) {
                sig_up = 0;
                toml_up = 0;
                proved = 0;
                bound = 0;
                phase = 5;
            }
            action AbandonDeletesRelease when (Buggy == 1 && (phase == 2 || phase == 4) && headed == 0) {
                sig_up = 0;
                toml_up = 0;
                proved = 0;
                bound = 0;
                source = 0;
                phase = 5;
            }
            action Done when (phase == 5 || phase == 6) {
                phase = phase;
            }

            invariant NoEmptyHead:
                latest <= 0 || latest > 1 || (sig_up == 1 && toml_up == 1);
            invariant SignatureBeforeAppcast: toml_up == 0 || sig_up == 1;
            invariant ProofAfterTheLastUpload: proved == 0 || toml_up == 1;
            invariant NoUnprovedHead: headed == 0 || (proved == 1 && toml_up == 1);
            invariant NeverDisplacesNewer: displaced == 0;
            invariant NeverBehindTheFleet: behind == 0;
            invariant PublishedMeansHead:
                phase <= 5 || (latest == 1 && prerelease == 0 && headed == 1);
            invariant BoundOnAFloorRead: bound == 0 || checked == 1 || joined == 1;
            invariant AbandonLeavesTheSourceRelease:
                phase <= 4 ||
                    phase > 5 ||
                    (source == 1 && sig_up == 0 && toml_up == 0 && (latest <= 0 || latest > 1));
            invariant MadeHeadFinishes: headed == 0 || phase == 3 || phase == 6;
        }
    }
}
