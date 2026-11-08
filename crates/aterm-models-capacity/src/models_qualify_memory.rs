// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Qualify's unit admission, in hundredths of a GiB, including the m3 regression.

use aterm_spec::derive::*;

/// Slices precede the driver. Each compiler unit reserves memory only while running;
/// releasing a slice permits wider admission without replacing the driver claim.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn qualify_memory_model() -> Model {
    aterm_spec::ty_model! {
        QualifyMemory {
            const Buggy = 0;
            const Memory = 7580;
            const Native = 4200;
            const Compat = 2300;
            const Driver = 200;
            const Unit = 200;
            const Heavy = 2352;
            const Full = 5112;
            var phase = 0;
            var held = 0;
            var early = 0;
            var native = 0;
            var compat = 0;
            var small = 0;
            var heavy = 0;
            action NativeFirst when (phase == 0 && held + Native <= Memory) {
                phase = 1;
                native = 1;
                held = held + Native;
            }
            action CompatNext when (phase == 1 && held + Compat <= Memory) {
                phase = 2;
                compat = 1;
                held = held + Compat;
            }
            action StartDriver when (phase == 2 && held + Driver <= Memory) {
                phase = 3;
                held = held + Driver;
            }
            action SmallUnit when (phase == 3 && small + heavy <= 13 && held + Unit <= Memory) {
                small = small + 1;
                held = held + Unit;
            }
            action FinishSmall when (phase == 3 && small > 0) {
                small = small - 1;
                held = held - Unit;
            }
            action HeavyUnit when (phase == 3 && heavy == 0 && small <= 13 && held + Heavy <= Memory) {
                heavy = 1;
                held = held + Heavy;
            }
            action FinishHeavy when (phase == 3 && heavy == 1) {
                heavy = 0;
                held = held - Heavy;
            }
            action NativeEnds when (phase == 3 && native == 1) {
                native = 0;
                held = held - Native;
            }
            action CompatEnds when (phase == 3 && compat == 1) {
                compat = 0;
                held = held - Compat;
            }
            action TreeFirst when (Buggy == 1 && phase == 0) {
                phase = 3;
                held = Full;
                early = 1;
            }
            action FullTree when (Buggy == 1 && phase == 2) {
                phase = 3;
                held = held + Full;
            }
            invariant SlicesFirst: early == 0;
            invariant FitsRealMemory: held <= Memory;
        }
    }
}
