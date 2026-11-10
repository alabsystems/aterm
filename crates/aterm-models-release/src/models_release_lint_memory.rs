// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The lint admission edge and qualify memory, in MiB. The lint peak is m112's
//! fresh 26,452,845,576 bytes, rounded up; its claim adds the shared 20% margin.
//! The fresh native peak is 46,219,853,824 bytes and compat's is
//! 25,890,799,616 bytes. The existing unit chooser rounds those to 44 and 25 GiB.
//! Neither link is made smaller. Two unit-admission drivers hold 2 GiB each. Memory is the
//! 75.8 GiB m3 case already used by the capacity conformance model.
//!
//! Buggy 1 admits a one-MiB lint claim, 2 ignores the native link's budget, and 3
//! launches slices before lint admission. Each breaks an independent safety law.

use aterm_spec::derive::*;

/// Lint reserves its envelope before slice launch; all subsequent work fits its claims.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn release_lint_memory_model() -> Model {
    aterm_spec::ty_model! {
        ReleaseLintMemory {
            const Buggy = 0;
            const Memory = 77619;
            const LintPeak = 25228;
            const LintClaim = 30273;
            const NativePeak = 44079;
            const NativeClaim = 45056;
            const CompatPeak = 24691;
            const CompatClaim = 25600;
            const Driver = 2048;
            var held = 0;
            var lint = 0;
            var lint_claimed = 0;
            var slices = 0;
            var native = 0;
            var compat = 0;

            action LintAdmitted when (lint == 0 && LintPeak <= LintClaim && held + LintClaim <= Memory) {
                lint = 1;
                lint_claimed = LintClaim;
                held = held + LintClaim;
            }
            action LintEnds when (lint == 1) {
                lint = 2;
                held = held - lint_claimed;
            }
            action TreeEndsWithoutLintSignal when (lint == 0) {
                lint = 2;
            }
            action SlicesStart when (slices == 0 && lint > 0 && held + Driver + Driver <= Memory) {
                slices = 1;
                held = held + Driver + Driver;
            }
            action NativeRuns when (slices == 1 && native == 0 && NativePeak <= NativeClaim && held + NativeClaim <= Memory) {
                native = 1;
                held = held + NativeClaim;
            }
            action CompatRuns when (slices == 1 && compat == 0 && CompatPeak <= CompatClaim && held + CompatClaim <= Memory) {
                compat = 1;
                held = held + CompatClaim;
            }
            action NativeEnds when (native == 1) {
                native = 2;
                held = held - NativeClaim - Driver;
            }
            action CompatEnds when (compat == 1) {
                compat = 2;
                held = held - CompatClaim - Driver;
            }
            action UndersizedLint when (Buggy == 1 && lint == 0 && held + 1 <= Memory) {
                lint = 1;
                lint_claimed = 1;
                held = held + 1;
            }
            action UncheckedNative when (Buggy == 2 && slices == 1 && native == 0) {
                native = 1;
                held = held + NativeClaim;
            }
            action SlicesBeforeLint when (Buggy == 3 && slices == 0 && lint == 0 && held + Driver + Driver <= Memory) {
                slices = 1;
                held = held + Driver + Driver;
            }
            invariant LintBeforeSlices: slices == 0 || lint > 0;
            invariant ClaimsCoverRecordedPeaks:
                (lint == 0 || lint == 2 || LintPeak <= lint_claimed) &&
                (native == 0 || native == 2 || NativePeak <= NativeClaim) &&
                (compat == 0 || compat == 2 || CompatPeak <= CompatClaim);
            invariant FitsMemory: held <= Memory;
            invariant FitsRecordedMemory:
                held + (if (lint == 1 && LintPeak > lint_claimed) { LintPeak - lint_claimed } else { 0 }) <= Memory;
        }
    }
}
