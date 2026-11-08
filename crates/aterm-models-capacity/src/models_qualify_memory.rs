// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Qualify's memory admission, in hundredths of a GiB, sized as on m3.

use aterm_spec::derive::*;

/// THE QUALIFY'S MEMORY, UNIT BY UNIT. Each release slice holds its driver ([`Driver`]) and each
/// of its compilers claims its own peak while it runs; the slices' last units, the fat-LTO links
/// (`Native`, `Compat`), are the heavy ones, and their claims LEAD
/// (`aterm_capacity::Need::leads`). The tree's compile holds its driver (`Tree`) and claims each
/// of its units, up to fourteen at once, one of them heavy, and ends once all have run. The
/// tree's driver is a PARENT while its units wait under it: it ends only once they have run. A
/// lead is SURE where it fits beside that parent, with its spare, both slices' drivers (the
/// ledger leaves a child's ancestors' claims out of its admission; while both are held that is
/// what the guards count, and the ledger is the stricter once one has ended), and each lead
/// ahead of it is sure. The tree's waiters count each waiting sure lead, and the leads keep
/// their own order of asking. A unit's spare is the tree's driver (2 GiB in a qualify), so each
/// guard counts every held claim and no margin. A slice ends with its last unit. Laws: `LeadsFirst`, no tree unit
/// is admitted where a waiting sure lead would then not fit; `FitsRealMemory`, what is held never
/// exceeds memory. The buggy dial at 1 lets a heavy tree unit pass a waiting sure lead (the
/// ledger's inside rule without `leads`) and a lead start unchecked; at 2 every lead is sure,
/// which deadlocks the qualify where a parent leaves a lead no room.
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
            const Tree = 200;
            const Unit = 200;
            const Heavy = 2352;
            var held = 0;
            var slices = 0;
            var tree = 0;
            var left = 13;
            var small = 0;
            var heavy = 0;
            var nwait = 0;
            var native = 0;
            var ndone = 0;
            var cwait = 0;
            var compat = 0;
            var cdone = 0;
            var cfirst = 0;
            var passed = 0;
            action SlicesStart when (slices == 0 && held + Driver + Driver <= Memory) {
                slices = 1;
                held = held + Driver + Driver;
            }
            action StartDriver when (slices == 1 && tree == 0
                && held + Tree + (if (nwait == 1 && ((if (tree == 1) { Tree } else { 0 }) + Native + Driver + Driver <= Memory || Buggy == 2) && (cwait == 0 || cfirst == 0 || ((if (tree == 1) { Tree } else { 0 }) + Compat + Driver + Driver <= Memory || Buggy == 2))) { Native } else { 0 })
                    + (if (cwait == 1 && ((if (tree == 1) { Tree } else { 0 }) + Compat + Driver + Driver <= Memory || Buggy == 2) && (nwait == 0 || cfirst == 1 || ((if (tree == 1) { Tree } else { 0 }) + Native + Driver + Driver <= Memory || Buggy == 2))) { Compat } else { 0 }) <= Memory) {
                tree = 1;
                held = held + Tree;
            }
            action SmallUnit when (tree == 1 && left > 0 && small + heavy <= 13
                && held + Unit + (if (nwait == 1 && ((if (tree == 1) { Tree } else { 0 }) + Native + Driver + Driver <= Memory || Buggy == 2) && (cwait == 0 || cfirst == 0 || ((if (tree == 1) { Tree } else { 0 }) + Compat + Driver + Driver <= Memory || Buggy == 2))) { Native } else { 0 })
                    + (if (cwait == 1 && ((if (tree == 1) { Tree } else { 0 }) + Compat + Driver + Driver <= Memory || Buggy == 2) && (nwait == 0 || cfirst == 1 || ((if (tree == 1) { Tree } else { 0 }) + Native + Driver + Driver <= Memory || Buggy == 2))) { Compat } else { 0 }) <= Memory) {
                small = small + 1;
                left = left - 1;
                held = held + Unit;
            }
            action FinishSmall when (small > 0) {
                small = small - 1;
                held = held - Unit;
            }
            action HeavyUnit when (tree == 1 && heavy == 0 && small <= 13
                && held + Heavy + (if (nwait == 1 && ((if (tree == 1) { Tree } else { 0 }) + Native + Driver + Driver <= Memory || Buggy == 2) && (cwait == 0 || cfirst == 0 || ((if (tree == 1) { Tree } else { 0 }) + Compat + Driver + Driver <= Memory || Buggy == 2))) { Native } else { 0 })
                    + (if (cwait == 1 && ((if (tree == 1) { Tree } else { 0 }) + Compat + Driver + Driver <= Memory || Buggy == 2) && (nwait == 0 || cfirst == 1 || ((if (tree == 1) { Tree } else { 0 }) + Native + Driver + Driver <= Memory || Buggy == 2))) { Compat } else { 0 }) <= Memory) {
                heavy = 1;
                held = held + Heavy;
            }
            action FinishHeavy when (heavy == 1) {
                heavy = 2;
                held = held - Heavy;
            }
            action TreeEnds when (tree == 1 && left == 0 && small == 0 && heavy == 2) {
                tree = 2;
                held = held - Tree;
            }
            action NativeAsks when (slices == 1 && nwait + native + ndone == 0) {
                nwait = 1;
            }
            action CompatAsks when (slices == 1 && cwait + compat + cdone == 0) {
                cwait = 1;
                cfirst = 1 - nwait;
            }
            action NativeRuns when (nwait == 1
                && held + Native
                    + (if (cwait == 1 && cfirst == 1 && ((if (tree == 1) { Tree } else { 0 }) + Compat + Driver + Driver <= Memory || Buggy == 2)) { Compat } else { 0 }) <= Memory) {
                nwait = 0;
                native = 1;
                held = held + Native;
            }
            action CompatRuns when (cwait == 1
                && held + Compat
                    + (if (nwait == 1 && cfirst == 0 && ((if (tree == 1) { Tree } else { 0 }) + Native + Driver + Driver <= Memory || Buggy == 2)) { Native } else { 0 }) <= Memory) {
                cwait = 0;
                compat = 1;
                held = held + Compat;
            }
            action NativeEnds when (native == 1) {
                native = 0;
                ndone = 1;
                held = held - Native - Driver;
            }
            action CompatEnds when (compat == 1) {
                compat = 0;
                cdone = 1;
                held = held - Compat - Driver;
            }
            action HeavyPasses when (Buggy == 1 && tree == 1 && heavy == 0 && nwait + cwait > 0
                && held + Heavy <= Memory
                && held + Heavy + (if (nwait == 1 && ((if (tree == 1) { Tree } else { 0 }) + Native + Driver + Driver <= Memory || Buggy == 2) && (cwait == 0 || cfirst == 0 || ((if (tree == 1) { Tree } else { 0 }) + Compat + Driver + Driver <= Memory || Buggy == 2))) { Native } else { 0 })
                    + (if (cwait == 1 && ((if (tree == 1) { Tree } else { 0 }) + Compat + Driver + Driver <= Memory || Buggy == 2) && (nwait == 0 || cfirst == 1 || ((if (tree == 1) { Tree } else { 0 }) + Native + Driver + Driver <= Memory || Buggy == 2))) { Compat } else { 0 }) > Memory) {
                heavy = 1;
                held = held + Heavy;
                passed = 1;
            }
            action NativeUnchecked when (Buggy == 1 && nwait == 1) {
                nwait = 0;
                native = 1;
                held = held + Native;
            }
            invariant LeadsFirst: passed == 0;
            invariant FitsRealMemory: held <= Memory;
        }
    }
}
