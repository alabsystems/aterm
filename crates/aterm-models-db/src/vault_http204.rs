// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Actual HTTP 204 provenance, exact request/context admission, coalescing, and
/// immutable response time across guarded reads and the real state codec. Kind
/// selects concrete native-backed scenarios, not a server identity or vault proof.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn vault_http204_model() -> Model {
    aterm_spec::ty_model! {
        DbVaultHttp204 {
            const Buggy = 0;
            // 0..2 first actual204 (valid/tagless/bad ETag); 3..8 transport
            // failures/other statuses; 9..16 raw/cache/path or wrong context;
            // 17 early repeat,18 boundary repeat,19 changed context,20 clock
            // rollback,21 optional identity absent,22 materialization blocked,
            // 23..24 context/recipient changed after recording.
            const Kind = 3;
            scenario Kind = 0; scenario Kind = 1; scenario Kind = 2;
            scenario Kind = 4; scenario Kind = 5; scenario Kind = 6;
            scenario Kind = 7; scenario Kind = 8; scenario Kind = 9;
            scenario Kind = 10; scenario Kind = 11; scenario Kind = 12;
            scenario Kind = 13; scenario Kind = 14; scenario Kind = 15;
            scenario Kind = 16; scenario Kind = 17; scenario Kind = 18;
            scenario Kind = 19; scenario Kind = 20; scenario Kind = 21;
            scenario Kind = 22; scenario Kind = 23; scenario Kind = 24;
            var phase = 0; var wrote = 0; var stamp = 0; var shown = 0;
            action Receive when (phase == 0) {
                phase = 1;
                wrote = if (Buggy == 2 && Kind == 3) || (Buggy == 4 && Kind == 17)
                    || (Buggy == 5 && Kind == 12) {1}
                    else if Buggy == 3 && Kind == 0 {0}
                    else if Kind <= 2 || Kind == 18 || Kind == 19 || (Kind > 20 && 25 > Kind) {1} else {0};
                stamp = if Buggy == 6 && Kind == 0 {3}
                    else if (Buggy == 2 && Kind == 3) || (Buggy == 4 && Kind == 17)
                    || (Buggy == 5 && Kind == 12) {2}
                    else if Buggy == 3 && Kind == 0 {1}
                    else if Kind <= 2 || Kind == 18 || Kind == 19 || (Kind > 20 && 25 > Kind) {2} else {1};
            }
            action Read when (phase == 1) {
                phase = 2;
                stamp = if Buggy == 1 {3} else {stamp};
                shown = if Buggy == 7 && Kind == 23 {1}
                    else if Buggy == 9 && Kind == 22 {0}
                    else if Kind == 15 || Kind == 16 || Kind == 23 || Kind == 24 {0} else {1};
            }
            action Reload when (phase == 2) {
                phase = 3;
                stamp = if Buggy == 8 {3} else {stamp};
            }
            invariant ExactAdmission: phase == 0 || wrote == if Kind <= 2 || Kind == 18
                || Kind == 19 || (Kind > 20 && 25 > Kind) {1} else {0};
            invariant OriginalResponseTime: phase == 0 || stamp == if Kind <= 2 || Kind == 18
                || Kind == 19 || (Kind > 20 && 25 > Kind) {2} else {1};
            invariant ContextProjection: 2 > phase || shown == if Kind == 15 || Kind == 16
                || Kind == 23 || Kind == 24 {0} else {1};
            bound PhaseRange: phase <= 3;
            bound StampRange: stamp <= 3;
        }
    }
}
