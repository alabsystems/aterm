// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Step 4 of a release, qualify (tree ∥ build ∥ export), and the steps that read its records:
//! 5 bundle, 6 ready, 7 publish, 8 cut (B4 of `docs/DESIGN-fast-build-release-2026-11-05.md`).
//!
//! # Variables
//! - `phase`: the release's step. 0 plan (step 4's skips being chosen), 1 qualify (step 4 runs),
//!   2 bundle (step 4 passed; step 5 runs), 3 ready (step 6 passed), 4 published (step 7 done),
//!   5 cut (step 8 done), 6 refused (at step 4, 5 or 6).
//! - `tree`, `build`: a qualify branch (the tree's checks, `verify release --tree`; the bundle
//!   build). 0 idle (planned, not started), 1 running, 2 passed, 3 red, 4 could not run,
//!   5 stopped (killed on a red), 6 skipped (its record stood at the plan).
//! - `export`: the export branch (`pub stage`), as above, plus 7 pushing (the staging push began;
//!   a push is never stopped, it ends passed, red or could not run).
//! - `bundle`: step 5, the bundle half (`verify release --bundle`: appkit and measure on the built
//!   bundle). 0 idle, 1 running, 2 passed, 3 red, 4 could not run, 6 skipped (its record stood).
//! - `tree_cpu`, `build_cpu`, `export_cpu`: the Release jobserver tokens the branch holds; their
//!   sum is the pool's charge, at most `S`. A branch's end returns every token it held.
//! - `red`: 1 once a qualify branch ended red. `kept_build`: 1 when an executing-test red
//!   retains a build already running, so that build finishes and can serve a test-only hop.
//! - `held`: 1 while R's tree and the build-inputs seal have held for the tree branch's run; 0 once
//!   either moved before the tree branch ended (a move after its end changes nothing).
//! - `tree_rec`: R's `RELEASE_TREE` records. 0 none written, 1 the records of the checks that
//!   finished but not every one (a tree that did not pass, on a held tree), 2 every one (a pass on
//!   a held tree, or standing at the plan, which skips the tree).
//! - `bundle_rec`: 1 once the bundle record stands (step 5 passed, or it stood and skipped it).
//! - `pushed`: 1 once the staging push began.
//! - `ready`, `public`, `cut`: 1 once step 6 passed, R went public, R was cut.
//! - `started_after_red`, `pushed_after_red`: history ghosts, 1 once a branch started, or the
//!   staging push began, while `red` was 1. No committed action sets them.
//!
//! # Actions
//! Plan (phase 0): `SkipTree` (sets `tree_rec` 2), `SkipBuild`, `SkipExport`, then `Plan` (to 1).
//! `TreeMoves` (any time) is the environment: R's tree or the seal moved.
//! Qualify (phase 1): `StartTree`; `StartBuild` once the tree is running, ended or skipped;
//! `StartExport` once the build is; none after a red. `TreeAcquire`/`BuildAcquire`/
//! `ExportAcquire` take a token while the branch runs and the pool has one; `TreeRelease`/
//! `BuildRelease`/`ExportRelease` give one back. `TreePass`, `TreeRed`, `TreeCouldNotRun` and
//! `StopTree` (on a red) end the tree, writing its records only on a held tree; `BuildPass`,
//! `BuildRed`, `BuildCouldNotRun`, `StopBuild`; `ExportPush` (not after a red), `ExportPass`
//! (from pushing), `ExportRed` and `ExportCouldNotRun` (from running or pushing), `StopExport`
//! (running only). `QualifyPass` once every branch passed or was skipped (to 2); `QualifyRefuse`
//! once no branch runs and step 4 cannot pass (to 6): after a red, or once every branch ended
//! and one did not pass.
//! Bundle (phase 2): `SkipBundle` (its record stood) or `StartBundle`; `BundlePass` writes the
//! bundle record; `BundleRed`, `BundleCouldNotRun` refuse (to 6). `Ready` once every record
//! stands (to 3), else `NotReady` (to 6). `Publish` (to 4), `Cut` (to 5).
//!
//! # Mutants
//! `Buggy == k` enables the one action written for dial k, and each breaks the law after it:
//! 1 `BundleBeforeTreeEnds` (`BundleAfterBuildAndTree`), 2 `TreeWritesOnAMovedTree`
//! (`RecordsOnlyOfAHeldTree`), 3 `ExportPushesAfterARed` (`NoPushAfterARed`),
//! 4 `BuildStartsAfterARed` (`NoStartAfterARed`), 5 `ExportAcquiresPastThePool`
//! (`ChargesWithinThePool`), 6 `PublishBeforeReady` (`NothingPublicBeforeReady`),
//! 7 `CutBeforeReady` (`CutHoldsEveryRecord`), 8 `ReadyOnVerdicts` (`ReadyHoldsEveryRecord`),
//! 9 `BuildStartsBeforeTheTree` (`StartsInOrder`), 10 `StopOnACouldNotRun` (`StopOnlyOnARed`),
//! 11 `StopAPushInFlight` (`PushInFlightFinishes`), 12 `RefuseBeforeTheBranchesEnd`
//! (`NoBranchOutlivesTheStep`), 13 `PassOnACouldNotRun` (`QualifyPassesOnlyOnEveryPass`),
//! 14 `BuildEndsHoldingTokens` (`NoTokenOutlivesItsBranch`), 15 `StopKeptBuild`
//! (`TestRedKeepsTheBuild`).

use aterm_spec::derive::*;

/// The qualify graph and the records that make a release ready.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn release_qualify_model() -> Model {
    aterm_spec::ty_model! {
        ReleaseQualify {
            const Buggy = 0;
            const S = 2;
            var phase = 0;
            var tree = 0;
            var build = 0;
            var export = 0;
            var bundle = 0;
            var tree_cpu = 0;
            var build_cpu = 0;
            var export_cpu = 0;
            var red = 0;
            var kept_build = 0;
            var held = 1;
            var tree_rec = 0;
            var bundle_rec = 0;
            var pushed = 0;
            var ready = 0;
            var public = 0;
            var cut = 0;
            var started_after_red = 0;
            var pushed_after_red = 0;

            action SkipTree when (phase == 0 && tree == 0) {
                tree = 6;
                tree_rec = 2;
            }
            action SkipBuild when (phase == 0 && build == 0) {
                build = 6;
            }
            action SkipExport when (phase == 0 && export == 0) {
                export = 6;
            }
            action Plan when (phase == 0) {
                phase = 1;
            }
            action TreeMoves {
                held = if tree <= 1 { 0 } else { held };
            }

            action StartTree when (phase == 1 && tree == 0 && red == 0) {
                tree = 1;
            }
            action StartBuild when (phase == 1 && build == 0 && red == 0 && tree > 0) {
                build = 1;
            }
            action StartExport when (phase == 1 && export == 0 && red == 0 && build > 0) {
                export = 1;
            }

            action TreeAcquire when (tree == 1 && S > tree_cpu + build_cpu + export_cpu) {
                tree_cpu = tree_cpu + 1;
            }
            action BuildAcquire when (build == 1 && S > tree_cpu + build_cpu + export_cpu) {
                build_cpu = build_cpu + 1;
            }
            action ExportAcquire when (export == 1 && S > tree_cpu + build_cpu + export_cpu) {
                export_cpu = export_cpu + 1;
            }
            action TreeRelease when (tree_cpu > 0) {
                tree_cpu = tree_cpu - 1;
            }
            action BuildRelease when (build_cpu > 0) {
                build_cpu = build_cpu - 1;
            }
            action ExportRelease when (export_cpu > 0) {
                export_cpu = export_cpu - 1;
            }

            action TreePass when (tree == 1) {
                tree = 2;
                tree_cpu = 0;
                tree_rec = if held == 1 { 2 } else { 0 };
            }
            action TreeRed when (tree == 1) {
                tree = 3;
                tree_cpu = 0;
                red = 1;
                tree_rec = if held == 1 { 1 } else { 0 };
            }
            action TreeTestRed when (tree == 1) {
                tree = 3;
                tree_cpu = 0;
                red = 1;
                kept_build = if build == 1 { 1 } else { 0 };
                tree_rec = if held == 1 { 1 } else { 0 };
            }
            action TreeCouldNotRun when (tree == 1) {
                tree = 4;
                tree_cpu = 0;
                tree_rec = if held == 1 { 1 } else { 0 };
            }
            action StopTree when (tree == 1 && red == 1) {
                tree = 5;
                tree_cpu = 0;
                tree_rec = if held == 1 { 1 } else { 0 };
            }

            action BuildPass when (build == 1) {
                build = 2;
                build_cpu = 0;
            }
            action BuildRed when (build == 1) {
                build = 3;
                build_cpu = 0;
                red = 1;
            }
            action BuildCouldNotRun when (build == 1) {
                build = 4;
                build_cpu = 0;
            }
            action StopBuild when (build == 1 && red == 1 && kept_build == 0) {
                build = 5;
                build_cpu = 0;
            }

            action ExportPush when (export == 1 && red == 0) {
                export = 7;
                pushed = 1;
            }
            action ExportPass when (export == 7) {
                export = 2;
                export_cpu = 0;
            }
            action ExportRed when (export == 1 || export == 7) {
                export = 3;
                export_cpu = 0;
                red = 1;
            }
            action ExportCouldNotRun when (export == 1 || export == 7) {
                export = 4;
                export_cpu = 0;
            }
            action StopExport when (export == 1 && red == 1) {
                export = 5;
                export_cpu = 0;
            }

            action QualifyPass when (
                phase == 1 && (tree == 2 || tree == 6) && (build == 2 || build == 6) &&
                    (export == 2 || export == 6)
            ) {
                phase = 2;
            }
            action QualifyRefuse when (
                phase == 1 && (tree == 0 || tree > 1) && (build == 0 || build > 1) &&
                    (export == 0 || (export > 1 && export <= 6)) &&
                    (red == 1 ||
                        (tree > 1 && build > 1 && export > 1 &&
                            (tree == 4 || build == 4 || export == 4)))
            ) {
                phase = 6;
            }

            action SkipBundle when (phase == 2 && bundle == 0) {
                bundle = 6;
                bundle_rec = 1;
            }
            action StartBundle when (phase == 2 && bundle == 0) {
                bundle = 1;
            }
            action BundlePass when (phase == 2 && bundle == 1) {
                bundle = 2;
                bundle_rec = 1;
            }
            action BundleRed when (phase == 2 && bundle == 1) {
                bundle = 3;
                phase = 6;
            }
            action BundleCouldNotRun when (phase == 2 && bundle == 1) {
                bundle = 4;
                phase = 6;
            }

            action Ready when (
                phase == 2 && (bundle == 2 || bundle == 6) && tree_rec == 2 && bundle_rec == 1
            ) {
                phase = 3;
                ready = 1;
            }
            action NotReady when (
                phase == 2 && (bundle == 2 || bundle == 6) && (tree_rec <= 1 || bundle_rec == 0)
            ) {
                phase = 6;
            }
            action Publish when (phase == 3) {
                phase = 4;
                public = 1;
            }
            action Cut when (phase == 4) {
                phase = 5;
                cut = 1;
            }

            action BundleBeforeTreeEnds when (
                Buggy == 1 && phase == 1 && bundle == 0 && (build == 2 || build == 6) && tree == 1
            ) {
                bundle = 1;
            }
            action TreeWritesOnAMovedTree when (Buggy == 2 && tree == 1 && held == 0) {
                tree = 2;
                tree_cpu = 0;
                tree_rec = 2;
            }
            action ExportPushesAfterARed when (Buggy == 3 && export == 1 && red == 1) {
                export = 7;
                pushed = 1;
                pushed_after_red = 1;
            }
            action BuildStartsAfterARed when (
                Buggy == 4 && phase == 1 && build == 0 && red == 1 && tree > 0
            ) {
                build = 1;
                started_after_red = 1;
            }
            action ExportAcquiresPastThePool when (Buggy == 5 && export == 1 && S > export_cpu) {
                export_cpu = export_cpu + 1;
            }
            action PublishBeforeReady when (Buggy == 6 && phase == 2 && public == 0) {
                public = 1;
            }
            action CutBeforeReady when (Buggy == 7 && phase == 2 && cut == 0) {
                cut = 1;
            }
            action ReadyOnVerdicts when (
                Buggy == 8 && phase == 2 && (bundle == 2 || bundle == 6)
            ) {
                phase = 3;
                ready = 1;
            }
            action BuildStartsBeforeTheTree when (
                Buggy == 9 && phase == 1 && build == 0 && red == 0 && tree == 0
            ) {
                build = 1;
            }
            action StopOnACouldNotRun when (
                Buggy == 10 && build == 1 && red == 0 && (tree == 4 || export == 4)
            ) {
                build = 5;
                build_cpu = 0;
            }
            action StopAPushInFlight when (Buggy == 11 && export == 7 && red == 1) {
                export = 5;
                export_cpu = 0;
            }
            action RefuseBeforeTheBranchesEnd when (Buggy == 12 && phase == 1 && red == 1) {
                phase = 6;
            }
            action PassOnACouldNotRun when (
                Buggy == 13 && phase == 1 && red == 0 && tree > 1 && build > 1 && export > 1 &&
                    export <= 6
            ) {
                phase = 2;
            }
            action BuildEndsHoldingTokens when (Buggy == 14 && build == 1 && build_cpu > 0) {
                build = 2;
            }
            action StopKeptBuild when (Buggy == 15 && build == 1 && red == 1 && kept_build == 1) {
                build = 5;
                build_cpu = 0;
            }

            invariant TestRedKeepsTheBuild: kept_build == 0 || build <= 4 || build > 5;

            invariant CutHoldsEveryRecord: cut == 0 || (tree_rec == 2 && bundle_rec == 1);
            invariant ReadyHoldsEveryRecord: ready == 0 || (tree_rec == 2 && bundle_rec == 1);
            invariant NothingPublicBeforeReady: public == 0 || ready == 1;
            invariant NoStartAfterARed: started_after_red == 0;
            invariant NoPushAfterARed: pushed_after_red == 0;
            invariant RecordsOnlyOfAHeldTree: tree_rec == 0 || tree == 6 || held == 1;
            invariant BundleAfterBuildAndTree:
                bundle == 0 || bundle == 6 ||
                    ((build == 2 || build == 6) && (tree == 2 || tree == 6));
            invariant ChargesWithinThePool: tree_cpu + build_cpu + export_cpu <= S;
            invariant NoTokenOutlivesItsBranch:
                (tree_cpu == 0 || tree == 1) && (build_cpu == 0 || build == 1) &&
                    (export_cpu == 0 || export == 1 || export == 7);
            invariant StartsInOrder:
                (build == 0 || build == 6 || tree > 0) && (export == 0 || export == 6 || build > 0);
            invariant StopOnlyOnARed:
                red == 1 ||
                    ((tree <= 4 || tree == 6) && (build <= 4 || build == 6) &&
                        (export <= 4 || export > 5));
            invariant PushInFlightFinishes:
                pushed == 0 || export == 7 || (export > 1 && export <= 4);
            invariant NoBranchOutlivesTheStep:
                phase <= 1 ||
                    ((tree == 0 || tree > 1) && (build == 0 || build > 1) &&
                        (export == 0 || (export > 1 && export <= 6)));
            invariant QualifyPassesOnlyOnEveryPass:
                phase <= 1 || phase == 6 ||
                    ((tree == 2 || tree == 6) && (build == 2 || build == 6) &&
                        (export == 2 || export == 6));
            bound QualifyBox:
                phase <= 6 && tree <= 6 && build <= 6 && export <= 7 && bundle <= 6 &&
                    tree_cpu <= S && build_cpu <= S && export_cpu <= S && red <= 1 &&
                    held <= 1 && tree_rec <= 2 && bundle_rec <= 1 && pushed <= 1 &&
                    ready <= 1 && public <= 1 && cut <= 1 && started_after_red <= 1 &&
                    pushed_after_red <= 1 && kept_build <= 1;
        }
    }
}
